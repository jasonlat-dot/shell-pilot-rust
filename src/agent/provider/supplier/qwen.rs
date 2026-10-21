use super::*;

pub(super) struct Qwen;
impl SupplierPolicy for Qwen {
    fn completion_effort(&self, _: &ProviderKind) -> Option<ReasoningEffort> { Some(ReasoningEffort::None) }
    fn requires_thinking_signature(&self) -> bool {
        false
    }
    fn prefers_streaming(&self) -> bool {
        true
    }
    fn normalize(&self, config: &mut ModelConfig) -> Result<()> {
        validate(config.supplier.as_ref(), &config.provider)?;
        config.output_token_parameter = OutputTokenParameter::MaxTokens;
        if config.temperature.is_some_and(|value| value >= 2.0) {
            return Err(ModelError::Invalid(
                "Qwen temperature must be between 0 (inclusive) and 2 (exclusive)",
            ));
        }
        if config.reasoning_effort.as_ref().is_some_and(|effort| {
            !matches!(
                effort,
                ReasoningEffort::None
                    | ReasoningEffort::Enabled
                    | ReasoningEffort::Low
                    | ReasoningEffort::Medium
                    | ReasoningEffort::Xhigh
            )
        }) {
            return Err(ModelError::Invalid(
                "Qwen thinking must be none, enabled, low, medium or xhigh",
            ));
        }
        Ok(())
    }
    fn decorate(&self, config: &ModelConfig, body: &mut Value) {
        let Some(effort) = &config.reasoning_effort else {
            return;
        };
        let enabled = *effort != ReasoningEffort::None;
        if config.provider == ProviderKind::AnthropicMessages {
            body["thinking"] = json!({"type":if enabled {"enabled"} else {"disabled"}});
        } else {
            body["enable_thinking"] = enabled.into();
        }
        // A toggle does not invent an effort or token budget for older models.
        if matches!(effort, ReasoningEffort::None | ReasoningEffort::Enabled) {
            if let Some(object) = body.as_object_mut() {
                object.remove("reasoning_effort");
                object.remove("output_config");
            }
        }
    }
    fn catalog(&self, _: &ProviderKind) -> &'static dyn super::super::catalog::CatalogAdapter {
        &super::super::catalog::QWEN
    }
    fn catalog_headers(&self, _: &ProviderKind, key: Option<&SecretKey>) -> Result<HeaderMap> {
        super::super::adapter::credential_headers(key, header::AUTHORIZATION, true)
    }
}

#[cfg(test)]
mod tests;
