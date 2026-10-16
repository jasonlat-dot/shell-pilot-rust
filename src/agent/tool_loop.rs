//! D1 native main-model function loop. This is not an SSH executor or an IPC entry.
//! The host must supply a dispatcher with its own live target and authorization checks.
//! D2 supplies the bound SSH dispatcher; D3 adds durable approval/admission.
//! D4 connects full output and compaction to the product execution path.
mod schema;

use super::approvals::{Approvals, Policy};
use super::provider::{
    prepare_request, AssistantReply, ChatMessage, ChatRequest, FunctionDefinition, ModelClient,
    ModelError, ProviderKind, ToolCall,
};
use futures_util::{future::BoxFuture, stream::FuturesUnordered, StreamExt};
use schema::Schema;
use serde_json::{json, Value};
use std::{collections::BTreeMap, time::Duration};
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Invalid tool registry or loop limits")]
    Invalid,
    #[error("This tool loop has already started; it cannot be replayed")]
    AlreadyStarted,
    #[error("Tool-loop compaction is required before another model request")]
    NeedsCompaction,
    #[error("Tool-loop round or call limit reached")]
    Limit,
    #[error("Model request round limit reached")]
    ModelRoundLimit,
    #[error("Tool call limit reached")]
    ToolCallLimit,
    #[error("Tool loop cancelled before dispatch")]
    Cancelled,
    #[error("Tool outcome is unknown; do not automatically execute it again")]
    OutcomeUnknown,
    #[error("Persistent tool context changed or cannot be stored")]
    Context,
    #[error(transparent)]
    Model(#[from] ModelError),
}
pub type Result<T> = std::result::Result<T, Error>;
pub(crate) fn external_schema_supported(schema: &Value) -> bool {
    Schema::compile_mcp(schema).is_some()
}
pub(crate) fn validate_external_arguments(schema: &Value, raw: &str) -> Option<Value> {
    Schema::compile_mcp(schema)?.parse(raw, |_| {})
}

#[cfg(test)]
mod external_schema_tests {
    use super::*;
    #[test]
    fn mcp_optional_properties_keep_exact_validation_and_reject_duplicate_arguments() {
        let schema = json!({"type":"object","properties":{"name":{"type":"string"},"limit":{"type":"integer","minimum":1}},"required":["name"],"additionalProperties":false});
        assert!(external_schema_supported(&schema));
        assert!(validate_external_arguments(&schema, r#"{"name":"sample"}"#).is_some());
        assert!(validate_external_arguments(&schema, r#"{"name":"sample","limit":1}"#).is_some());
        for invalid in [
            r#"{"name":"a","name":"b"}"#,
            r#"{"name":"a","limit":0}"#,
            r#"{"name":"a","other":true}"#,
            r#"{}"#,
        ] {
            assert!(validate_external_arguments(&schema, invalid).is_none());
        }
        assert!(!external_schema_supported(
            &json!({"type":"object","properties":{"x":{"$ref":"external"}}})
        ));
        assert!(
            Schema::compile(&schema).is_none(),
            "native closed schema contract remains strict"
        );
        for invalid in [
            json!({"type":"object","properties":false}),
            json!({"type":"object","required":"name"}),
            json!({"type":"object","properties":{},"required":["absent"]}),
        ] {
            assert!(!external_schema_supported(&invalid));
            assert!(validate_external_arguments(&invalid, "{}").is_none());
        }
    }
}

/// Immutable during a turn; only tools explicitly registered by the native host exist.
#[derive(Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, (FunctionDefinition, Schema)>,
}
impl ToolRegistry {
    pub fn register(&mut self, definition: FunctionDefinition) -> Result<()> {
        let valid_name = !definition.name.is_empty()
            && definition.name.len() <= 64
            && definition
                .name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'));
        if !valid_name
            || self.tools.len() >= 64
            || self.tools.contains_key(&definition.name)
            || definition.description.len() > 16000
            || definition.parameters.to_string().len() > 65536
        {
            return Err(Error::Invalid);
        }
        let schema = Schema::compile(&definition.parameters).ok_or(Error::Invalid)?;
        self.tools
            .insert(definition.name.clone(), (definition, schema));
        Ok(())
    }
    pub fn definitions(&self) -> Vec<FunctionDefinition> {
        self.tools
            .values()
            .map(|(definition, _)| definition.clone())
            .collect()
    }
}

/// Created only after native schema validation. Model arguments are data, never permissions.
pub struct ValidatedCall {
    call: ToolCall,
    arguments: Value,
}
impl ValidatedCall {
    pub fn id(&self) -> &str {
        &self.call.id
    }
    pub fn name(&self) -> &str {
        &self.call.function.name
    }
    pub(crate) fn scoped(&self, prefix: &str) -> Self {
        let mut call = self.call.clone();
        call.id = scoped_call_id(prefix, &self.call.id);
        Self {
            call,
            arguments: self.arguments.clone(),
        }
    }
    pub fn arguments(&self) -> &Value {
        &self.arguments
    }
}
pub(crate) fn scoped_call_id(parent: &str, id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hash = Sha256::new();
    hash.update(parent.as_bytes());
    hash.update([0]);
    hash.update(id.as_bytes());
    format!("child_{}", super::output_model::hex(&hash.finalize()))
}
pub enum ToolOutcome {
    /// Model-visible bounded data; full outputs are referenced separately.
    Success(Value),
    Failed,
    Failure {
        code: &'static str,
        data: Value,
    },
    Denied,
    OutcomeUnknown,
}
pub trait ToolExecutor: Send + Sync {
    /// Only host-validated output reads may be exempted; unknown calls still count.
    fn counts_toward_tool_limit(&self, _call: &ValidatedCall) -> bool {
        true
    }
    /// Host-owned scheduling capability. Unknown and side-effecting tools are exclusive.
    fn supports_parallel(&self, _call: &ValidatedCall) -> bool {
        false
    }

    /// Shared admission hook for EVERY tool category, not just shell commands.
    /// The native host must use the current conversation's approval policy and a trusted
    /// tool adapter. Registering a function alone never grants execution rights.
    /// Adapters with durable approvals must still recheck permission at dispatch.
    fn authorize<'a>(
        &'a self,
        _call: &'a ValidatedCall,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }

    /// Called by the shared conversation boundary before EVERY registered tool.
    /// Confirm/Auto require an adapter that implements durable human/reviewer
    /// approval for the exact call. Merely registering an MCP/native function
    /// or implementing legacy authorize() cannot bypass those modes.
    /// FullAccess still requires the adapter's target/access authorization.
    fn authorize_with_policy<'a>(
        &'a self,
        call: &'a ValidatedCall,
        policy: Policy,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move { policy == Policy::FullAccess && self.authorize(call, cancel).await })
    }

    /// Native-only normalization of parsed arguments, before schema validation.
    /// The default preserves arguments. The complete schema is checked afterwards.
    fn normalize_arguments(&self, _tool_name: &str, _arguments: &mut Value) {}

    /// Native policy only: whether this tool is bounded by Limits::tool_timeout.
    /// Tools that opt out must enforce their own timeout and respect cancellation.
    /// New tools (including MCP tools) use the total timeout unless opted out here.
    fn uses_total_timeout(&self, _tool_name: &str) -> bool {
        true
    }

    /// Native-only. Bind the task/target outside model arguments and recheck permissions
    /// on every dispatch. Respect cancellation; do not detach untracked side effects.
    fn execute<'a>(
        &'a self,
        call: &'a ValidatedCall,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, ToolOutcome>;
}
/// Trusted native persistence and context hooks; model payloads cannot install them.
pub trait LoopContext: Send + Sync {
    fn streams_replies(&self) -> bool {
        false
    }
    fn text_delta<'a>(
        &'a self,
        _text: String,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, super::provider::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn continue_after_reply<'a>(
        &'a self,
        _reply: &'a AssistantReply,
        _cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async { Ok(false) })
    }
    fn before_model<'a>(
        &'a self,
        request: &'a ChatRequest,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<ChatRequest>>;
    fn begin_batch<'a>(
        &'a self,
        calls: &'a [ToolCall],
        text: Option<&'a str>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, Result<()>>;
    fn started(&self, call_id: &str) -> Result<()>;
    fn result<'a>(&'a self, call_id: &'a str, content: &'a str) -> BoxFuture<'a, Result<()>>;
}
#[derive(Clone)]
pub struct Limits {
    pub model_rounds: usize,
    pub tool_calls: usize,
    pub result_bytes: usize,
    pub tool_timeout: Duration,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            model_rounds: 32,
            tool_calls: 128,
            result_bytes: 8192,
            tool_timeout: Duration::from_secs(30),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum State {
    Ready,
    Running,
    Finished,
    Halted,
}

/// Native-owned identity, never taken from model tool arguments. The same
/// boundary can be installed for general, SSH and future MCP tool registries.
pub struct ApprovalBoundary {
    approvals: Approvals,
    session: String,
    user: String,
}
impl ApprovalBoundary {
    pub fn new(approvals: Approvals, session: String, user: String) -> Self {
        Self {
            approvals,
            session,
            user,
        }
    }
    async fn mode(&self) -> Policy {
        self.approvals
            .conversation_policy(self.session.clone(), self.user.clone())
            .await
            .map(|(mode, _)| mode)
            .unwrap_or(Policy::Deny)
    }
}

pub struct ToolLoop<'a> {
    registry: &'a ToolRegistry,
    context: Option<&'a dyn LoopContext>,
    approval_boundary: Option<ApprovalBoundary>,
    request: ChatRequest,
    limits: Limits,
    state: State,
    model_rounds: usize,
    tool_calls: usize,
}
impl<'a> ToolLoop<'a> {
    pub fn new(
        registry: &'a ToolRegistry,
        messages: Vec<ChatMessage>,
        limits: Limits,
    ) -> Result<Self> {
        if !(1..=256).contains(&limits.model_rounds)
            || !(1..=1024).contains(&limits.tool_calls)
            || !(256..=32768).contains(&limits.result_bytes)
            || limits.tool_timeout.is_zero()
            || limits.tool_timeout > Duration::from_secs(300)
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            registry,
            context: None,
            approval_boundary: None,
            request: ChatRequest {
                messages,
                tools: registry.definitions(),
            },
            limits,
            state: State::Ready,
            model_rounds: 0,
            tool_calls: 0,
        })
    }
    pub fn with_context(mut self, context: &'a dyn LoopContext) -> Self {
        self.context = Some(context);
        self
    }
    pub fn with_approval_boundary(mut self, boundary: ApprovalBoundary) -> Self {
        self.approval_boundary = Some(boundary);
        self
    }
    pub fn state(&self) -> State {
        self.state
    }
    /// Complete call/result groups, including explicit unknown/not-executed outcomes on
    /// interruption. In-memory only: this is not durable execution recovery or approval.
    pub fn transcript(&self) -> &ChatRequest {
        &self.request
    }
    pub fn model_rounds(&self) -> usize {
        self.model_rounds
    }
    pub fn tool_calls(&self) -> usize {
        self.tool_calls
    }

    pub async fn run(
        &mut self,
        client: &ModelClient,
        executor: &impl ToolExecutor,
        cancel: &CancellationToken,
    ) -> Result<AssistantReply> {
        if self.state != State::Ready {
            return Err(Error::AlreadyStarted);
        }
        self.state = State::Running;
        let result = self.run_inner(client, executor, cancel).await;
        self.state = if result.is_ok() {
            State::Finished
        } else {
            State::Halted
        };
        result
    }
    async fn run_inner(
        &mut self,
        client: &ModelClient,
        executor: &impl ToolExecutor,
        cancel: &CancellationToken,
    ) -> Result<AssistantReply> {
        // Responses currently owns opaque text-only windows. Never flatten them or
        // silently use a Chat Completions tool loop with a different protocol.
        if client.config().provider != ProviderKind::OpenAiCompatible {
            return Err(ModelError::Invalid("tool loop requires Chat Completions").into());
        }
        loop {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if self.model_rounds >= self.limits.model_rounds {
                return Err(Error::ModelRoundLimit);
            }
            if let Some(context) = self.context {
                self.request = context.before_model(&self.request, cancel).await?;
            }
            self.boundary(client)?;
            self.model_rounds += 1;
            let reply =
                if let Some(context) = self.context.filter(|context| context.streams_replies()) {
                    client
                        .stream_tools(&self.request, cancel, |text| {
                            context.text_delta(text, cancel)
                        })
                        .await?
                        .message
                } else {
                    client.complete(&self.request, cancel).await?.message
                };
            if reply.tool_calls.is_empty() {
                if let Some(context) = self.context {
                    if context.continue_after_reply(&reply, cancel).await? {
                        continue;
                    }
                }
                self.request.messages.push(ChatMessage::Assistant {
                    content: reply.content.clone().or_else(|| reply.refusal.clone()),
                    tool_calls: vec![],
                });
                return Ok(reply);
            }

            // Validate every call before any dispatch. Paired placeholders also let the
            // provider validate cross-round duplicate IDs and full protocol ordering.
            let assistant_text = reply.content.clone();
            let mut batch = self.request.clone();
            batch.messages.push(ChatMessage::Assistant {
                content: reply.content,
                tool_calls: reply.tool_calls.clone(),
            });
            let result_start = batch.messages.len();
            let validated: Vec<_> = reply
                .tool_calls
                .iter()
                .map(|call| {
                    self.registry
                        .tools
                        .get(&call.function.name)
                        .and_then(|(_, schema)| {
                            schema.parse(&call.function.arguments, |arguments| {
                                executor.normalize_arguments(&call.function.name, arguments);
                            })
                        })
                        .map(|arguments| ValidatedCall {
                            call: call.clone(),
                            arguments,
                        })
                })
                .collect();
            for (call, valid) in reply.tool_calls.iter().zip(&validated) {
                batch.messages.push(result_message(
                    &call.id,
                    failure(if valid.is_some() {
                        "not_executed"
                    } else {
                        "invalid_arguments"
                    }),
                ));
            }
            // Protocol errors leave the previous complete transcript untouched.
            // Hard/soft budget errors retain this whole unexecuted batch for the host.
            let preflight = if self.context.is_some() {
                // A pending batch may cross the soft trigger. Compact only after real results.
                prepare_request(client.config(), &batch)
                    .map(|_| ())
                    .map_err(Error::from)
            } else {
                Self::check_boundary(client, &batch)
            };
            match preflight {
                Err(Error::NeedsCompaction) => {
                    self.request = batch;
                    return Err(Error::NeedsCompaction);
                }
                Err(error) => return Err(error),
                Ok(()) => {
                    self.request = batch;
                }
            }
            let charged = validated
                .iter()
                .filter(|call| {
                    call.as_ref()
                        .is_none_or(|call| executor.counts_toward_tool_limit(call))
                })
                .count();
            if self.model_rounds >= self.limits.model_rounds {
                return Err(Error::ModelRoundLimit);
            }
            if self.tool_calls + charged > self.limits.tool_calls {
                return Err(Error::ToolCallLimit);
            }
            if let Some(context) = self.context {
                context
                    .begin_batch(&reply.tool_calls, assistant_text.as_deref(), cancel)
                    .await?;
            }
            self.tool_calls += charged;
            // Parallelize only a contiguous group of host-approved independent calls.
            // Exclusive calls form fences; later calls cannot jump ahead of them.
            let mut start = 0;
            while start < validated.len() {
                let parallel = validated[start]
                    .as_ref()
                    .is_some_and(|c| executor.supports_parallel(c));
                let mut end = start + 1;
                if parallel {
                    while end < validated.len()
                        && validated[end]
                            .as_ref()
                            .is_some_and(|c| executor.supports_parallel(c))
                    {
                        end += 1;
                    }
                }
                let group = cancel.child_token();
                let _group_guard = group.clone().drop_guard();
                let mut active = FuturesUnordered::new();
                let mut next = start;
                let mut failure = None;
                let mut completed = vec![];
                loop {
                    while failure.is_none() && next < end && active.len() < 4 {
                        let index = next;
                        next += 1;
                        let operation = self.run_call(
                            executor,
                            validated[index].as_ref(),
                            &reply.tool_calls[index],
                            &group,
                        );
                        active.push(async move { (index, operation.await) });
                    }
                    let Some((index, result)) = active.next().await else {
                        break;
                    };
                    match result {
                        Ok((content, unknown)) => {
                            completed.push((index, content));
                            if unknown {
                                failure.get_or_insert(Error::OutcomeUnknown);
                                group.cancel();
                            }
                        }
                        Err(error) => {
                            failure.get_or_insert(error);
                            group.cancel();
                        }
                    }
                }
                drop(active);
                for (index, content) in completed {
                    self.request.messages[result_start + index] = ChatMessage::Tool {
                        tool_call_id: reply.tool_calls[index].id.clone(),
                        content,
                    };
                }
                if let Some(error) = failure {
                    return Err(error);
                }
                start = end;
            }
            // Re-check the complete real results, definitions and framing, not just
            // the user request. Never prune tool pairs or resend an oversized request.
        }
    }
    fn run_call<'b>(
        &'b self,
        executor: &'b impl ToolExecutor,
        call: Option<&'b ValidatedCall>,
        raw: &'b ToolCall,
        cancel: &'b CancellationToken,
    ) -> BoxFuture<'b, Result<(String, bool)>> {
        Box::pin(async move {
            let Some(call) = call else {
                let content = failure("invalid_arguments").to_string();
                if let Some(context) = self.context {
                    context.result(&raw.id, &content).await?;
                }
                return Ok((content, false));
            };
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let authorized = tokio::select! {biased;
                _=cancel.cancelled()=>return Err(Error::Cancelled),
                result=async {match &self.approval_boundary {
                    Some(boundary)=>{let mode=boundary.mode().await;mode!=Policy::Deny&&executor.authorize_with_policy(call,mode,cancel).await},
                    None if self.context.is_none()=>executor.authorize(call,cancel).await,
                    None=>false,
                }}=>result,
            };
            if !authorized {
                let content = failure("denied").to_string();
                if let Some(context) = self.context {
                    context.result(call.id(), &content).await?;
                }
                return Ok((content, false));
            }
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            if let Some(context) = self.context {
                context.started(call.id())?;
            }
            let child = cancel.child_token();
            let _guard = child.clone().drop_guard();
            let outcome = tokio::select! {biased;
                _=cancel.cancelled()=>ToolOutcome::OutcomeUnknown,
                result=async {
                    let execution=executor.execute(call,&child);
                    if executor.uses_total_timeout(call.name()){tokio::time::timeout(self.limits.tool_timeout,execution).await.unwrap_or(ToolOutcome::OutcomeUnknown)}else{execution.await}
                }=>result,
            };
            child.cancel();
            let (value, unknown) = match outcome {
                ToolOutcome::Success(data) => (json!({"ok":true,"data":data}), false),
                ToolOutcome::Failed => (failure("tool_failed"), false),
                ToolOutcome::Failure { code, data } => {
                    (json!({"ok":false,"error":code,"data":data}), false)
                }
                ToolOutcome::Denied => (failure("denied"), false),
                ToolOutcome::OutcomeUnknown => (failure("result_unknown"), true),
            };
            let mut content = value.to_string();
            if content.len() > self.limits.result_bytes {
                content = failure("output_too_large").to_string();
            }
            if let Some(context) = self.context {
                context.result(call.id(), &content).await?;
            }
            Ok((content, unknown))
        })
    }
    fn boundary(&self, client: &ModelClient) -> Result<()> {
        if self.context.is_some() {
            prepare_request(client.config(), &self.request)?;
            Ok(())
        } else {
            Self::check_boundary(client, &self.request)
        }
    }
    fn check_boundary(client: &ModelClient, request: &ChatRequest) -> Result<()> {
        let measured = super::provider::measure_request(client.config(), request)?;
        if measured.should_compact
            || request.messages.len() > 256
            || measured.body.len() > 2 * 1024 * 1024
        {
            return Err(Error::NeedsCompaction);
        }
        prepare_request(client.config(), request)?;
        Ok(())
    }
}
fn failure(code: &str) -> Value {
    json!({"ok":false,"error":code})
}
fn result_message(id: &str, value: Value) -> ChatMessage {
    ChatMessage::Tool {
        tool_call_id: id.into(),
        content: value.to_string(),
    }
}
