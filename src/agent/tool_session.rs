//! D4 native tool turn: original events, full SSH captures and bounded output reads.
//! The desktop text-chat IPC is intentionally not wired here; D5 owns that UI lifecycle.
mod context;
use super::{
    provider::{
        AssistantReply, ChatMessage, ChatRequest, ModelClient, ModelRepository, RuntimeConfig,
        ToolCall,
    },
    ssh_tools::{Command, SshExecutor, SSH_EXECUTE},
    tool_loop::{
        self, Limits, LoopContext, ToolExecutor, ToolLoop, ToolOutcome, ToolRegistry, ValidatedCall,
    },
    tool_output::Capture,
    *,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};
use tokio_util::sync::CancellationToken;

pub const READ_OUTPUT: &str = "read_tool_output";
pub fn output_definition() -> provider::FunctionDefinition {
    provider::FunctionDefinition {
        name: READ_OUTPUT.into(),
        description: "Read original bytes of a linked output from this task, without SSH execution or approval. Offsets and limits count bytes. bytesHex is exact; text is a possibly lossy UTF-8 view. Never repeat a command to fetch its output.".into(),
        parameters: json!({"type":"object","properties":{
            "outputId":{"type":"string","minLength":36,"maxLength":36},
            "offset":{"type":"integer","minimum":0,"maximum":16777216},
            "limit":{"type":"integer","minimum":1,"maximum":512}
        },"required":["outputId","offset","limit"],"additionalProperties":false}),
    }
}
struct Pending {
    id: String,
    started: bool,
    capture: Option<Arc<Capture>>,
}
struct Data {
    snapshot: Snapshot,
    latest_user: u64,
    message: String,
    runtime: RuntimeConfig,
    pending: Vec<Pending>,
}
pub struct ToolSession {
    executor: SshExecutor,
    repository: Repository,
    started: AtomicBool,
    data: Arc<Mutex<Option<Data>>>,
}
async fn worker<T: Send + 'static>(
    action: impl FnOnce() -> tool_loop::Result<T> + Send + 'static,
) -> tool_loop::Result<T> {
    tokio::task::spawn_blocking(action)
        .await
        .map_err(|_| tool_loop::Error::Context)?
}
fn context_error(_: super::Error) -> tool_loop::Error {
    tool_loop::Error::Context
}
impl ToolSession {
    /// Caller installs D3 approval/authorizer on the executor before passing it here.
    pub fn new(executor: SshExecutor) -> Self {
        Self {
            repository: Repository::new(executor.store()),
            executor,
            started: AtomicBool::new(false),
            data: Arc::new(Mutex::new(None)),
        }
    }
    /// Native storage quotas, never model arguments or execution permission.
    pub fn with_output_limits(mut self, limits: OutputLimits) -> tool_loop::Result<Self> {
        if self.started.load(Ordering::SeqCst) {
            return Err(tool_loop::Error::AlreadyStarted);
        }
        self.repository =
            Repository::with_output_limits(self.executor.store(), limits).map_err(context_error)?;
        Ok(self)
    }
    pub async fn run(
        &self,
        request_id: &str,
        message: &str,
        limits: Limits,
        cancel: &CancellationToken,
    ) -> tool_loop::Result<AssistantReply> {
        if limits.result_bytes < 8192 {
            return Err(tool_loop::Error::Invalid);
        }
        let mut registry = ToolRegistry::default();
        self.executor.register(&mut registry)?;
        registry.register(output_definition())?;
        // Check the loop limits and input shape before mutating persistent state.
        let mut loop_ = ToolLoop::new(
            &registry,
            vec![ChatMessage::User {
                content: message.into(),
            }],
            limits,
        )?
        .with_context(self);
        model::identifier(request_id).map_err(context_error)?;
        model::text_value(message, 16000).map_err(context_error)?;
        if cancel.is_cancelled() {
            return Err(tool_loop::Error::Cancelled);
        }
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(tool_loop::Error::AlreadyStarted);
        }
        let repository = self.repository.clone();
        let binding = self.executor.binding().clone();
        let request_id = request_id.to_owned();
        let message = message.to_owned();
        let data = self.data.clone();
        let tools = registry.definitions();
        let runtime = worker(move || {
            let snapshot = repository.snapshot(&binding.session_id,&binding.user_id).map_err(context_error)?;
            if ToolBinding::from_snapshot(&snapshot).map_err(context_error)? != binding.tool {
                return Err(tool_loop::Error::Context);
            }
            if !snapshot.pending_tool_call_ids.is_empty() {
                // A crashed dispatch has no reusable execution permission.
                return Err(tool_loop::Error::OutcomeUnknown);
            }
            let running = repository.db(|db| Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_chat_turns WHERE session_id=?1 AND state='running')",
                [&binding.session_id], |r|r.get::<_,bool>(0))?)).map_err(context_error)?;
            if running { return Err(tool_loop::Error::Context); }
            let runtime = ModelRepository::new(repository.store.clone())
                .runtime(&snapshot.session.agent_id,&binding.user_id)
                .map_err(|_|tool_loop::Error::Context)?;
            if runtime.config.provider != provider::ProviderKind::OpenAiCompatible
                || !runtime.config.capabilities.tool_calls {
                return Err(tool_loop::Error::Context);
            }
            context::require_pinned(&snapshot,&binding,&message,&runtime,&tools)?;
            let appended = repository.append(&binding.session_id,&binding.user_id,snapshot.session.revision,
                &request_id,Payload::UserMessage{text:message.clone()}).map_err(context_error)?;
            if !appended.inserted { return Err(tool_loop::Error::AlreadyStarted); }
            let mut snapshot = snapshot;
            snapshot.session.revision += 1;
            snapshot.through_event_id = appended.event.id;
            *data.lock().map_err(|_|tool_loop::Error::Context)? = Some(Data {
                snapshot,latest_user:appended.event.id,message,runtime:runtime.clone(),pending:vec![],
            });
            Ok(runtime)
        }).await?;
        let _cleanup = Cleanup {
            repository: self.repository.clone(),
            data: self.data.clone(),
        };
        let client = ModelClient::new(runtime)?;
        let reply = loop_.run(&client, self, cancel).await?;
        let repository = self.repository.clone();
        let data = self.data.clone();
        let text = reply.content.clone().or_else(|| reply.refusal.clone());
        if let Some(text) = text.filter(|t| !t.trim().is_empty()) {
            worker(move || {
                let mut guard = data.lock().map_err(|_| tool_loop::Error::Context)?;
                let data = guard.as_mut().ok_or(tool_loop::Error::Context)?;
                ensure_current(&repository, data)?;
                let s = &data.snapshot.session;
                let result = repository
                    .append(
                        &s.id,
                        &s.user_id,
                        s.revision,
                        &uuid::Uuid::new_v4().to_string(),
                        Payload::AssistantMessage { text },
                    )
                    .map_err(context_error)?;
                data.snapshot.session.revision += 1;
                data.snapshot.through_event_id = result.event.id;
                Ok(())
            })
            .await?;
        }
        Ok(reply)
    }
}
fn ensure_current(repository: &Repository, data: &Data) -> tool_loop::Result<()> {
    if repository
        .snapshot(&data.snapshot.session.id, &data.snapshot.session.user_id)
        .map_err(context_error)?
        != data.snapshot
    {
        return Err(tool_loop::Error::Context);
    }
    Ok(())
}
impl LoopContext for ToolSession {
    fn before_model<'a>(
        &'a self,
        request: &'a ChatRequest,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, tool_loop::Result<ChatRequest>> {
        Box::pin(self.prepare(request, cancel))
    }
    fn begin_batch<'a>(
        &'a self,
        calls: &'a [ToolCall],
        text: Option<&'a str>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, tool_loop::Result<()>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(tool_loop::Error::Cancelled);
            }
            let repository = self.repository.clone();
            let data = self.data.clone();
            let binding = self.executor.binding().tool.clone();
            let calls = calls.to_vec();
            let text = text.filter(|t| !t.trim().is_empty()).map(str::to_owned);
            worker(move||{
                let mut guard=data.lock().map_err(|_|tool_loop::Error::Context)?;
                let data=guard.as_mut().ok_or(tool_loop::Error::Context)?;
                ensure_current(&repository,data)?;
                if !data.pending.is_empty(){return Err(tool_loop::Error::Context);}
                let payloads=calls.iter().map(|call|Payload::ToolCall {
                    call_id:call.id.clone(),tool_name:call.function.name.clone(),
                    // Preserve invalid JSON as data too; routing is still host-owned.
                    arguments:json!({"modelArguments":serde_json::from_str::<Value>(&call.function.arguments)
                        .unwrap_or_else(|_|Value::String(call.function.arguments.clone()))}),
                }).collect();
                repository.append_tool_batch(&data.snapshot,&binding,payloads,text).map_err(context_error)?;
                data.snapshot=repository.snapshot(&data.snapshot.session.id,&data.snapshot.session.user_id).map_err(context_error)?;
                data.pending=calls.into_iter().map(|call|Pending{id:call.id,started:false,capture:None}).collect();
                Ok(())
            }).await
        })
    }
    fn started(&self, call_id: &str) -> tool_loop::Result<()> {
        let mut guard = self.data.lock().map_err(|_| tool_loop::Error::Context)?;
        let data = guard.as_mut().ok_or(tool_loop::Error::Context)?;
        let call = data
            .pending
            .iter_mut()
            .find(|p| p.id == call_id)
            .ok_or(tool_loop::Error::Context)?;
        call.started = true;
        Ok(())
    }
    fn result<'a>(
        &'a self,
        call_id: &'a str,
        content: &'a str,
    ) -> BoxFuture<'a, tool_loop::Result<()>> {
        let repository = self.repository.clone();
        let data = self.data.clone();
        let call_id = call_id.to_owned();
        let content = content.to_owned();
        Box::pin(worker(move || {
            let mut guard = data.lock().map_err(|_| tool_loop::Error::Context)?;
            let data = guard.as_mut().ok_or(tool_loop::Error::Context)?;
            ensure_current(&repository, data)?;
            publish(&repository, data, &call_id, &content)
        }))
    }
}
fn publish(
    repository: &Repository,
    data: &mut Data,
    call_id: &str,
    content: &str,
) -> tool_loop::Result<()> {
    let index = data
        .pending
        .iter()
        .position(|p| p.id == call_id)
        .ok_or(tool_loop::Error::Context)?;
    let p = &data.pending[index];
    let outputs = if let Some(capture) = &p.capture {
        capture
            .finish()
            .map_err(context_error)?
            .into_iter()
            .map(|o| o.reference)
            .collect()
    } else {
        vec![]
    };
    let value: Value = serde_json::from_str(content).map_err(|_| tool_loop::Error::Context)?;
    let outcome = if value["error"] == "result_unknown" {
        Outcome::Unknown
    } else if value["ok"] == true {
        Outcome::Success
    } else {
        Outcome::Error
    };
    let exit_code = value["data"]["exitCode"]
        .as_u64()
        .and_then(|code| i32::try_from(code).ok());
    let s = &data.snapshot.session;
    let receipt = repository
        .append(
            &s.id,
            &s.user_id,
            s.revision,
            &uuid::Uuid::new_v4().to_string(),
            Payload::ToolResult {
                call_id: call_id.into(),
                outcome,
                exit_code,
                text: content.into(),
                outputs,
            },
        )
        .map_err(context_error)?;
    data.snapshot = repository
        .snapshot(&s.id, &s.user_id)
        .map_err(context_error)?;
    debug_assert_eq!(data.snapshot.through_event_id, receipt.event.id);
    data.pending.remove(index);
    Ok(())
}
/// The owning run guard closes every admitted batch on cancel/drop. A crash leaves
/// raw pending calls (uncertain), never a retry permit. Late results retain original task IDs.
struct Cleanup {
    repository: Repository,
    data: Arc<Mutex<Option<Data>>>,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        let Ok(mut guard) = self.data.lock() else {
            return;
        };
        let Some(data) = guard.as_mut() else { return };
        if data.pending.is_empty() {
            return;
        }
        if let Ok(snapshot) = self
            .repository
            .snapshot(&data.snapshot.session.id, &data.snapshot.session.user_id)
        {
            data.snapshot = snapshot;
            while let Some(pending) = data.pending.first() {
                let id = pending.id.clone();
                let content=json!({"ok":false,"error":if pending.started{"result_unknown"}else{"not_executed"}}).to_string();
                if publish(&self.repository, data, &id, &content).is_err() {
                    break;
                }
            }
        }
    }
}
impl ToolExecutor for ToolSession {
    fn execute<'a>(
        &'a self,
        call: &'a ValidatedCall,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            if call.name() == READ_OUTPUT {
                return self.read(call.arguments().clone()).await;
            }
            if call.name() != SSH_EXECUTE {
                return ToolOutcome::Denied;
            }
            let Ok(command) = serde_json::from_value::<Command>(call.arguments().clone()) else {
                return ToolOutcome::Denied;
            };
            let binding = self.executor.binding();
            let capture = Capture::new(
                self.repository.clone(),
                binding.session_id.clone(),
                binding.user_id.clone(),
                call.id().into(),
            );
            let revision = {
                let Ok(mut guard) = self.data.lock() else {
                    return ToolOutcome::OutcomeUnknown;
                };
                let Some(data) = guard.as_mut() else {
                    return ToolOutcome::Denied;
                };
                let revision = data.snapshot.session.revision;
                let Some(p) = data.pending.iter_mut().find(|p| p.id == call.id()) else {
                    return ToolOutcome::Denied;
                };
                p.capture = Some(capture.clone());
                revision
            };
            match self
                .executor
                .execute_inner(call.id(), &command, cancel, Some(capture), Some(revision))
                .await
            {
                Ok(result) => {
                    let code = if result.output_error {
                        Some("output_storage_failed")
                    } else if result.exit_code != 0 {
                        Some("execution_failed")
                    } else {
                        None
                    };
                    if let Some(code) = code {
                        ToolOutcome::Failure {
                            code,
                            data: json!(result),
                        }
                    } else {
                        ToolOutcome::Success(json!(result))
                    }
                }
                Err(ssh_tools::Error::OutcomeUnknown) => ToolOutcome::OutcomeUnknown,
                Err(error) => {
                    let code = match error {
                        ssh_tools::Error::TargetUnavailable | ssh_tools::Error::Unavailable => {
                            "target_unavailable"
                        }
                        ssh_tools::Error::Busy => "execution_busy",
                        ssh_tools::Error::Storage => "output_storage_failed",
                        ssh_tools::Error::Conflict => "execution_conflict",
                        ssh_tools::Error::InvalidArguments => "invalid_arguments",
                        ssh_tools::Error::Cancelled => "not_executed",
                        ssh_tools::Error::TaskChanged => "task_changed",
                        ssh_tools::Error::TargetMismatch => "target_mismatch",
                        _ => "denied",
                    };
                    ToolOutcome::Failure {
                        code,
                        data: json!({}),
                    }
                }
            }
        })
    }
}
impl ToolSession {
    async fn read(&self, args: Value) -> ToolOutcome {
        let repository = self.repository.clone();
        let binding = self.executor.binding().clone();
        let read=tokio::task::spawn_blocking(move||->super::Result<Value>{
            let output_id=args["outputId"].as_str().ok_or(Error::OutputUnavailable)?;
            let offset=args["offset"].as_u64().ok_or(Error::OutputUnavailable)?;
            let limit=args["limit"].as_u64().filter(|n|(1..=512).contains(n)).ok_or(Error::OutputUnavailable)? as usize;
            let output=repository.output(&binding.session_id,&binding.user_id,output_id)?;
            if output.task_id!=binding.tool.task_id || output.event_id.is_none() {return Err(Error::OutputUnavailable);}
            let range=repository.read_output_range(&binding.session_id,&binding.user_id,output_id,offset,limit)?;
            Ok(json!({"outputId":output_id,"offset":range.offset,"nextOffset":range.next_offset,
                "eof":range.eof,"size":range.size,"bytesHex":output_model::hex(&range.bytes),
                "text":String::from_utf8_lossy(&range.bytes),"lossyUtf8":std::str::from_utf8(&range.bytes).is_err()}))
        }).await;
        match read {
            Ok(Ok(value)) => ToolOutcome::Success(value),
            _ => ToolOutcome::Failure {
                code: "output_unavailable",
                data: json!({}),
            },
        }
    }
}
