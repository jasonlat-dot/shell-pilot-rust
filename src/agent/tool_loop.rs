//! D1 native main-model function loop. This is not an SSH executor or an IPC entry.
//! The host must supply a dispatcher with its own live target and authorization checks.
//! D2 supplies the bound SSH dispatcher; D3 adds durable approval/admission.
//! D4 connects full output and compaction to the product execution path.
mod schema;

use super::provider::{
    prepare_request, AssistantReply, ChatMessage, ChatRequest, FunctionDefinition, ModelClient,
    ModelError, ProviderKind, ToolCall,
};
use futures_util::future::BoxFuture;
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
    pub fn arguments(&self) -> &Value {
        &self.arguments
    }
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
            model_rounds: 8,
            tool_calls: 32,
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

pub struct ToolLoop<'a> {
    registry: &'a ToolRegistry,
    context: Option<&'a dyn LoopContext>,
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
        if !(1..=64).contains(&limits.model_rounds)
            || !(1..=256).contains(&limits.tool_calls)
            || !(256..=32768).contains(&limits.result_bytes)
            || limits.tool_timeout.is_zero()
            || limits.tool_timeout > Duration::from_secs(300)
        {
            return Err(Error::Invalid);
        }
        Ok(Self {
            registry,
            context: None,
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
                return Err(Error::Limit);
            }
            if let Some(context) = self.context {
                self.request = context.before_model(&self.request, cancel).await?;
            }
            self.boundary(client)?;
            self.model_rounds += 1;
            let reply = client.complete(&self.request, cancel).await?.message;
            if reply.tool_calls.is_empty() {
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
                        .and_then(|(_, schema)| schema.parse(&call.function.arguments))
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
            if self.model_rounds >= self.limits.model_rounds
                || self.tool_calls + reply.tool_calls.len() > self.limits.tool_calls
            {
                return Err(Error::Limit);
            }
            if let Some(context) = self.context {
                context
                    .begin_batch(&reply.tool_calls, assistant_text.as_deref(), cancel)
                    .await?;
            }
            self.tool_calls += reply.tool_calls.len();
            for (index, call) in validated.iter().enumerate() {
                let Some(call) = call else {
                    if let Some(context) = self.context {
                        if let ChatMessage::Tool {
                            tool_call_id,
                            content,
                        } = &self.request.messages[result_start + index]
                        {
                            context.result(tool_call_id, content).await?;
                        }
                    }
                    continue;
                };
                if cancel.is_cancelled() {
                    return Err(Error::Cancelled);
                }
                let slot = result_start + index;
                // Set unknown *before* awaiting: even dropping this future cannot make
                // an in-flight call look unexecuted. A started loop cannot be replayed.
                self.request.messages[slot] = result_message(call.id(), failure("result_unknown"));
                if let Some(context) = self.context {
                    context.started(call.id())?;
                }
                let child = cancel.child_token();
                let _cancel_on_drop = child.clone().drop_guard();
                let outcome = tokio::select! {
                    biased;
                    _ = cancel.cancelled() => ToolOutcome::OutcomeUnknown,
                    outcome = tokio::time::timeout(self.limits.tool_timeout, async { executor.execute(call, &child).await }) =>
                        outcome.unwrap_or(ToolOutcome::OutcomeUnknown),
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
                self.request.messages[slot] = ChatMessage::Tool {
                    tool_call_id: call.id().into(),
                    content: content.clone(),
                };
                if let Some(context) = self.context {
                    context.result(call.id(), &content).await?;
                }
                if unknown {
                    return Err(Error::OutcomeUnknown);
                }
            }
            // Re-check the complete real results, definitions and framing, not just
            // the user request. Never prune tool pairs or resend an oversized request.
        }
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
