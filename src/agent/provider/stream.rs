use super::{protocol::decode_response, transport::network, *};
use reqwest::header;
use serde::Deserialize;
use serde_json::{json, Value};
use std::{future::Future, time::Duration};
use tokio_util::sync::CancellationToken;

/// Byte framing keeps UTF-8 correct across arbitrary HTTP chunk boundaries.
#[derive(Default)]
struct Sse {
    line: Vec<u8>,
    data: String,
}
impl Sse {
    fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
        let mut frames = Vec::new();
        for &byte in bytes {
            if byte != b'\n' {
                self.line.push(byte);
                if self.line.len() > 65536 {
                    return Err(ModelError::ResponseTooLarge);
                }
                continue;
            }
            let line = std::mem::take(&mut self.line);
            let line = line.strip_suffix(b"\r").unwrap_or(&line);
            let line = std::str::from_utf8(line).map_err(|_| ModelError::Response)?;
            if line.is_empty() {
                if !self.data.is_empty() {
                    self.data.pop(); // Remove the final SSE data newline only.
                    frames.push(std::mem::take(&mut self.data));
                }
            } else if let Some(data) = line.strip_prefix("data:") {
                self.data.push_str(data.strip_prefix(' ').unwrap_or(data));
                self.data.push('\n');
                if self.data.len() > 65536 {
                    return Err(ModelError::ResponseTooLarge);
                }
            }
        }
        Ok(frames)
    }
}
#[derive(Deserialize)]
struct Delta {
    role: Option<String>,
    content: Option<String>,
    refusal: Option<String>,
    tool_calls: Option<Value>,
    function_call: Option<Value>,
}
#[derive(Deserialize)]
struct Choice {
    index: usize,
    delta: Delta,
    finish_reason: Option<String>,
}
#[derive(Deserialize)]
struct Chunk {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

impl ModelClient {
    /// Text-only B2 stream. Tool streaming/execution is deliberately left to D1.
    pub async fn stream<F, Fut>(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
        mut on_text: F,
    ) -> Result<Completion>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        if !request.tools.is_empty() {
            return Err(ModelError::Invalid("streaming tools are not enabled"));
        }
        let prepared = prepare_request(&self.config, request)?;
        let mut body: Value =
            serde_json::from_str(&prepared.body).map_err(|_| ModelError::Response)?;
        body["stream"] = true.into();
        let operation = async {
            let mut request = self
                .client
                .post(prepared.endpoint)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "text/event-stream")
                .json(&body);
            if let Some(auth) = &self.authorization {
                request = request.header(header::AUTHORIZATION, auth.clone());
            }
            let mut response = request.send().await.map_err(network)?;
            if !response.status().is_success() {
                return Err(ModelError::Http(response.status().as_u16()));
            }
            if !response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .is_some_and(|h| {
                    h.split(';')
                        .next()
                        .is_some_and(|m| m.trim().eq_ignore_ascii_case("text/event-stream"))
                })
            {
                return Err(ModelError::Response);
            }
            let mut parser = Sse::default();
            let mut text = String::new();
            let mut encoded_text = 2usize;
            let mut usage = None;
            let mut finished = false;
            let mut received = 0usize;
            while let Some(bytes) = response.chunk().await.map_err(network)? {
                received = received.saturating_add(bytes.len());
                if received > self.config.max_response_bytes {
                    return Err(ModelError::ResponseTooLarge);
                }
                for frame in parser.feed(&bytes)? {
                    if frame == "[DONE]" {
                        if !finished || text.trim().is_empty() {
                            return Err(ModelError::Incomplete);
                        }
                        return decode_response(&serde_json::to_vec(&json!({"choices":[{"index":0,
                            "message":{"role":"assistant","content":text},"finish_reason":"stop"}],"usage":usage}))
                            .map_err(|_|ModelError::Response)?, &prepared.allowed_tools);
                    }
                    let chunk: Chunk =
                        serde_json::from_str(&frame).map_err(|_| ModelError::Response)?;
                    if let Some(value) = chunk.usage {
                        usage = Some(value);
                    }
                    if chunk.choices.is_empty() {
                        if !finished || usage.is_none() {
                            return Err(ModelError::Response);
                        }
                        continue;
                    }
                    if chunk.choices.len() != 1 {
                        return Err(ModelError::Response);
                    }
                    let choice = &chunk.choices[0];
                    let delta = &choice.delta;
                    if finished
                        || choice.index != 0
                        || delta.role.as_deref().is_some_and(|r| r != "assistant")
                    {
                        return Err(ModelError::Response);
                    }
                    if delta.refusal.as_ref().is_some_and(|s| !s.is_empty()) {
                        return Err(ModelError::Incomplete);
                    }
                    if delta.function_call.is_some()
                        || delta
                            .tool_calls
                            .as_ref()
                            .is_some_and(|v| v.as_array().is_none_or(|a| !a.is_empty()))
                    {
                        return Err(ModelError::ToolProtocol);
                    }
                    if let Some(content) = &delta.content {
                        encoded_text = encoded_text.saturating_add(
                            serde_json::to_string(content)
                                .map_err(|_| ModelError::Response)?
                                .len()
                                .saturating_sub(2),
                        );
                        if text.len().saturating_add(content.len()) > 16000 || encoded_text > 60000
                        {
                            return Err(ModelError::ResponseTooLarge);
                        }
                        text.push_str(content);
                        if !content.is_empty() {
                            on_text(content.clone()).await?;
                        }
                    }
                    if let Some(reason) = &choice.finish_reason {
                        if reason != "stop" {
                            return Err(ModelError::Incomplete);
                        }
                        finished = true;
                    }
                }
            }
            // Neither HTTP EOF nor a finish_reason without the terminal marker is success.
            Err(ModelError::Incomplete)
        };
        tokio::select! {
            biased;
            _=cancel.cancelled()=>Err(ModelError::Cancelled),
            result=tokio::time::timeout(Duration::from_secs(self.config.timeout_seconds),operation)=>
                result.unwrap_or(Err(ModelError::Timeout)),
        }
    }
}
