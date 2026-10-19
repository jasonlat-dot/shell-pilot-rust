//! Anthropic Messages wire adapter. No provider-specific shapes enter the Agent loop.
mod stream;
use super::{
    adapter::{ProviderAdapter, ResponseStream},
    *,
};
use crate::agent::attachments::Part;
use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde::{Deserialize, Serialize};
use serde_json::{json, value::RawValue, Value};
use std::collections::HashSet;

pub(super) struct AnthropicMessages;
impl ProviderAdapter for AnthropicMessages {
    fn normalize_config(&self, config: &mut ModelConfig) -> Result<()> {
        config.output_token_parameter = OutputTokenParameter::MaxTokens;
        if matches!(config.reasoning_effort, Some(ReasoningEffort::None | ReasoningEffort::Minimal)) {
            return Err(ModelError::Invalid("Messages effort must be low, medium, high, xhigh or max"));
        }
        if config.temperature.is_some_and(|value| value > 1.0) {
            return Err(ModelError::Invalid(
                "Messages temperature must be between 0 and 1",
            ));
        }
        Ok(())
    }

    fn headers(&self, key: Option<&SecretKey>) -> Result<HeaderMap> {
        let mut headers =
            super::adapter::credential_headers(key, HeaderName::from_static("x-api-key"), false)?;
        headers.insert(
            HeaderName::from_static("anthropic-version"),
            HeaderValue::from_static("2023-06-01"),
        );
        Ok(headers)
    }
    fn measure(&self, config: &ModelConfig, request: &ChatRequest) -> Result<PreparedRequest> {
        let allowed = super::protocol::validate_request(config, request)?;
        let body = encode(config, request)?;
        let body = serde_json::to_string(&body).map_err(|_| ModelError::ToolProtocol)?;
        if body.len() > 32 * 1024 * 1024 {
            return Err(ModelError::Invalid("model planning request exceeds 32 MiB"));
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
        let response: Message = serde_json::from_slice(bytes).map_err(|_| ModelError::Response)?;
        if response.kind != "message" || response.role != "assistant" {
            return Err(ModelError::Response);
        }
        assemble(
            response.content,
            response.usage,
            response.stop_reason,
            allowed,
        )
    }
    fn stream(&self, _: bool) -> Result<Box<dyn ResponseStream>> {
        Ok(Box::<stream::MessagesStream>::default())
    }
    fn json_stream_fallback(&self, _: bool) -> bool {
        true
    }
}
fn push(messages: &mut Vec<Value>, role: &str, mut blocks: Vec<Value>) -> Result<()> {
    if blocks.is_empty() {
        return Err(ModelError::ToolProtocol);
    }
    if let Some(previous) = messages.last_mut().filter(|m| m["role"] == role) {
        previous["content"]
            .as_array_mut()
            .ok_or(ModelError::ToolProtocol)?
            .append(&mut blocks);
    } else {
        messages.push(json!({"role":role,"content":blocks}));
    }
    Ok(())
}
fn text(value: &str) -> Value {
    json!({"type":"text","text":value})
}
fn encode(config: &ModelConfig, request: &ChatRequest) -> Result<Value> {
    let mut system = Vec::new();
    let mut messages = Vec::new();
    for message in &request.messages {
        match message {
            ChatMessage::Replay { state } => {
                let blocks = state.wire(config)?.as_array().ok_or(ModelError::ContextMismatch)?.clone();
                push(&mut messages, "assistant", blocks)?;
            }
            ChatMessage::System { content } | ChatMessage::Developer { content } => {
                system.push(content.as_str())
            }
            ChatMessage::User { content } => push(&mut messages, "user", vec![text(content)])?,
            ChatMessage::UserMedia { content } => {
                let mut blocks = Vec::new();
                for part in content {
                    blocks.push(match part {
                        Part::Text{text:value}=>text(value),
                        Part::ImageUrl{image_url}=>{
                            let (prefix,data)=image_url.url.split_once(";base64,").ok_or(ModelError::ToolProtocol)?;
                            let media=prefix.strip_prefix("data:").filter(|v|matches!(*v,"image/jpeg"|"image/png"|"image/webp"|"image/gif")).ok_or(ModelError::ToolProtocol)?;
                            if data.is_empty() {return Err(ModelError::ToolProtocol);}
                            json!({"type":"image","source":{"type":"base64","media_type":media,"data":data}})
                        }
                    });
                }
                push(&mut messages, "user", blocks)?;
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                let mut blocks = Vec::new();
                if let Some(value) = content.as_ref().filter(|v| !v.is_empty()) {
                    blocks.push(text(value));
                }
                for call in tool_calls {
                    let input: Value = serde_json::from_str(&call.function.arguments)
                        .map_err(|_| ModelError::ToolProtocol)?;
                    blocks.push(json!({"type":"tool_use","id":call.id,"name":call.function.name,"input":input}));
                }
                push(&mut messages, "assistant", blocks)?;
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => push(
                &mut messages,
                "user",
                vec![json!({"type":"tool_result","tool_use_id":tool_call_id,"content":content})],
            )?,
            ChatMessage::Responses { .. } => return Err(ModelError::ContextMismatch),
        }
    }
    if messages.is_empty() {
        return Err(ModelError::Invalid("Messages requires conversation input"));
    }
    let mut body = json!({"model":config.model,"max_tokens":config.output_reserve,"messages":messages,"stream":false});
    if let Some(effort) = &config.reasoning_effort {
        body["output_config"] = json!({"effort":effort});
    }
    if !system.is_empty() {
        body["system"] = system.join("\n\n").into();
    }
    if let Some(value) = config.temperature {
        body["temperature"] = json!(value);
    }
    if !request.tools.is_empty() {
        body["tools"]=request.tools.iter().map(|tool|json!({"name":tool.name,"description":tool.description,"input_schema":tool.parameters})).collect::<Vec<_>>().into();
    }
    Ok(body)
}
#[derive(Deserialize)]
struct Message {
    #[serde(rename = "type")]
    kind: String,
    role: String,
    content: Vec<ContentBlock>,
    #[serde(default)]
    usage: Option<Value>,
    #[serde(default)]
    stop_reason: Option<String>,
}
#[derive(Deserialize, Serialize)]
pub(super) struct ContentBlock {
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<Box<RawValue>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signature: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<String>,
}
fn usage(value: Option<Value>) -> Result<Option<Usage>> {
    let Some(value) = value else { return Ok(None) };
    let mut input = value["input_tokens"].as_u64().ok_or(ModelError::Response)?;
    for name in ["cache_creation_input_tokens", "cache_read_input_tokens"] {
        if let Some(count) = value.get(name) {
            input = input
                .checked_add(count.as_u64().ok_or(ModelError::Response)?)
                .ok_or(ModelError::Response)?;
        }
    }
    let output = value["output_tokens"]
        .as_u64()
        .ok_or(ModelError::Response)?;
    Ok(Some(Usage {
        prompt_tokens: input,
        completion_tokens: output,
        total_tokens: input.checked_add(output).ok_or(ModelError::Response)?,
    }))
}
pub(super) fn assemble(
    blocks: Vec<ContentBlock>,
    tokens: Option<Value>,
    reason: Option<String>,
    allowed: &HashSet<String>,
) -> Result<Completion> {
    if blocks.len() > 128 {
        return Err(ModelError::ResponseTooLarge);
    }
    let reason = reason.ok_or(ModelError::Incomplete)?;
    let finish = match reason.as_str() {
        "end_turn" | "stop_sequence" | "refusal" => "stop",
        "tool_use" => "tool_calls",
        _ => return Err(ModelError::Incomplete),
    };
    let mut content = String::new();
    let mut calls = Vec::new();
    let mut thinking = false;
    let wire = serde_json::to_value(&blocks).map_err(|_| ModelError::Response)?;
    for block in blocks {
        match block.kind.as_str() {
            "text" => {
                let text = block.text.ok_or(ModelError::Response)?;
                if content.len().saturating_add(text.len()) > 16000 {
                    return Err(ModelError::ResponseTooLarge);
                }
                content.push_str(&text);
            }
            "tool_use" => calls.push(ToolCall {
                id: block.id.ok_or(ModelError::ToolProtocol)?,
                kind: "function".into(),
                function: FunctionCall {
                    name: block.name.ok_or(ModelError::ToolProtocol)?,
                    arguments: block.input.ok_or(ModelError::ToolProtocol)?.get().into(),
                },
            }),
            "thinking" => {
                if block.thinking.is_none() || block.signature.as_deref().is_none_or(str::is_empty) { return Err(ModelError::ToolProtocol); }
                thinking = true;
            }
            "redacted_thinking" => {
                if block.data.as_deref().is_none_or(str::is_empty) { return Err(ModelError::ToolProtocol); }
                thinking = true;
            }
            _ => return Err(ModelError::Response),
        }
    }
    if content.trim().is_empty() && calls.is_empty() {
        return Err(ModelError::Incomplete);
    }
    let (text, refusal) = if reason == "refusal" {
        (None, Some(content))
    } else {
        ((!content.is_empty()).then_some(content), None)
    };
    let mut completion = super::protocol::validate_completion(
        AssistantReply {
            continuation: None,
            role: "assistant".into(),
            content: text,
            refusal,
            tool_calls: calls,
        },
        usage(tokens)?,
        finish.into(),
        allowed,
    )?;
    if thinking { completion.message.continuation = Some(Continuation::new(ProviderKind::AnthropicMessages, wire, &completion.message)?); }
    Ok(completion)
}

#[cfg(test)]
mod tests;
