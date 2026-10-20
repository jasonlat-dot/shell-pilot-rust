use super::*;

pub(super) struct Grok;
impl SupplierPolicy for Grok {
    fn normalize(&self, config: &mut ModelConfig) -> Result<()> {
        validate(config.supplier.as_ref(), &config.provider)?;
        // Responses uses max_output_tokens; the compatible Chat endpoint uses max_completion_tokens.
        config.output_token_parameter = OutputTokenParameter::MaxCompletionTokens;
        if config.reasoning_effort.as_ref().is_some_and(|effort| {
            !matches!(
                effort,
                ReasoningEffort::Low
                    | ReasoningEffort::Medium
                    | ReasoningEffort::High
                    | ReasoningEffort::Xhigh
            )
        }) {
            return Err(ModelError::Invalid("Grok supports low/medium/high/xhigh; leave effort unset for models without configurable reasoning"));
        }
        Ok(())
    }

    fn local_response_count(&self) -> bool {
        true
    }
}

#[cfg(test)]
mod tests;
