//! Messages SSE state machine. Partial JSON and thinking never reach tool dispatch or visible text.
use super::*;
use crate::agent::provider::adapter::{ResponseStream, StreamFrame};
#[derive(Default)]
pub(super) struct MessagesStream {
    started: bool,
    terminal: bool,
    delta_seen: bool,
    blocks: Vec<Block>,
    usage: Option<Value>,
    reason: Option<String>,
    text_bytes: usize,
    encoded_text: usize,
}
struct Block {
    content: ContentBlock,
    json: String,
    closed: bool,
}
#[derive(Deserialize)]
struct BlockStart {
    content_block: ContentBlock,
}
impl MessagesStream {
    fn visible(&mut self, text: String) -> Result<StreamFrame> {
        self.text_bytes = self.text_bytes.saturating_add(text.len());
        self.encoded_text = self.encoded_text.saturating_add(
            serde_json::to_string(&text)
                .map_err(|_| ModelError::Response)?
                .len()
                .saturating_sub(2),
        );
        if self.text_bytes > 16000 || self.encoded_text > 59998 {
            return Err(ModelError::ResponseTooLarge);
        }
        Ok(StreamFrame { text, done: false })
    }
    fn index(value: &Value) -> Result<usize> {
        value["index"]
            .as_u64()
            .filter(|v| *v < 128)
            .map(|v| v as usize)
            .ok_or(ModelError::Response)
    }
}
impl ResponseStream for MessagesStream {
    fn push(&mut self, frame: &str) -> Result<StreamFrame> {
        if self.terminal {
            return Err(ModelError::Response);
        }
        let value: Value = serde_json::from_str(frame).map_err(|_| ModelError::Response)?;
        let empty = || {
            Ok(StreamFrame {
                text: String::new(),
                done: false,
            })
        };
        match value["type"].as_str().ok_or(ModelError::Response)? {
            "error" => Err(ModelError::Response),
            "ping" => empty(),
            "message_start" => {
                if self.started
                    || value["message"]["type"] != "message"
                    || value["message"]["role"] != "assistant"
                    || value["message"]["content"]
                        .as_array()
                        .is_none_or(|v| !v.is_empty())
                {
                    return Err(ModelError::Response);
                }
                self.started = true;
                self.usage = value["message"].get("usage").cloned();
                empty()
            }
            "content_block_start" => {
                let index = Self::index(&value)?;
                if !self.started || self.delta_seen || index != self.blocks.len() {
                    return Err(ModelError::Response);
                }
                let content = serde_json::from_str::<BlockStart>(frame)
                    .map_err(|_| ModelError::Response)?
                    .content_block;
                if !matches!(
                    content.kind.as_str(),
                    "text" | "tool_use" | "thinking" | "redacted_thinking"
                ) {
                    return Err(ModelError::Response);
                }
                let text = if content.kind == "text" {
                    content.text.clone().ok_or(ModelError::Response)?
                } else {
                    String::new()
                };
                self.blocks.push(Block {
                    content,
                    json: String::new(),
                    closed: false,
                });
                self.visible(text)
            }
            "content_block_delta" => {
                if !self.started || self.delta_seen {
                    return Err(ModelError::Response);
                }
                let index = Self::index(&value)?;
                let block = self
                    .blocks
                    .get_mut(index)
                    .filter(|b| !b.closed)
                    .ok_or(ModelError::Response)?;
                let delta = &value["delta"];
                let text = match (block.content.kind.as_str(), delta["type"].as_str()) {
                    ("text", Some("text_delta")) => {
                        let text = delta["text"]
                            .as_str()
                            .ok_or(ModelError::Response)?
                            .to_owned();
                        let content = block.content.text.as_mut().ok_or(ModelError::Response)?;
                        if content.len().saturating_add(text.len()) > 16000 {
                            return Err(ModelError::ResponseTooLarge);
                        }
                        content.push_str(&text);
                        text
                    }
                    ("tool_use", Some("input_json_delta")) => {
                        let part = delta["partial_json"]
                            .as_str()
                            .ok_or(ModelError::ToolProtocol)?;
                        if block.json.len().saturating_add(part.len()) > 65536 {
                            return Err(ModelError::ResponseTooLarge);
                        }
                        block.json.push_str(part);
                        String::new()
                    }
                    ("thinking", Some("thinking_delta" | "signature_delta")) => {
                        let signature = delta["type"] == "signature_delta";
                        let fragment = delta[if signature {"signature"} else {"thinking"}].as_str().ok_or(ModelError::Response)?;
                        let target = if signature {&mut block.content.signature} else {&mut block.content.thinking};
                        let value = target.get_or_insert_with(String::new);
                        if value.len().saturating_add(fragment.len()) > 1024 * 1024 { return Err(ModelError::ResponseTooLarge); }
                        value.push_str(fragment);
                        String::new()
                    }
                    _ => return Err(ModelError::Response),
                };
                self.visible(text)
            }
            "content_block_stop" => {
                if !self.started || self.delta_seen {
                    return Err(ModelError::Response);
                }
                let index = Self::index(&value)?;
                let block = self
                    .blocks
                    .get_mut(index)
                    .filter(|b| !b.closed)
                    .ok_or(ModelError::Response)?;
                if block.content.kind == "tool_use" && !block.json.is_empty() {
                    block.content.input = Some(
                        serde_json::from_str::<Box<RawValue>>(&block.json)
                            .map_err(|_| ModelError::ToolProtocol)?,
                    );
                }
                block.closed = true;
                empty()
            }
            "message_delta" => {
                if !self.started || self.blocks.iter().any(|b| !b.closed) {
                    return Err(ModelError::Response);
                }
                self.delta_seen = true;
                if let Some(reason) = value["delta"]["stop_reason"].as_str() {
                    if self.reason.as_deref().is_some_and(|old| old != reason) {
                        return Err(ModelError::Response);
                    }
                    self.reason = Some(reason.into());
                }
                if let Some(tokens) = value.get("usage") {
                    let tokens = tokens.as_object().ok_or(ModelError::Response)?;
                    let usage = self
                        .usage
                        .get_or_insert_with(|| json!({}))
                        .as_object_mut()
                        .ok_or(ModelError::Response)?;
                    for (key, value) in tokens {
                        usage.insert(key.clone(), value.clone());
                    }
                }
                empty()
            }
            "message_stop" => {
                if !self.started
                    || !self.delta_seen
                    || self.reason.is_none()
                    || self.blocks.iter().any(|b| !b.closed)
                {
                    return Err(ModelError::Incomplete);
                }
                self.terminal = true;
                Ok(StreamFrame {
                    text: String::new(),
                    done: true,
                })
            }
            // New metadata events may be added by the API. They cannot manufacture a terminal marker.
            _ => empty(),
        }
    }
    fn finish(self: Box<Self>, allowed: &HashSet<String>) -> Result<Completion> {
        if !self.terminal {
            return Err(ModelError::Incomplete);
        }
        assemble(
            self.blocks.into_iter().map(|block| block.content).collect(),
            self.usage,
            self.reason,
            allowed,
        )
    }
}
