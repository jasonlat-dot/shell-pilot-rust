use super::{protocol::decode_response, *};
use reqwest::{header, redirect::Policy, Client};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

/// A native model-service client; it never opens a ShellPilot backend listening port.
/// Intentionally not Debug: credentials remain inside the native request headers.
pub struct ModelClient {
    pub(super) config: ModelConfig,
    pub(super) client: Client,
    pub(super) authorization: Option<header::HeaderValue>,
}
pub(super) fn network(error: reqwest::Error) -> ModelError {
    if error.is_timeout() {
        ModelError::Timeout
    } else {
        ModelError::Network
    }
}
impl ModelClient {
    pub fn new(runtime: RuntimeConfig) -> Result<Self> {
        let config = runtime.config.normalized()?;
        if config.require_api_key && runtime.api_key.is_none() {
            return Err(ModelError::KeyMissing);
        }
        let authorization = runtime
            .api_key
            .map(|key| {
                let key = SecretKey::new(key.0)?;
                let mut value = header::HeaderValue::from_str(&format!("Bearer {}", key.0))
                    .map_err(|_| ModelError::Credentials)?;
                value.set_sensitive(true);
                Ok::<_, ModelError>(value)
            })
            .transpose()?;
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
            config,
            client,
            authorization,
        })
    }

    /// B1 non-streaming adapter. Streaming and persisted Agent turns are B2/B3.
    pub async fn complete(
        &self,
        request: &ChatRequest,
        cancel: &CancellationToken,
    ) -> Result<Completion> {
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled);
        }
        let prepared = prepare_request(&self.config, request)?;
        let operation = async {
            let mut request = self
                .client
                .post(prepared.endpoint)
                .header(header::CONTENT_TYPE, "application/json")
                .header(header::ACCEPT, "application/json")
                .body(prepared.body);
            if let Some(auth) = &self.authorization {
                request = request.header(header::AUTHORIZATION, auth.clone());
            }
            let mut response = request.send().await.map_err(network)?;
            if !response.status().is_success() {
                // Do not return provider error bodies: providers may echo secrets or prompts.
                return Err(ModelError::Http(response.status().as_u16()));
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
            decode_response(&bytes, &prepared.allowed_tools)
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ModelError::Cancelled),
            result = tokio::time::timeout(Duration::from_secs(self.config.timeout_seconds), operation) =>
                result.unwrap_or(Err(ModelError::Timeout)),
        }
    }
}
