use super::{protocol::decode_response, *};
use serde::Deserialize;
use serde_json::{json, Value};

/// Byte framing keeps UTF-8 correct across arbitrary HTTP chunk boundaries.
#[derive(Default)]
pub(super) struct Sse {
    line: Vec<u8>,
    data: String,
}
impl Sse {
    pub(super) fn feed(&mut self, bytes: &[u8]) -> Result<Vec<String>> {
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
    reasoning_content: Option<String>,
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

#[derive(Default)]
struct TextReply {
    text: String,
    encoded_text: usize,
    usage: Option<Usage>,
    finished: bool,
    terminal: bool,
    reasoning: Option<String>,
}
pub(super) fn decoder() -> Box<dyn super::adapter::ResponseStream> {
    Box::new(TextReply {
        encoded_text: 2,
        ..TextReply::default()
    })
}
impl super::adapter::ResponseStream for TextReply {
    fn push(&mut self, frame: &str) -> Result<super::adapter::StreamFrame> {
        if self.terminal {
            return Err(ModelError::Response);
        }
        if frame == "[DONE]" {
            if !self.finished || self.text.trim().is_empty() {
                return Err(ModelError::Incomplete);
            }
            self.terminal = true;
            return Ok(super::adapter::StreamFrame {
                text: String::new(),
                done: true,
            });
        }
        let chunk: Chunk = serde_json::from_str(frame).map_err(|_| ModelError::Response)?;
        let supplied_usage = chunk.usage.is_some();
        if let Some(value) = chunk.usage {
            self.usage = Some(value);
        }
        if chunk.choices.is_empty() {
            if !self.finished || !supplied_usage {
                return Err(ModelError::Response);
            }
            return Ok(super::adapter::StreamFrame {
                text: String::new(),
                done: false,
            });
        }
        if chunk.choices.len() != 1 {
            return Err(ModelError::Response);
        }
        let choice = &chunk.choices[0];
        let delta = &choice.delta;
        if self.finished
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
        let text = delta.content.clone().unwrap_or_default();
        if let Some(reasoning) = &delta.reasoning_content {
            let stored = self.reasoning.get_or_insert_with(String::new);
            if stored.len().saturating_add(reasoning.len()) > 1024 * 1024 { return Err(ModelError::ResponseTooLarge); }
            stored.push_str(reasoning);
        }
        self.encoded_text = self.encoded_text.saturating_add(
            serde_json::to_string(&text)
                .map_err(|_| ModelError::Response)?
                .len()
                .saturating_sub(2),
        );
        if self.text.len().saturating_add(text.len()) > 16000 || self.encoded_text > 60000 {
            return Err(ModelError::ResponseTooLarge);
        }
        self.text.push_str(&text);
        if let Some(reason) = &choice.finish_reason {
            if reason != "stop" {
                return Err(ModelError::Incomplete);
            }
            self.finished = true;
        }
        Ok(super::adapter::StreamFrame { text, done: false })
    }
    fn finish(self: Box<Self>, allowed: &std::collections::HashSet<String>) -> Result<Completion> {
        if !self.terminal {
            return Err(ModelError::Incomplete);
        }
        let mut value = json!({"choices":[{"index":0,"message":{"role":"assistant","content":self.text},"finish_reason":"stop"}],"usage":self.usage});
        if let Some(reasoning) = self.reasoning { value["choices"][0]["message"]["reasoning_content"] = reasoning.into(); }
        decode_response(&serde_json::to_vec(&value).map_err(|_|ModelError::Response)?,allowed)
    }
}
