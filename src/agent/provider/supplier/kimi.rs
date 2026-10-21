use super::*;

pub(super) struct Kimi;
impl SupplierPolicy for Kimi {
    fn completion_effort(&self, provider: &ProviderKind) -> Option<ReasoningEffort> { (*provider == ProviderKind::OpenAiCompatible).then_some(ReasoningEffort::None) }
    fn requires_thinking_signature(&self) -> bool {
        false
    }

    fn normalize(&self, config: &mut ModelConfig) -> Result<()> {
        validate(config.supplier.as_ref(), &config.provider)?;
        config.output_token_parameter = OutputTokenParameter::MaxTokens;
        // Current Kimi models fix sampling internally. Never infer a value from
        // a model ID (including aliases), or silently change an explicit value.
        if config.temperature.is_some() {
            return Err(ModelError::Invalid(
                "Kimi requires supplier-default temperature; leave Temperature unset",
            ));
        }
        let valid = match &config.reasoning_effort {
            None | Some(ReasoningEffort::Low | ReasoningEffort::High | ReasoningEffort::Max) => {
                true
            }
            Some(ReasoningEffort::None | ReasoningEffort::Enabled) => {
                config.provider == ProviderKind::OpenAiCompatible
            }
            _ => false,
        };
        if !valid {
            return Err(ModelError::Invalid("Kimi supports low/high/max; thinking toggle requires Chat Completions and a compatible model"));
        }
        Ok(())
    }

    fn decorate(&self, config: &ModelConfig, body: &mut Value) {
        // K2.x exposes a switch; K3 exposes effort. Sending both would break K3.
        if let Some(effort @ (ReasoningEffort::None | ReasoningEffort::Enabled)) =
            &config.reasoning_effort
        {
            if let Some(object) = body.as_object_mut() {
                object.remove("reasoning_effort");
            }
            body["thinking"] =
                json!({"type":if *effort == ReasoningEffort::None {"disabled"} else {"enabled"}});
        }
    }

    fn headers(&self, _: &ProviderKind, key: Option<&SecretKey>) -> Result<HeaderMap> {
        super::super::adapter::credential_headers(key, header::AUTHORIZATION, true)
    }
    fn catalog(&self, _: &ProviderKind) -> &'static dyn super::super::catalog::CatalogAdapter {
        &MODELS
    }
    fn catalog_headers(
        &self,
        provider: &ProviderKind,
        key: Option<&SecretKey>,
    ) -> Result<HeaderMap> {
        self.headers(provider, key)
    }
}

struct KimiModels;
static MODELS: KimiModels = KimiModels;
impl super::super::catalog::CatalogAdapter for KimiModels {
    fn endpoint(&self, base: &Url, cursor: Option<&str>) -> Result<Url> {
        let mut base = base.clone();
        if let Some(prefix) = base
            .path()
            .strip_suffix("/anthropic/v1/")
            .or_else(|| base.path().strip_suffix("/anthropic/"))
        {
            let path = format!("{prefix}/v1/");
            base.set_path(&path);
        }
        super::super::catalog::CatalogAdapter::endpoint(
            &super::super::catalog::OPENAI,
            &base,
            cursor,
        )
    }
    fn decode(&self, value: Value) -> Result<super::super::catalog::Page> {
        super::super::catalog::CatalogAdapter::decode(&super::super::catalog::OPENAI, value)
    }
}

#[cfg(test)]
mod tests;
