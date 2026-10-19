use super::*;
use reqwest::{header, redirect::Policy, Client};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// A native model-service client; it never opens a ShellPilot backend listening port.
/// Intentionally not Debug: credentials remain inside the native request headers.
pub struct ModelClient {
    pub(super) config: ModelConfig,
    pub(super) binding: super::responses::Binding,
    pub(super) client: Client,
    pub(super) headers: header::HeaderMap,
    pub(super) adapter: &'static dyn super::adapter::ProviderAdapter,
}
pub(super) fn network(error: reqwest::Error) -> ModelError {
    crate::diagnostics::event(crate::diagnostics::Level::Error,"model.transport_error",serde_json::json!({"timeout":error.is_timeout(),"connect":error.is_connect(),"body":error.is_body(),"decode":error.is_decode()}));
    if error.is_timeout() {
        ModelError::Timeout
    } else {
        ModelError::Network
    }
}
impl ModelClient {
    /// The normalized configuration actually used by this transport.
    pub fn config(&self) -> &ModelConfig {
        &self.config
    }

    pub fn new(runtime: RuntimeConfig) -> Result<Self> {
        let binding = super::responses::Binding::from_runtime(&runtime)?;
        let config = runtime.config.normalized()?;
        if config.require_api_key && runtime.api_key.is_none() {
            return Err(ModelError::KeyMissing);
        }
        let adapter = super::adapter::select(&config.provider);
        let headers = adapter.headers(runtime.api_key.as_ref())?;
        let endpoint = config.endpoint()?;
        let mut builder = Client::builder()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(config.timeout_seconds.min(30)));
        // Local test/model services should not go through environment HTTP proxies.
        if endpoint.host().is_some_and(|host| match host {
            url::Host::Domain(host) => host == "localhost",
            url::Host::Ipv4(ip) => ip.is_loopback(),
            url::Host::Ipv6(ip) => ip.is_loopback(),
        }) {
            builder = builder.no_proxy();
        }
        let client = builder.build().map_err(network)?;
        Ok(Self {
            binding,
            config,
            client,
            headers,
            adapter,
        })
    }

    /// Non-streaming request through the selected wire strategy.
    pub async fn complete(&self, request: &ChatRequest, cancel: &CancellationToken) -> Result<Completion> {
        super::diagnostics::observe(&self.config,request,"complete",self.complete_inner(request,cancel)).await
    }
    async fn complete_inner(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<Completion> {
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        if self.adapter.response_windows() {
            return self.complete_responses(request, cancel).await;
        }
        let prepared = prepare_request(&self.config, request)?;
        let operation = async {
            let request = self
                .client
                .post(prepared.endpoint)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .headers(self.headers.clone())
                .body(prepared.body);
            let mut response = request.send().await.map_err(network)?;
            super::diagnostics::response(&response);
            if !response.status().is_success() {
                return Err(super::diagnostics::http_error(response).await);
            }
            if response
                .content_length()
                .is_some_and(|n| n > self.config.max_response_bytes as u64)
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
            self.adapter.decode(&bytes, &prepared.allowed_tools)
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ModelError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(self.config.timeout_seconds), operation) =>
                result.unwrap_or(Err(ModelError::Timeout)),
        }
    }
}
