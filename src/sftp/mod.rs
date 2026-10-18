mod http;
pub mod model;
mod path;
mod transfer;
mod wire;
use crate::{
    store::Store,
    terminal::{authenticate, Transport},
};
use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
};
pub(crate) use http::routes;
use model::*;
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_util::sync::CancellationToken;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    pub status: StatusCode,
}
impl Error {
    fn new(code: &'static str, message: impl Into<String>, status: StatusCode) -> Self {
        Self {
            code,
            message: message.into(),
            status,
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new("SFTP_INVALID", message, StatusCode::BAD_REQUEST)
    }
    fn path(message: impl Into<String>) -> Self {
        Self::new("SFTP_PATH_FORBIDDEN", message, StatusCode::FORBIDDEN)
    }
    fn conflict(message: impl Into<String>) -> Self {
        Self::new("SFTP_CONFLICT", message, StatusCode::CONFLICT)
    }
    fn missing() -> Self {
        Self::new(
            "SFTP_NOT_FOUND",
            "连接、会话、任务或文件不存在",
            StatusCode::NOT_FOUND,
        )
    }
    fn busy() -> Self {
        Self::new(
            "SFTP_BUSY",
            "文件管理并发或任务容量已满，请稍后重试",
            StatusCode::TOO_MANY_REQUESTS,
        )
    }
    fn limit() -> Self {
        Self::new(
            "SFTP_LIMIT",
            "清单条目、路径深度或文件大小超出限制",
            StatusCode::PAYLOAD_TOO_LARGE,
        )
    }
    fn cancelled() -> Self {
        Self::new(
            "SFTP_CANCELLED",
            "传输已取消或会话已关闭",
            StatusCode::CONFLICT,
        )
    }
    fn timeout() -> Self {
        Self::new(
            "SFTP_IO_ERROR",
            "SFTP 操作长时间无进展",
            StatusCode::BAD_GATEWAY,
        )
    }
    fn io(message: impl Into<String>) -> Self {
        Self::new("SFTP_IO_ERROR", message, StatusCode::BAD_GATEWAY)
    }
    fn size() -> Self {
        Self::new(
            "SFTP_SIZE_MISMATCH",
            "文件字节数与清单不符",
            StatusCode::BAD_REQUEST,
        )
    }
    fn source() -> Self {
        Self::new(
            "SFTP_SOURCE_CHANGED",
            "远程源文件类型或大小已变化，请重新创建任务",
            StatusCode::CONFLICT,
        )
    }
    fn ssh(error: crate::error::Error) -> Self {
        match error {
            crate::error::Error::NotFound => Self::missing(),
            crate::error::Error::Invalid(message) => Self::invalid(message),
            other => Self::new(
                "SFTP_UNAVAILABLE",
                other.to_string(),
                StatusCode::BAD_GATEWAY,
            ),
        }
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        crate::error::failure(self.status, self.code, self.message)
    }
}
impl From<russh_sftp::client::error::Error> for Error {
    fn from(error: russh_sftp::client::error::Error) -> Self {
        use russh_sftp::{client::error::Error as E, protocol::StatusCode as C};
        match error {
            E::Status(s) if s.status_code == C::NoSuchFile => Self::missing(),
            E::Status(s) if s.status_code == C::PermissionDenied => Self::new(
                "SFTP_FORBIDDEN",
                "远程账号没有文件操作权限",
                StatusCode::FORBIDDEN,
            ),
            E::Status(s) if s.status_code == C::OpUnsupported => Self::new(
                "SFTP_UNSUPPORTED",
                "远程 SFTP 服务不支持所需操作",
                StatusCode::BAD_GATEWAY,
            ),
            E::Timeout => Self::timeout(),
            _ => Self::io("远程 SFTP 协议或文件操作失败"),
        }
    }
}
#[derive(Clone)]
pub struct Settings {
    pub max_sessions: usize,
    pub max_tasks: usize,
    pub max_items: usize,
    pub max_tracked_items: usize,
    pub idle_timeout: Duration,
    pub retention: Duration,
    pub reaper_interval: Duration,
    pub stream_idle_timeout: Duration,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            max_sessions: 50,
            max_tasks: 100,
            max_items: 10000,
            max_tracked_items: 100000,
            idle_timeout: Duration::from_secs(1800),
            retention: Duration::from_secs(1800),
            reaper_interval: Duration::from_secs(1),
            stream_idle_timeout: Duration::from_secs(120),
        }
    }
}
pub struct Manager {
    store: Arc<Store>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    session_slots: Arc<Semaphore>,
    openings: Arc<Semaphore>,
    transfers: Arc<Semaphore>,
    capacity: Arc<Semaphore>,
    pub settings: Settings,
    stop: CancellationToken,
}
pub(crate) struct Session {
    pub view: SessionView,
    pub transport: Arc<Transport>,
    // Session-only secret. Never serialized, logged, or written to saved connections.
    pub sudo_password: Mutex<Option<String>>,
    pub connect_timeout: Duration,
    pub openings: Arc<Semaphore>,
    pub slots: Arc<Semaphore>,
    pub metadata: Semaphore,
    pub tasks: Mutex<HashMap<String, Arc<Task>>>,
    pub stop: CancellationToken,
    pub subscription: Mutex<Option<CancellationToken>>,
    touched: Mutex<Instant>,
    active: AtomicUsize,
    _slot: OwnedSemaphorePermit,
}
impl Session {
    fn begin(self: &Arc<Self>) -> Active {
        self.active.fetch_add(1, Ordering::SeqCst);
        *self.touched.lock().unwrap() = Instant::now();
        Active(self.clone())
    }
    fn close(&self) {
        self.stop.cancel();
        self.sudo_password.lock().unwrap().take();
        for task in self.tasks.lock().unwrap().values() {
            task.cancel();
        }
        self.transport.shutdown();
    }
}
pub(crate) struct Active(Arc<Session>);
impl Drop for Active {
    fn drop(&mut self) {
        *self.0.touched.lock().unwrap() = Instant::now();
        self.0.active.fetch_sub(1, Ordering::SeqCst);
    }
}
impl Manager {
    pub fn new(store: Arc<Store>) -> Arc<Self> {
        Self::with_settings(store, Settings::default())
    }
    pub fn with_settings(store: Arc<Store>, settings: Settings) -> Arc<Self> {
        let manager = Arc::new(Self {
            store,
            sessions: Mutex::new(HashMap::new()),
            session_slots: Arc::new(Semaphore::new(settings.max_sessions)),
            openings: Arc::new(Semaphore::new(64)),
            transfers: Arc::new(Semaphore::new(4)),
            capacity: Arc::new(Semaphore::new(settings.max_tracked_items)),
            settings,
            stop: CancellationToken::new(),
        });
        if tokio::runtime::Handle::try_current().is_ok() {
            let weak = Arc::downgrade(&manager);
            let token = manager.stop.clone();
            let interval = manager
                .settings
                .reaper_interval
                .max(Duration::from_millis(10));
            tokio::spawn(async move {
                loop {
                    tokio::select! {_ = token.cancelled()=>break,_=tokio::time::sleep(interval)=>{let Some(manager)=weak.upgrade()else{break};manager.reap();}}
                }
            });
        }
        manager
    }
    fn reap(&self) {
        let mut sessions = self.sessions.lock().unwrap();
        sessions.retain(|_, session| {
            if session.stop.is_cancelled()
                || session.transport.is_closed()
                || (session.active.load(Ordering::SeqCst) == 0
                    && session.touched.lock().unwrap().elapsed() > self.settings.idle_timeout)
            {
                session.close();
                false
            } else {
                session.tasks.lock().unwrap().retain(|_, task| {
                    let data = task.data.lock().unwrap();
                    data.items.iter().any(|i| i.status == Status::Running)
                        || data.touched.elapsed() < self.settings.retention
                });
                true
            }
        });
    }
    pub fn shutdown(&self) {
        self.stop.cancel();
        for session in self.sessions.lock().unwrap().drain().map(|(_, s)| s) {
            session.close();
        }
    }
    pub async fn open(&self, request: OpenRequest) -> Result<SessionView> {
        if request
            .sudo_password
            .as_ref()
            .is_some_and(|value| value.len() > 4096 || value.contains(['\r', '\n', '\0']))
        {
            return Err(Error::invalid(
                "sudo 密码不能包含换行、空字符或超过 4096 字节",
            ));
        }
        if request.access_mode == AccessMode::Login && request.sudo_password.is_some() {
            return Err(Error::invalid("普通 SFTP 连接不接受 sudo 密码"));
        }
        if self.stop.is_cancelled() {
            return Err(Error::cancelled());
        }
        let slot = self
            .session_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let store = self.store.clone();
        let id = request.connection_id;
        let (connection, credentials) = tokio::task::spawn_blocking(move || {
            let connection = store.get(&id)?;
            if connection.user_id != "default" {
                return Err(crate::error::Error::NotFound);
            }
            Ok((connection, store.credentials(&id)?))
        })
        .await
        .map_err(|_| Error::io("读取连接失败"))?
        .map_err(Error::ssh)?;
        let timeout = Duration::from_secs(u64::from(connection.connect_timeout));
        let transport = tokio::select! {biased;_=self.stop.cancelled()=>return Err(Error::cancelled()),result=tokio::time::timeout(timeout,authenticate(&connection,credentials))=>result.map_err(|_|Error::timeout())?.map_err(Error::ssh)?};
        let provisional = Arc::new(Session {
            view: SessionView {
                sftp_session_id: uuid::Uuid::new_v4().to_string(),
                connection_id: connection.connection_id,
                root_path: "/".into(),
                login_username: connection.username.clone(),
                access_username: if request.access_mode == AccessMode::SudoRoot {
                    "root".into()
                } else {
                    connection.username.clone()
                },
                access_mode: request.access_mode,
                created_at: chrono::Utc::now(),
            },
            transport: Arc::new(transport),
            sudo_password: Mutex::new(request.sudo_password.filter(|value| !value.is_empty())),
            connect_timeout: timeout,
            openings: self.openings.clone(),
            slots: Arc::new(Semaphore::new(2)),
            metadata: Semaphore::new(1),
            tasks: Mutex::new(HashMap::new()),
            stop: self.stop.child_token(),
            subscription: Mutex::new(None),
            touched: Mutex::new(Instant::now()),
            active: AtomicUsize::new(0),
            _slot: slot,
        });
        let root = wire::operation(&provisional, async {
            let wire = wire::Wire::new(&provisional).await?;
            let root = match request
                .root_path
                .as_deref()
                .map(str::trim)
                .filter(|v| !v.is_empty())
            {
                Some(value) => {
                    let root = path::absolute(value)?;
                    wire.checked("/", &root, false).await?;
                    wire.realpath(&root).await?
                }
                None => wire.realpath(".").await?,
            };
            wire.checked("/", &root, false).await?;
            if !wire.stat(&root).await?.is_some_and(|m| m.is_dir()) {
                return Err(Error::invalid("根路径必须是目录"));
            }
            Ok(root)
        })
        .await?;
        let mut session = Arc::try_unwrap(provisional).map_err(|_| Error::io("初始化会话失败"))?;
        session.view.root_path = root;
        let session = Arc::new(session);
        let view = session.view.clone();
        // Synchronize publication with shutdown, so an in-flight open cannot outlive it.
        let mut sessions = self.sessions.lock().unwrap();
        if self.stop.is_cancelled() {
            session.close();
            return Err(Error::cancelled());
        }
        sessions.insert(view.sftp_session_id.clone(), session);
        Ok(view)
    }
    pub(crate) fn session(&self, id: &str) -> Result<Arc<Session>> {
        let session = self
            .sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or_else(Error::missing)?;
        if session.stop.is_cancelled() || session.transport.is_closed() {
            return Err(Error::missing());
        }
        Ok(session)
    }
    pub fn session_view(&self, id: &str) -> Result<SessionView> {
        Ok(self.session(id)?.view.clone())
    }
    pub fn close(&self, id: &str) -> Result<()> {
        let session = self
            .sessions
            .lock()
            .unwrap()
            .remove(id)
            .ok_or_else(Error::missing)?;
        session.close();
        Ok(())
    }
    pub async fn entries(&self, id: &str, path: Option<&str>) -> Result<Vec<Entry>> {
        let session = self.session(id)?;
        let _active = session.begin();
        wire::operation(&session, async {
            let _lock = session
                .metadata
                .acquire()
                .await
                .map_err(|_| Error::cancelled())?;
            let wire = wire::Wire::new(&session).await?;
            let path = wire
                .checked(
                    &session.view.root_path,
                    path.filter(|p| !p.is_empty())
                        .unwrap_or(&session.view.root_path),
                    false,
                )
                .await?;
            if !wire.stat(&path).await?.is_some_and(|m| m.is_dir()) {
                return Err(Error::invalid("只能浏览目录"));
            }
            wire.list(&path, self.settings.max_items).await
        })
        .await
    }
    pub async fn change_entry(&self, id: &str, path: &str, action: &str) -> Result<()> {
        let session = self.session(id)?;
        let _active = session.begin();
        wire::operation(&session, async {
            let _lock = session
                .metadata
                .acquire()
                .await
                .map_err(|_| Error::cancelled())?;
            let wire = wire::Wire::new(&session).await?;
            let target = wire
                .checked(&session.view.root_path, path, action != "delete")
                .await?;
            if target == session.view.root_path {
                return Err(Error::path("不能修改会话根目录"));
            }
            match action {
                "mkdir" => {
                    if wire.stat(&target).await?.is_some() {
                        return Err(Error::conflict("目标已存在"));
                    }
                    wire.raw
                        .mkdir(target, russh_sftp::protocol::FileAttributes::default())
                        .await?;
                }
                "file" => {
                    if wire.stat(&target).await?.is_some() {
                        return Err(Error::conflict("目标已存在"));
                    }
                    wire.empty_file(&target).await?;
                }
                "delete" => {
                    let attrs = wire.stat(&target).await?.ok_or_else(Error::missing)?;
                    if attrs.is_dir() {
                        wire.raw.rmdir(target).await?;
                    } else if attrs.is_regular() {
                        wire.raw.remove(target).await?;
                    } else {
                        return Err(Error::path("只允许删除普通文件或空目录"));
                    }
                }
                _ => return Err(Error::invalid("不支持的操作")),
            }
            Ok(())
        })
        .await
    }
    pub fn tasks(&self, id: &str) -> Result<Vec<TransferView>> {
        let session = self.session(id)?;
        let mut tasks = session
            .tasks
            .lock()
            .unwrap()
            .values()
            .map(|t| t.view())
            .collect::<Vec<_>>();
        tasks.sort_by(|a, b| a.transfer_id.cmp(&b.transfer_id));
        Ok(tasks)
    }
    pub(crate) fn task(&self, id: &str) -> Result<(Arc<Session>, Arc<Task>)> {
        let sessions = self
            .sessions
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for session in sessions {
            if session.stop.is_cancelled() {
                continue;
            }
            let task = session.tasks.lock().unwrap().get(id).cloned();
            if let Some(task) = task {
                return Ok((session, task));
            }
        }
        Err(Error::missing())
    }
    pub fn transfer_view(&self, id: &str) -> Result<TransferView> {
        Ok(self.task(id)?.1.view())
    }
    pub fn cancel(&self, id: &str) -> Result<()> {
        self.task(id)?.1.cancel();
        Ok(())
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        self.shutdown();
    }
}
