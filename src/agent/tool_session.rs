//! D4 native tool turn: original events, full SSH captures and bounded output reads.
//! D5 desktop integration uses a host-owned durable turn and bounded event channel.
mod child;
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
const OUTPUT_PAGE_BYTES: u64 = 512;
fn normalize_output_arguments(arguments: &mut Value) {
    if let Some(limit) = arguments.get_mut("limit") {
        if limit
            .as_u64()
            .is_some_and(|value| value > OUTPUT_PAGE_BYTES)
        {
            *limit = json!(OUTPUT_PAGE_BYTES);
        }
    }
}
#[cfg(test)]
mod output_argument_tests {
    use super::*;
    #[test]
    fn caps_only_oversized_positive_integer_pages_without_rewriting_the_output_or_offset() {
        for size in [1000u64, 1500, 4096, u64::MAX] {
            let mut args = json!({"outputId":"unchanged","offset":1024,"limit":size});
            normalize_output_arguments(&mut args);
            assert_eq!(
                args,
                json!({"outputId":"unchanged","offset":1024,"limit":512})
            );
        }
        for limit in [
            json!(0),
            json!(-1),
            json!(1.5),
            json!("4096"),
            Value::Null,
            json!(512),
        ] {
            let mut args = json!({"outputId":"unchanged","offset":0,"limit":limit});
            let original = args.clone();
            normalize_output_arguments(&mut args);
            assert_eq!(args, original);
        }
        let mut missing = json!({"outputId":"unchanged","offset":0});
        let original = missing.clone();
        normalize_output_arguments(&mut missing);
        assert_eq!(missing, original);
    }
}
pub fn output_definition() -> provider::FunctionDefinition {
    provider::FunctionDefinition {
        name: READ_OUTPUT.into(),
        description: "Read original bytes of a linked output from this task, without SSH execution or approval. Reads do not consume tool-call quota; requesting the model again still consumes a model round. Use outputs[].id, never a tool call ID. Offsets and limits count bytes. Each page returns at most 512 bytes; larger positive integer limits are capped to 512. When eof=false, continue with nextOffset and the same outputId. bytesHex is exact; text is a possibly lossy UTF-8 view. Never repeat a command to fetch its output.".into(),
        parameters: json!({"type":"object","properties":{
            "outputId":{"type":"string","minLength":36,"maxLength":36},
            "offset":{"type":"integer","minimum":0,"maximum":16777216,"description":"Byte offset, initially 0; use the previous result's nextOffset for the next page."},
            "limit":{"type":"integer","minimum":1,"maximum":OUTPUT_PAGE_BYTES,"description":"Requested page bytes, 1–512. The host caps larger positive integers to 512; continue with nextOffset until eof=true."}
        },"required":["outputId","offset","limit"],"additionalProperties":false}),
    }
}
struct Pending {
    id: String,
    agent: Option<String>,
    started: bool,
    capture: Option<Arc<Capture>>,
    tool: ToolCall,
    announced: bool,
}
struct Data {
    intermediate: String,
    snapshot: Snapshot,
    latest_user: u64,
    message: String,
    runtime: RuntimeConfig,
    pending: Vec<Pending>,
}
#[derive(Clone)]
struct RuntimeBinding {
    session_id: String,
    user_id: String,
    task_id: String,
    target: Option<super::Target>,
}
impl RuntimeBinding {
    fn snapshot(s: &Snapshot) -> tool_loop::Result<Self> {
        let t = s
            .session
            .task
            .as_ref()
            .filter(|t| t.status == super::TaskStatus::Active)
            .ok_or(tool_loop::Error::Context)?;
        Ok(Self {
            session_id: s.session.id.clone(),
            user_id: s.session.user_id.clone(),
            task_id: t.id.clone(),
            target: t.target.clone(),
        })
    }
    fn matches(&self, s: &Snapshot) -> bool {
        s.session.task.as_ref().is_some_and(|t| {
            t.status == super::TaskStatus::Active && t.id == self.task_id && t.target == self.target
        })
    }
}
pub struct ToolSession {
    executor: Option<SshExecutor>,
    operation_gate: tokio::sync::RwLock<()>,
    journal_gate: tokio::sync::RwLock<()>,
    binding: RuntimeBinding,
    extensions: Option<super::extensions::Runtime>,
    repository: Repository,
    started: AtomicBool,
    data: Arc<Mutex<Option<Data>>>,
    managed: Option<(chat::Request, String)>,
    events: Option<tokio::sync::mpsc::Sender<chat::StreamEvent>>,
    partial: Arc<Mutex<String>>,
    reporter: context_status::Reporter,
}
async fn worker<T: Send + 'static>(
    action: impl FnOnce() -> tool_loop::Result<T> + Send + 'static,
) -> tool_loop::Result<T> {
    crate::diagnostics::spawn_blocking(action)
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
            binding: RuntimeBinding {
                session_id: executor.binding().session_id.clone(),
                user_id: executor.binding().user_id.clone(),
                task_id: executor.binding().tool.task_id.clone(),
                target: Some(executor.binding().tool.target.clone()),
            },
            executor: Some(executor),
            operation_gate: tokio::sync::RwLock::new(()),
            journal_gate: tokio::sync::RwLock::new(()),
            extensions: None,
            started: AtomicBool::new(false),
            data: Arc::new(Mutex::new(None)),
            managed: None,
            events: None,
            partial: Arc::new(Mutex::new(String::new())),
            reporter: context_status::Reporter::default(),
        }
    }
    fn notification(&self, id: &str, status: &str) -> tool_loop::Result<Option<chat::StreamEvent>> {
        let mut guard = self.data.lock().map_err(|_| tool_loop::Error::Context)?;
        let pending = guard
            .as_mut()
            .and_then(|d| d.pending.iter_mut().find(|p| p.id == id))
            .ok_or(tool_loop::Error::Context)?;
        if pending.announced {
            return Ok(None);
        }
        pending.announced = true;
        let args: Value =
            serde_json::from_str(&pending.tool.function.arguments).unwrap_or(Value::Null);
        Ok(Some(chat::StreamEvent::ToolCall {
            tool_call_id: id.into(),
            tool_name: pending.tool.function.name.clone(),
            agent_call_id: pending.agent.clone(),
            command: args["command"].as_str().map(str::to_owned),
            status: status.into(),
        }))
    }
    pub(crate) async fn announce(
        &self,
        id: &str,
        cancel: &CancellationToken,
    ) -> tool_loop::Result<()> {
        if let Some(event) = self.notification(id, "running")? {
            self.repository.db(|db|{
                db.execute("INSERT INTO agent_workflow_events(session_id,task_id,parent_call_id,kind,content,created_at) VALUES(?1,?2,?3,'tool_started','{}',?4)",rusqlite::params![self.binding.session_id,self.binding.task_id,id,chrono::Utc::now().timestamp_millis()])?;Ok(())
            }).map_err(context_error)?;
            if let Some(events) = &self.events {
                tokio::select! {biased;_=cancel.cancelled()=>return Err(tool_loop::Error::Cancelled),result=events.send(event)=>result.map_err(|_|tool_loop::Error::Context)?};
            }
        }
        Ok(())
    }
    pub(crate) fn general(
        store: Arc<crate::store::Store>,
        snapshot: &Snapshot,
    ) -> tool_loop::Result<Self> {
        Ok(Self {
            executor: None,
            operation_gate: tokio::sync::RwLock::new(()),
            journal_gate: tokio::sync::RwLock::new(()),
            binding: RuntimeBinding::snapshot(snapshot)?,
            extensions: None,
            repository: Repository::new(store),
            started: AtomicBool::new(false),
            data: Arc::new(Mutex::new(None)),
            managed: None,
            events: None,
            partial: Arc::new(Mutex::new(String::new())),
            reporter: context_status::Reporter::default(),
        })
    }
    pub(crate) fn with_extensions(mut self, extensions: super::extensions::Runtime) -> Self {
        self.extensions = Some(extensions);
        self
    }
    async fn target_available(&self) -> bool {
        match &self.executor {
            Some(e) => e.verify().await.is_ok(),
            None => true,
        }
    }
    fn ssh_binding(&self) -> Option<&ssh_tools::SshBinding> {
        self.executor.as_ref().map(|e| e.binding())
    }
    pub(crate) fn managed(
        mut self,
        request: chat::Request,
        owner: String,
        events: tokio::sync::mpsc::Sender<chat::StreamEvent>,
        reporter: context_status::Reporter,
        partial: Arc<Mutex<String>>,
    ) -> Self {
        self.managed = Some((request, owner));
        self.events = Some(events);
        self.partial = partial;
        self.reporter = reporter;
        self
    }
    pub(crate) fn revision(&self) -> Option<u64> {
        self.data
            .lock()
            .ok()?
            .as_ref()
            .map(|d| d.snapshot.session.revision)
    }
    pub(crate) fn preflight(
        snapshot: &Snapshot,
        binding: Option<&ssh_tools::SshBinding>,
        message: &str,
        runtime: &RuntimeConfig,
        media: ChatMessage,
        children: bool,
    ) -> tool_loop::Result<()> {
        // Validate host definitions before a durable running turn is created.
        // A malformed schema is a configuration error, not a failed SSH action.
        let mut registry = ToolRegistry::default();
        if binding.is_some() {
            registry.register(ssh_tools::definition())?;
        }
        for definition in super::extensions::definitions(children) {
            registry.register(definition)?;
        }
        registry.register(output_definition())?;
        context::require_pinned(
            snapshot,
            binding,
            message,
            runtime,
            &registry.definitions(),
            Some(&media),
        )
    }
    /// Native storage quotas, never model arguments or execution permission.
    pub fn with_output_limits(mut self, limits: OutputLimits) -> tool_loop::Result<Self> {
        if self.started.load(Ordering::SeqCst) {
            return Err(tool_loop::Error::AlreadyStarted);
        }
        self.repository = Repository::with_output_limits(self.repository.store.clone(), limits)
            .map_err(context_error)?;
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
        if let Some(executor) = &self.executor {
            executor.register(&mut registry)?;
        }
        if let Some(extensions) = &self.extensions {
            extensions.register(&mut registry)?;
        }
        registry.register(output_definition())?;
        // Check the loop limits and input shape before mutating persistent state.
        let mut loop_ = ToolLoop::new(
            &registry,
            vec![ChatMessage::User {
                content: message.into(),
            }],
            limits,
        )?
        .with_context(self)
        .with_approval_boundary(tool_loop::ApprovalBoundary::new(
            approvals::Approvals::new(self.repository.store.clone()),
            self.binding.session_id.clone(),
            self.binding.user_id.clone(),
        ));
        model::identifier(request_id).map_err(context_error)?;
        model::text_value(message, 16000).map_err(context_error)?;
        if cancel.is_cancelled() {
            return Err(tool_loop::Error::Cancelled);
        }
        if self.started.swap(true, Ordering::SeqCst) {
            return Err(tool_loop::Error::AlreadyStarted);
        }
        let repository = self.repository.clone();
        let binding = self.binding.clone();
        let request_id = request_id.to_owned();
        let message = message.to_owned();
        let data = self.data.clone();
        let tools = registry.definitions();
        let managed = self.managed.clone();
        let runtime = worker(move || {
            let snapshot = repository.snapshot(&binding.session_id,&binding.user_id).map_err(context_error)?;
            if !binding.matches(&snapshot) {
                return Err(tool_loop::Error::Context);
            }
            if !snapshot.pending_tool_call_ids.is_empty() {
                // A crashed dispatch has no reusable execution permission.
                return Err(tool_loop::Error::OutcomeUnknown);
            }
            let running = repository.db(|db| Ok(db.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_chat_turns WHERE session_id=?1 AND state='running')",
                [&binding.session_id], |r|r.get::<_,bool>(0))?)).map_err(context_error)?;
            if running && managed.is_none() { return Err(tool_loop::Error::Context); }
            if let Some((r,owner))=&managed {
                let valid=repository.db(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM agent_chat_turns t JOIN agent_tool_turns k USING(session_id,request_id) WHERE t.session_id=?1 AND t.request_id=?2 AND t.owner=?3 AND t.state='running' AND t.cancel_requested=0 AND t.lease_until>=?4)",rusqlite::params![r.session_id,r.request_id,owner,chrono::Utc::now().timestamp_millis()],|r|r.get::<_,bool>(0))?)).map_err(context_error)?;
                if !valid{return Err(tool_loop::Error::Context);}
            }
            let runtime = ModelRepository::new(repository.store.clone())
                .runtime(&snapshot.session.agent_id,&binding.user_id)
                .map_err(|_|tool_loop::Error::Context)?;
            if !runtime.config.supports_tools() {
                return Err(tool_loop::Error::Context);
            }
            context::require_pinned(&snapshot,None,&message,&runtime,&tools,None)?;
            let (snapshot,latest_user)=if let Some((r,_))=&managed {
                let cursor=repository.db(|db|Ok(db.query_row("SELECT user_event_id FROM agent_chat_turns WHERE session_id=?1 AND request_id=?2",rusqlite::params![r.session_id,r.request_id],|r|r.get(0))?)).map_err(context_error)?;
                (snapshot,cursor)
            } else {
                let appended=repository.append(&binding.session_id,&binding.user_id,snapshot.session.revision,&request_id,Payload::UserMessage{text:message.clone()}).map_err(context_error)?;
                if !appended.inserted{return Err(tool_loop::Error::AlreadyStarted);}
                let mut snapshot=snapshot;snapshot.session.revision+=1;snapshot.through_event_id=appended.event.id;
                (snapshot,appended.event.id)
            };
            *data.lock().map_err(|_|tool_loop::Error::Context)? = Some(Data {
                snapshot,latest_user,message,runtime:runtime.clone(),pending:vec![],intermediate:String::new(),
            });
            Ok(runtime)
        }).await?;
        let _cleanup = Cleanup {
            repository: self.repository.clone(),
            data: self.data.clone(),
        };
        let client = ModelClient::new(runtime)?;
        let mut reply = loop_.run(&client, self, cancel).await?;
        if self.streams_replies() {
            reply.content = Some(
                self.partial
                    .lock()
                    .map_err(|_| tool_loop::Error::Context)?
                    .clone(),
            );
            reply.refusal = None;
        } else if let Ok(guard) = self.data.lock() {
            if let Some(data) = guard.as_ref() {
                if !data.intermediate.is_empty() {
                    reply.content = Some(format!(
                        "{}{}",
                        data.intermediate,
                        reply
                            .content
                            .as_deref()
                            .or(reply.refusal.as_deref())
                            .unwrap_or("")
                    ));
                }
            }
        }
        if reply
            .content
            .as_ref()
            .is_some_and(|text| text.len() > 16000)
        {
            return Err(tool_loop::Error::Limit);
        }
        let repository = self.repository.clone();
        let data = self.data.clone();
        let text = reply.content.clone().or_else(|| reply.refusal.clone());
        if let Some(text) = text.filter(|t| !t.trim().is_empty() && self.managed.is_none()) {
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
    fn streams_replies(&self) -> bool {
        self.events.is_some()
    }
    fn text_delta<'a>(
        &'a self,
        text: String,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, provider::Result<()>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(provider::ModelError::Cancelled);
            }
            {
                let mut partial = self
                    .partial
                    .lock()
                    .map_err(|_| provider::ModelError::Database)?;
                if partial.len().saturating_add(text.len()) > 16000 {
                    return Err(provider::ModelError::ResponseTooLarge);
                }
                partial.push_str(&text);
            }
            if let Some(events) = &self.events {
                tokio::select! {biased;
                    _=cancel.cancelled()=>return Err(provider::ModelError::Cancelled),
                    result=events.send(chat::StreamEvent::Text{content:text})=>result.map_err(|_|provider::ModelError::Cancelled)?,
                }
            }
            Ok(())
        })
    }
    fn continue_after_reply<'a>(
        &'a self,
        reply: &'a AssistantReply,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, tool_loop::Result<bool>> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return Err(tool_loop::Error::Cancelled);
            }
            self.apply_inputs(Some(reply.clone()), true, cancel).await
        })
    }
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
            let _journal = self.journal_gate.read().await;
            let repository = self.repository.clone();
            let data = self.data.clone();
            let binding = self.executor.as_ref().map(|e| e.binding().tool.clone());
            let calls = calls.to_vec();
            let text = text.filter(|t| !t.trim().is_empty()).map(str::to_owned);
            let text_present = text.is_some();

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
                repository.append_runtime_batch(&data.snapshot,binding.as_ref(),payloads,text,None).map_err(context_error)?;
                data.snapshot=repository.snapshot(&data.snapshot.session.id,&data.snapshot.session.user_id).map_err(context_error)?;
                data.pending=calls.into_iter().map(|call|Pending{id:call.id.clone(),tool:call,announced:false,agent:None,started:false,capture:None}).collect();
                Ok(())
            }).await?;
            if self.streams_replies() && text_present {
                self.text_delta("\n\n".into(), cancel).await?;
            }
            Ok(())
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
        let events = self.events.clone();
        let agent = self.data.lock().ok().and_then(|g| {
            g.as_ref().and_then(|d| {
                d.pending
                    .iter()
                    .find(|p| p.id == call_id)
                    .and_then(|p| p.agent.clone())
            })
        });
        Box::pin(async move {
            let _journal = self.journal_gate.read().await;
            let final_status = match serde_json::from_str::<Value>(&content) {
                Ok(v) if v["ok"] == true => "success",
                Ok(v) if v["error"] == "result_unknown" => "unknown",
                _ => "error",
            };
            let notification = self.notification(&call_id, final_status)?;
            let id = call_id.clone();
            let text = content.clone();
            worker(move || {
                let mut guard = data.lock().map_err(|_| tool_loop::Error::Context)?;
                let data = guard.as_mut().ok_or(tool_loop::Error::Context)?;
                ensure_current(&repository, data)?;
                publish(&repository, data, &call_id, &content)
            })
            .await?;
            if let Some(events) = events {
                if let Some(event) = notification {
                    events
                        .send(event)
                        .await
                        .map_err(|_| tool_loop::Error::Context)?;
                }
                let value: Value =
                    serde_json::from_str(&text).map_err(|_| tool_loop::Error::Context)?;
                let status = if value["ok"] == true {
                    "success"
                } else if value["error"] == "result_unknown" {
                    "unknown"
                } else {
                    "error"
                };
                events
                    .send(chat::StreamEvent::ToolResult {
                        tool_call_id: id,
                        agent_call_id: agent,
                        content: text,
                        status: status.into(),
                    })
                    .await
                    .map_err(|_| tool_loop::Error::Context)?;
            }
            Ok(())
        })
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
    fn counts_toward_tool_limit(&self, call: &ValidatedCall) -> bool {
        call.name() != READ_OUTPUT
    }
    fn supports_parallel(&self, call: &ValidatedCall) -> bool {
        matches!(
            call.name(),
            READ_OUTPUT | "list_skills" | "read_skill" | "list_mcp_tools" | "delegate_agent"
        ) || self
            .extensions
            .as_ref()
            .is_some_and(|e| e.parallel_mcp(call))
    }
    fn authorize_with_policy<'a>(
        &'a self,
        call: &'a ValidatedCall,
        policy: approvals::Policy,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            // Bounded reads need no new approval; SSH owns its durable review,
            // exact-call decision and final admission under this SAME mode.
            // Future tool adapters must implement the equivalent workflow.
            policy != approvals::Policy::Deny && self.authorize(call, cancel).await
        })
    }
    fn authorize<'a>(
        &'a self,
        call: &'a ValidatedCall,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            if cancel.is_cancelled() {
                return false;
            }
            let binding = &self.binding;
            // The mode belongs to the conversation and applies before every dispatch,
            // including tools that don't execute SSH commands.
            let mode = super::approvals::Approvals::new(self.repository.store.clone())
                .policy(
                    binding.session_id.clone(),
                    binding.user_id.clone(),
                    binding.task_id.clone(),
                )
                .await;
            if !matches!(mode, Ok((policy, _)) if policy != super::approvals::Policy::Deny) {
                return false;
            }
            match call.name() {
                // Trusted, bounded read of this task's captured output. Like
                // Codex's safe reads, this doesn't prompt on every invocation.
                READ_OUTPUT => true,
                // SSH performs its own durable review/decision/admission using
                // the SAME policy; it is not approved by this routing check.
                SSH_EXECUTE => match &self.executor {
                    Some(e) => e.authorize(call, cancel).await,
                    None => false,
                },
                _ if self.extensions.is_some() => {
                    self.extensions
                        .as_ref()
                        .unwrap()
                        .authorize(call, cancel)
                        .await
                }
                // MCP/other tools must supply a native approval adapter before
                // being dispatched. Neither a model name nor a skill can bypass it.
                _ => false,
            }
        })
    }

    fn normalize_arguments(&self, tool_name: &str, arguments: &mut Value) {
        if tool_name == READ_OUTPUT {
            normalize_output_arguments(arguments);
        }
        if let Some(executor) = &self.executor {
            executor.normalize_arguments(tool_name, arguments);
        }
    }

    fn uses_total_timeout(&self, tool_name: &str) -> bool {
        self.executor
            .as_ref()
            .is_none_or(|e| e.uses_total_timeout(tool_name))
    }

    fn execute<'a>(
        &'a self,
        call: &'a ValidatedCall,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            let _operation = if matches!(call.name(), SSH_EXECUTE | "call_mcp_tool") {
                Some(
                    if self
                        .extensions
                        .as_ref()
                        .is_some_and(|e| e.parallel_mcp(call))
                    {
                        futures_util::future::Either::Left(
                            tokio::select! {biased;_=cancel.cancelled()=>return ToolOutcome::Denied,guard=self.operation_gate.read()=>guard},
                        )
                    } else {
                        futures_util::future::Either::Right(
                            tokio::select! {biased;_=cancel.cancelled()=>return ToolOutcome::Denied,guard=self.operation_gate.write()=>guard},
                        )
                    },
                )
            } else {
                None
            };
            let _journal = if call.name() == SSH_EXECUTE {
                Some(
                    tokio::select! {biased;_=cancel.cancelled()=>return ToolOutcome::Denied,guard=self.journal_gate.write()=>guard},
                )
            } else {
                None
            };
            // UI starts only after scheduling/resource admission, never when a batch is queued.
            // MCP announces after acquiring its per-service transport as well.
            if call.name() != "call_mcp_tool" && self.announce(call.id(), cancel).await.is_err() {
                return ToolOutcome::Denied;
            }
            if call.name() == READ_OUTPUT {
                return self.read(call.arguments().clone()).await;
            }
            if call.name() != SSH_EXECUTE {
                return match &self.extensions {
                    Some(e) => e.execute(call, cancel, self).await,
                    None => ToolOutcome::Denied,
                };
            }
            let Some(executor) = &self.executor else {
                return ToolOutcome::Denied;
            };
            let Ok(command) = serde_json::from_value::<Command>(call.arguments().clone()) else {
                return ToolOutcome::Denied;
            };
            let binding = &self.binding;
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
            match executor
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
                        ssh_tools::Error::TerminalNotReady => "terminal_shell_not_ready",
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
                        data: json!({"message":error.to_string()}),
                    }
                }
            }
        })
    }
}
impl ToolSession {
    async fn read(&self, args: Value) -> ToolOutcome {
        let repository = self.repository.clone();
        let binding = self.binding.clone();
        let read=crate::diagnostics::spawn_blocking(move||->super::Result<Value>{
            let output_id=args["outputId"].as_str().ok_or(Error::OutputUnavailable)?;
            let offset=args["offset"].as_u64().ok_or(Error::OutputUnavailable)?;
            let limit=args["limit"].as_u64().filter(|n|(1..=OUTPUT_PAGE_BYTES).contains(n)).ok_or(Error::OutputUnavailable)? as usize;
            let output=repository.output(&binding.session_id,&binding.user_id,output_id)?;
            if output.task_id!=binding.task_id || output.event_id.is_none() {return Err(Error::OutputUnavailable);}
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
