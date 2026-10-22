use super::{ModelError, Result};
use crate::agent::budget::ModelBudget;
use serde::{Deserialize, Serialize};
use std::fmt;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProviderKind {
    OpenAiCompatible,
    OpenAiResponses,
    AnthropicMessages,
    GoogleGemini,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputTokenParameter {
    MaxCompletionTokens,
    MaxTokens,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReasoningEffort {
    None,
    /// Supplier-defined thinking switch, without imposing an effort level.
    Enabled,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

fn enabled_by_default() -> bool {
    true
}
fn default_context_window() -> u64 {
    256_000
}
fn default_output_reserve() -> u64 {
    2_048
}
fn default_safety_reserve() -> u64 {
    8_192
}
fn default_compact_percent() -> u8 {
    75
}
fn default_timeout_seconds() -> u64 {
    120
}
fn default_max_response_bytes() -> usize {
    1_048_576
}

/// Endpoint/model capabilities. Function calling and prepared image input are
/// always enabled by product policy; stored opt-out values are normalized.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelCapabilities {
    #[serde(default = "enabled_by_default")]
    pub tool_calls: bool,
    pub developer_role: bool,
    pub temperature: bool,
    pub reasoning_effort: bool,
    #[serde(default = "enabled_by_default")]
    pub vision: bool,
}
impl Default for ModelCapabilities {
    fn default() -> Self {
        Self {
            tool_calls: true,
            developer_role: false,
            temperature: true,
            reasoning_effort: false,
            vision: true,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelConfig {
    pub provider: ProviderKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supplier: Option<super::SupplierKind>,
    #[serde(default)]
    pub remote_compaction: bool,
    /// Base includes any proxy prefix, e.g. https://api.openai.com/v1/.
    pub base_url: String,
    /// Relative suffix, e.g. chat/completions. No absolute URL or parent traversal.
    pub completions_path: String,
    pub model: String,
    #[serde(default = "default_context_window")]
    pub context_window: u64,
    #[serde(default = "default_output_reserve")]
    pub output_reserve: u64,
    #[serde(default = "default_safety_reserve")]
    pub safety_reserve: u64,
    #[serde(default = "default_compact_percent")]
    pub compact_percent: u8,
    pub output_token_parameter: OutputTokenParameter,
    pub capabilities: ModelCapabilities,
    pub temperature: Option<f64>,
    pub reasoning_effort: Option<ReasoningEffort>,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
    #[serde(default = "default_max_response_bytes")]
    pub max_response_bytes: usize,
    pub require_api_key: bool,
}
impl ModelConfig {
    pub fn supports_tools(&self) -> bool {
        super::adapter::select(&self.provider).supports_tools() && self.capabilities.tool_calls
    }
    pub(crate) fn uses_response_windows(&self) -> bool {
        super::adapter::select(&self.provider).response_windows()
    }

    pub fn budget(&self) -> ModelBudget {
        ModelBudget {
            context_window: self.context_window,
            output_reserve: self.output_reserve,
            safety_reserve: self.safety_reserve,
            compact_percent: self.compact_percent,
        }
    }
    pub fn normalized(mut self) -> Result<Self> {
        // Product defaults also apply to profiles saved before the opt-in
        // settings were removed. Protocol support is checked separately.
        self.capabilities.tool_calls = true;
        self.capabilities.vision = true;
        if self.model.trim().is_empty()
            || self.model.len() > 256
            || self.model.chars().any(char::is_control)
        {
            return Err(ModelError::Invalid("model name is empty or too long"));
        }
        self.model = self.model.trim().into();
        if self.remote_compaction && !self.uses_response_windows() {
            return Err(ModelError::Invalid("remote compaction requires Responses"));
        }
        if !(256..=16_777_216).contains(&self.context_window)
            || !(1..=600).contains(&self.timeout_seconds)
            || !(1024..=8 * 1024 * 1024).contains(&self.max_response_bytes)
        {
            return Err(ModelError::Invalid(
                "window, timeout or response-size limit out of range",
            ));
        }
        self.budget()
            .available_input()
            .map_err(|_| ModelError::Invalid("invalid token reserves or compaction threshold"))?;
        if self
            .temperature
            .is_some_and(|t| !t.is_finite() || !(0.0..=2.0).contains(&t))
            || self.temperature.is_some() && !self.capabilities.temperature
            || self.reasoning_effort.is_some() && !self.capabilities.reasoning_effort
        {
            return Err(ModelError::Invalid(
                "parameter is invalid or disabled by model capabilities",
            ));
        }
        super::supplier::validate(self.supplier.as_ref(), &self.provider)?;
        self.supplier = super::supplier::canonical(self.supplier);
        super::supplier::select(self.supplier.as_ref()).normalize(&mut self)?;
        self.base_url = normalize_base_url(&self.base_url)?.to_string();
        if self.completions_path.trim().starts_with("//") {
            return Err(ModelError::Invalid(
                "completion path must not be a network URL",
            ));
        }
        let path = self.completions_path.trim().trim_start_matches('/');
        if path.is_empty()
            || path.len() > 1024
            || path.chars().any(|c| c.is_control() || c.is_whitespace())
            || path.contains([':', '?', '#', '\\', '%'])
            || path
                .split('/')
                .any(|s| s.is_empty() || matches!(s, "." | ".."))
        {
            return Err(ModelError::Invalid(
                "completion path must be a simple relative path",
            ));
        }
        self.completions_path = path.into();
        self.endpoint()?;
        Ok(self)
    }
    pub fn endpoint(&self) -> Result<Url> {
        super::adapter::select(&self.provider).endpoint(self)
    }
    pub(super) fn relative_endpoint(&self) -> Result<Url> {
        let base =
            Url::parse(&self.base_url).map_err(|_| ModelError::Invalid("invalid API base URL"))?;
        let endpoint = base
            .join(&self.completions_path)
            .map_err(|_| ModelError::Invalid("invalid completion path"))?;
        if endpoint.origin() != base.origin() || !endpoint.path().starts_with(base.path()) {
            return Err(ModelError::Invalid(
                "completion endpoint escaped the configured API prefix",
            ));
        }
        Ok(endpoint)
    }
}

/// Intentionally has no Serialize implementation and no public plaintext accessor.
#[derive(Clone, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct SecretKey(pub(crate) String);
impl SecretKey {
    pub fn new(value: impl Into<String>) -> Result<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > 8192 || !value.bytes().all(|b| (33..=126).contains(&b))
        {
            return Err(ModelError::Invalid(
                "API key is empty, too long or contains whitespace/control characters",
            ));
        }
        Ok(Self(value))
    }
}
impl fmt::Debug for SecretKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretKey([REDACTED])")
    }
}
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum KeyChange {
    #[default]
    Keep,
    Set {
        value: SecretKey,
    },
    Clear,
}
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ModelConfigUpdate {
    pub config: ModelConfig,
    #[serde(default)]
    pub api_key: KeyChange,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProfile {
    pub id: String,
    pub display_name: String,
    pub revision: u64,
    pub config: ModelConfig,
    pub api_key_configured: bool,
    pub ready: bool,
    #[serde(default = "enabled_by_default")]
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel_name: Option<String>,
}

impl ModelProfile {
    pub fn require_available(&self) -> Result<()> {
        if !self.enabled {return Err(ModelError::Disabled);}
        if !self.ready {return Err(ModelError::KeyMissing);}
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelModel {
    pub id: String,
    pub model: String,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChannel {
    #[serde(flatten)]
    pub profile: ModelProfile,
    pub models: Vec<ChannelModel>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelChannelIssue {
    pub id: String,
    pub display_name: String,
    pub message: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct ModelChannelReport {
    pub items: Vec<ModelChannel>,
    pub issues: Vec<ModelChannelIssue>,
}

/// Shared base validation for inference and model discovery; preserve proxy prefixes.
pub(super) fn normalize_base_url(value: &str) -> Result<Url> {
    if value.len() > 4096 {
        return Err(ModelError::Invalid("API URL too long"));
    }
    let mut base =
        Url::parse(value.trim()).map_err(|_| ModelError::Invalid("invalid API base URL"))?;
    if !matches!(base.scheme(), "http" | "https")
        || base.host_str().is_none()
        || !base.username().is_empty()
        || base.password().is_some()
        || base.query().is_some()
        || base.fragment().is_some()
    {
        return Err(ModelError::Invalid(
            "API URL must be HTTP(S), without login information, query or fragment",
        ));
    }
    if base.path().contains('%') || base.path().contains('\\') {
        return Err(ModelError::Invalid(
            "API base path must not contain encoded or ambiguous segments",
        ));
    }
    base.set_path(&format!("{}/", base.path().trim_end_matches('/')));
    Ok(base)
}

#[cfg(test)]
mod default_tests {
    use super::*;

    #[test]
    fn omitted_limits_use_product_defaults_and_explicit_limits_are_preserved() {
        let base = serde_json::json!({
            "provider":"openAiCompatible","baseUrl":"http://127.0.0.1/v1/",
            "completionsPath":"chat/completions","model":"test",
            "outputTokenParameter":"maxCompletionTokens",
            "capabilities":{"toolCalls":true,"developerRole":false,"temperature":false,"reasoningEffort":false,"vision":true},
            "requireApiKey":false
        });
        let defaults: ModelConfig = serde_json::from_value(base.clone()).unwrap();
        assert_eq!(
            (
                defaults.context_window,
                defaults.output_reserve,
                defaults.safety_reserve
            ),
            (256_000, 2_048, 8_192)
        );
        assert_eq!(
            (
                defaults.compact_percent,
                defaults.timeout_seconds,
                defaults.max_response_bytes
            ),
            (75, 120, 1_048_576)
        );
        assert_eq!(defaults.temperature, None);
        assert!(defaults.normalized().is_ok());
        let mut explicit = base;
        explicit["contextWindow"] = 8192.into();
        explicit["outputReserve"] = 1024.into();
        explicit["safetyReserve"] = 512.into();
        explicit["compactPercent"] = 60.into();
        let explicit: ModelConfig = serde_json::from_value(explicit).unwrap();
        assert_eq!(
            (
                explicit.context_window,
                explicit.output_reserve,
                explicit.safety_reserve,
                explicit.compact_percent
            ),
            (8192, 1024, 512, 60)
        );
    }
}
