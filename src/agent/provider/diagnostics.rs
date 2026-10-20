//! Metadata-only provider diagnostics. Error bodies are classified, never persisted.
use super::*;
use crate::diagnostics::{event, Level};
use serde_json::{json, Value};
use std::{future::Future, time::Instant};

tokio::task_local! { static MODEL_CALL: String; }
fn model_call_id() -> Option<String> {
    MODEL_CALL.try_with(Clone::clone).ok()
}

pub(super) async fn observe(
    config: &ModelConfig,
    request: &ChatRequest,
    mode: &str,
    future: impl Future<Output = Result<Completion>>,
) -> Result<Completion> {
    let started = Instant::now();
    let call_id = uuid::Uuid::new_v4().to_string();
    event(
        Level::Info,
        "model.request_started",
        json!({"modelCallId":call_id,"provider":config.provider,"supplier":config.supplier,"model":config.model,"mode":mode,"endpointHost":config.endpoint().ok().and_then(|u|u.host_str().map(str::to_owned))}),
    );
    let shape: Vec<Value> = request.messages.iter().map(|message|match message {
        ChatMessage::Replay{state} => json!({"role":"assistant","bytes":state.content.as_ref().map_or(0,String::len),"toolCallCount":state.calls.len(),"hasContinuation":true}),
        ChatMessage::System{content} => json!({"role":"system","bytes":content.len()}),
        ChatMessage::Developer{content} => json!({"role":"developer","bytes":content.len()}),
        ChatMessage::User{content} => json!({"role":"user","bytes":content.len()}),
        ChatMessage::Assistant{content,tool_calls} => json!({"role":"assistant","bytes":content.as_ref().map_or(0,String::len),"toolCallCount":tool_calls.len()}),
        ChatMessage::Tool{tool_call_id,content} => json!({"role":"tool","callId":tool_call_id,"bytes":content.len()}),
        ChatMessage::UserMedia{content} => json!({"role":"user","mediaParts":content.len()}),
        ChatMessage::Responses{..} => json!({"role":"responses_window"}),
    }).collect();
    event(
        Level::Debug,
        "model.request_shape",
        json!({"modelCallId":call_id,"messageCount":request.messages.len(),"messages":shape,"tools":request.tools.iter().map(|t|t.name.as_str()).collect::<Vec<_>>(),"timeoutSeconds":config.timeout_seconds,"outputReserve":config.output_reserve,"outputTokenParameter":config.output_token_parameter,"reasoningEffort":config.reasoning_effort,"contextWindow":config.context_window}),
    );
    let result = MODEL_CALL.scope(call_id.clone(), future).await;
    match &result {
        Ok(reply) => event(
            Level::Info,
            "model.request_completed",
            json!({"modelCallId":call_id,"durationMs":started.elapsed().as_millis(),"finishReason":reply.finish_reason,"toolCallCount":reply.message.tool_calls.len(),"replyBytes":reply.message.content.as_ref().map_or(0,String::len),"usage":reply.usage}),
        ),
        Err(error) => event(
            if matches!(error, ModelError::Cancelled) {
                Level::Warn
            } else {
                Level::Error
            },
            "model.request_failed",
            json!({"modelCallId":call_id,"durationMs":started.elapsed().as_millis(),"errorCode":error_code(error),"httpStatus":if let ModelError::Http(status)=error{Some(*status)}else{None}}),
        ),
    }
    result
}
fn error_code(error: &ModelError) -> &'static str {
    match error {
        ModelError::Http(_) => "MODEL_HTTP_ERROR",
        ModelError::Timeout => "MODEL_TIMEOUT",
        ModelError::Network => "MODEL_NETWORK_ERROR",
        ModelError::Cancelled => "AGENT_CANCELLED",
        ModelError::ToolProtocol => "MODEL_TOOL_PROTOCOL_FAILED",
        ModelError::BudgetExceeded => "MODEL_CONTEXT_LIMIT",
        ModelError::ResponseTooLarge => "MODEL_RESPONSE_LIMIT",
        _ => "MODEL_REQUEST_FAILED",
    }
}
fn identifier(value: Option<&str>) -> Option<String> {
    value
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 128
                && s.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
        })
        .map(str::to_owned)
}
pub(super) fn response(response: &reqwest::Response) {
    event(
        Level::Debug,
        "model.http_response",
        json!({"modelCallId":model_call_id(),"httpStatus":response.status().as_u16(),"providerRequestId":request_id(response)}),
    );
}
fn request_id(response: &reqwest::Response) -> Option<String> {
    [
        "x-request-id",
        "request-id",
        "x-ds-request-id",
        "x-amzn-requestid",
    ]
    .iter()
    .find_map(|name| identifier(response.headers().get(*name).and_then(|v| v.to_str().ok())))
}
pub(super) async fn http_error(mut response: reqwest::Response) -> ModelError {
    let status = response.status().as_u16();
    let request_id = request_id(&response);
    // Always persist the status before reading an optional body that might stall.
    event(
        Level::Error,
        "model.http_error",
        json!({"modelCallId":model_call_id(),"httpStatus":status,"providerRequestId":request_id}),
    );
    let mut bytes = Vec::new();
    while let Ok(Some(chunk)) = response.chunk().await {
        if bytes.len() + chunk.len() > 16384 {
            break;
        }
        bytes.extend_from_slice(&chunk);
    }
    let body: Value = serde_json::from_slice(&bytes).unwrap_or_default();
    let error = body.get("error").unwrap_or(&body);
    let message = error
        .get("message")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let reason = if message.contains("reasoning_content") {
        "reasoning_content_required"
    } else if message.contains("context")
        && (message.contains("length") || message.contains("limit"))
    {
        "context_limit"
    } else if status == 429 {
        "rate_limited"
    } else if matches!(status, 401 | 403) {
        "authentication"
    } else if status >= 500 {
        "provider_unavailable"
    } else {
        "provider_rejected"
    };
    event(
        Level::Error,
        "model.http_error_detail",
        json!({"modelCallId":model_call_id(),"httpStatus":status,"providerRequestId":request_id,"errorType":identifier(error.get("type").and_then(Value::as_str)),"providerErrorCode":identifier(error.get("code").and_then(Value::as_str)),"reason":reason}),
    );
    ModelError::Http(status)
}
