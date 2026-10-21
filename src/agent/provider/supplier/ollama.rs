use super::*;

pub(super) struct Ollama;
impl SupplierPolicy for Ollama {
    fn completion_effort(&self, _: &ProviderKind) -> Option<ReasoningEffort> { Some(ReasoningEffort::None) }
    fn decode_chat(&self, bytes: &[u8], allowed: &std::collections::HashSet<String>) -> Result<Completion> {
        let mut raw: Value = serde_json::from_slice(bytes).map_err(|_| ModelError::Response)?;
        normalize_reasoning(&mut raw, "message")?;
        restore_reasoning(super::super::protocol::decode_response(&serde_json::to_vec(&raw).map_err(|_| ModelError::Response)?, allowed)?)
    }
    fn stream_chat(&self, tools: bool) -> Box<dyn super::super::adapter::ResponseStream> {
        Box::new(OllamaStream { inner: if tools { super::super::tool_stream::decoder() } else { super::super::stream::decoder() } })
    }
    fn normalize(&self, config: &mut ModelConfig) -> Result<()> {
        validate(config.supplier.as_ref(), &config.provider)?;
        config.output_token_parameter = OutputTokenParameter::MaxTokens;
        if config.reasoning_effort.as_ref().is_some_and(|effort| {
            !matches!(effort, ReasoningEffort::None | ReasoningEffort::Low | ReasoningEffort::Medium | ReasoningEffort::High)
        }) {
            return Err(ModelError::Invalid("Ollama effort must be none, low, medium or high; leave unset to use the model default"));
        }
        // Model IDs are local deployment names, including DeepSeek/Qwen aliases.
        // They must not enable the upstream vendors' thinking fields or policies.
        Ok(())
    }
}

// Ollama's OpenAI compatibility uses message/delta.reasoning, including tool replay.
// Reuse strict generic decoding internally, then retain the original wire field.
fn normalize_reasoning(raw: &mut Value, field: &str) -> Result<()> {
    if let Some(choices) = raw["choices"].as_array_mut() {
        for choice in choices {
            if let Some(message) = choice[field].as_object_mut() {
                if let Some(reasoning) = message.remove("reasoning") {
                    if !reasoning.is_null() && !reasoning.is_string() || message.get("reasoning_content").is_some_and(|other| other != &reasoning) {
                        return Err(ModelError::Response);
                    }
                    message.insert("reasoning_content".into(), reasoning);
                }
            }
        }
    }
    Ok(())
}
fn restore_reasoning(mut completion: Completion) -> Result<Completion> {
    if let Some(state) = completion.message.continuation.as_mut() { state.rename_chat_reasoning("reasoning")?; }
    Ok(completion)
}
struct OllamaStream { inner: Box<dyn super::super::adapter::ResponseStream> }
impl super::super::adapter::ResponseStream for OllamaStream {
    fn push(&mut self, frame: &str) -> Result<super::super::adapter::StreamFrame> {
        if frame == "[DONE]" { return self.inner.push(frame); }
        let mut raw: Value = serde_json::from_str(frame).map_err(|_| ModelError::Response)?;
        normalize_reasoning(&mut raw, "delta")?;
        self.inner.push(&raw.to_string())
    }
    fn finish(self: Box<Self>, allowed: &std::collections::HashSet<String>) -> Result<Completion> {
        restore_reasoning(self.inner.finish(allowed)?)
    }
}

#[cfg(test)]
mod tests;
