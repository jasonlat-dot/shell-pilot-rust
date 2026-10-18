pub(crate) mod agent_exec;
mod host_key;
mod http;
mod output;
mod shared_shell;
mod sftp_access;
use crate::{
    error::Error,
    model::{Connection, ConnectionRequest, Credentials},
    store::Store,
};
use host_key::HostVerifier;
pub(crate) use http::routes;
pub use output::{ConnectionState, DisconnectReason, ReadResult};
use output::{Output, Session};
use russh::{
    client,
    keys::{Algorithm, EcdsaCurve, HashAlg, PrivateKeyWithHashAlg},
    Channel, ChannelMsg,
};
use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};
use tokio_util::sync::CancellationToken;

/// Non-secret identity captured from the connection actually used to open this channel.
#[derive(Clone)]
pub(crate) struct AgentIdentity {
    pub connection_id: String,
    pub user_id: String,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub host_key: String,
}

const HISTORY_TTL: Duration = Duration::from_secs(300);
const MAX_REQUESTS: usize = 4096;

#[derive(Clone)]
pub struct TerminalSettings {
    pub poll_wait: Duration,
    pub idle_timeout: Duration,
    pub buffer_bytes: usize,
    pub channels_per_transport: usize,
    pub transports_per_connection: usize,
}
impl Default for TerminalSettings {
    fn default() -> Self {
        Self {
            poll_wait: Duration::from_secs(25),
            idle_timeout: Duration::from_secs(1800),
            buffer_bytes: 1024 * 1024,
            channels_per_transport: 8,
            transports_per_connection: 32,
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenRequest {
    pub connection_id: String,
    pub request_id: String,
    pub cols: Option<u32>,
    pub rows: Option<u32>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenResult {
    pub session_id: String,
    pub connection_id: String,
    pub initial_output: String,
}

// Close the actual wire if russh has spawned a handshake task and the
// connecting future is subsequently dropped.
struct NetworkGuard(std::net::TcpStream);
impl Drop for NetworkGuard {
    fn drop(&mut self) {
        let _ = self.0.shutdown(std::net::Shutdown::Both);
    }
}
pub(crate) struct Transport {
    _network: NetworkGuard,
    handle: Option<client::Handle<HostVerifier>>,
    identity: AgentIdentity,
    trust_profile: (bool, Option<String>),
}
impl Transport {
    fn matches_profile(&self, connection: &Connection) -> bool {
        self.identity.connection_id == connection.connection_id
            && self.identity.user_id == connection.user_id
            && self
                .identity
                .host
                .trim_matches(['[', ']'])
                .eq_ignore_ascii_case(connection.host.trim_matches(['[', ']']))
            && self.identity.port == connection.port
            && self.identity.username == connection.username
            && self.trust_profile.0 == connection.strict_host_key_check
            && self.trust_profile.1 == connection.known_hosts
    }
    pub(crate) fn is_closed(&self) -> bool {
        self.handle().is_closed()
    }
    pub(crate) fn shutdown(&self) {
        let _ = self._network.0.shutdown(std::net::Shutdown::Both);
    }
    pub(crate) async fn sftp_stream(
        self: Arc<Self>,
        limit: Duration,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<russh::ChannelStream<client::Msg>, Error> {
        let mut opening = open_channel(self.clone(), limit, slot, None).await?;
        let channel = opening.0.as_mut().unwrap();
        let setup = async {
            channel
                .request_subsystem(true, "sftp")
                .await
                .map_err(|_| Error::Ssh)?;
            loop {
                match channel.wait().await {
                    Some(ChannelMsg::Success) => return Ok(()),
                    Some(ChannelMsg::Failure | ChannelMsg::Close | ChannelMsg::Eof) | None => {
                        return Err(Error::Ssh)
                    }
                    Some(ChannelMsg::Data { .. } | ChannelMsg::ExtendedData { .. }) => {
                        return Err(Error::Ssh)
                    }
                    _ => {}
                }
            }
        };
        tokio::time::timeout(limit, setup)
            .await
            .map_err(|_| Error::ConnectTimeout)??;
        Ok(opening.0.take().unwrap().into_stream())
    }
    fn handle(&self) -> &client::Handle<HostVerifier> {
        self.handle.as_ref().expect("live transport")
    }
}
impl Drop for Transport {
    fn drop(&mut self) {
        if let (Some(handle), Ok(runtime)) =
            (self.handle.take(), tokio::runtime::Handle::try_current())
        {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(
                    Duration::from_secs(1),
                    handle.disconnect(russh::Disconnect::ByApplication, "", "en"),
                )
                .await;
            });
        }
    }
}
struct PooledTransport {
    transport: Arc<Transport>,
    slots: Arc<Semaphore>,
    total_slots: usize,
    observed_limit: AtomicUsize,
}
impl PooledTransport {
    fn active(&self) -> usize {
        self.total_slots - self.slots.available_permits()
    }
    fn available(&self) -> bool {
        !self.transport.is_closed() && self.active() < self.observed_limit.load(Ordering::SeqCst)
    }
}
#[derive(Default)]
struct Runtime {
    transports: Vec<Arc<PooledTransport>>,
}
struct Attempt {
    token: CancellationToken,
    started: bool,
    session_id: Option<String>,
    created: Instant,
}
// Cancellation also covers dropping an HTTP future during handshake/setup.
struct AttemptGuard {
    token: CancellationToken,
    complete: bool,
}
impl Drop for AttemptGuard {
    fn drop(&mut self) {
        if !self.complete {
            self.token.cancel();
        }
    }
}
struct OpeningChannel(
    Option<Channel<client::Msg>>,
    Option<tokio::sync::OwnedSemaphorePermit>,
);
impl Drop for OpeningChannel {
    fn drop(&mut self) {
        let lease = self.1.take();
        if let (Some(channel), Ok(runtime)) = (self.0.take(), tokio::runtime::Handle::try_current())
        {
            runtime.spawn(async move {
                let _lease = lease;
                let _ = tokio::time::timeout(Duration::from_secs(1), channel.close()).await;
            });
        }
    }
}

struct OpeningWriter(Option<Arc<russh::ChannelWriteHalf<client::Msg>>>);
impl Drop for OpeningWriter {
    fn drop(&mut self) {
        if let (Some(writer), Ok(runtime)) = (self.0.take(), tokio::runtime::Handle::try_current())
        {
            runtime.spawn(async move {
                let _ = tokio::time::timeout(Duration::from_secs(1), writer.close()).await;
            });
        }
    }
}
pub struct TerminalManager {
    store: Arc<Store>,
    runtimes: Mutex<HashMap<String, Arc<AsyncMutex<Runtime>>>>,
    attempts: Mutex<HashMap<(String, String), Attempt>>,
    sessions: Mutex<HashMap<String, Arc<Session>>>,
    slots: Arc<Semaphore>,
    opening_slots: Arc<Semaphore>,
    settings: TerminalSettings,
    shutdown: CancellationToken,
}

impl TerminalManager {
    pub fn new(store: Arc<Store>) -> Self {
        Self::with_settings(store, TerminalSettings::default())
    }
    pub fn with_settings(store: Arc<Store>, mut settings: TerminalSettings) -> Self {
        settings.buffer_bytes = settings.buffer_bytes.max(4);
        settings.channels_per_transport = settings.channels_per_transport.clamp(1, 64);
        settings.transports_per_connection = settings.transports_per_connection.clamp(1, 256);
        Self {
            store,
            runtimes: Mutex::new(HashMap::new()),
            attempts: Mutex::new(HashMap::new()),
            sessions: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(256)),
            opening_slots: Arc::new(Semaphore::new(64)),
            settings,
            shutdown: CancellationToken::new(),
        }
    }
    fn runtime(&self, id: &str) -> Arc<AsyncMutex<Runtime>> {
        self.runtimes
            .lock()
            .unwrap()
            .entry(id.into())
            .or_default()
            .clone()
    }
    async fn blocking<T: Send + 'static>(
        &self,
        action: impl FnOnce(&Store) -> Result<T, Error> + Send + 'static,
    ) -> Result<T, Error> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || action(&store))
            .await
            .map_err(|_| Error::Worker)?
    }
    async fn pooled_transport(
        &self,
        runtime: &mut Runtime,
        connection: &Connection,
        credentials: &Credentials,
        fresh: bool,
    ) -> Result<Arc<PooledTransport>, Error> {
        runtime
            .transports
            .retain(|entry| !entry.transport.is_closed() && entry.active() > 0);
        if let Some(entry) = runtime.transports.iter().find(|entry| {
            !fresh && entry.available() && entry.transport.matches_profile(connection)
        }) {
            return Ok(entry.clone());
        }
        if runtime.transports.len() >= self.settings.transports_per_connection {
            return Err(Error::Busy);
        }
        let entry = Arc::new(PooledTransport {
            transport: Arc::new(authenticate(connection, credentials.clone()).await?),
            slots: Arc::new(Semaphore::new(self.settings.channels_per_transport)),
            total_slots: self.settings.channels_per_transport,
            observed_limit: AtomicUsize::new(self.settings.channels_per_transport),
        });
        runtime.transports.push(entry.clone());
        Ok(entry)
    }
    fn prune(&self) {
        self.sessions.lock().unwrap().retain(|_, session| {
            session
                .output
                .lock()
                .unwrap()
                .ended_at
                .is_none_or(|ended| ended.elapsed() < HISTORY_TTL)
        });
        // Keep active requests until their sessions end, even if they are old.
        let sessions = self.sessions.lock().unwrap();
        self.attempts.lock().unwrap().retain(|_, attempt| {
            attempt.created.elapsed() < HISTORY_TTL
                || (attempt.started && !attempt.token.is_cancelled())
                || attempt
                    .session_id
                    .as_ref()
                    .and_then(|id| sessions.get(id))
                    .is_some_and(|s| s.output.lock().unwrap().reason.is_none())
        });
    }
    fn validate_ids(connection: &str, request: &str) -> Result<(), Error> {
        if connection.trim().is_empty()
            || request.trim().is_empty()
            || connection.len() > 128
            || request.len() > 128
        {
            return Err(Error::Invalid(
                "connectionId/requestId 不能为空且不能超过 128 字节".into(),
            ));
        }
        Ok(())
    }
    fn start_attempt(&self, connection: &str, request: &str) -> Result<CancellationToken, Error> {
        self.prune();
        let mut attempts = self.attempts.lock().unwrap();
        if attempts.len() >= MAX_REQUESTS {
            return Err(Error::Busy);
        }
        let attempt = attempts
            .entry((connection.into(), request.into()))
            .or_insert_with(|| Attempt {
                token: CancellationToken::new(),
                started: false,
                session_id: None,
                created: Instant::now(),
            });
        if attempt.token.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if attempt.started {
            return Err(Error::Invalid(
                "requestId 已使用，请为每次连接生成新 ID".into(),
            ));
        }
        attempt.started = true;
        Ok(attempt.token.clone())
    }

    pub async fn connect(&self, request: OpenRequest) -> Result<OpenResult, Error> {
        Self::validate_ids(&request.connection_id, &request.request_id)?;
        let cols = request.cols.unwrap_or(120);
        let rows = request.rows.unwrap_or(24);
        dimensions(cols, rows)?;
        let token = self.start_attempt(&request.connection_id, &request.request_id)?;
        let mut guard = AttemptGuard {
            token: token.clone(),
            complete: false,
        };
        let operation = async {
            let slot = self
                .slots
                .clone()
                .try_acquire_owned()
                .map_err(|_| Error::Busy)?;
            let runtime = self.runtime(&request.connection_id);
            let mut runtime = runtime.lock().await;
            let id = request.connection_id.clone();
            let (connection, credentials) = self
                .blocking(move |store| Ok((store.get(&id)?, store.credentials(&id)?)))
                .await?;
            let setup = async {
                let (entry, mut opening) = {
                    let mut opened = None;
                    for attempt in 0..2 {
                        let entry = self
                            .pooled_transport(&mut runtime, &connection, &credentials, attempt > 0)
                            .await?;
                        let channel_slot = entry
                            .slots
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| Error::Busy)?;
                        let opening_slot = self
                            .opening_slots
                            .clone()
                            .try_acquire_owned()
                            .map_err(|_| Error::Busy)?;
                        match open_channel(
                            entry.transport.clone(),
                            Duration::from_secs(u64::from(connection.connect_timeout)),
                            opening_slot,
                            Some(channel_slot),
                        )
                        .await
                        {
                            Ok(opening) => {
                                opened = Some((entry, opening));
                                break;
                            }
                            Err(Error::ChannelRejected) if attempt == 0 && entry.active() > 0 => {
                                entry
                                    .observed_limit
                                    .fetch_min(entry.active(), Ordering::SeqCst);
                                // Only channel creation is retried, before PTY/shell/startupCommand.
                                // Existing windows stay on their authenticated transport.
                            }
                            Err(error) => return Err(error),
                        }
                    }
                    opened.ok_or(Error::ChannelRejected)?
                };
                let mut initial = Vec::new();
                let channel = opening.0.as_mut().unwrap();
                channel
                    .request_pty(true, "xterm-256color", cols, rows, 0, 0, &[])
                    .await
                    .map_err(|_| Error::Ssh)?;
                confirmation(channel, &mut initial, self.settings.buffer_bytes).await?;
                channel.request_shell(true).await.map_err(|_| Error::Ssh)?;
                confirmation(channel, &mut initial, self.settings.buffer_bytes).await?;
                if let Some(command) = connection
                    .startup_command
                    .as_ref()
                    .filter(|s| !s.is_empty())
                {
                    let command = format!("{}\n", command.trim_end_matches(['\r', '\n']));
                    channel
                        .data_bytes(command.into_bytes())
                        .await
                        .map_err(|_| Error::Ssh)?;
                }
                let channel_slot = opening.1.take().unwrap();
                let (reader, writer) = opening.0.take().unwrap().split();
                let writer = Arc::new(writer);
                let mut opening_writer = OpeningWriter(Some(writer.clone()));
                let session_id = uuid::Uuid::new_v4().to_string();
                let session = Arc::new(Session {
                    id: session_id.clone(),
                    connection_id: request.connection_id.clone(),
                    identity: entry.transport.identity.clone(),
                    transport: Arc::downgrade(&entry),
                    trust_profile: entry.transport.trust_profile.clone(),
                    writer: Mutex::new(Some(writer.clone())),
                    input_gate: Arc::new(AsyncMutex::new(())),
                    input_revision: std::sync::atomic::AtomicU64::new(0),
                    input_dirty: std::sync::atomic::AtomicBool::new(false),
                    shell_uncertain: std::sync::atomic::AtomicBool::new(false),
                    shell_probe: Mutex::new(None),
                    shell_display: Mutex::new(None),
                    terminal_size: Mutex::new((cols, rows)),
                    size_changed: Notify::new(),
                    output: Mutex::new(Output::new()),
                    changed: Notify::new(),
                    stop: CancellationToken::new(),
                });
                session
                    .output
                    .lock()
                    .unwrap()
                    .append(&initial, self.settings.buffer_bytes);
                // Match lock order in prune: sessions -> attempts. Publishing and
                // cancel lookup are atomic with respect to the request record.
                {
                    let mut sessions = self.sessions.lock().unwrap();
                    let mut attempts = self.attempts.lock().unwrap();
                    if token.is_cancelled() || self.shutdown.is_cancelled() {
                        return Err(Error::Cancelled);
                    }
                    sessions.insert(session_id.clone(), session.clone());
                    attempts
                        .get_mut(&(request.connection_id.clone(), request.request_id.clone()))
                        .unwrap()
                        .session_id = Some(session_id.clone());
                }
                let initial_output = std::mem::take(&mut session.output.lock().unwrap().text);
                opening_writer.0.take();
                let settings = self.settings.clone();
                let reader_runtime = self.runtime(&request.connection_id);
                let attempt_token = token.clone();
                tokio::spawn(async move {
                    let _slot = slot;

                    run_reader(session, reader, writer, attempt_token, settings).await;
                    drop(channel_slot);
                    let mut runtime = reader_runtime.lock().await;
                    runtime
                        .transports
                        .retain(|cached| !Arc::ptr_eq(cached, &entry) || entry.active() > 0);
                });
                Ok(OpenResult {
                    session_id,
                    connection_id: request.connection_id.clone(),
                    initial_output,
                })
            };
            let result = tokio::time::timeout(
                Duration::from_secs(u64::from(connection.connect_timeout)),
                setup,
            )
            .await
            .map_err(|_| Error::ConnectTimeout)?;
            runtime
                .transports
                .retain(|entry| !entry.transport.is_closed() && entry.active() > 0);
            result
        };
        let result = tokio::select! {
            biased;
            _ = self.shutdown.cancelled() => Err(Error::Cancelled),
            _ = token.cancelled() => Err(Error::Cancelled),
            value = operation => value,
        };
        if result.is_ok() {
            guard.complete = true;
        }
        result
    }

    pub fn cancel(&self, connection: &str, request: &str) -> Result<(), Error> {
        Self::validate_ids(connection, request)?;
        self.prune();
        let session_id = {
            let mut attempts = self.attempts.lock().unwrap();
            if attempts.len() >= MAX_REQUESTS
                && !attempts.contains_key(&(connection.into(), request.into()))
            {
                return Err(Error::Busy);
            }
            let attempt = attempts
                .entry((connection.into(), request.into()))
                .or_insert_with(|| Attempt {
                    token: CancellationToken::new(),
                    started: false,
                    session_id: None,
                    created: Instant::now(),
                });
            attempt.token.cancel();
            attempt.session_id.clone()
        };
        if let Some(id) = session_id {
            self.close(&id);
        }
        Ok(())
    }

    fn session(&self, id: &str) -> Result<Arc<Session>, Error> {
        self.sessions
            .lock()
            .unwrap()
            .get(id)
            .cloned()
            .ok_or(Error::TerminalMissing)
    }
    pub(crate) fn agent_identity(&self, id: &str) -> Option<(AgentIdentity, bool)> {
        self.prune();
        let session = self.session(id).ok()?;
        Some((
            session.identity.clone(),
            session.state().connected && !session.stop.is_cancelled(),
        ))
    }
    pub fn connected(&self, id: &str) -> ConnectionState {
        self.prune();
        self.session(id)
            .map(|s| s.state())
            .unwrap_or_else(|_| ConnectionState {
                session_id: id.into(),
                connection_id: None,
                connected: false,
                disconnect_reason: Some(DisconnectReason::SessionNotFound),
                reconnect_allowed: false,
            })
    }
    pub async fn read(&self, id: &str) -> ReadResult {
        self.read_limited(id, usize::MAX).await
    }
    /// IPC emits bounded UTF-8 frames and leaves the rest in the session's bounded buffer.
    pub async fn read_chunk(&self, id: &str) -> ReadResult {
        self.read_limited(id, 65536).await
    }
    async fn read_limited(&self, id: &str, limit: usize) -> ReadResult {
        let Ok(session) = self.session(id) else {
            return ReadResult::new(
                "DISCONNECTED",
                "".into(),
                false,
                false,
                Some(DisconnectReason::SessionNotFound),
            );
        };
        let generation = {
            let mut output = session.output.lock().unwrap();
            output.generation = output.generation.wrapping_add(1);
            output.generation
        };
        session.changed.notify_waiters();
        let deadline = tokio::time::Instant::now() + self.settings.poll_wait;
        loop {
            let changed = session.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            {
                let mut output = session.output.lock().unwrap();
                let connected = output.reason.is_none();
                if output.generation != generation {
                    return ReadResult::new("REPLACED", "".into(), connected, false, None);
                }
                if !output.text.is_empty() {
                    let mut end = output.text.len().min(limit);
                    while !output.text.is_char_boundary(end) {
                        end -= 1;
                    }
                    let text = output.text.drain(..end).collect();
                    let overflow = std::mem::take(&mut output.overflow);
                    return ReadResult::new("DATA", text, connected, overflow, None);
                }
                if let Some(reason) = output.reason {
                    let status = if reason == DisconnectReason::ReaderError {
                        "READER_ERROR"
                    } else {
                        "DISCONNECTED"
                    };
                    return ReadResult::new(status, "".into(), false, false, Some(reason));
                }
                if tokio::time::Instant::now() >= deadline {
                    return ReadResult::new("TIMEOUT", "".into(), true, false, None);
                }
            }
            let _ = tokio::time::timeout_at(deadline, &mut changed).await;
        }
    }
    pub async fn write(&self, id: &str, input: String) -> Result<(), Error> {
        if input.len() > 256 * 1024 {
            return Err(Error::Invalid("单次终端输入不能超过 256 KiB".into()));
        }
        let session = self.session(id)?;
        let _input = session.input_gate.try_lock().map_err(|_| {
            Error::Invalid(
                "AI 工具正在使用此终端，请等待完成或先停止生成；本次手工输入未发送。".into(),
            )
        })?;
        if !input.is_empty() {
            session.input_revision.fetch_add(1, Ordering::SeqCst);
            session
                .input_dirty
                .store(!input.ends_with(['\r', '\n', '\u{3}']), Ordering::SeqCst);
        }
        let writer = session
            .writer
            .lock()
            .unwrap()
            .clone()
            .ok_or(Error::TerminalMissing)?;
        session.output.lock().unwrap().last_active = Instant::now();
        tokio::select! {
            _ = session.stop.cancelled() => Err(Error::TerminalMissing),
            result = tokio::time::timeout(Duration::from_secs(10), writer.data_bytes(input.into_bytes())) =>
                result.map_err(|_| Error::Ssh)?.map_err(|_| Error::Ssh),
        }
    }
    pub async fn resize(&self, id: &str, cols: u32, rows: u32) -> Result<(), Error> {
        dimensions(cols, rows)?;
        let session = self.session(id)?;
        let writer = session
            .writer
            .lock()
            .unwrap()
            .clone()
            .ok_or(Error::TerminalMissing)?;
        session.output.lock().unwrap().last_active = Instant::now();
        let result = tokio::select! {
            _ = session.stop.cancelled() => Err(Error::TerminalMissing),
            result = tokio::time::timeout(Duration::from_secs(5), writer.window_change(cols, rows, 0, 0)) =>
                result.map_err(|_| Error::Ssh)?.map_err(|_| Error::Ssh),
        };
        if result.is_ok() {
            *session.terminal_size.lock().unwrap() = (cols, rows);
            session.size_changed.notify_one();
        }
        result
    }
    pub fn close(&self, id: &str) {
        if let Ok(session) = self.session(id) {
            session.finish(DisconnectReason::ClientClosed);
        }
    }

    pub async fn delete_connection(&self, id: String) -> Result<(), Error> {
        let runtime = self.runtime(&id);
        let mut runtime = runtime.lock().await;
        if self
            .sessions
            .lock()
            .unwrap()
            .values()
            .any(|s| s.connection_id == id && s.output.lock().unwrap().reason.is_none())
        {
            return Err(Error::Invalid(
                "该连接仍被其他窗口使用，请先关闭所有终端窗口".into(),
            ));
        }
        self.blocking(move |s| s.delete(&id)).await?;
        runtime.transports.clear();
        Ok(())
    }
    pub async fn update_connection(&self, request: ConnectionRequest) -> Result<Connection, Error> {
        let id = request
            .connection_id
            .clone()
            .ok_or_else(|| Error::Invalid("更新时必须提供 connectionId".into()))?;
        let runtime = self.runtime(&id);
        let mut runtime = runtime.lock().await;
        let result = self.blocking(move |s| s.save(request, true)).await?;
        // Existing channels own their transport and continue; newly opened
        // windows use the new credentials/configuration.
        runtime.transports.clear();
        Ok(result)
    }
    pub async fn shutdown(&self) {
        self.shutdown.cancel();
        for attempt in self.attempts.lock().unwrap().values() {
            attempt.token.cancel();
        }
        for session in self.sessions.lock().unwrap().values() {
            session.finish(DisconnectReason::ClientClosed);
        }
        let runtimes = self
            .runtimes
            .lock()
            .unwrap()
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for runtime in runtimes {
            for entry in runtime.lock().await.transports.drain(..) {
                entry.transport.shutdown();
            }
        }
    }
}
impl Drop for TerminalManager {
    fn drop(&mut self) {
        self.shutdown.cancel();
        for attempt in self.attempts.get_mut().unwrap().values() {
            attempt.token.cancel();
        }
        for session in self.sessions.get_mut().unwrap().values() {
            session.finish(DisconnectReason::ClientClosed);
        }
        for runtime in self.runtimes.get_mut().unwrap().values() {
            if let Ok(runtime) = runtime.try_lock() {
                for entry in &runtime.transports {
                    entry.transport.shutdown();
                }
            }
        }
    }
}

fn dimensions(cols: u32, rows: u32) -> Result<(), Error> {
    if cols == 0 || rows == 0 || cols > 4096 || rows > 4096 {
        return Err(Error::Invalid("终端列数和行数必须在 1 到 4096 之间".into()));
    }
    Ok(())
}
pub(crate) async fn authenticate(
    connection: &Connection,
    credentials: Credentials,
) -> Result<Transport, Error> {
    let verifier = HostVerifier::new(connection)?;
    let rejected = verifier.rejected.clone();
    let host_key = verifier.identity.clone();
    let preferred = russh::Preferred {
        key: Cow::Owned(vec![
            Algorithm::Ed25519,
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP256,
            },
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP384,
            },
            Algorithm::Ecdsa {
                curve: EcdsaCurve::NistP521,
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha512),
            },
            Algorithm::Rsa {
                hash: Some(HashAlg::Sha256),
            },
        ]),
        host_key_certificates: Cow::Borrowed(&[]),
        compression: if connection.compression {
            Cow::Owned(vec![
                russh::compression::ZLIB_LEGACY,
                russh::compression::NONE,
            ])
        } else {
            Cow::Borrowed(&[russh::compression::NONE])
        },
        ..Default::default()
    };
    let config = Arc::new(client::Config {
        preferred,
        keepalive_interval: (connection.keepalive_interval > 0)
            .then(|| Duration::from_secs(u64::from(connection.keepalive_interval))),
        keepalive_max: 3,
        nodelay: true,
        channel_buffer_size: 32,
        ..Default::default()
    });
    let socket =
        tokio::net::TcpStream::connect((connection.host.trim_matches(['[', ']']), connection.port))
            .await
            .map_err(|_| Error::Ssh)?;
    socket.set_nodelay(true).map_err(|_| Error::Ssh)?;
    let socket = socket.into_std().map_err(|_| Error::Ssh)?;
    let network = NetworkGuard(socket.try_clone().map_err(|_| Error::Ssh)?);
    let socket = tokio::net::TcpStream::from_std(socket).map_err(|_| Error::Ssh)?;
    let handle = client::connect_stream(config, socket, verifier)
        .await
        .map_err(|_| {
            if rejected.load(Ordering::SeqCst) {
                Error::HostKey
            } else {
                Error::Ssh
            }
        })?;
    let mut transport = Transport {
        _network: network,
        handle: Some(handle),
        identity: AgentIdentity {
            connection_id: connection.connection_id.clone(),
            user_id: connection.user_id.clone(),
            host: connection.host.clone(),
            port: connection.port,
            username: connection.username.clone(),
            host_key: host_key.lock().unwrap().clone().ok_or(Error::HostKey)?,
        },
        trust_profile: (
            connection.strict_host_key_check,
            connection.known_hosts.clone(),
        ),
    };
    let handle = transport.handle.as_mut().unwrap();
    let result = if connection.auth_type == 1 {
        let password = credentials
            .password
            .ok_or_else(|| Error::Invalid("该连接未保存密码".into()))?;
        handle
            .authenticate_password(&connection.username, password)
            .await
            .map_err(|_| Error::Ssh)?
    } else {
        let private = credentials
            .private_key
            .ok_or_else(|| Error::Invalid("该连接未保存私钥".into()))?;
        let passphrase = credentials.password;
        let key = tokio::task::spawn_blocking(move || {
            russh::keys::decode_secret_key(&private, passphrase.as_deref())
        })
        .await
        .map_err(|_| Error::Worker)?
        .map_err(|_| Error::Invalid("私钥格式不合法、加密口令错误或算法不受支持".into()))?;
        let hash = if key.algorithm().is_rsa() {
            Some(
                handle
                    .best_supported_rsa_hash()
                    .await
                    .map_err(|_| Error::Ssh)?
                    .flatten()
                    .unwrap_or(HashAlg::Sha512),
            )
        } else {
            None
        };
        handle
            .authenticate_publickey(
                &connection.username,
                PrivateKeyWithHashAlg::new(Arc::new(key), hash),
            )
            .await
            .map_err(|_| Error::Ssh)?
    };
    if !result.success() {
        return Err(Error::Authentication);
    }
    Ok(transport)
}
// Ownership survives dropping the HTTP waiter. If confirmation arrives later,
// the undelivered OpeningChannel closes just that channel. A server that never
// confirms within the connection timeout is treated as a failed transport.
async fn open_channel(
    transport: Arc<Transport>,
    limit: Duration,
    slot: tokio::sync::OwnedSemaphorePermit,
    channel_slot: Option<tokio::sync::OwnedSemaphorePermit>,
) -> Result<OpeningChannel, Error> {
    let (sender, receiver) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let _slot = slot;
        let mut channel_slot = channel_slot;
        let result =
            match tokio::time::timeout(limit, transport.handle().channel_open_session()).await {
                Ok(Ok(channel)) => Ok(OpeningChannel(Some(channel), channel_slot.take())),
                Ok(Err(russh::Error::ChannelOpenFailure(
                    russh::ChannelOpenFailure::AdministrativelyProhibited
                    | russh::ChannelOpenFailure::ResourceShortage,
                ))) => Err(Error::ChannelRejected),
                Ok(Err(_)) => Err(Error::Ssh),
                Err(_) => {
                    let _ = transport._network.0.shutdown(std::net::Shutdown::Both);
                    Err(Error::ConnectTimeout)
                }
            };
        drop(channel_slot);
        // Failed delivery drops the guard and closes an already confirmed channel.
        let _ = sender.send(result);
    });
    receiver.await.map_err(|_| Error::Worker)?
}
async fn confirmation(
    channel: &mut Channel<client::Msg>,
    initial: &mut Vec<u8>,
    limit: usize,
) -> Result<(), Error> {
    loop {
        match channel.wait().await {
            Some(ChannelMsg::Success) => return Ok(()),
            Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                if initial.len() + data.len() > limit {
                    return Err(Error::Ssh);
                }
                initial.extend_from_slice(&data);
            }
            Some(ChannelMsg::Failure | ChannelMsg::Close | ChannelMsg::Eof) | None => {
                return Err(Error::Ssh)
            }
            _ => {}
        }
    }
}
async fn run_reader(
    session: Arc<Session>,
    mut reader: russh::ChannelReadHalf,
    writer: Arc<russh::ChannelWriteHalf<client::Msg>>,
    attempt: CancellationToken,
    settings: TerminalSettings,
) {
    let mut tick = tokio::time::interval(
        settings
            .idle_timeout
            .min(Duration::from_secs(30))
            .max(Duration::from_millis(10)),
    );
    let reason = loop {
        tokio::select! {
            biased;
            _ = session.stop.cancelled() => break DisconnectReason::ClientClosed,
            _ = attempt.cancelled() => break DisconnectReason::ClientClosed,
            _ = tick.tick() => {
                if session.output.lock().unwrap().last_active.elapsed() >= settings.idle_timeout { break DisconnectReason::IdleTimeout; }
            }
            message = reader.wait() => match message {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    shared_shell::display_pty(&session, &data, settings.buffer_bytes);
                    session.changed.notify_waiters();
                    if let Some(probe) = session.shell_probe.lock().unwrap().as_ref() {
                        // This observer only acknowledges a harmless readiness probe. Tool results
                        // NEVER come from the PTY, including manual/background command output.
                        let _ = probe.try_send(data.to_vec());
                    }
                }
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => break DisconnectReason::ChannelDisconnected,
                Some(ChannelMsg::Failure) => break DisconnectReason::ReaderError,
                _ => {}
            }
        }
    };
    session.finish(reason);
    attempt.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(1), writer.close()).await;
}
