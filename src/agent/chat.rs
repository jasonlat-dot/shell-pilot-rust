//! Basic text conversations over the native model stream. No tools or UI permissions yet.
use super::{
    provider::{self, ChatMessage, ChatRequest, ModelClient, ModelError, ModelRepository},
    Payload, Repository, Target, TaskRequest, TaskStatus,
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
    #[error("工作区已停止")]
    Stopped,
    #[error("Agent 日志操作失败")]
    Storage,
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
    pub fn code(&self) -> &'static str {
        match self {
            Self::Model(ModelError::Cancelled) => "AGENT_CANCELLED",
            Self::Model(ModelError::Timeout) => "MODEL_TIMEOUT",
            Self::Model(ModelError::Incomplete) => "MODEL_INCOMPLETE",
            Self::Model(ModelError::BudgetExceeded) => "MODEL_CONTEXT_LIMIT",
            Self::Model(ModelError::KeyMissing | ModelError::Credentials) => "MODEL_CREDENTIALS",
            Self::Model(_) => "MODEL_REQUEST_FAILED",
            Self::NotFound => "AGENT_NOT_FOUND",
            Self::Busy => "AGENT_BUSY",
            Self::Invalid => "AGENT_INVALID_REQUEST",
            Self::TargetMismatch => "AGENT_TARGET_MISMATCH",
            Self::Unsupported => "FEATURE_NOT_MIGRATED",
            Self::HistoryLimit => "AGENT_HISTORY_LIMIT",
            Self::Stopped => "BACKEND_UNAVAILABLE",
            Self::Storage => "AGENT_STORAGE_FAILED",
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
            Self::Model(_) => 502,
            Self::Storage => 500,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentConfig {
    pub agent_id: String,
    pub agent_name: String,
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
    pub agent_id: String,
    pub user_id: String,
    pub session_id: String,
    pub terminal_session_id: String,
    pub message: String,
    #[serde(default)]
    pub attachments: Vec<Attachment>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum StreamEvent {
    Text { content: String },
    Done { content: String },
    Error { content: String, code: String },
}
type ActiveKey = (String, String);
pub struct Manager {
    store: Arc<Store>,
    stop: CancellationToken,
    active: Mutex<HashMap<ActiveKey, CancellationToken>>,
}
struct Turn<'a> {
    manager: &'a Manager,
    key: ActiveKey,
    token: CancellationToken,
}
impl Drop for Turn<'_> {
    fn drop(&mut self) {
        self.token.cancel();
        self.manager.active.lock().unwrap().remove(&self.key);
    }
}
async fn worker<T: Send + 'static>(
    action: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|_| Error::Storage)?
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
        for token in self.active.lock().unwrap().values() {
            token.cancel();
        }
    }
    pub async fn list(&self, user: String) -> Result<Vec<AgentConfig>> {
        let store = self.store.clone();
        worker(move || {
            Ok(ModelRepository::new(store)
                .list(&user)?
                .into_iter()
                .filter(|p| p.ready)
                .map(|p| AgentConfig {
                    agent_id: p.id,
                    agent_name: p.config.model.clone(),
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
        worker(move || {
            let profile = ModelRepository::new(store.clone()).get(&agent, &user)?;
            if !profile.ready {
                return Err(ModelError::KeyMissing.into());
            }
            Ok(Repository::new(store).create_session(&agent, &user)?.id)
        })
        .await
    }
    /// A true receipt means the currently active turn was signalled, not that provider computation ended.
    pub async fn stop_chat(&self, agent: String, user: String, session: String) -> Result<bool> {
        let store = self.store.clone();
        let check_session = session.clone();
        let check_user = user.clone();
        worker(move || {
            let s = Repository::new(store).session(&check_session, &check_user)?;
            if s.agent_id != agent {
                return Err(Error::NotFound);
            }
            Ok(())
        })
        .await?;
        let active = self.active.lock().unwrap();
        if let Some(token) = active.get(&(user, session)) {
            token.cancel();
            Ok(true)
        } else {
            Ok(false)
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
        for id in [&request.agent_id, &request.user_id, &request.session_id] {
            super::model::identifier(id).map_err(|_| Error::Invalid)?;
        }
        super::model::text_value(&request.message, 16000).map_err(|_| Error::Invalid)?;
        Payload::UserMessage {
            text: request.message.clone(),
        }
        .validate()
        .map_err(|_| Error::Invalid)?;
        if !request.attachments.is_empty() {
            return Err(Error::Unsupported);
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
        let store = self.store.clone();
        let r = request.clone();
        let runtime = worker(move || {
            let s = Repository::new(store.clone()).session(&r.session_id, &r.user_id)?;
            if s.agent_id != r.agent_id {
                return Err(Error::NotFound);
            }
            Ok(ModelRepository::new(store).runtime(&r.agent_id, &r.user_id)?)
        })
        .await?;
        let config = runtime.config.clone();
        let client = ModelClient::new(runtime)?;
        let token = self.stop.child_token();
        let key = (request.user_id.clone(), request.session_id.clone());
        {
            let mut active = self.active.lock().unwrap();
            if active.contains_key(&key) || active.len() >= 32 {
                return Err(Error::Busy);
            }
            active.insert(key.clone(), token.clone());
        }
        let _turn = Turn {
            manager: self,
            key,
            token: token.clone(),
        };
        let turn_id = uuid::Uuid::new_v4().to_string();
        let store = self.store.clone();
        let r = request.clone();
        let id = turn_id.clone();
        let setup_stop = token.clone();
        let caller_stop = cancel.clone();
        let (revision,chat)=worker(move|| {
            if setup_stop.is_cancelled() || caller_stop.is_cancelled() {return Err(ModelError::Cancelled.into());}
            let repo=Repository::new(store);
            let mut s=repo.session(&r.session_id,&r.user_id)?;
            if s.checkpoint_id.is_some() { return Err(Error::HistoryLimit); }
            if s.task.as_ref().is_some_and(|t|t.target!=target) { return Err(Error::TargetMismatch); }
            let events=repo.events(&s.id,&s.user_id,0,200)?;
            if events.len()>=200 { return Err(Error::HistoryLimit); }
            let needs_task=s.task.as_ref().is_none_or(|t|t.status!=TaskStatus::Active);
            let goal=if needs_task {r.message.clone()} else {s.task.as_ref().unwrap().goal.clone()};
            let constraints=s.task.as_ref().map(|t|t.constraints.clone()).unwrap_or_default();
            let task_state=serde_json::json!({"goal":goal,"constraints":constraints,"target":target});
            let mut messages=vec![ChatMessage::System { content:format!(
                "你是 ShellPilot 的文本助手。本阶段没有命令执行工具；不要声称执行过命令或读取过服务器。\n可信任务快照（不产生执行授权）：{task_state}") }];
            for e in events {
                match e.payload {
                    Payload::UserMessage{text}=>messages.push(ChatMessage::User{content:text}),
                    Payload::AssistantMessage{text}=>messages.push(ChatMessage::Assistant{content:Some(text),tool_calls:vec![]}),
                    Payload::ToolCall{..}|Payload::ToolResult{..}=>return Err(Error::Unsupported),_=>{}
                }
            }
            messages.push(ChatMessage::User{content:r.message.clone()});
            let chat=ChatRequest {messages,tools:vec![]};
            provider::prepare_request(&config,&chat)?;
            if needs_task {
                repo.start_task(&s.id,&s.user_id,s.revision,&format!("{id}:task"),TaskRequest {
                    goal,constraints,target })?;
                s=repo.session(&s.id,&s.user_id)?;
            }
            repo.append(&s.id,&s.user_id,s.revision,&format!("{id}:user"),Payload::UserMessage{text:r.message})?;
            Ok((repo.session(&s.id,&s.user_id)?.revision,chat))
        }).await?;
        let result = tokio::select! {biased; _=cancel.cancelled()=>Err(ModelError::Cancelled),
        result=client.stream(&chat,&token,|content| {
            let future=send(StreamEvent::Text{content});
            async move {future.await.map_err(|error|match error {Error::Model(error)=>error,_=>ModelError::Cancelled})}
        })=>result };
        let store = self.store.clone();
        let r = request.clone();
        let id = turn_id.clone();
        match result {
            Ok(completion) => {
                let text = completion.message.content.ok_or(Error::Invalid)?;
                let saved = text.clone();
                worker(move || {
                    Repository::new(store).append(
                        &r.session_id,
                        &r.user_id,
                        revision,
                        &format!("{id}:assistant"),
                        Payload::AssistantMessage { text: saved },
                    )?;
                    Ok(())
                })
                .await?;
                send(StreamEvent::Done { content: text }).await
            }
            Err(error) => {
                worker(move || {
                    Repository::new(store).append(
                        &r.session_id,
                        &r.user_id,
                        revision,
                        &format!("{id}:cancelled"),
                        Payload::Cancelled {
                            reason: "本轮生成未完整完成；不视为成功".into(),
                        },
                    )?;
                    Ok(())
                })
                .await?;
                Err(error.into())
            }
        }
    }
}
