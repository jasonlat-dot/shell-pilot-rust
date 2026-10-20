//! One factory selects immutable wire strategies; the Agent only sees normalized messages.
use super::*;
use reqwest::header::{self, HeaderMap, HeaderValue};
use std::collections::HashSet;

pub(super) struct StreamFrame {
    pub text: String,
    pub done: bool,
}
pub(super) trait ResponseStream: Send {
    fn push(&mut self, frame: &str) -> Result<StreamFrame>;
    fn finish(self: Box<Self>, allowed: &HashSet<String>) -> Result<Completion>;
}
pub(super) trait ProviderAdapter: Sync {
    fn catalog(&self) -> &'static dyn super::catalog::CatalogAdapter { &super::catalog::OPENAI }
    fn normalize_config(&self, config: &mut ModelConfig) -> Result<()> {
        if config.reasoning_effort == Some(ReasoningEffort::Max) {
            return Err(ModelError::Invalid("max effort requires Messages"));
        }
        Ok(())
    }
    fn supports_tools(&self) -> bool {
        true
    }
    fn response_windows(&self) -> bool {
        false
    }
    fn endpoint(&self, config: &ModelConfig) -> Result<url::Url> { config.relative_endpoint() }
    fn prepare_stream(&self, endpoint: &mut url::Url, body: &mut serde_json::Value) -> Result<()> {
        let _ = endpoint;
        body["stream"] = true.into();
        Ok(())
    }
    fn finish_on_eof(&self) -> bool { false }
    fn headers(&self, key: Option<&SecretKey>) -> Result<HeaderMap>;
    fn measure(&self, config: &ModelConfig, request: &ChatRequest) -> Result<PreparedRequest>;
    fn decode(&self, bytes: &[u8], allowed: &HashSet<String>) -> Result<Completion>;
    fn stream(&self, tools: bool) -> Result<Box<dyn ResponseStream>>;
    fn json_stream_fallback(&self, tools: bool) -> bool {
        tools
    }
}
struct ChatCompletions;
struct Responses;
static CHAT: ChatCompletions = ChatCompletions;
static RESPONSES: Responses = Responses;
static GOOGLE: super::google::GoogleGemini = super::google::GoogleGemini;
static ANTHROPIC: super::anthropic::AnthropicMessages = super::anthropic::AnthropicMessages;
pub(super) fn select(kind: &ProviderKind) -> &'static dyn ProviderAdapter {
    match kind {
        ProviderKind::OpenAiCompatible => &CHAT,
        ProviderKind::OpenAiResponses => &RESPONSES,
        ProviderKind::AnthropicMessages => &ANTHROPIC,
        ProviderKind::GoogleGemini => &GOOGLE,
    }
}
pub(super) fn credential_headers(
    key: Option<&SecretKey>,
    name: header::HeaderName,
    bearer: bool,
) -> Result<HeaderMap> {
    let mut headers = HeaderMap::new();
    if let Some(key) = key {
        let key = SecretKey::new(key.0.clone())?;
        let raw = if bearer {
            format!("Bearer {}", key.0)
        } else {
            key.0
        };
        let mut value = HeaderValue::from_str(&raw).map_err(|_| ModelError::Credentials)?;
        value.set_sensitive(true);
        headers.insert(name, value);
    }
    Ok(headers)
}
impl ProviderAdapter for ChatCompletions {
    fn headers(&self, key: Option<&SecretKey>) -> Result<HeaderMap> {
        credential_headers(key, header::AUTHORIZATION, true)
    }
    fn measure(&self, c: &ModelConfig, r: &ChatRequest) -> Result<PreparedRequest> {
        super::protocol::measure_chat(c, r)
    }
    fn decode(&self, b: &[u8], a: &HashSet<String>) -> Result<Completion> {
        super::protocol::decode_response(b, a)
    }
    fn stream(&self, tools: bool) -> Result<Box<dyn ResponseStream>> {
        Ok(if tools {
            super::tool_stream::decoder()
        } else {
            super::stream::decoder()
        })
    }
}
impl ProviderAdapter for Responses {
    fn response_windows(&self) -> bool {
        true
    }
    fn headers(&self, key: Option<&SecretKey>) -> Result<HeaderMap> {
        credential_headers(key, header::AUTHORIZATION, true)
    }
    fn measure(&self, c: &ModelConfig, r: &ChatRequest) -> Result<PreparedRequest> {
        super::responses::measure(c, r)
    }
    fn decode(&self, _: &[u8], _: &HashSet<String>) -> Result<Completion> {
        Err(ModelError::ContextMismatch)
    }
    fn stream(&self, _: bool) -> Result<Box<dyn ResponseStream>> {
        Err(ModelError::ContextMismatch)
    }
}
