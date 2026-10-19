//! Stateless Responses text transport and complete provider-owned compaction windows.
use super::{stream::Sse, transport::network, *};
use reqwest::header;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    time::Duration,
};
use tokio_util::sync::CancellationToken;

pub(crate) const MAX_WINDOW_BYTES: usize = 8 * 1024 * 1024;
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Binding {
    provider: ProviderKind,
    endpoint: String,
    model: String,
    credential: String,
}
impl Binding {
    pub(crate) fn from_runtime(runtime: &RuntimeConfig) -> Result<Self> {
        let config = runtime.config.clone().normalized()?;
        Ok(Self {
            provider: config.provider.clone(),
            endpoint: config.endpoint()?.to_string(),
            model: config.model,
            credential: runtime.api_key.as_ref().map_or_else(
                || "no-key".into(),
                |key| {
                    Sha256::digest(key.0.as_bytes())
                        .iter()
                        .map(|byte| format!("{byte:02x}"))
                        .collect()
                },
            ),
        })
    }
    pub(crate) fn matches_config(&self, config: &ModelConfig) -> Result<()> {
        if self.provider != config.provider
            || self.endpoint != config.endpoint()?.as_str()
            || self.model != config.model
        {
            return Err(ModelError::ContextMismatch);
        }
        Ok(())
    }
}
/// Native-only: IPC deserialization cannot construct ChatMessage::Responses.
#[derive(Clone)]
pub struct ResponseWindow {
    pub(crate) binding: Binding,
    pub(crate) items: Vec<Value>,
}
impl std::fmt::Debug for ResponseWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ResponseWindow([NATIVE_ONLY])")
    }
}
impl ResponseWindow {
    pub(crate) fn validate_binding(&self, binding: &Binding) -> Result<()> {
        if &self.binding != binding {
            return Err(ModelError::ContextMismatch);
        }
        validate_items(&self.items, false)?;
        Ok(())
    }
}
fn validate_items(items: &[Value], compact: bool) -> Result<()> {
    if items.is_empty()
        || items.len() > 512
        || serde_json::to_vec(items)
            .map_err(|_| ModelError::Response)?
            .len()
            > MAX_WINDOW_BYTES
    {
        return Err(ModelError::ResponseTooLarge);
    }
    let mut ids = HashSet::new();
    let mut has_compaction = false;
    for item in items {
        if !item.is_object() {
            return Err(ModelError::Response);
        }
        if let Some(id) = item.get("id").filter(|id| !id.is_null()) {
            let id = id
                .as_str()
                .filter(|id| !id.is_empty() && id.len() <= 256)
                .ok_or(ModelError::Response)?;
            if !ids.insert(id) {
                return Err(ModelError::Response);
            }
        }
        match item["type"].as_str() {
            Some("compaction") => {
                if item["encrypted_content"].as_str().is_none_or(str::is_empty)
                    || item.get("role").is_some()
                {
                    return Err(ModelError::Response);
                }
                has_compaction = true;
            }
            Some("reasoning") => {
                if item["summary"].as_array().is_none()
                    || item.get("status").is_some_and(|s| s != "completed")
                    || item.get("role").is_some()
                {
                    return Err(ModelError::Response);
                }
                if item
                    .get("encrypted_content")
                    .is_some_and(|s| !s.is_null() && !s.is_string())
                {
                    return Err(ModelError::Response);
                }
            }
            Some("message") => {
                if !matches!(item["role"].as_str(), Some("user" | "assistant"))
                    || item.get("status").is_some_and(|s| s != "completed")
                {
                    return Err(ModelError::Response);
                }
                if item["content"].is_string() {
                    continue;
                }
                let content = item["content"].as_array().ok_or(ModelError::Response)?;
                if content.is_empty() {
                    return Err(ModelError::Response);
                }
                for part in content {
                    match part["type"].as_str() {
                        Some("input_text" | "output_text") if part["text"].is_string() => {}
                        Some("input_image")
                            if item["role"] == "user"
                                && part["image_url"].as_str().is_some_and(|s| {
                                    s.starts_with("data:image/jpeg;base64,")
                                        && s.len() < 1100 * 1024
                                }) => {}
                        Some("refusal") => return Err(ModelError::Incomplete),
                        _ => return Err(ModelError::ToolProtocol),
                    }
                }
            }
            Some("function_call") if !compact && item["call_id"].as_str().is_some_and(|v| !v.is_empty())
                && item["name"].is_string() && item["arguments"].is_string()
                && item.get("status").is_none_or(|v| v == "completed") => {}
            Some("function_call_output") if !compact && item["call_id"].is_string() && item["output"].is_string() => {}
            _ => return Err(ModelError::ToolProtocol),
        }
    }
    if compact && !has_compaction {
        return Err(ModelError::Response);
    }
    Ok(())
}
pub(crate) fn body(config: &ModelConfig, request: &ChatRequest) -> Result<Value> {
    if config.provider != ProviderKind::OpenAiResponses {
        return Err(ModelError::ContextMismatch);
    }
    if request.messages.is_empty() || request.messages.len() > 1024 {
        return Err(ModelError::ToolProtocol);
    }
    let mut validation = ChatRequest { messages: request.messages.iter().filter(|m| !matches!(m, ChatMessage::Responses {..})).cloned().collect(), tools: request.tools.clone() };
    if validation.messages.is_empty() { validation.messages.push(ChatMessage::System {content:String::new()}); }
    super::protocol::validate_request(config, &validation)?;
    let mut input = Vec::new();
    let mut instructions = Vec::new();
    for message in &request.messages {
        match message {
            ChatMessage::System { content } => instructions.push(content.clone()),
            ChatMessage::Developer { content } if config.capabilities.developer_role => {
                instructions.push(content.clone())
            }
            ChatMessage::User { content } => input.push(json!({"role":"user","content":content})),
            ChatMessage::UserMedia { content } => {
                let mut parts = vec![];
                for part in content {
                    match part {
                        crate::agent::attachments::Part::Text { text } => {
                            parts.push(json!({"type":"input_text","text":text}))
                        }
                        crate::agent::attachments::Part::ImageUrl { image_url } => {
                            parts.push(json!({"type":"input_image","image_url":image_url.url,"detail":"high"}));
                        }
                    }
                }
                input.push(json!({"role":"user","content":parts}));
            }
            ChatMessage::Assistant { content, tool_calls } => {
                if let Some(content) = content { input.push(json!({"role":"assistant","content":content})); }
                for call in tool_calls { input.push(json!({"type":"function_call","call_id":call.id,"name":call.function.name,"arguments":call.function.arguments})); }
            }
            ChatMessage::Replay { state } => input.extend(state.wire(config)?.as_array().ok_or(ModelError::ContextMismatch)?.clone()),
            ChatMessage::Tool { tool_call_id, content } => input.push(json!({"type":"function_call_output","call_id":tool_call_id,"output":content})),
            ChatMessage::Responses { window } => {
                window.binding.matches_config(config)?;
                validate_items(&window.items, false)?;
                input.extend(window.items.clone());
            }
            _ => return Err(ModelError::ToolProtocol),
        }
    }
    if input.len() > 512 {
        return Err(ModelError::ResponseTooLarge);
    }
    let mut result = json!({"model":config.model,"input":input,"instructions":instructions.join("\n"),
        "max_output_tokens":config.output_reserve,"stream":false,"store":false,
        "truncation":"disabled","include":["reasoning.encrypted_content"]});
    if !request.tools.is_empty() {
        result["tools"] = Value::Array(request.tools.iter().map(|t| json!({"type":"function","name":t.name,"description":t.description,"parameters":t.parameters})).collect());
    }
    if let Some(temperature) = config.temperature {
        result["temperature"] = temperature.into();
    }
    if let Some(effort) = &config.reasoning_effort {
        result["reasoning"] = json!({"effort":effort});
    }
    if serde_json::to_vec(&result)
        .map_err(|_| ModelError::Response)?
        .len()
        > 8 * 1024 * 1024
    {
        return Err(ModelError::ResponseTooLarge);
    }
    Ok(result)
}
pub(crate) fn measure(config: &ModelConfig, request: &ChatRequest) -> Result<PreparedRequest> {
    let body = serde_json::to_string(&body(config, request)?).map_err(|_| ModelError::Response)?;
    let cost = super::protocol::media_adjusted_cost(&body)?
        .saturating_add(request.messages.len() as u64 * 12);
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
        allowed_tools: request.tools.iter().map(|t| t.name.clone()).collect(),
    })
}
pub(crate) struct PreparedResponses {
    body: Value,
    pub(crate) input_tokens: u64,
    binding: Binding,
    allowed: HashSet<String>,
}
impl ModelClient {
    fn validate_windows(&self, request: &ChatRequest) -> Result<()> {
        for message in &request.messages {
            if let ChatMessage::Responses { window } = message {
                window.validate_binding(&self.binding)?;
            }
        }
        Ok(())
    }
    async fn json_call(
        &self,
        suffix: &str,
        value: &Value,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let endpoint = if suffix.is_empty() {
            self.config.endpoint()?
        } else {
            let mut config = self.config.clone();
            config.completions_path = format!("{}/{}", config.completions_path, suffix);
            config.endpoint()?
        };
        let bytes = serde_json::to_vec(value).map_err(|_| ModelError::Response)?;
        if bytes.len() > MAX_WINDOW_BYTES {
            return Err(ModelError::ResponseTooLarge);
        }
        let operation = async {
            let request = self
                .client
                .post(endpoint)
                .headers(self.headers.clone())
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .body(bytes);
            let mut response = request.send().await.map_err(network)?;
            super::diagnostics::response(&response);
            if !response.status().is_success() {
                let unsupported = !suffix.is_empty() && matches!(response.status().as_u16(),404|405|501);
                let error = super::diagnostics::http_error(response).await;
                return Err(if unsupported { ModelError::RemoteUnsupported } else { error });
            }
            if response
                .content_length()
                .is_some_and(|size| size > self.config.max_response_bytes as u64)
            {
                return Err(ModelError::ResponseTooLarge);
            }
            let mut bytes = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(network)? {
                if bytes.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                    return Err(ModelError::ResponseTooLarge);
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|_| ModelError::Response)
        };
        tokio::select! {biased;
            _=cancel.cancelled()=>Err(ModelError::Cancelled),
            result=tokio::time::timeout(Duration::from_secs(self.config.timeout_seconds),operation)=>
                result.unwrap_or(Err(ModelError::Timeout)),
        }
    }
    pub(crate) async fn count_body(
        &self,
        value: &Value,
        cancel: &CancellationToken,
    ) -> Result<u64> {
        let mut counted = json!({"model":self.config.model,"input":value["input"],"instructions":value["instructions"]});
        if let Some(tools) = value.get("tools") { counted["tools"] = tools.clone(); }
        let value = counted;
        let result = self.json_call("input_tokens", &value, cancel).await?;
        if result["object"] != "response.input_tokens" {
            return Err(ModelError::Response);
        }
        result["input_tokens"].as_u64().ok_or(ModelError::Response)
    }
    pub(crate) async fn prepare_responses(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<(PreparedResponses, u64)> {
        self.validate_windows(request)?;
        self.validate_continuations(request)?;
        let value = body(&self.config, request)?;
        if serde_json::to_vec(&value)
            .map_err(|_| ModelError::Response)?
            .len()
            > MAX_WINDOW_BYTES
        {
            return Err(ModelError::ResponseTooLarge);
        }
        let count = self.count_body(&value, cancel).await?;
        Ok((
            PreparedResponses {
                body: value,
                input_tokens: count,
                binding: self.binding.clone(),
                allowed: request.tools.iter().map(|t| t.name.clone()).collect(),
            },
            count,
        ))
    }
    pub(crate) fn require_fit(&self, tokens: u64) -> Result<()> {
        if tokens
            > self
                .config
                .budget()
                .available_input()
                .map_err(|_| ModelError::BudgetExceeded)?
        {
            return Err(ModelError::BudgetExceeded);
        }
        Ok(())
    }
    pub(crate) async fn compact_responses(
        &self,
        value: &Value,
        cancel: &CancellationToken,
    ) -> Result<ResponseWindow> {
        if !self.config.remote_compaction {
            return Err(ModelError::RemoteUnsupported);
        }
        let request = json!({"model":self.config.model,"input":value["input"],"instructions":value["instructions"]});
        if self.count_body(&request, cancel).await? > self.config.context_window {
            return Err(ModelError::BudgetExceeded);
        }
        let response = self.json_call("compact", &request, cancel).await?;
        if response["object"] != "response.compaction" {
            return Err(ModelError::Response);
        }
        let items = response["output"]
            .as_array()
            .ok_or(ModelError::Response)?
            .clone();
        validate_items(&items, true)?;
        Ok(ResponseWindow {
            items,
            binding: self.binding.clone(),
        })
    }
    pub(super) async fn complete_responses(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<Completion> {
        let (prepared, tokens) = self.prepare_responses(request, cancel).await?;
        self.require_fit(tokens)?;
        let response = self.json_call("", &prepared.body, cancel).await?;
        decode(&response, &self.binding, &prepared.allowed)
    }
    pub(super) async fn stream_responses<F, Fut>(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
        on_text: F,
    ) -> Result<Completion>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        let (prepared, tokens) = self.prepare_responses(request, cancel).await?;
        self.require_fit(tokens)?;
        self.stream_prepared_responses(prepared, cancel, on_text)
            .await
    }
    pub(crate) async fn stream_with_responses<F,Fut>(&self, request: &ChatRequest, prepared: Option<PreparedResponses>, cancel: &CancellationToken, on_text: F) -> Result<Completion>
    where F: FnMut(String)->Fut, Fut: Future<Output=Result<()>> {
        if prepared.is_none() {return self.stream(request,cancel,on_text).await;}
        super::diagnostics::observe(&self.config,request,"responses_stream",self.stream_with_responses_inner(request,prepared,cancel,on_text)).await
    }
    async fn stream_with_responses_inner<F, Fut>(
        &self,
        request: &ChatRequest,
        prepared: Option<PreparedResponses>,
        cancel: &CancellationToken,
        on_text: F,
    ) -> Result<Completion>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if let Some(prepared) = prepared {
            self.stream_prepared_responses(prepared, cancel, on_text)
                .await
        } else {
            self.stream(request, cancel, on_text).await
        }
    }
    async fn stream_prepared_responses<F, Fut>(
        &self,
        mut prepared: PreparedResponses,
        cancel: &CancellationToken,
        mut on_text: F,
    ) -> Result<Completion>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if prepared.binding != self.binding {
            return Err(ModelError::ContextMismatch);
        }
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        self.require_fit(prepared.input_tokens)?;
        prepared.body["stream"] = true.into();
        let operation = async {
            let request = self
                .client
                .post(self.config.endpoint()?)
                .headers(self.headers.clone())
                .header(header::ACCEPT, "text/event-stream")
                .json(&prepared.body);
            let mut response = request.send().await.map_err(network)?;
            super::diagnostics::response(&response);
            if !response.status().is_success() {
                return Err(super::diagnostics::http_error(response).await);
            }
            if !response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|s| {
                    s.split(';')
                        .next()
                        .is_some_and(|s| s.trim().eq_ignore_ascii_case("text/event-stream"))
                })
            {
                return Err(ModelError::Response);
            }
            let mut parser = Sse::default();
            let mut text = String::new();
            let mut size = 0usize;
            let mut response_id = None;
            let mut sequence = None;
            let mut output_items: HashMap<u64, (String, String)> = HashMap::new();
            let mut completed_items: HashMap<u64, Value> = HashMap::new();
            let mut text_parts: HashMap<(u64, u64), String> = HashMap::new();
            let mut arguments: HashMap<u64,String> = HashMap::new();
            while let Some(bytes) = response.chunk().await.map_err(network)? {
                size = size.saturating_add(bytes.len());
                if size > self.config.max_response_bytes {
                    return Err(ModelError::ResponseTooLarge);
                }
                for frame in parser.feed(&bytes)? {
                    let event: Value =
                        serde_json::from_str(&frame).map_err(|_| ModelError::Response)?;
                    let next = event["sequence_number"]
                        .as_u64()
                        .ok_or(ModelError::Response)?;
                    if sequence.is_some_and(|old| next <= old) {
                        return Err(ModelError::Response);
                    }
                    sequence = Some(next);
                    if event["type"] != "response.created"
                        && event.get("response").is_some()
                        && response_id.as_deref() != event["response"]["id"].as_str()
                    {
                        return Err(ModelError::Response);
                    }
                    match event["type"].as_str() {
                        Some("response.created") => {
                            let id = event["response"]["id"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .ok_or(ModelError::Response)?;
                            if response_id.replace(id.to_owned()).is_some() {
                                return Err(ModelError::Response);
                            }
                        }
                        Some("response.output_text.delta") => {
                            if response_id.is_none() {
                                return Err(ModelError::Response);
                            }
                            let index =
                                event["output_index"].as_u64().ok_or(ModelError::Response)?;
                            let item_id = event["item_id"].as_str().ok_or(ModelError::Response)?;
                            if event["content_index"].as_u64().is_none()
                                || output_items
                                    .get(&index)
                                    .is_none_or(|(id, kind)| id != item_id || kind != "message")
                                || completed_items.contains_key(&index)
                            {
                                return Err(ModelError::Response);
                            }
                            let delta = event["delta"].as_str().ok_or(ModelError::Response)?;
                            let part = event["content_index"]
                                .as_u64()
                                .ok_or(ModelError::Response)?;
                            if part >= 512 {
                                return Err(ModelError::Response);
                            }
                            text_parts.entry((index, part)).or_default().push_str(delta);
                            text.push_str(delta);
                            if text.len() > 16000
                                || serde_json::to_vec(&text)
                                    .map_err(|_| ModelError::Response)?
                                    .len()
                                    > 60000
                            {
                                return Err(ModelError::ResponseTooLarge);
                            }
                            if !delta.is_empty() {
                                on_text(delta.to_owned()).await?;
                            }
                        }
                        Some("response.function_call_arguments.delta" | "response.function_call_arguments.done") => {
                            let index = event["output_index"].as_u64().ok_or(ModelError::Response)?;
                            let id = event["item_id"].as_str().ok_or(ModelError::Response)?;
                            if output_items.get(&index).is_none_or(|(old,kind)| old != id || kind != "function_call") || completed_items.contains_key(&index) {
                                return Err(ModelError::ToolProtocol);
                            }
                            if event["type"] == "response.function_call_arguments.delta" {
                                let delta = event["delta"].as_str().ok_or(ModelError::ToolProtocol)?;
                                let args = arguments.entry(index).or_default();
                                if args.len().saturating_add(delta.len()) > 65536 { return Err(ModelError::ResponseTooLarge); }
                                args.push_str(delta);
                            } else if arguments.get(&index).map(String::as_str).unwrap_or("") != event["arguments"].as_str().ok_or(ModelError::ToolProtocol)? {
                                return Err(ModelError::ToolProtocol);
                            }
                        }
                        Some("response.completed") => {
                            if response_id.as_deref() != event["response"]["id"].as_str() {
                                return Err(ModelError::Response);
                            }
                            let final_items = event["response"]["output"]
                                .as_array()
                                .ok_or(ModelError::Response)?;
                            if final_items.len() != completed_items.len()
                                || final_items
                                    .iter()
                                    .enumerate()
                                    .any(|(i, item)| completed_items.get(&(i as u64)) != Some(item))
                            {
                                return Err(ModelError::Response);
                            }
                            let mut expected_parts = 0;
                            for (i, item) in final_items.iter().enumerate() {
                                if item["type"] == "function_call" && arguments.get(&(i as u64)).is_some_and(|args| Some(args.as_str()) != item["arguments"].as_str()) { return Err(ModelError::ToolProtocol); }
                                if item["type"] == "message" {
                                    for (j, part) in item["content"]
                                        .as_array()
                                        .ok_or(ModelError::Response)?
                                        .iter()
                                        .enumerate()
                                    {
                                        let final_text =
                                            part["text"].as_str().ok_or(ModelError::Response)?;
                                        if !final_text.is_empty() {
                                            expected_parts += 1;
                                            if text_parts
                                                .get(&(i as u64, j as u64))
                                                .map(String::as_str)
                                                != Some(final_text)
                                            {
                                                return Err(ModelError::Response);
                                            }
                                        }
                                    }
                                }
                            }
                            if text_parts.len() != expected_parts {
                                return Err(ModelError::Response);
                            }
                            let completion = decode(&event["response"], &self.binding, &prepared.allowed)?;
                            if completion.message.content.as_deref().unwrap_or("") != text.as_str() {
                                return Err(ModelError::Response);
                            }
                            return Ok(completion);
                        }
                        Some(
                            "response.failed"
                            | "response.incomplete"
                            | "response.refusal.delta"
                            | "response.refusal.done",
                        ) => return Err(ModelError::Incomplete),
                        Some("error") => return Err(ModelError::Response),
                        Some("response.output_item.added" | "response.output_item.done") => {
                            if response_id.is_none() {
                                return Err(ModelError::Response);
                            }
                            let index =
                                event["output_index"].as_u64().ok_or(ModelError::Response)?;
                            let id = event["item"]["id"]
                                .as_str()
                                .filter(|s| !s.is_empty())
                                .ok_or(ModelError::Response)?;
                            let kind =
                                event["item"]["type"].as_str().ok_or(ModelError::Response)?;
                            if event["type"] == "response.output_item.added" {
                                if index >= 512
                                    || output_items
                                        .insert(index, (id.into(), kind.into()))
                                        .is_some()
                                {
                                    return Err(ModelError::Response);
                                }
                            } else if output_items
                                .get(&index)
                                .is_none_or(|(old, ty)| old != id || ty != kind)
                                || completed_items
                                    .insert(index, event["item"].clone())
                                    .is_some()
                            {
                                return Err(ModelError::Response);
                            }
                            if !matches!(
                                event["item"]["type"].as_str(),
                                Some("message" | "reasoning" | "function_call")
                            ) {
                                return Err(ModelError::ToolProtocol);
                            }
                            if event["item"]["type"] == "message"
                                && event["item"]["role"] != "assistant"
                            {
                                return Err(ModelError::Response);
                            }
                        }
                        Some(
                            "response.in_progress"
                            | "response.content_part.added"
                            | "response.content_part.done"
                            | "response.output_text.done"
                            | "response.output_text.annotation.added"
                            | "response.reasoning_summary_part.added"
                            | "response.reasoning_summary_part.done"
                            | "response.reasoning_summary_text.delta"
                            | "response.reasoning_summary_text.done"
                            | "response.reasoning_text.delta"
                            | "response.reasoning_text.done",
                        ) => {}
                        _ => return Err(ModelError::Response),
                    }
                }
            }
            Err(ModelError::Incomplete)
        };
        tokio::select! {biased;
            _=cancel.cancelled()=>Err(ModelError::Cancelled),
            result=tokio::time::timeout(Duration::from_secs(self.config.timeout_seconds),operation)=>
                result.unwrap_or(Err(ModelError::Timeout)),
        }
    }
}
fn decode(response: &Value, binding: &Binding, allowed: &HashSet<String>) -> Result<Completion> {
    if response["id"]
        .as_str()
        .is_none_or(|id| id.is_empty() || id.len() > 256)
        || response["object"] != "response"
        || response["status"] != "completed"
        || response.get("error").is_some_and(|e| !e.is_null())
        || response
            .get("incomplete_details")
            .is_some_and(|e| !e.is_null())
    {
        return Err(ModelError::Incomplete);
    }
    let items = response["output"]
        .as_array()
        .ok_or(ModelError::Response)?
        .clone();
    validate_items(&items, false)?;
    let mut text = String::new();
    let mut calls = vec![];
    for item in &items {
        if item["type"] == "compaction" {
            return Err(ModelError::Response);
        }
        if item["type"] == "function_call" {
            calls.push(ToolCall { id: item["call_id"].as_str().ok_or(ModelError::ToolProtocol)?.into(), kind: "function".into(), function: FunctionCall {
                name: item["name"].as_str().ok_or(ModelError::ToolProtocol)?.into(), arguments: item["arguments"].as_str().ok_or(ModelError::ToolProtocol)?.into(),
            }});
        } else if !matches!(item["type"].as_str(), Some("message" | "reasoning")) { return Err(ModelError::Response); }
        if item["type"] == "message" {
            if item["role"] != "assistant" {
                return Err(ModelError::Response);
            }
            for part in item["content"].as_array().ok_or(ModelError::Response)? {
                if part["type"] != "output_text" {
                    return Err(ModelError::Response);
                }
                text.push_str(part["text"].as_str().ok_or(ModelError::Response)?);
            }
        }
    }
    if text.trim().is_empty() && calls.is_empty() {
        return Err(ModelError::Incomplete);
    }
    if text.len() > 16000
        || serde_json::to_vec(&text)
            .map_err(|_| ModelError::Response)?
            .len()
            > 60000
    {
        return Err(ModelError::ResponseTooLarge);
    }
    let usage = if response.get("usage").is_some_and(|v| !v.is_null()) {
        let usage = &response["usage"];
        let input = usage["input_tokens"].as_u64().ok_or(ModelError::Response)?;
        let output = usage["output_tokens"]
            .as_u64()
            .ok_or(ModelError::Response)?;
        let total = usage["total_tokens"].as_u64().ok_or(ModelError::Response)?;
        if input.checked_add(output).is_none_or(|s| s > total) {
            return Err(ModelError::Response);
        }
        Some(Usage {
            prompt_tokens: input,
            completion_tokens: output,
            total_tokens: total,
        })
    } else {
        None
    };
    let reason = if calls.is_empty() { "stop" } else { "tool_calls" };
    let mut completion = super::protocol::validate_completion(AssistantReply {
        continuation: None, role: "assistant".into(), content: (!text.is_empty()).then_some(text), tool_calls: calls, refusal: None,
    }, usage, reason.into(), allowed)?;
    let mut state = Continuation::new(ProviderKind::OpenAiResponses, Value::Array(items.clone()), &completion.message)?;
    state.seal(binding);
    completion.message.continuation = Some(state);
    completion.response_output = Some(ResponseWindow { items, binding: binding.clone() });
    Ok(completion)
}
