use super::*;
use crate::agent::budget::estimate_text;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionCall,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "lowercase", deny_unknown_fields)]
pub enum ChatMessage {
    #[serde(skip)]
    Replay { state: super::Continuation },
    #[serde(skip)]
    Responses {
        window: super::responses::ResponseWindow,
    },
    System {
        content: String,
    },
    Developer {
        content: String,
    },
    User {
        content: String,
    },
    #[serde(skip)]
    UserMedia {
        content: Vec<crate::agent::attachments::Part>,
    },
    Assistant {
        content: Option<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        tool_calls: Vec<ToolCall>,
    },
    Tool {
        tool_call_id: String,
        content: String,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FunctionDefinition {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRequest {
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Vec<FunctionDefinition>,
}
pub struct PreparedRequest {
    pub endpoint: url::Url,
    pub body: String,
    pub estimated_input_tokens: u64,
    pub should_compact: bool,
    pub(crate) allowed_tools: HashSet<String>,
}
fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn validate_calls(
    calls: &[ToolCall],
    names: &HashSet<String>,
    seen: &mut HashSet<String>,
    pending: &mut HashSet<String>,
) -> Result<()> {
    if calls.len() > 64 {
        return Err(ModelError::ToolProtocol);
    }
    for call in calls {
        if call.id.is_empty()
            || call.id.len() > 128
            || call.id.chars().any(char::is_control)
            || call.kind != "function"
            || !names.contains(&call.function.name)
            || call.function.arguments.len() > 65536
            || !seen.insert(call.id.clone())
            || !pending.insert(call.id.clone())
        {
            return Err(ModelError::ToolProtocol);
        }
        let args: Value =
            serde_json::from_str(&call.function.arguments).map_err(|_| ModelError::ToolProtocol)?;
        if !args.is_object() {
            return Err(ModelError::ToolProtocol);
        }
    }
    Ok(())
}
pub fn prepare_request(config: &ModelConfig, request: &ChatRequest) -> Result<PreparedRequest> {
    if request.messages.len() > 256 {
        return Err(ModelError::Invalid("message/tool count out of range"));
    }
    let measured = measure_request(config, request)?;
    if measured.body.len() > request_byte_limit(request) {
        return Err(ModelError::Invalid("model request exceeds wire byte limit"));
    }
    if measured.estimated_input_tokens
        > config
            .budget()
            .available_input()
            .map_err(|_| ModelError::BudgetExceeded)?
    {
        return Err(ModelError::BudgetExceeded);
    }
    Ok(measured)
}
/// Planning only; transports must always call prepare_request's strict hard checks.
pub(crate) fn measure_request(
    config: &ModelConfig,
    request: &ChatRequest,
) -> Result<PreparedRequest> {
    let config = config.clone().normalized()?;
    super::adapter::select(&config.provider).measure(&config, request)
}
pub(super) fn validate_request(
    config: &ModelConfig,
    request: &ChatRequest,
) -> Result<HashSet<String>> {
    if request
        .messages
        .iter()
        .any(|m| matches!(m, ChatMessage::Responses { .. }))
    {
        return Err(ModelError::ContextMismatch);
    }
    if request.messages.is_empty() || request.messages.len() > 1024 || request.tools.len() > 64 {
        return Err(ModelError::Invalid("message/tool count out of range"));
    }
    if !request.tools.is_empty() && !config.capabilities.tool_calls {
        return Err(ModelError::Invalid("model tool calls are disabled"));
    }
    let mut allowed = HashSet::new();
    for tool in &request.tools {
        if !name(&tool.name)
            || !allowed.insert(tool.name.clone())
            || tool.description.len() > 16000
            || tool.parameters["type"] != "object"
            || serde_json::to_vec(&tool.parameters)
                .map_err(|_| ModelError::ToolProtocol)?
                .len()
                > 65536
        {
            return Err(ModelError::ToolProtocol);
        }
    }
    let mut seen = HashSet::new();
    let mut pending = HashSet::new();
    for message in &request.messages {
        match message {
            ChatMessage::Replay { state } => {
                state.wire(config)?;
                if !pending.is_empty() || state.content.is_none() && state.calls.is_empty() { return Err(ModelError::ToolProtocol); }
                validate_calls(&state.calls, &allowed, &mut seen, &mut pending)?;
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                if !pending.is_empty() || content.is_none() && tool_calls.is_empty() {
                    return Err(ModelError::ToolProtocol);
                }
                validate_calls(tool_calls, &allowed, &mut seen, &mut pending)?;
            }
            ChatMessage::Tool { tool_call_id, .. } => {
                if !pending.remove(tool_call_id) {
                    return Err(ModelError::ToolProtocol);
                }
            }
            _ if !pending.is_empty() => return Err(ModelError::ToolProtocol),
            ChatMessage::Developer { .. } if !config.capabilities.developer_role => {
                return Err(ModelError::Invalid("model developer role is disabled"))
            }
            _ => {}
        }
    }
    if !pending.is_empty() {
        return Err(ModelError::ToolProtocol);
    }
    Ok(allowed)
}
pub(super) fn measure_chat(config: &ModelConfig, request: &ChatRequest) -> Result<PreparedRequest> {
    let allowed = validate_request(config, request)?;
    let messages: Vec<Value> = request
        .messages
        .iter()
        .map(|m| match m {
            ChatMessage::Replay { state } => state.wire(config),
            ChatMessage::UserMedia { content } => Ok(json!({"role":"user","content":content})),
            _ => serde_json::to_value(m).map_err(|_| ModelError::ToolProtocol),
        })
        .collect::<Result<_>>()?;
    let mut body = json!({"model":config.model,"messages":messages,"stream":false});
    body[match config.output_token_parameter {
        OutputTokenParameter::MaxCompletionTokens => "max_completion_tokens",
        OutputTokenParameter::MaxTokens => "max_tokens",
    }] = config.output_reserve.into();
    if let Some(temperature) = config.temperature {
        body["temperature"] = temperature.into();
    }
    if let Some(effort) = &config.reasoning_effort {
        body["reasoning_effort"] =
            serde_json::to_value(effort).map_err(|_| ModelError::ToolProtocol)?;
    }
    super::supplier::select(config.supplier.as_ref()).decorate(config, &mut body);
    if !request.tools.is_empty() {
        body["tools"] = Value::Array(
            request
                .tools
                .iter()
                .map(|f| json!({"type":"function","function":f}))
                .collect(),
        );
    }
    let body = serde_json::to_string(&body).map_err(|_| ModelError::ToolProtocol)?;
    if body.len() > 32 * 1024 * 1024 {
        return Err(ModelError::Invalid("model planning request exceeds 32 MiB"));
    }
    let cost = media_adjusted_cost(&body)?
        .checked_add(request.messages.len() as u64 * 12)
        .ok_or(ModelError::BudgetExceeded)?;
    let available = config
        .budget()
        .available_input()
        .map_err(|_| ModelError::BudgetExceeded)?;
    let should_compact =
        u128::from(cost) * 100 >= u128::from(available) * u128::from(config.compact_percent);
    Ok(PreparedRequest {
        endpoint: config.endpoint()?,
        body,
        estimated_input_tokens: cost,
        should_compact,
        allowed_tools: allowed,
    })
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AssistantReply {
    #[serde(skip)]
    pub continuation: Option<super::Continuation>,
    pub role: String,
    pub content: Option<String>,
    #[serde(default)]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default)]
    pub refusal: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Completion {
    pub message: AssistantReply,
    pub usage: Option<Usage>,
    pub finish_reason: String,
    #[serde(skip)]
    pub(crate) response_output: Option<super::responses::ResponseWindow>,
}
#[derive(Deserialize)]
struct Choice {
    index: usize,
    message: AssistantReply,
    finish_reason: String,
}
#[derive(Deserialize)]
struct Response {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}
pub(crate) fn decode_response(bytes: &[u8], allowed: &HashSet<String>) -> Result<Completion> {
    let raw: Value = serde_json::from_slice(bytes).map_err(|_| ModelError::Response)?;
    // Wire-only vendor metadata never loosens the strict native ToolCall DTO.
    let mut normalized = raw.clone();
    if let Some(choices) = normalized["choices"].as_array_mut() {
        for choice in choices {
            if let Some(calls) = choice.get_mut("message").and_then(|m| m.get_mut("tool_calls")).and_then(Value::as_array_mut) {
                for call in calls {
                    if let Some(object) = call.as_object_mut() { object.remove("extra_content"); }
                }
            }
        }
    }
    let mut response: Response = serde_json::from_value(normalized).map_err(|_| ModelError::Response)?;
    if response.choices.len() != 1 {
        return Err(ModelError::Response);
    }
    let choice = response.choices.remove(0);
    if choice.index != 0 || choice.message.role != "assistant" {
        return Err(ModelError::Response);
    }
    let mut completion = validate_completion(
        choice.message,
        response.usage,
        choice.finish_reason,
        allowed,
    )?;
    completion.message.continuation = super::Continuation::chat(&raw["choices"][0]["message"], &completion.message)?;
    Ok(completion)
}
pub(super) fn validate_completion(
    message: AssistantReply,
    usage: Option<Usage>,
    finish_reason: String,
    allowed: &HashSet<String>,
) -> Result<Completion> {
    let has_calls = !message.tool_calls.is_empty();
    if !matches!(finish_reason.as_str(), "stop" | "tool_calls") {
        return Err(ModelError::Incomplete);
    }
    if has_calls != (finish_reason == "tool_calls")
        || !has_calls && message.content.is_none() && message.refusal.is_none()
    {
        return Err(ModelError::Response);
    }
    validate_calls(
        &message.tool_calls,
        allowed,
        &mut HashSet::new(),
        &mut HashSet::new(),
    )?;
    if let Some(usage) = &usage {
        if usage
            .prompt_tokens
            .checked_add(usage.completion_tokens)
            .is_none_or(|sum| sum > usage.total_tokens)
        {
            return Err(ModelError::Response);
        }
    }
    Ok(Completion {
        message,
        usage,
        finish_reason,
        response_output: None,
    })
}

pub(crate) fn request_byte_limit(request: &ChatRequest) -> usize {
    if request
        .messages
        .iter()
        .any(|m| matches!(m, ChatMessage::UserMedia { .. }))
    {
        8 * 1024 * 1024
    } else {
        2 * 1024 * 1024
    }
}
pub(crate) fn media_adjusted_cost(body: &str) -> Result<u64> {
    let mut value: Value = serde_json::from_str(body).map_err(|_| ModelError::ToolProtocol)?;
    fn replace(v: &mut Value) -> u64 {
        match v {
            Value::Object(object)
                if object
                    .get("type")
                    .is_some_and(|t| t == "image_url" || t == "input_image" || t == "image") =>
            {
                *v = json!({"type":"image","detail":"high"});
                crate::agent::attachments::IMAGE_TOKENS
            }
            Value::Object(object) if object.get("inlineData").is_some_and(|data| data["mimeType"].as_str().is_some_and(|mime| mime.starts_with("image/"))) => {
                *v = json!({"type":"image","detail":"high"});
                crate::agent::attachments::IMAGE_TOKENS
            }
            Value::Object(object) => object.values_mut().map(replace).sum(),
            Value::Array(array) => array.iter_mut().map(replace).sum(),
            _ => 0,
        }
    }
    let media = replace(&mut value);
    Ok(estimate_text(&value.to_string()).saturating_add(media))
}
