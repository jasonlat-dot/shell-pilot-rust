//! Supplier policy composes with wire adapters; protocol IDs stay stable for existing profiles.
use super::*;
use reqwest::{header, header::HeaderMap};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SupplierKind {
    Openai,
    Anthropic,
    Google,
    Deepseek,
    Glm,
    Qwen,
    Kimi,
    Grok,
    Ollama,
}

pub(super) trait SupplierPolicy: Sync {
    /// Request-local low-latency preset; ordinary chat options are untouched.
    fn completion_effort(&self, _provider: &ProviderKind) -> Option<ReasoningEffort> {
        None
    }
    fn normalize(&self, config: &mut ModelConfig) -> Result<()> {
        if config.reasoning_effort == Some(ReasoningEffort::Enabled) {
            return Err(ModelError::Invalid(
                "thinking toggle is not supported by this supplier",
            ));
        }
        super::adapter::select(&config.provider).normalize_config(config)
    }
    fn local_response_count(&self) -> bool { false }
    fn prefers_streaming(&self) -> bool {
        false
    }
    fn requires_thinking_signature(&self) -> bool {
        true
    }
    fn decorate(&self, _config: &ModelConfig, _body: &mut Value) {}
    fn decode_chat(&self, bytes: &[u8], allowed: &std::collections::HashSet<String>) -> Result<Completion> {
        super::protocol::decode_response(bytes, allowed)
    }
    fn stream_chat(&self, tools: bool) -> Box<dyn super::adapter::ResponseStream> {
        if tools { super::tool_stream::decoder() } else { super::stream::decoder() }
    }
    fn headers(&self, provider: &ProviderKind, key: Option<&SecretKey>) -> Result<HeaderMap> {
        super::adapter::select(provider).headers(key)
    }
    fn catalog(&self, provider: &ProviderKind) -> &'static dyn super::catalog::CatalogAdapter {
        super::adapter::select(provider).catalog()
    }
    fn catalog_headers(
        &self,
        provider: &ProviderKind,
        key: Option<&SecretKey>,
    ) -> Result<HeaderMap> {
        super::adapter::select(provider).headers(key)
    }
}
struct Compatible;
struct DeepSeek;
struct Glm;
mod qwen;
mod kimi;
mod grok;
mod ollama;
static COMPATIBLE: Compatible = Compatible;
static DEEPSEEK: DeepSeek = DeepSeek;
static GLM: Glm = Glm;
static QWEN: qwen::Qwen = qwen::Qwen;
static KIMI: kimi::Kimi = kimi::Kimi;
static GROK: grok::Grok = grok::Grok;
static OLLAMA: ollama::Ollama = ollama::Ollama;
impl SupplierPolicy for Compatible {}
pub(super) fn select(supplier: Option<&SupplierKind>) -> &'static dyn SupplierPolicy {
    match supplier {
        Some(SupplierKind::Deepseek) => &DEEPSEEK,
        Some(SupplierKind::Glm) => &GLM,
        Some(SupplierKind::Qwen) => &QWEN,
        Some(SupplierKind::Kimi) => &KIMI,
        Some(SupplierKind::Grok) => &GROK,
        Some(SupplierKind::Ollama) => &OLLAMA,
        _ => &COMPATIBLE,
    }
}
pub(super) fn validate(supplier: Option<&SupplierKind>, provider: &ProviderKind) -> Result<()> {
    let valid = match supplier {
        None => true,
        Some(SupplierKind::Ollama) => *provider == ProviderKind::OpenAiCompatible,
        Some(SupplierKind::Openai | SupplierKind::Grok) => matches!(
            provider,
            ProviderKind::OpenAiCompatible | ProviderKind::OpenAiResponses
        ),
        Some(SupplierKind::Anthropic) => *provider == ProviderKind::AnthropicMessages,
        Some(SupplierKind::Google) => *provider == ProviderKind::GoogleGemini,
        Some(SupplierKind::Deepseek | SupplierKind::Glm | SupplierKind::Qwen | SupplierKind::Kimi) => matches!(
            provider,
            ProviderKind::OpenAiCompatible | ProviderKind::AnthropicMessages
        ),
    };
    if valid {
        Ok(())
    } else {
        Err(ModelError::Invalid(
            "supplier does not support the selected protocol",
        ))
    }
}
/// Generic supplier labels are implied by their protocol; old profiles and replay bindings remain compatible.
pub(super) fn canonical(supplier: Option<SupplierKind>) -> Option<SupplierKind> {
    supplier.filter(|s| {
        matches!(
            s,
            SupplierKind::Deepseek | SupplierKind::Glm | SupplierKind::Qwen | SupplierKind::Kimi | SupplierKind::Grok | SupplierKind::Ollama
        )
    })
}
impl SupplierPolicy for Glm {
    fn completion_effort(&self, _: &ProviderKind) -> Option<ReasoningEffort> { Some(ReasoningEffort::None) }
    fn requires_thinking_signature(&self) -> bool {
        false
    }
    fn normalize(&self, config: &mut ModelConfig) -> Result<()> {
        validate(config.supplier.as_ref(), &config.provider)?;
        config.output_token_parameter = OutputTokenParameter::MaxTokens;
        if config.temperature.is_some_and(|value| value > 1.0) {
            return Err(ModelError::Invalid(
                "GLM temperature must be between 0 and 1",
            ));
        }
        if config.reasoning_effort.as_ref().is_some_and(|effort| {
            !matches!(
                effort,
                ReasoningEffort::None
                    | ReasoningEffort::Low
                    | ReasoningEffort::High
                    | ReasoningEffort::Max
            )
        }) {
            return Err(ModelError::Invalid(
                "GLM effort must be none, low, high or max",
            ));
        }
        Ok(())
    }
    fn decorate(&self, config: &ModelConfig, body: &mut Value) {
        // Defaults belong to the upstream model. Explicit settings use the selected
        // wire protocol's effort field; no model-name or endpoint guessing.
        if let Some(effort) = &config.reasoning_effort {
            let disabled = *effort == ReasoningEffort::None;
            body["thinking"] = json!({"type": if disabled { "disabled" } else { "enabled" }});
            if disabled {
                if let Some(object) = body.as_object_mut() {
                    object.remove("reasoning_effort");
                    object.remove("output_config");
                }
            }
        }
    }
    // GLM does not document a shared catalog across its API prefixes. Keep the
    // configured origin, prefix and protocol authentication for model discovery.
}
impl SupplierPolicy for DeepSeek {
    fn completion_effort(&self, _: &ProviderKind) -> Option<ReasoningEffort> { Some(ReasoningEffort::None) }
    fn requires_thinking_signature(&self) -> bool {
        false
    }
    fn normalize(&self, config: &mut ModelConfig) -> Result<()> {
        validate(config.supplier.as_ref(), &config.provider)?;
        config.output_token_parameter = OutputTokenParameter::MaxTokens;
        if config.reasoning_effort.as_ref().is_some_and(|e| {
            !matches!(
                e,
                ReasoningEffort::None
                    | ReasoningEffort::Low
                    | ReasoningEffort::High
                    | ReasoningEffort::Max
            )
        }) {
            return Err(ModelError::Invalid(
                "DeepSeek effort must be none, low, high or max",
            ));
        }
        Ok(())
    }
    fn decorate(&self, config: &ModelConfig, body: &mut Value) {
        if let Some(effort) = &config.reasoning_effort {
            let disabled = *effort == ReasoningEffort::None;
            body["thinking"] = json!({"type": if disabled { "disabled" } else { "enabled" }});
            if disabled {
                if let Some(object) = body.as_object_mut() {
                    object.remove("reasoning_effort");
                    object.remove("output_config");
                }
            }
        }
    }
    fn catalog(&self, provider: &ProviderKind) -> &'static dyn super::catalog::CatalogAdapter {
        if *provider == ProviderKind::AnthropicMessages {
            &DEEPSEEK_MODELS
        } else {
            &super::catalog::OPENAI
        }
    }
    fn catalog_headers(&self, _: &ProviderKind, key: Option<&SecretKey>) -> Result<HeaderMap> {
        super::adapter::credential_headers(key, header::AUTHORIZATION, true)
    }
}
struct DeepSeekModels;
static DEEPSEEK_MODELS: DeepSeekModels = DeepSeekModels;
impl super::catalog::CatalogAdapter for DeepSeekModels {
    fn endpoint(&self, base: &Url, cursor: Option<&str>) -> Result<Url> {
        // DeepSeek exposes one OpenAI-shaped model catalog for both chat protocols.
        // Strip only its known Messages suffix and retain any configured proxy prefix and origin.
        let mut base = base.clone();
        if let Some(prefix) = base
            .path()
            .strip_suffix("/anthropic/v1/")
            .or_else(|| base.path().strip_suffix("/anthropic/"))
        {
            let prefix = format!("{prefix}/");
            base.set_path(&prefix);
        }
        super::catalog::CatalogAdapter::endpoint(&super::catalog::OPENAI, &base, cursor)
    }
    fn decode(&self, value: Value) -> Result<super::catalog::Page> {
        super::catalog::CatalogAdapter::decode(&super::catalog::OPENAI, value)
    }
}

#[cfg(test)]
mod glm_tests;
#[cfg(test)]
mod tests;
