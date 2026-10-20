//! Google GenerateContent codec. Authentication, URLs and wire state stay in this strategy.
use super::{
    adapter::{ProviderAdapter, ResponseStream, StreamFrame},
    *,
};
use crate::agent::attachments::Part;
use reqwest::header::{HeaderMap, HeaderName};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};

pub(super) struct GoogleGemini;
impl ProviderAdapter for GoogleGemini {
    fn normalize_config(&self, config: &mut ModelConfig) -> Result<()> {
        config.output_token_parameter = OutputTokenParameter::MaxTokens;
        let model = config
            .model
            .strip_prefix("models/")
            .unwrap_or(&config.model);
        if model.is_empty()
            || !model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(ModelError::Invalid(
                "Gemini model must be a model ID, optionally prefixed with models/",
            ));
        }
        config.model = model.into();
        if matches!(
            config.reasoning_effort,
            Some(ReasoningEffort::None | ReasoningEffort::Xhigh | ReasoningEffort::Max)
        ) {
            return Err(ModelError::Invalid(
                "Gemini thinking level must be minimal, low, medium or high",
            ));
        }
        Ok(())
    }
    fn endpoint(&self, config: &ModelConfig) -> Result<url::Url> {
        let mut endpoint = config.relative_endpoint()?;
        let model = config
            .model
            .strip_prefix("models/")
            .unwrap_or(&config.model);
        if model.is_empty()
            || !model
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
        {
            return Err(ModelError::Invalid("invalid Gemini model ID"));
        }
        endpoint.set_path(&format!(
            "{}/{}:generateContent",
            endpoint.path().trim_end_matches('/'),
            model
        ));
        Ok(endpoint)
    }
    fn prepare_stream(&self, endpoint: &mut url::Url, _: &mut Value) -> Result<()> {
        let prefix = endpoint
            .path()
            .strip_suffix(":generateContent")
            .ok_or(ModelError::ContextMismatch)?
            .to_owned();
        endpoint.set_path(&format!("{prefix}:streamGenerateContent"));
        endpoint.query_pairs_mut().append_pair("alt", "sse");
        Ok(())
    }
    fn finish_on_eof(&self) -> bool {
        true
    }
    fn headers(&self, key: Option<&SecretKey>) -> Result<HeaderMap> {
        super::adapter::credential_headers(key, HeaderName::from_static("x-goog-api-key"), false)
    }
    fn measure(&self, config: &ModelConfig, request: &ChatRequest) -> Result<PreparedRequest> {
        let allowed = super::protocol::validate_request(config, request)?;
        let body = serde_json::to_string(&encode(config, request)?)
            .map_err(|_| ModelError::ToolProtocol)?;
        if body.len() > 32 * 1024 * 1024 {
            return Err(ModelError::ResponseTooLarge);
        }
        let cost = super::protocol::media_adjusted_cost(&body)?
            .checked_add(request.messages.len() as u64 * 12)
            .ok_or(ModelError::BudgetExceeded)?;
        let available = config
            .budget()
            .available_input()
            .map_err(|_| ModelError::BudgetExceeded)?;
        Ok(PreparedRequest {
            endpoint: config.endpoint()?,
            body,
            estimated_input_tokens: cost,
            should_compact: u128::from(cost) * 100
                >= u128::from(available) * u128::from(config.compact_percent),
            allowed_tools: allowed,
        })
    }
    fn decode(&self, bytes: &[u8], allowed: &HashSet<String>) -> Result<Completion> {
        let value: Value =
            crate::agent::strict_json::parse_slice(bytes).map_err(|_| ModelError::ToolProtocol)?;
        let mut stream = GeminiStream::default();
        stream.accept(value)?;
        stream.assemble(allowed)
    }
    fn stream(&self, _: bool) -> Result<Box<dyn ResponseStream>> {
        Ok(Box::new(GeminiStream::default()))
    }
    fn json_stream_fallback(&self, _: bool) -> bool {
        true
    }
}
fn push(contents: &mut Vec<Value>, role: &str, parts: Vec<Value>) -> Result<()> {
    if parts.is_empty() {
        return Err(ModelError::ToolProtocol);
    }
    if let Some(last) = contents.last_mut().filter(|last| last["role"] == role) {
        last["parts"]
            .as_array_mut()
            .ok_or(ModelError::ToolProtocol)?
            .extend(parts);
    } else {
        contents.push(json!({"role":role,"parts":parts}));
    }
    Ok(())
}
fn encode(config: &ModelConfig, request: &ChatRequest) -> Result<Value> {
    let mut contents = Vec::new();
    let mut system = Vec::new();
    // Local call IDs correlate host results; wire IDs are optional in GenerateContent.
    let mut calls: HashMap<String, (String, Option<String>)> = HashMap::new();
    for message in &request.messages {
        match message {
            ChatMessage::System { content } | ChatMessage::Developer { content } => {
                system.push(json!({"text":content}))
            }
            ChatMessage::User { content } => {
                push(&mut contents, "user", vec![json!({"text":content})])?
            }
            ChatMessage::UserMedia { content } => {
                let parts = content
                    .iter()
                    .map(|part| match part {
                        Part::Text { text } => Ok(json!({"text":text})),
                        Part::ImageUrl { image_url } => {
                            let (mime, data) = image_url
                                .url
                                .split_once(";base64,")
                                .ok_or(ModelError::ToolProtocol)?;
                            let mime = mime
                                .strip_prefix("data:")
                                .filter(|mime| {
                                    matches!(
                                        *mime,
                                        "image/png" | "image/jpeg" | "image/webp" | "image/gif"
                                    )
                                })
                                .ok_or(ModelError::ToolProtocol)?;
                            if data.is_empty() {
                                return Err(ModelError::ToolProtocol);
                            }
                            Ok(json!({"inlineData":{"mimeType":mime,"data":data}}))
                        }
                    })
                    .collect::<Result<Vec<_>>>()?;
                push(&mut contents, "user", parts)?;
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut parts = Vec::new();
                if let Some(text) = content.as_ref().filter(|s| !s.is_empty()) {
                    parts.push(json!({"text":text}));
                }
                for call in tool_calls {
                    calls.insert(
                        call.id.clone(),
                        (call.function.name.clone(), Some(call.id.clone())),
                    );
                    let args: Value = serde_json::from_str(&call.function.arguments)
                        .map_err(|_| ModelError::ToolProtocol)?;
                    parts.push(json!({"functionCall":{"id":call.id,"name":call.function.name,"args":args}}));
                }
                push(&mut contents, "model", parts)?;
            }
            ChatMessage::Replay { state } => {
                let wire = state.wire(config)?;
                let parts = wire["parts"]
                    .as_array()
                    .ok_or(ModelError::ContextMismatch)?;
                let raw_calls: Vec<_> =
                    parts.iter().filter_map(|p| p.get("functionCall")).collect();
                if raw_calls.len() != state.calls.len() {
                    return Err(ModelError::ContextMismatch);
                }
                for (call, raw) in state.calls.iter().zip(raw_calls) {
                    if raw["name"] != call.function.name {
                        return Err(ModelError::ContextMismatch);
                    }
                    calls.insert(
                        call.id.clone(),
                        (
                            call.function.name.clone(),
                            raw["id"].as_str().map(str::to_owned),
                        ),
                    );
                }
                // Preserve part boundaries, thoughts and signatures byte-for-byte as JSON values.
                contents.push(wire);
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                let (name, id) = calls.remove(tool_call_id).ok_or(ModelError::ToolProtocol)?;
                let response: Value =
                    serde_json::from_str(content).unwrap_or_else(|_| json!({"result":content}));
                let response = if response.is_object() {
                    response
                } else {
                    json!({"result":response})
                };
                let mut result = json!({"name":name,"response":response});
                if let Some(id) = id {
                    result["id"] = id.into();
                }
                push(
                    &mut contents,
                    "user",
                    vec![json!({"functionResponse":result})],
                )?;
            }
            ChatMessage::Responses { .. } => return Err(ModelError::ContextMismatch),
        }
    }
    let mut generation = json!({"maxOutputTokens":config.output_reserve,"candidateCount":1});
    if let Some(t) = config.temperature {
        generation["temperature"] = json!(t);
    }
    if let Some(level) = &config.reasoning_effort {
        generation["thinkingConfig"] = json!({"thinkingLevel":serde_json::to_value(level).map_err(|_|ModelError::ToolProtocol)?.as_str().ok_or(ModelError::ToolProtocol)?.to_ascii_uppercase()});
    }
    let mut body = json!({"contents":contents,"generationConfig":generation});
    if !system.is_empty() {
        body["systemInstruction"] = json!({"parts":system});
    }
    if !request.tools.is_empty() {
        body["tools"] = json!([{"functionDeclarations":request.tools.iter().map(|t|json!({"name":t.name,"description":t.description,"parametersJsonSchema":t.parameters})).collect::<Vec<_>>()}]);
    }
    Ok(body)
}
#[derive(Default)]
struct GeminiStream {
    parts: Vec<Value>,
    reason: Option<String>,
    usage: Option<Value>,
    text_bytes: usize,
}
impl GeminiStream {
    fn accept(&mut self, value: Value) -> Result<String> {
        if value.get("error").is_some() {
            return Err(ModelError::Response);
        }
        if value["promptFeedback"].get("blockReason").is_some() {
            return Err(ModelError::Incomplete);
        }
        if let Some(usage) = value.get("usageMetadata") {
            if !usage.is_object() {
                return Err(ModelError::Response);
            }
            let target = self.usage.get_or_insert_with(|| json!({}));
            target
                .as_object_mut()
                .ok_or(ModelError::Response)?
                .extend(usage.as_object().ok_or(ModelError::Response)?.clone());
        }
        let candidates = value.get("candidates").and_then(Value::as_array);
        let Some(candidate) = candidates.and_then(|c| c.first()) else {
            return if self.reason.is_some() && self.usage.is_some() {
                Ok(String::new())
            } else {
                Err(ModelError::Incomplete)
            };
        };
        if candidates.is_some_and(|c| c.len() != 1)
            || candidate.get("index").is_some_and(|v| v != 0)
        {
            return Err(ModelError::Response);
        }
        let mut text = String::new();
        if let Some(content) = candidate.get("content") {
            if self.reason.is_some() || content.get("role").is_some_and(|r| r != "model") {
                return Err(ModelError::Response);
            }
            let parts = content["parts"].as_array().ok_or(ModelError::Response)?;
            for part in parts {
                if part.get("thought").is_some_and(|v| !v.is_boolean())
                    || part.get("text").is_some() && part.get("functionCall").is_some()
                {
                    return Err(ModelError::Response);
                }
                if let Some(signature) = part.get("thoughtSignature") {
                    if signature.as_str().is_none_or(str::is_empty) {
                        return Err(ModelError::Response);
                    }
                }
                if let Some(value) = part.get("text") {
                    let value = value.as_str().ok_or(ModelError::Response)?;
                    if part.get("thought").is_none_or(|v| v == false) {
                        text.push_str(value);
                    }
                } else if part.get("functionCall").is_none() {
                    return Err(ModelError::Response);
                }
                self.parts.push(part.clone());
            }
        }
        self.text_bytes = self.text_bytes.saturating_add(text.len());
        if self.text_bytes > 16000 || self.parts.len() > 4096 {
            return Err(ModelError::ResponseTooLarge);
        }
        if let Some(reason) = candidate.get("finishReason") {
            let reason = reason.as_str().ok_or(ModelError::Response)?;
            if self.reason.as_deref().is_some_and(|old| old != reason) {
                return Err(ModelError::Response);
            }
            self.reason = Some(reason.into());
        }
        Ok(text)
    }
    fn assemble(self, allowed: &HashSet<String>) -> Result<Completion> {
        if self.reason.as_deref() != Some("STOP") {
            return Err(ModelError::Incomplete);
        }
        let mut text = String::new();
        let mut calls = Vec::new();
        for part in &self.parts {
            if part["thought"] != true {
                if let Some(value) = part["text"].as_str() {
                    text.push_str(value);
                }
            }
            if let Some(call) = part.get("functionCall") {
                let args = call.get("args").cloned().unwrap_or_else(|| json!({}));
                if !args.is_object() {
                    return Err(ModelError::ToolProtocol);
                }
                let id = match call.get("id") {
                    Some(id) => id
                        .as_str()
                        .filter(|s| !s.is_empty())
                        .ok_or(ModelError::ToolProtocol)?
                        .into(),
                    None => format!("gemini-{}", uuid::Uuid::new_v4()),
                };
                calls.push(ToolCall {
                    id,
                    kind: "function".into(),
                    function: FunctionCall {
                        name: call["name"]
                            .as_str()
                            .ok_or(ModelError::ToolProtocol)?
                            .into(),
                        arguments: serde_json::to_string(&args)
                            .map_err(|_| ModelError::ToolProtocol)?,
                    },
                });
            }
        }
        if text.trim().is_empty() && calls.is_empty() {
            return Err(ModelError::Incomplete);
        }
        let usage = self
            .usage
            .map(|u| -> Result<Usage> {
                for key in [
                    "promptTokenCount",
                    "candidatesTokenCount",
                    "thoughtsTokenCount",
                    "totalTokenCount",
                ] {
                    if u.get(key).is_some_and(|value| value.as_u64().is_none()) {
                        return Err(ModelError::Response);
                    }
                }
                let input = u["promptTokenCount"].as_u64().ok_or(ModelError::Response)?;
                let output = u["candidatesTokenCount"]
                    .as_u64()
                    .unwrap_or(0)
                    .checked_add(u["thoughtsTokenCount"].as_u64().unwrap_or(0))
                    .ok_or(ModelError::Response)?;
                let total = input.checked_add(output).ok_or(ModelError::Response)?;
                Ok(Usage {
                    prompt_tokens: input,
                    completion_tokens: output,
                    total_tokens: u["totalTokenCount"].as_u64().unwrap_or(total),
                })
            })
            .transpose()?;
        let finish = if calls.is_empty() {
            "stop"
        } else {
            "tool_calls"
        };
        let mut reply = super::protocol::validate_completion(
            AssistantReply {
                role: "assistant".into(),
                content: (!text.is_empty()).then_some(text),
                refusal: None,
                tool_calls: calls,
                continuation: None,
            },
            usage,
            finish.into(),
            allowed,
        )?;
        reply.message.continuation = Some(Continuation::new(
            ProviderKind::GoogleGemini,
            json!({"role":"model","parts":self.parts}),
            &reply.message,
        )?);
        Ok(reply)
    }
}
impl ResponseStream for GeminiStream {
    fn push(&mut self, frame: &str) -> Result<StreamFrame> {
        let value =
            crate::agent::strict_json::parse(frame).map_err(|_| ModelError::ToolProtocol)?;
        Ok(StreamFrame {
            text: self.accept(value)?,
            done: false,
        })
    }
    fn finish(self: Box<Self>, allowed: &HashSet<String>) -> Result<Completion> {
        self.assemble(allowed)
    }
}

#[cfg(test)]
mod tests;
