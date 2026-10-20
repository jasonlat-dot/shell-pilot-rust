//! Shared streaming HTTP lifecycle. Codecs alone interpret provider events and tool fragments.
use super::{stream::Sse, transport::network, *};
use reqwest::header;
use serde_json::Value;
use std::{future::Future, time::Duration};
use tokio_util::sync::CancellationToken;
impl ModelClient {
    pub async fn stream<F,Fut>(&self, request: &ChatRequest, cancel: &CancellationToken, on_text: F) -> Result<Completion>
    where F: FnMut(String)->Fut, Fut: Future<Output=Result<()>> {
        self.validate_continuations(request)?;
        let completion = super::diagnostics::observe(&self.config,request,"stream",Box::pin(self.stream_inner(request,cancel,on_text))).await?;
        Ok(self.seal_completion(completion))
    }
    async fn stream_inner<F, Fut>(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
        on_text: F,
    ) -> Result<Completion>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if self.adapter.response_windows() {
            return self.stream_responses(request, cancel, on_text).await;
        }
        if !request.tools.is_empty() {
            return Err(ModelError::Invalid("text streaming does not accept tools"));
        }
        self.stream_protocol(request, cancel, on_text, false).await
    }
    pub async fn stream_tools<F,Fut>(&self, request: &ChatRequest, cancel: &CancellationToken, on_text: F) -> Result<Completion>
    where F: FnMut(String)->Fut, Fut: Future<Output=Result<()>> {
        self.validate_continuations(request)?;
        let completion = super::diagnostics::observe(&self.config,request,"stream_tools",Box::pin(self.stream_tools_inner(request,cancel,on_text))).await?;
        Ok(self.seal_completion(completion))
    }
    async fn stream_tools_inner<F, Fut>(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
        on_text: F,
    ) -> Result<Completion>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if !self.config.supports_tools() {
            return Err(ModelError::Invalid(
                "provider does not support tool streaming",
            ));
        }
        if self.adapter.response_windows() { return self.stream_responses(request, cancel, on_text).await; }
        self.stream_protocol(request, cancel, on_text, true).await
    }
    async fn stream_protocol<F, Fut>(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
        mut on_text: F,
        tools: bool,
    ) -> Result<Completion>
    where
        F: FnMut(String) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let prepared = prepare_request(&self.config, request)?;
        let mut body: Value =
            serde_json::from_str(&prepared.body).map_err(|_| ModelError::Response)?;
        let mut endpoint = prepared.endpoint.clone();
        self.adapter.prepare_stream(&mut endpoint, &mut body)?;
        let mut decoder = self.adapter.stream(tools)?;
        let operation = async {
            let mut response = self
                .client
                .post(endpoint)
                .headers(self.headers.clone())
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "text/event-stream")
                .json(&body)
                .send()
                .await
                .map_err(network)?;
            super::diagnostics::response(&response);
            if !response.status().is_success() {
                return Err(super::diagnostics::http_error(response).await);
            }
            if response
                .content_length()
                .is_some_and(|size| size > self.config.max_response_bytes as u64)
            {
                return Err(ModelError::ResponseTooLarge);
            }
            let mime = response
                .headers()
                .get(header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.split(';').next())
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            if mime == "application/json" && self.adapter.json_stream_fallback(tools) {
                let mut bytes = Vec::new();
                while let Some(chunk) = response.chunk().await.map_err(network)? {
                    if bytes.len().saturating_add(chunk.len()) > self.config.max_response_bytes {
                        return Err(ModelError::ResponseTooLarge);
                    }
                    bytes.extend_from_slice(&chunk);
                }
                let completion = self.adapter.decode(&bytes, &prepared.allowed_tools)?;
                if let Some(text) = completion
                    .message
                    .content
                    .as_ref()
                    .or(completion.message.refusal.as_ref())
                    .filter(|text| !text.is_empty())
                {
                    on_text(text.clone()).await?;
                }
                return Ok(completion);
            }
            if mime != "text/event-stream" {
                return Err(ModelError::Response);
            }
            let mut parser = Sse::default();
            let mut received = 0usize;
            while let Some(bytes) = response.chunk().await.map_err(network)? {
                received = received.saturating_add(bytes.len());
                if received > self.config.max_response_bytes {
                    return Err(ModelError::ResponseTooLarge);
                }
                for frame in parser.feed(&bytes)? {
                    let frame = decoder.push(&frame)?;
                    if !frame.text.is_empty() {
                        on_text(frame.text).await?;
                    }
                    if frame.done {
                        return decoder.finish(&prepared.allowed_tools);
                    }
                }
            }
            if self.adapter.finish_on_eof() && parser.is_complete() { decoder.finish(&prepared.allowed_tools) } else { Err(ModelError::Incomplete) }
        };
        tokio::select! {biased;_=cancel.cancelled()=>Err(ModelError::Cancelled),result=tokio::time::timeout(Duration::from_secs(self.config.timeout_seconds),operation)=>result.unwrap_or(Err(ModelError::Timeout))}
    }
}
