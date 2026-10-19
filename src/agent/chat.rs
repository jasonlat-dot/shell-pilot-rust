//! Desktop text and explicitly enabled SSH tool turns share durable lifecycle and cancellation.
mod execution;
mod manual_compact;
pub use super::turns::{History, Turn, TurnState};
use super::{
    provider::{ModelClient, ModelError, ModelRepository},
    turns::Turns,
    Payload, Repository, Target,
};
use crate::store::Store;
pub use manual_compact::CompactionResult;
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
    #[error("审批状态或任务已变化，请刷新后重新核对命令")]
    ApprovalChanged,
    #[error("工具执行结果未知；请先核查服务器状态，不会自动重试")]
    ToolUnknown,
    #[error("本次处理达到执行资源上限，请核对结果后继续")]
    ToolLimit,
    #[error("本次处理达到模型请求轮数上限，可在工具与 Skills 设置中调整")]
    ModelRoundLimit,
    #[error("本次处理达到工具调用次数上限，输出回查不占此额度，可在工具与 Skills 设置中调整")]
    ToolCallLimit,
    #[error("当前模型协议不支持工具调用")]
    ToolsUnsupported,
    #[error("Agent 会话或配置不存在")]
    NotFound,
    #[error("该会话正在生成，请先停止或等待结束")]
    Busy,
    #[error("Agent 请求参数无效")]
    Invalid,
    #[error("当前连接与任务的服务器身份不一致，请选择对应任务或新建服务器任务")]
    TargetMismatch,
    #[error("当前 SSH 连接不可用；仍可讨论已有记录，新建服务器任务或实时操作需先连接")]
    TargetUnavailable,
    #[error("{0}")]
    Attachment(String),
    #[error("{0}")]
    Memory(String),
    #[error("记忆已被修改或删除，请刷新列表后重新编辑")]
    MemoryChanged,
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
            Self::Model(ModelError::ContextMismatch) => "MODEL_CONTEXT_BINDING_CHANGED",
            Self::Model(ModelError::RemoteUnsupported) => "MODEL_RESPONSES_UNSUPPORTED",
            Self::Model(ModelError::BudgetExceeded) => "MODEL_CONTEXT_LIMIT",
            Self::Model(ModelError::KeyMissing | ModelError::Credentials) => "MODEL_CREDENTIALS",
            Self::Model(ModelError::ResponseTooLarge) => "MODEL_RESPONSE_LIMIT",
            Self::Model(ModelError::Response) => "MODEL_PROTOCOL_FAILED",
            Self::Model(ModelError::ToolProtocol) => "MODEL_TOOL_PROTOCOL_FAILED",
            Self::Model(ModelError::Http(_)) => "MODEL_HTTP_ERROR",
            Self::Model(_) => "MODEL_REQUEST_FAILED",
            Self::ApprovalChanged => "AGENT_APPROVAL_CHANGED",
            Self::ToolUnknown => "AGENT_TOOL_UNKNOWN",
            Self::ToolLimit => "AGENT_TOOL_LIMIT",
            Self::ModelRoundLimit => "AGENT_MODEL_ROUND_LIMIT",
            Self::ToolCallLimit => "AGENT_TOOL_CALL_LIMIT",
            Self::ToolsUnsupported => "AGENT_TOOLS_UNSUPPORTED",
            Self::NotFound => "AGENT_NOT_FOUND",
            Self::Busy => "AGENT_BUSY",
            Self::Invalid => "AGENT_INVALID_REQUEST",
            Self::TargetMismatch => "AGENT_TARGET_MISMATCH",
            Self::TargetUnavailable => "AGENT_TARGET_UNAVAILABLE",
            Self::Attachment(_) => "AGENT_ATTACHMENT_INVALID",
            Self::Memory(_) => "AGENT_MEMORY_INVALID",
            Self::MemoryChanged => "AGENT_MEMORY_CHANGED",
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
            Self::ApprovalChanged
            | Self::ToolUnknown
            | Self::ToolLimit
            | Self::ModelRoundLimit
            | Self::ToolCallLimit => 409,
            Self::ToolsUnsupported => 400,
            Self::NotFound => 404,
            Self::Busy | Self::TargetMismatch | Self::TargetUnavailable => 409,
            Self::Invalid | Self::Attachment(_) | Self::Memory(_) => 400,
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
            Self::Conflict | Self::ContextChanged | Self::MemoryChanged => 409,
            Self::Recorded(code) => match code.as_str() {
                "MODEL_TIMEOUT" => 504,
                "AGENT_INTERRUPTED"
                | "AGENT_CANCELLED"
                | "AGENT_CONTEXT_CHANGED"
                | "AGENT_TARGET_UNAVAILABLE" => 409,
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
    pub tools_available: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
#[serde(
    tag = "event",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum StreamEvent {
    AgentStart {
        agent_call_id: String,
        agent_name: String,
        task: String,
        parent_tool_call_id: String,
    },
    AgentResult {
        agent_call_id: String,
        agent_name: String,
        content: String,
        status: String,
    },
    Input {
        input: super::steering::Input,
    },
    ToolCall {
        tool_call_id: String,
        tool_name: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_call_id: Option<String>,
        command: Option<String>,
        status: String,
    },
    ToolResult {
        tool_call_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        agent_call_id: Option<String>,
        content: String,
        status: String,
    },
    Context {
        status: super::context_status::Status,
    },
    Text {
        content: String,
    },
    Done {
        content: String,
    },
    Error {
        content: String,
        code: String,
    },
}
type ActiveKey = (String, String);
struct Active {
    request: String,
    token: CancellationToken,
    started: Arc<std::sync::atomic::AtomicBool>,
}
pub struct Manager {
    pub(super) store: Arc<Store>,
    pub(super) stop: CancellationToken,
    pub(super) files: Option<Arc<crate::files::Manager>>,
    active: Arc<Mutex<HashMap<ActiveKey, Active>>>,
    live: super::live::Source,
    terminals: Option<Arc<crate::terminal::TerminalManager>>,
    approvals: super::approvals::Approvals,
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
    observation: Option<super::context_status::Observation>,
}
impl Drop for Running<'_> {
    fn drop(&mut self) {
        if let Some(observation) = self.observation.take() {
            let code = if self.token.is_cancelled() {
                "AGENT_CANCELLED"
            } else {
                "AGENT_INTERRUPTED"
            };
            if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                runtime.spawn_blocking(move || {
                    let _ = observation
                        .apply(super::context_status::Update::Finished(Some(code.into())));
                });
            }
        }
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
    crate::diagnostics::spawn_blocking(action)
        .await
        .map_err(|_| Error::Storage)?
}
async fn forward_context<F, Fut>(
    observation: &super::context_status::Observation,
    update: super::context_status::Update,
    send: &mut F,
) -> Result<()>
where
    F: FnMut(StreamEvent) -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let observed = observation.clone();
    if let Some(status) = worker(move || observed.apply(update)).await? {
        send(StreamEvent::Context { status }).await?;
    }
    Ok(())
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
        text: if request.message.trim().is_empty() && !request.attachments.is_empty() {
            "请分析所附文件。".into()
        } else {
            request.message.clone()
        },
    }
    .validate()
    .map_err(|_| Error::Invalid)?;
    if request.attachments.len() > 4 {
        return Err(Error::Attachment("每条消息最多附加 4 个文件".into()));
    }
    let mut ids = std::collections::HashSet::new();
    for file in &request.attachments {
        if uuid::Uuid::parse_str(&file.file_id).is_err() || !ids.insert(&file.file_id) {
            return Err(Error::Attachment("附件引用无效或重复".into()));
        }
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
            files: None,
            live: super::live::Source::new(store.clone(), None),
            terminals: None,
            approvals: super::approvals::Approvals::new(store.clone()),
            store,
            stop,
            active: Arc::new(Mutex::new(HashMap::new())),
        }
    }
    pub fn with_terminals(
        store: Arc<Store>,
        stop: CancellationToken,
        terminals: Arc<crate::terminal::TerminalManager>,
    ) -> Self {
        let mut manager = Self::new(store.clone(), stop);
        manager.live = super::live::Source::new(store, Some(terminals.clone()));
        manager.terminals = Some(terminals);
        manager
    }
    async fn capture_live(
        &self,
        request: Request,
        target: Option<Target>,
    ) -> Result<super::live::Live> {
        let source = self.live.clone();
        worker(move || source.capture_for(&request, &target)).await
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
                    agent_desc: format!(
                        "{} · {}",
                        if p.config.supports_tools() {
                            "工具默认可用"
                        } else {
                            "文字对话"
                        },
                        p.config.model
                    ),
                    tools_available: p.config.supports_tools(),
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
    pub async fn create_general_session(
        &self,
        agent: String,
        user: String,
    ) -> Result<super::Session> {
        self.create_general_session_with_policy(agent, user, None)
            .await
    }
    pub async fn create_general_session_with_policy(
        &self,
        agent: String,
        user: String,
        policy: Option<super::approvals::Policy>,
    ) -> Result<super::Session> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        let store = self.store.clone();
        worker(move || {
            if !ModelRepository::new(store.clone())
                .get(&agent, &user)?
                .ready
            {
                return Err(ModelError::KeyMissing.into());
            }
            Repository::new(store)
                .create_general_session_with_policy(&agent, &user, policy)
                .map_err(Error::from)
        })
        .await
    }
    pub async fn server_scope(
        &self,
        agent: String,
        user: String,
        root: String,
        terminal: String,
        new_task: bool,
    ) -> Result<super::Session> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        let source = self.live.clone();
        let store = self.store.clone();
        worker(move || {
            let (connection, identity) = source.server_identity(&terminal, &user)?;
            Repository::new(store)
                .server_scope(
                    &agent,
                    &user,
                    &root,
                    &Target {
                        connection_id: connection,
                        terminal_session_id: terminal,
                    },
                    identity,
                    new_task,
                )
                .map_err(Error::from)
        })
        .await
    }
    pub async fn scopes(
        &self,
        agent: String,
        user: String,
        root: String,
    ) -> Result<Vec<super::Session>> {
        let store = self.store.clone();
        worker(move || {
            Repository::new(store)
                .scopes(&agent, &user, &root)
                .map_err(Error::from)
        })
        .await
    }
    pub async fn projects(&self, user: String) -> Result<Vec<super::Project>> {
        let repo = Repository::new(self.store.clone());
        worker(move || repo.projects(&user).map_err(Error::from)).await
    }
    pub async fn create_project(
        &self,
        user: String,
        name: String,
        connection: String,
    ) -> Result<super::Project> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        let repo = Repository::new(self.store.clone());
        worker(move || {
            repo.create_project(&user, &name, &connection)
                .map_err(Error::from)
        })
        .await
    }
    pub async fn project_chats(
        &self,
        user: String,
        project: Option<String>,
        after: String,
    ) -> Result<super::ProjectChats> {
        let repo = Repository::new(self.store.clone());
        worker(move || {
            repo.project_chats(&user, project.as_deref(), &after)
                .map_err(Error::from)
        })
        .await
    }
    pub async fn project_terminal(
        &self,
        user: String,
        project: String,
        terminal: String,
    ) -> Result<super::Project> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        let source = self.live.clone();
        let repo = Repository::new(self.store.clone());
        worker(move || {
            let (connection, identity) = source.server_identity(&terminal, &user)?;
            repo.bind_project(&project, &user, &connection, &identity)
                .map_err(Error::from)
        })
        .await
    }
    pub async fn project_chat(
        &self,
        user: String,
        project: String,
        agent: String,
        terminal: String,
    ) -> Result<super::Session> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        let source = self.live.clone();
        let store = self.store.clone();
        worker(move || {
            if !ModelRepository::new(store.clone())
                .get(&agent, &user)?
                .ready
            {
                return Err(ModelError::KeyMissing.into());
            }
            let (connection, identity) = source.server_identity(&terminal, &user)?;
            Repository::new(store)
                .create_project_chat(
                    &project,
                    &user,
                    &agent,
                    &Target {
                        connection_id: connection,
                        terminal_session_id: terminal,
                    },
                    identity,
                )
                .map_err(Error::from)
        })
        .await
    }
    pub async fn text_target(&self, request: Request) -> Result<Option<Target>> {
        let store = self.store.clone();
        worker(move || {
            let session = Repository::new(store).session(&request.session_id, &request.user_id)?;
            if session.agent_id != request.agent_id {
                return Err(Error::NotFound);
            }
            Ok(session.task.and_then(|t| t.target))
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
    pub async fn context_status(
        &self,
        agent: String,
        user: String,
        session: String,
    ) -> Result<super::context_status::Status> {
        let store = self.store.clone();
        worker(move || super::context_status::read(store, &agent, &user, &session)).await
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
    pub async fn steer(
        &self,
        agent: String,
        user: String,
        session: String,
        expected_request: String,
        id: String,
        message: String,
    ) -> Result<super::steering::Input> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        let repo = Repository::new(self.store.clone());
        worker(move || {
            super::steering::accept(
                &repo,
                &agent,
                &user,
                &session,
                &expected_request,
                &id,
                &message,
            )
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
        // Resolve the active exact request before the independent stop IPC changes state.
        if let Some(active)=self.active.lock().unwrap().get(&(user.clone(),session.clone())) {
            if request.as_ref().is_none_or(|id| id==&active.request) {
                self.store.diagnostics.trace(&active.request,&session,&agent).event(crate::diagnostics::Level::Warn,"request.stop_requested",serde_json::json!({"source":"user"}));
            }
        }
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
        &self, request: Request, target: Option<Target>, cancel: &CancellationToken, send: F,
    ) -> Result<()> where F: FnMut(StreamEvent) -> Fut, Fut: Future<Output=Result<()>> {
        let trace = self.store.diagnostics.trace(&request.request_id,&request.session_id,&request.agent_id);
        let fields = serde_json::json!({"messageBytes":request.message.len(),"attachmentCount":request.attachments.len(),"entry":"text"});
        trace.request(fields,self.chat_inner(request,target,cancel,send)).await
    }
    async fn chat_inner<F, Fut>(
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
        let request_copy = request.clone();
        let store = self.store.clone();
        let scoped = worker(move || {
            Ok(Repository::new(store)
                .session(&request_copy.session_id, &request_copy.user_id)?
                .scope
                .is_some())
        })
        .await?;
        if !scoped
            && !request.terminal_session_id.is_empty()
            && request.terminal_session_id
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
            observation: None,
        };
        tokio::select! {biased; _=cancel.cancelled()=>return Err(ModelError::Cancelled.into()), _=token.cancelled()=>return Err(ModelError::Cancelled.into()), result=self.prepare_attachments(&request,&token)=>result?};
        use super::context_status::{Observation, Reporter, Source, Update};
        let store = self.store.clone();
        let r = request.clone();
        let observation = worker(move || Observation::begin(store, &r)).await?;
        running.observation = Some(observation.clone());
        let (reporter, mut updates) = Reporter::channel(observation.clone());
        let controller =
            super::compact::auto::Controller::with_source(self.store.clone(), self.live.clone())
                .with_reporter(reporter.clone());
        let preparing = async {
            let prepared = controller
                .prepare_text(&request, target.clone(), &token, cancel)
                .await?;
            let prepared = controller
                .refresh_text(prepared, &request, target.clone(), &token, cancel)
                .await?;
            let (tokens, source) = if let Some(response) = &prepared.responses {
                (response.input_tokens, Source::ProviderCount)
            } else {
                (
                    super::provider::measure_request(&prepared.runtime.config, &prepared.request)?
                        .estimated_input_tokens,
                    Source::Estimated,
                )
            };
            reporter.measured(&prepared.runtime.config, tokens, source)?;
            Ok::<_, Error>(prepared)
        };
        tokio::pin!(preparing);
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        let prepared = loop {
            tokio::select! {biased;
                Some(update)=updates.recv()=>forward_context(&observation,update,&mut send).await?,
                result=&mut preparing=>break result,
                _=tick.tick()=>{let observed=observation.clone();worker(move||observed.heartbeat()).await?;},
            }
        };
        while let Ok(update) = updates.try_recv() {
            forward_context(&observation, update, &mut send).await?;
        }
        let failure = prepared.as_ref().err().map(|e| e.code().to_owned());
        forward_context(&observation, Update::Finished(failure), &mut send).await?;
        let prepared = prepared?;
        if token.is_cancelled() || cancel.is_cancelled() {
            return Err(ModelError::Cancelled.into());
        }
        let runtime = prepared.runtime;
        let mut response_prepared = prepared.responses;
        let mut chat = prepared.request;
        let revision = prepared.revision;
        let target_for_admission = target.clone();
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
        self.capture_live(request.clone(), admission.0.target.clone())
            .await?;
        let repo = Repository::new(self.store.clone());
        let r = request.clone();
        let mut current_revision =
            worker(move || Ok(super::steering::snapshot(&repo, &r)?.session.revision)).await?;
        let mut rounds = 0;
        let result = loop {
            rounds += 1;
            let result = tokio::select! {biased;_=cancel.cancelled()=>Err(ModelError::Cancelled),
            result=client.stream_with_responses(&chat, response_prepared.take(),&token,|content| {
                let (current,exceeded)={let mut text=partial.lock().unwrap();let exceeded=text.len()+content.len()>16000;if !exceeded{text.push_str(&content);}(text.clone(),exceeded)};
                let save=persisted==0||current.len().saturating_sub(persisted)>=1024||last_save.elapsed()>=std::time::Duration::from_millis(250);
                if save{persisted=current.len();last_save=std::time::Instant::now();}
                let future=send(StreamEvent::Text{content});let turns=turns.clone();let r=request.clone();let who=owner.clone();
                async move {
                    if exceeded{return Err(ModelError::ResponseTooLarge);}
                    if save && !matches!(worker(move||turns.checkpoint_partial(&r,&who,Some(&current))).await,Ok(true)){return Err(ModelError::Cancelled);}
                    future.await.map_err(|e|match e{Error::Model(error)=>error,_=>ModelError::Cancelled})
                }
            })=>result};
            let mut completion = match result {
                Ok(c) => c,
                Err(error) => break Err(error),
            };
            let repo = Repository::new(self.store.clone());
            let r = request.clone();
            let c = completion.clone();
            let inputs =
                worker(move || super::steering::drain(&repo, &r, current_revision, Some(&c), true))
                    .await?;
            if inputs.is_empty() {
                completion.message.content = Some(partial.lock().unwrap().clone());
                break Ok(completion);
            }
            let repo = Repository::new(self.store.clone());
            let r = request.clone();
            current_revision =
                worker(move || Ok(super::steering::snapshot(&repo, &r)?.session.revision)).await?;
            if let Some(window) = completion.response_output.take() {
                chat.messages
                    .push(super::provider::ChatMessage::Responses { window });
            } else {
                chat.messages.push(super::provider::ChatMessage::Assistant {
                    content: completion.message.content.clone(),
                    tool_calls: vec![],
                });
            }
            for input in inputs {
                chat.messages.push(super::provider::ChatMessage::User {
                    content: input.message.clone(),
                });
                send(StreamEvent::Input { input }).await?;
            }
            if rounds >= 8 {
                break Err(ModelError::ResponseTooLarge);
            }
            partial.lock().unwrap().push_str("\n\n");
            send(StreamEvent::Text {
                content: "\n\n".into(),
            })
            .await?;
        };
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

impl crate::diagnostics::Failure for Error {
    fn diagnostic_code(&self) -> &str { self.code() }
    fn upstream_status(&self) -> Option<u16> { match self { Self::Model(ModelError::Http(status)) => Some(*status), _ => None } }
    fn cancelled(&self) -> bool { matches!(self.code(), "AGENT_CANCELLED"|"AGENT_INTERRUPTED"|"BACKEND_UNAVAILABLE") }
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
