//! Native-only provider continuation. Strategies preserve protocol state, never
//! expose it as visible text or infer behavior from a model-name substring.
use super::*;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Clone, Serialize, Deserialize)]
pub struct Continuation {
    pub(crate) binding: Option<super::responses::Binding>,
    protocol: ProviderKind,
    wire: Value,
    pub(crate) content: Option<String>,
    pub(crate) calls: Vec<ToolCall>,
}
impl std::fmt::Debug for Continuation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Continuation([NATIVE_ONLY])")
    }
}
trait ReplayStrategy: Sync {
    fn replay(&self, state: &Continuation) -> Result<Value>;
}
struct ChatReplay;
struct MessagesReplay;
struct ResponsesReplay;
struct GoogleReplay;
static GOOGLE: GoogleReplay = GoogleReplay;
static CHAT: ChatReplay = ChatReplay;
static MESSAGES: MessagesReplay = MessagesReplay;
static RESPONSES: ResponsesReplay = ResponsesReplay;
fn strategy(protocol: &ProviderKind) -> &'static dyn ReplayStrategy {
    match protocol {
        ProviderKind::OpenAiCompatible => &CHAT,
        ProviderKind::AnthropicMessages => &MESSAGES,
        ProviderKind::OpenAiResponses => &RESPONSES,
        ProviderKind::GoogleGemini => &GOOGLE,
    }
}
impl ReplayStrategy for ChatReplay {
    fn replay(&self, state: &Continuation) -> Result<Value> {
        if state.wire["role"] != "assistant" {
            return Err(ModelError::ContextMismatch);
        }
        Ok(state.wire.clone())
    }
}
impl ReplayStrategy for MessagesReplay {
    fn replay(&self, state: &Continuation) -> Result<Value> {
        if !state.wire.is_array() {
            return Err(ModelError::ContextMismatch);
        }
        Ok(state.wire.clone())
    }
}
impl ReplayStrategy for ResponsesReplay {
    fn replay(&self, state: &Continuation) -> Result<Value> {
        if !state.wire.is_array() {
            return Err(ModelError::ContextMismatch);
        }
        Ok(state.wire.clone())
    }
}
impl ReplayStrategy for GoogleReplay {
    fn replay(&self, state: &Continuation) -> Result<Value> {
        if state.wire["role"] != "model" || !state.wire["parts"].is_array() { return Err(ModelError::ContextMismatch); }
        Ok(state.wire.clone())
    }
}
impl Continuation {
    pub(crate) fn new(protocol: ProviderKind, wire: Value, reply: &AssistantReply) -> Result<Self> {
        if serde_json::to_vec(&wire)
            .map_err(|_| ModelError::Response)?
            .len()
            > 8 * 1024 * 1024
        {
            return Err(ModelError::ResponseTooLarge);
        }
        Ok(Self {
            binding: None,
            protocol,
            wire,
            content: reply.content.clone().or_else(|| reply.refusal.clone()),
            calls: reply.tool_calls.clone(),
        })
    }
    pub(crate) fn chat(raw: &Value, reply: &AssistantReply) -> Result<Option<Self>> {
        let mut wire = json!({"role":"assistant","content":reply.content});
        if !reply.tool_calls.is_empty() { wire["tool_calls"] = serde_json::to_value(&reply.tool_calls).map_err(|_|ModelError::Response)?; }
        if let Some(refusal) = &reply.refusal { wire["refusal"] = refusal.clone().into(); }
        let mut extra = false;
        if let Some(reasoning) = raw.get("reasoning_content").filter(|v| !v.is_null()) {
            if !reasoning.is_string() {
                return Err(ModelError::Response);
            }
            wire["reasoning_content"] = reasoning.clone();
            extra = true;
        }
        if let Some(calls) = raw["tool_calls"].as_array() {
            for (index, call) in calls.iter().enumerate() {
                if let Some(signature) = call.pointer("/extra_content/google/thought_signature") {
                    if !signature.is_string() || signature.as_str() == Some("") {
                        return Err(ModelError::ToolProtocol);
                    }
                    let target = wire["tool_calls"]
                        .get_mut(index)
                        .ok_or(ModelError::ToolProtocol)?;
                    target["extra_content"] = json!({"google":{"thought_signature":signature}});
                    extra = true;
                }
            }
        }
        if extra {
            Ok(Some(Self::new(
                ProviderKind::OpenAiCompatible,
                wire,
                reply,
            )?))
        } else {
            Ok(None)
        }
    }
    pub(crate) fn seal(&mut self, binding: &super::responses::Binding) {
        self.binding = Some(binding.clone());
    }
    pub(super) fn rename_chat_reasoning(&mut self, field: &str) -> Result<()> {
        if self.protocol != ProviderKind::OpenAiCompatible { return Err(ModelError::ContextMismatch); }
        let wire = self.wire.as_object_mut().ok_or(ModelError::ContextMismatch)?;
        if let Some(reasoning) = wire.remove("reasoning_content") { wire.insert(field.into(), reasoning); }
        Ok(())
    }
    pub(crate) fn validate_binding(&self, binding: &super::responses::Binding) -> Result<()> {
        if self.binding.as_ref() != Some(binding) {
            return Err(ModelError::ContextMismatch);
        }
        Ok(())
    }
    pub(crate) fn wire(&self, config: &ModelConfig) -> Result<Value> {
        if self.protocol != config.provider {
            return Err(ModelError::ContextMismatch);
        }
        self.binding
            .as_ref()
            .ok_or(ModelError::ContextMismatch)?
            .matches_config(config)?;
        strategy(&self.protocol).replay(self)
    }
    pub(crate) fn message(self) -> ChatMessage {
        ChatMessage::Replay { state: self }
    }
}
impl AssistantReply {
    pub(crate) fn history_message(&self) -> ChatMessage {
        match &self.continuation {
            Some(state) => state.clone().message(),
            None => ChatMessage::Assistant {
                content: self.content.clone().or_else(|| self.refusal.clone()),
                tool_calls: self.tool_calls.clone(),
            },
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    pub(crate) fn runtime() -> RuntimeConfig {
        RuntimeConfig { api_key: Some(SecretKey::new("test-only-key").unwrap()), config: serde_json::from_value(json!({
            "provider":"openAiCompatible","baseUrl":"https://example.invalid/v1/","completionsPath":"chat/completions",
            "model":"arbitrary-name","outputTokenParameter":"maxTokens","capabilities":{"developerRole":false,"temperature":false,"reasoningEffort":false},"requireApiKey":true
        })).unwrap() }
    }
    pub(crate) fn reply(runtime: &RuntimeConfig, calls: bool) -> AssistantReply {
        let tool_calls = if calls {
            json!([{"id":"one","type":"function","function":{"name":"inspect","arguments":"{}"}}])
        } else {
            json!([])
        };
        let bytes = serde_json::to_vec(&json!({"choices":[{"index":0,"message":{"role":"assistant","content":"visible","reasoning_content":"private-reasoning","tool_calls":tool_calls},"finish_reason":if calls {"tool_calls"} else {"stop"}}]})).unwrap();
        let mut completion = super::super::protocol::decode_response(
            &bytes,
            &std::collections::HashSet::from(["inspect".into()]),
        )
        .unwrap();
        completion
            .message
            .continuation
            .as_mut()
            .unwrap()
            .seal(&super::super::responses::Binding::from_runtime(runtime).unwrap());
        completion.message
    }
    #[test]
    fn ordinary_models_do_not_get_reasoning_fields_and_native_state_cannot_be_injected() {
        let raw = json!({"choices":[{"index":0,"message":{"role":"assistant","content":"visible"},"finish_reason":"stop"}]});
        assert!(super::super::protocol::decode_response(
            &serde_json::to_vec(&raw).unwrap(),
            &Default::default()
        )
        .unwrap()
        .message
        .continuation
        .is_none());
        assert!(
            serde_json::from_value::<ChatMessage>(json!({"role":"replay","state":{}})).is_err()
        );
        let runtime = runtime();
        let reply = reply(&runtime, false);
        assert!(!serde_json::to_string(&reply)
            .unwrap()
            .contains("private-reasoning"));
        assert!(!format!("{:?}", reply).contains("private-reasoning"));
        assert!(serde_json::to_value(reply.history_message()).is_err());
    }
    #[test]
    fn state_is_bound_to_protocol_model_endpoint_and_credentials() {
        let runtime = runtime();
        let state = reply(&runtime, true).continuation.unwrap();
        assert!(state
            .validate_binding(&super::super::responses::Binding::from_runtime(&runtime).unwrap())
            .is_ok());
        for change in 0..4 {
            let mut other = runtime.clone();
            match change {
                0 => other.config.model = "different".into(),
                1 => other.config.base_url = "https://other.invalid/v1/".into(),
                2 => other.api_key = Some(SecretKey::new("other-key").unwrap()),
                _ => {
                    other.config.provider = ProviderKind::AnthropicMessages;
                    other.config.completions_path = "messages".into();
                }
            }
            assert!(state
                .validate_binding(&super::super::responses::Binding::from_runtime(&other).unwrap())
                .is_err());
        }
    }
}
