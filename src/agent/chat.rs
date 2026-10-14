//! Basic text conversations over the native model stream. No tools or UI permissions yet.
pub use super::turns::{History, Turn, TurnState};
use super::{
    provider::{ModelClient, ModelError, ModelRepository},
    turns::Turns,
    Payload, Repository, Target,
};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    future::Future,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Model(ModelError),
    #[error("Agent 会话或配置不存在")]
    NotFound,
    #[error("该会话正在生成，请先停止或等待结束")]
    Busy,
    #[error("Agent 请求参数无效")]
    Invalid,
    #[error("会话绑定的终端与本次目标不一致，请新建会话")]
    TargetMismatch,
    #[error("附件和工具对话尚未接入")]
    Unsupported,
    #[error("历史过长或已有检查点，需要后续压缩/恢复支持")]
    HistoryLimit,
    #[error("上下文整理失败，原始历史已保留；请检查模型接口后手动重试")]
    CompactionFailed,
    #[error("当前请求与保留上下文无法降到可用容量，请缩短输入或新建会话")]
    CompactionLimit,
    #[error("工作区已停止")]
    Stopped,
    #[error("Agent 日志操作失败")]
    Storage,
    #[error("请求 ID 已被用于不同的内容")]
    Conflict,
    #[error("生成期间上下文已改变，旧回复未提交")]
    ContextChanged,
    #[error("上次请求已失败或中断；请查看历史，重新发送需使用新请求 ID")]
    Recorded(String),
}
pub type Result<T> = std::result::Result<T, Error>;
impl From<ModelError> for Error {
    fn from(error: ModelError) -> Self {
        match error {
            ModelError::NotFound => Self::NotFound,
            ModelError::Invalid(_) => Self::Invalid,
            ModelError::Stale => Self::Busy,
            _ => Self::Model(error),
        }
    }
}
impl From<super::Error> for Error {
    fn from(error: super::Error) -> Self {
        match error {
            super::Error::NotFound => Self::NotFound,
            super::Error::Stale => Self::Busy,
            super::Error::TargetMismatch => Self::TargetMismatch,
            _ => Self::Storage,
        }
    }
}
impl Error {
    pub fn code(&self) -> &str {
        match self {
            Self::Model(ModelError::Cancelled) => "AGENT_CANCELLED",
            Self::Model(ModelError::Timeout) => "MODEL_TIMEOUT",
            Self::Model(ModelError::Incomplete) => "MODEL_INCOMPLETE",
            Self::Model(ModelError::BudgetExceeded) => "MODEL_CONTEXT_LIMIT",
            Self::Model(ModelError::KeyMissing | ModelError::Credentials) => "MODEL_CREDENTIALS",
            Self::Model(ModelError::ResponseTooLarge) => "MODEL_RESPONSE_LIMIT",
            Self::Model(ModelError::Response) => "MODEL_PROTOCOL_FAILED",
            Self::Model(ModelError::ToolProtocol) => "MODEL_TOOL_PROTOCOL_FAILED",
            Self::Model(ModelError::Http(_)) => "MODEL_HTTP_ERROR",
            Self::Model(_) => "MODEL_REQUEST_FAILED",
            Self::NotFound => "AGENT_NOT_FOUND",
            Self::Busy => "AGENT_BUSY",
            Self::Invalid => "AGENT_INVALID_REQUEST",
            Self::TargetMismatch => "AGENT_TARGET_MISMATCH",
            Self::Unsupported => "FEATURE_NOT_MIGRATED",
            Self::HistoryLimit => "AGENT_HISTORY_LIMIT",
            Self::CompactionFailed => "AGENT_COMPACTION_FAILED",
            Self::CompactionLimit => "AGENT_COMPACTION_LIMIT",
            Self::Stopped => "BACKEND_UNAVAILABLE",
            Self::Storage => "AGENT_STORAGE_FAILED",
            Self::Conflict => "AGENT_REQUEST_CONFLICT",
            Self::ContextChanged => "AGENT_CONTEXT_CHANGED",
            Self::Recorded(code) => code,
        }
    }
    pub fn status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::Busy | Self::TargetMismatch => 409,
            Self::Invalid => 400,
            Self::Unsupported => 501,
            Self::Stopped => 503,
            Self::Model(ModelError::Cancelled) => 409,
            Self::Model(ModelError::Timeout) => 504,
            Self::HistoryLimit | Self::Model(ModelError::BudgetExceeded) => 413,
            Self::Model(ModelError::ResponseTooLarge) => 413,
            Self::Model(_) => 502,
            Self::CompactionFailed => 502,
            Self::CompactionLimit => 413,
            Self::Storage => 500,
            Self::Conflict | Self::ContextChanged => 409,
            Self::Recorded(code) => match code.as_str() {
                "MODEL_TIMEOUT" => 504,
                "AGENT_INTERRUPTED" | "AGENT_CANCELLED" | "AGENT_CONTEXT_CHANGED" => 409,
                "MODEL_RESPONSE_LIMIT" | "MODEL_CONTEXT_LIMIT" | "AGENT_HISTORY_LIMIT" => 413,
                _ => 502,
            },
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub agent_id: String,
    pub agent_name: String,
    pub model_name: String,
    pub agent_desc: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Attachment {
    pub file_id: String,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Request {
    #[serde(default = "new_request_id")]
    pub request_id: String,
    pub agent_id: String,
    pub user_id: String,
    pub session_id: String,
    pub terminal_session_id: String,
    pub message: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}
fn new_request_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    Text { content: String },
    Done { content: String },
    Error { content: String, code: String },
}
type ActiveKey = (String, String);
struct Active {
    request: String,
    token: CancellationToken,
    started: Arc<std::sync::atomic::AtomicBool>,
}
pub struct Manager {
    store: Arc<Store>,
    stop: CancellationToken,
    active: Mutex<HashMap<ActiveKey, Active>>,
}
struct Running<'a> {
    manager: &'a Manager,
    key: ActiveKey,
    token: CancellationToken,
    heartbeat: CancellationToken,
    request: Request,
    owner: String,
    partial: Arc<Mutex<String>>,
    finished: bool,
}
impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.token.cancel();
        self.heartbeat.cancel();
        self.manager.active.lock().unwrap().remove(&self.key);
        if !self.finished {
            let turns = Turns::new(self.manager.store.clone());
            let request = self.request.clone();
            let owner = self.owner.clone();
            let partial = self.partial.lock().unwrap().clone();
            // A dropped future is not a successful reply. Crash recovery also has a durable lease fallback.
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    let _ = turns.finish(
                        &request,
                        &owner,
                        TurnState::Interrupted,
                        &partial,
                        Some("AGENT_INTERRUPTED"),
                        None,
                    );
                });
            }
        }
    }
}
async fn worker<T: Send + 'static>(
    action: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|_| Error::Storage)?
}
fn validate(request: &Request) -> Result<()> {
    for id in [
        &request.agent_id,
        &request.user_id,
        &request.session_id,
        &request.request_id,
    ] {
        super::model::identifier(id).map_err(|_| Error::Invalid)?;
    }
    if request.request_id.len() > 64 {
        return Err(Error::Invalid);
    }
    Payload::UserMessage {
        text: request.message.clone(),
    }
    .validate()
    .map_err(|_| Error::Invalid)?;
    if !request.attachments.is_empty() {
        return Err(Error::Unsupported);
    }
    Ok(())
}
fn recorded(turn: &Turn) -> Error {
    match turn.state {
        TurnState::Running => Error::Busy,
        _ => Error::Recorded(
            turn.error_code
                .clone()
                .unwrap_or_else(|| "AGENT_INTERRUPTED".into()),
        ),
    }
}
impl Manager {
    pub fn new(store: Arc<Store>, stop: CancellationToken) -> Self {
        Self {
            store,
            stop,
            active: Mutex::new(HashMap::new()),
        }
    }
    pub fn shutdown(&self) {
        self.stop.cancel();
        for active in self.active.lock().unwrap().values() {
            active.token.cancel();
        }
    }
    pub async fn list(&self, user: String) -> Result<Vec<AgentConfig>> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        let store = self.store.clone();
        worker(move || {
            Ok(ModelRepository::new(store)
                .list(&user)?
                .into_iter()
                .filter(|p| p.ready)
                .map(|p| AgentConfig {
                    agent_id: p.id,
                    agent_name: p.display_name,
                    model_name: p.config.model.clone(),
                    agent_desc: format!("基础文本对话 · {}", p.config.model),
                })
                .collect())
        })
        .await
    }
    pub async fn create_session(&self, agent: String, user: String) -> Result<String> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        let store = self.store.clone();
        let stop = self.stop.clone();
        worker(move || {
            if stop.is_cancelled() {
                return Err(Error::Stopped);
            }
            if !ModelRepository::new(store.clone())
                .get(&agent, &user)?
                .ready
            {
                return Err(ModelError::KeyMissing.into());
            }
            Ok(Repository::new(store).create_session(&agent, &user)?.id)
        })
        .await
    }
    pub async fn receipt(&self, request: Request) -> Result<Option<Turn>> {
        validate(&request)?;
        let turns = Turns::new(self.store.clone());
        worker(move || turns.receipt(&request)).await
    }
    pub async fn history(
        &self,
        agent: String,
        user: String,
        session: String,
        after: u64,
        limit: usize,
    ) -> Result<History> {
        let turns = Turns::new(self.store.clone());
        worker(move || turns.history(&agent, &user, &session, after, limit)).await
    }
    pub async fn events(
        &self,
        agent: String,
        user: String,
        session: String,
        after: u64,
        limit: usize,
    ) -> Result<Vec<super::Event>> {
        if !(1..=50).contains(&limit) {
            return Err(Error::Invalid);
        }
        let store = self.store.clone();
        worker(move || {
            let repo = Repository::new(store);
            if repo.session(&session, &user)?.agent_id != agent {
                return Err(Error::NotFound);
            }
            Ok(repo.events(&session, &user, after, limit)?)
        })
        .await
    }
    pub async fn sessions(
        &self,
        agent: String,
        user: String,
        after: String,
        limit: usize,
    ) -> Result<Vec<super::Session>> {
        if !(1..=20).contains(&limit) {
            return Err(Error::Invalid);
        }
        let store = self.store.clone();
        worker(move|| {
            let db=store.database.lock().map_err(|_|Error::Storage)?;
            let cursor=if after.is_empty(){None} else {
                Some(db.query_row("SELECT created_at FROM agent_sessions WHERE id=?1 AND user_id=?2 AND agent_id=?3",rusqlite::params![after,user,agent],|r|r.get::<_,i64>(0)).map_err(|_|Error::NotFound)?)
            };
            let mut stmt=db.prepare("SELECT id FROM agent_sessions WHERE user_id=?1 AND agent_id=?2 AND (?3 IS NULL OR created_at<?3 OR (created_at=?3 AND id<?4)) ORDER BY created_at DESC,id DESC LIMIT ?5").map_err(|_|Error::Storage)?;
            let ids=stmt.query_map(rusqlite::params![user,agent,cursor,after,limit],|r|r.get::<_,String>(0)).map_err(|_|Error::Storage)?
                .collect::<std::result::Result<Vec<_>,_>>().map_err(|_|Error::Storage)?;
            ids.into_iter().map(|id|super::repository::read_session(&db,&id,&user).map_err(Error::from)).collect()
        }).await
    }
    /// Prefer stop_request: a delayed receipt for an old request must not stop a new turn.
    pub async fn stop_chat(&self, agent: String, user: String, session: String) -> Result<bool> {
        self.stop_request(agent, user, session, None).await
    }
    pub async fn stop_request(
        &self,
        agent: String,
        user: String,
        session: String,
        request: Option<String>,
    ) -> Result<bool> {
        let store = self.store.clone();
        let who = user.clone();
        let id = session.clone();
        let expected = agent.clone();
        worker(move || {
            if Repository::new(store).session(&id, &who)?.agent_id != expected {
                return Err(Error::NotFound);
            }
            Ok(())
        })
        .await?;
        // Preparing work has no turn row yet. Cancel its exact token before waiting on SQLite.
        let preparing = {
            let active = self.active.lock().unwrap();
            active
                .get(&(user.clone(), session.clone()))
                .filter(|a| {
                    request.as_ref().is_none_or(|id| a.request == *id)
                        && !a.started.load(std::sync::atomic::Ordering::Acquire)
                })
                .map(|a| a.token.clone())
        };
        if let Some(token) = &preparing {
            token.cancel();
        }
        let turns = Turns::new(self.store.clone());
        let who = user.clone();
        let id = session.clone();
        let stopped = worker(move || turns.stop(&agent, &who, &id, request.as_deref())).await?;
        if let Some(id) = stopped {
            if let Some(active) = self.active.lock().unwrap().get(&(user, session)) {
                if active.request == id {
                    active.token.cancel();
                }
            }
            Ok(true)
        } else {
            Ok(preparing.is_some())
        }
    }
    pub async fn chat<F, Fut>(
        &self,
        request: Request,
        target: Option<Target>,
        cancel: &CancellationToken,
        mut send: F,
    ) -> Result<()>
    where
        F: FnMut(StreamEvent) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled.into());
        }
        validate(&request)?;
        // Immutable receipts do not require the old model credentials or a live SSH terminal.
        if let Some(turn) = self.receipt(request.clone()).await? {
            if turn.state == TurnState::Completed {
                return send(StreamEvent::Done {
                    content: turn.result_text.ok_or(Error::Storage)?,
                })
                .await;
            }
            return Err(recorded(&turn));
        }
        if request.terminal_session_id
            != target
                .as_ref()
                .map(|t| t.terminal_session_id.as_str())
                .unwrap_or("")
        {
            return Err(Error::TargetMismatch);
        }
        if let Some(target) = &target {
            target.validate().map_err(|_| Error::Invalid)?;
        }
        let token = self.stop.child_token();
        let key = (request.user_id.clone(), request.session_id.clone());
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut active = self.active.lock().unwrap();
            if active.contains_key(&key) || active.len() >= 32 {
                return Err(Error::Busy);
            }
            active.insert(
                key.clone(),
                Active {
                    request: request.request_id.clone(),
                    token: token.clone(),
                    started: started.clone(),
                },
            );
        }
        let owner = uuid::Uuid::new_v4().to_string();
        let partial = Arc::new(Mutex::new(String::new()));
        let heartbeat = CancellationToken::new();
        let mut running = Running {
            manager: self,
            key,
            token: token.clone(),
            heartbeat: heartbeat.clone(),
            request: request.clone(),
            owner: owner.clone(),
            partial: partial.clone(),
            finished: true,
        };
        let prepared = super::compact::auto::Controller::new(self.store.clone())
            .prepare_text(&request, target.clone(), &token, cancel)
            .await?;
        if token.is_cancelled() || cancel.is_cancelled() {
            return Err(ModelError::Cancelled.into());
        }
        let runtime = prepared.runtime;
        let chat = prepared.request;
        let revision = prepared.revision;
        let target_for_admission = target;
        let turns = Turns::new(self.store.clone());
        let r = request.clone();
        let who = owner.clone();
        let requested_target = target_for_admission;
        let admission = worker(move || turns.begin(&r, requested_target, &who, revision)).await?;
        if !admission.1 {
            let turn = admission.0;
            if turn.state == TurnState::Completed {
                return send(StreamEvent::Done {
                    content: turn.result_text.ok_or(Error::Storage)?,
                })
                .await;
            }
            return Err(recorded(&turn));
        }
        running.finished = false;
        started.store(true, std::sync::atomic::Ordering::Release);
        let turns = Turns::new(self.store.clone());
        let r = request.clone();
        let who = owner.clone();
        let lease_stop = heartbeat.clone();
        let request_stop = token.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {biased;_=lease_stop.cancelled()=>break,_=tokio::time::sleep(std::time::Duration::from_secs(1))=>{}}
                let turns = turns.clone();
                let r = r.clone();
                let who = who.clone();
                if !matches!(
                    worker(move || turns.checkpoint_partial(&r, &who, None)).await,
                    Ok(true)
                ) {
                    request_stop.cancel();
                    break;
                }
            }
        });
        let client = ModelClient::new(runtime)?;
        let turns = Turns::new(self.store.clone());
        let mut persisted = 0usize;
        let mut last_save = std::time::Instant::now();
        let result = tokio::select! {biased;_=cancel.cancelled()=>Err(ModelError::Cancelled),
        result=client.stream(&chat,&token,|content| {
            let current={let mut text=partial.lock().unwrap();text.push_str(&content);text.clone()};
            let save=persisted==0||current.len().saturating_sub(persisted)>=1024||last_save.elapsed()>=std::time::Duration::from_millis(250);
            if save{persisted=current.len();last_save=std::time::Instant::now();}
            let future=send(StreamEvent::Text{content});let turns=turns.clone();let r=request.clone();let who=owner.clone();
            async move {
                if save && !matches!(worker(move||turns.checkpoint_partial(&r,&who,Some(&current))).await,Ok(true)){return Err(ModelError::Cancelled);}
                future.await.map_err(|e|match e{Error::Model(error)=>error,_=>ModelError::Cancelled})
            }
        })=>result};
        let (state, code, text) = match &result {
            Ok(c) => (
                TurnState::Completed,
                None,
                c.message.content.clone().ok_or(Error::Invalid)?,
            ),
            Err(error) => {
                let e = Error::from(error.clone());
                (
                    if matches!(error, ModelError::Cancelled) {
                        TurnState::Cancelled
                    } else {
                        TurnState::Failed
                    },
                    Some(e.code().to_owned()),
                    partial.lock().unwrap().clone(),
                )
            }
        };
        // The persistent stop flag and completion commit share a SQLite write transaction.
        let turns = Turns::new(self.store.clone());
        let r = request.clone();
        let who = owner.clone();
        let external_cancel = cancel.clone();
        let internal_cancel = token.clone();
        let completion = result.as_ref().ok().cloned();
        let turn = worker(move || {
            let cancelled = external_cancel.is_cancelled() || internal_cancel.is_cancelled();
            let (state, code) = if cancelled && state == TurnState::Completed {
                (TurnState::Cancelled, Some("AGENT_CANCELLED".into()))
            } else {
                (state, code)
            };
            turns.finish(&r, &who, state, &text, code.as_deref(), completion.as_ref())
        })
        .await?;
        running.finished = true;
        heartbeat.cancel();
        if turn.state == TurnState::Completed {
            send(StreamEvent::Done {
                content: turn.result_text.ok_or(Error::Storage)?,
            })
            .await
        } else if let Err(error) = result {
            let error: Error = error.into();
            if turn.error_code.as_deref() == Some(error.code()) {
                Err(error)
            } else {
                Err(recorded(&turn))
            }
        } else {
            Err(recorded(&turn))
        }
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    #[tokio::test]
    async fn preparing_stop_checks_agent_scope_before_signalling_the_exact_token() {
        let store = Arc::new(Store::in_memory().unwrap());
        let session = Repository::new(store.clone())
            .create_session("right", "default")
            .unwrap();
        let manager = Manager::new(store, CancellationToken::new());
        let token = CancellationToken::new();
        manager.active.lock().unwrap().insert(
            ("default".into(), session.id.clone()),
            Active {
                request: "preparing".into(),
                token: token.clone(),
                started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        );
        assert!(matches!(
            manager
                .stop_request(
                    "wrong".into(),
                    "default".into(),
                    session.id.clone(),
                    Some("preparing".into())
                )
                .await,
            Err(Error::NotFound)
        ));
        assert!(!token.is_cancelled());
        assert!(!manager
            .stop_request(
                "right".into(),
                "default".into(),
                session.id.clone(),
                Some("old-request".into())
            )
            .await
            .unwrap());
        assert!(!token.is_cancelled());
        assert!(manager
            .stop_request(
                "right".into(),
                "default".into(),
                session.id,
                Some("preparing".into())
            )
            .await
            .unwrap());
        assert!(token.is_cancelled());
    }
}
