//! Streaming Chat Completions with tool fragments kept private until a complete reply.
use super::{protocol::decode_response, *};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Deserialize)]
struct Fragment {
    index: usize,
    id: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    function: Option<FunctionFragment>,
}
#[derive(Deserialize)]
struct FunctionFragment {
    name: Option<String>,
    arguments: Option<String>,
}
#[derive(Deserialize)]
struct Delta {
    role: Option<String>,
    content: Option<String>,
    refusal: Option<String>,
    tool_calls: Option<Vec<Fragment>>,
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
struct Reply {
    text: String,
    refusal: String,
    calls: BTreeMap<usize, ToolCall>,
    finish: Option<String>,
    usage: Option<Usage>,
}

fn append(target: &mut String, fragment: &str, limit: usize) -> Result<()> {
    if target.len().saturating_add(fragment.len()) > limit {
        return Err(ModelError::ResponseTooLarge);
    }
    target.push_str(fragment);
    Ok(())
}
impl Reply {
    fn push(&mut self, frame: &str) -> Result<String> {
        let chunk: Chunk = serde_json::from_str(frame).map_err(|_| ModelError::Response)?;
        let usage = chunk.usage.is_some();
        if let Some(usage) = chunk.usage {
            self.usage = Some(usage);
        }
        if chunk.choices.is_empty() {
            return if self.finish.is_some() && usage {
                Ok(String::new())
            } else {
                Err(ModelError::Response)
            };
        }
        if chunk.choices.len() != 1 || self.finish.is_some() {
            return Err(ModelError::Response);
        }
        let choice = chunk.choices.into_iter().next().unwrap();
        let delta = choice.delta;
        if choice.index != 0
            || delta
                .role
                .as_deref()
                .is_some_and(|role| role != "assistant")
        {
            return Err(ModelError::Response);
        }
        if delta.function_call.is_some() {
            return Err(ModelError::ToolProtocol);
        }
        let mut visible = String::new();
        if let Some(text) = delta.content {
            append(&mut self.text, &text, 16000)?;
            visible.push_str(&text);
        }
        if let Some(text) = delta.refusal {
            append(&mut self.refusal, &text, 16000)?;
            visible.push_str(&text);
        }
        for fragment in delta.tool_calls.unwrap_or_default() {
            if fragment.index >= 64 {
                return Err(ModelError::ToolProtocol);
            }
            let call = self
                .calls
                .entry(fragment.index)
                .or_insert_with(|| ToolCall {
                    id: String::new(),
                    kind: String::new(),
                    function: FunctionCall {
                        name: String::new(),
                        arguments: String::new(),
                    },
                });
            if let Some(id) = fragment.id {
                append(&mut call.id, &id, 128)?;
            }
            if let Some(kind) = fragment.kind {
                if kind != "function" || (!call.kind.is_empty() && call.kind != kind) {
                    return Err(ModelError::ToolProtocol);
                }
                call.kind = kind;
            }
            if let Some(function) = fragment.function {
                if let Some(name) = function.name {
                    append(&mut call.function.name, &name, 64)?;
                }
                if let Some(arguments) = function.arguments {
                    append(&mut call.function.arguments, &arguments, 65536)?;
                }
            }
        }
        if let Some(reason) = choice.finish_reason {
            if !matches!(reason.as_str(), "stop" | "tool_calls") {
                return Err(ModelError::Incomplete);
            }
            self.finish = Some(reason);
        }
        Ok(visible)
    }
    fn complete(self, allowed: &std::collections::HashSet<String>) -> Result<Completion> {
        let reason = self.finish.ok_or(ModelError::Incomplete)?;
        if self.calls.keys().copied().ne(0..self.calls.len()) {
            return Err(ModelError::ToolProtocol);
        }
        if self.calls.is_empty() && self.text.trim().is_empty() && self.refusal.trim().is_empty() {
            return Err(ModelError::Incomplete);
        }
        decode_response(&serde_json::to_vec(&json!({"choices":[{"index":0,"message":{"role":"assistant",
            "content":(!self.text.is_empty()).then_some(self.text),"refusal":(!self.refusal.is_empty()).then_some(self.refusal),
            "tool_calls":self.calls.into_values().collect::<Vec<_>>()},"finish_reason":reason}],"usage":self.usage}))
            .map_err(|_|ModelError::Response)?,allowed)
    }
}

pub(super) fn decoder() -> Box<dyn super::adapter::ResponseStream> {
    Box::<Reply>::default()
}
impl super::adapter::ResponseStream for Reply {
    fn push(&mut self, frame: &str) -> Result<super::adapter::StreamFrame> {
        if frame == "[DONE]" {
            return Ok(super::adapter::StreamFrame {
                text: String::new(),
                done: true,
            });
        }
        Ok(super::adapter::StreamFrame {
            text: self.push(frame)?,
            done: false,
        })
    }
    fn finish(self: Box<Self>, allowed: &std::collections::HashSet<String>) -> Result<Completion> {
        (*self).complete(allowed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn delta(value: Value, finish: Option<&str>) -> String {
        json!({"choices":[{"index":0,"delta":value,"finish_reason":finish}]}).to_string()
    }
    fn allowed() -> std::collections::HashSet<String> {
        std::collections::HashSet::from(["inspect".into()])
    }
    #[test]
    fn fragmented_function_and_unicode_text_are_assembled_without_exposing_arguments() {
        let mut reply = Reply::default();
        assert_eq!(
            reply
                .push(&delta(json!({"content":"先检查汉🙂"}), None))
                .unwrap(),
            "先检查汉🙂"
        );
        assert_eq!(reply.push(&delta(json!({"tool_calls":[{"index":0,"id":"call","type":"function","function":{"name":"ins","arguments":"{\"val"}}]}),None)).unwrap(),"");
        reply.push(&delta(json!({"tool_calls":[{"index":0,"function":{"name":"pect","arguments":"ue\":\"汉🙂\"}"}}]}),Some("tool_calls"))).unwrap();
        let complete = reply.complete(&allowed()).unwrap();
        assert_eq!(
            complete.message.tool_calls[0].function.arguments,
            "{\"value\":\"汉🙂\"}"
        );
        assert_eq!(complete.message.content.as_deref(), Some("先检查汉🙂"));
    }
    #[test]
    fn missing_finish_unknown_tools_sparse_indices_and_incomplete_arguments_are_rejected() {
        for (index, name, args, finish) in [
            (0, "inspect", "{}", None),
            (0, "other", "{}", Some("tool_calls")),
            (1, "inspect", "{}", Some("tool_calls")),
            (0, "inspect", "{", Some("tool_calls")),
            (0, "inspect", "{}", Some("stop")),
        ] {
            let mut reply = Reply::default();
            reply.push(&delta(json!({"tool_calls":[{"index":index,"id":"call","type":"function","function":{"name":name,"arguments":args}}]}),finish)).unwrap();
            assert!(reply.complete(&allowed()).is_err());
        }
    }
    #[test]
    fn multiple_calls_can_be_interleaved_and_duplicate_ids_fail_completion() {
        for second_id in ["b", "a"] {
            let mut reply = Reply::default();
            reply.push(&delta(json!({"tool_calls":[{"index":1,"id":second_id,"type":"function","function":{"name":"inspect","arguments":"{"}},{"index":0,"id":"a","type":"function","function":{"name":"inspect","arguments":"{}"}}]}),None)).unwrap();
            reply
                .push(&delta(
                    json!({"tool_calls":[{"index":1,"function":{"arguments":"}"}}]}),
                    Some("tool_calls"),
                ))
                .unwrap();
            assert_eq!(reply.complete(&allowed()).is_ok(), second_id != "a");
        }
    }
    #[test]
    fn usage_after_finish_is_valid_but_late_content_and_filtered_finishes_are_not() {
        let mut reply = Reply::default();
        reply
            .push(&delta(json!({"content":"完成"}), Some("stop")))
            .unwrap();
        reply.push(&json!({"choices":[],"usage":{"prompt_tokens":2,"completion_tokens":2,"total_tokens":4}}).to_string()).unwrap();
        assert!(reply.push(&delta(json!({"content":"late"}), None)).is_err());
        assert_eq!(
            reply
                .complete(&allowed())
                .unwrap()
                .message
                .content
                .as_deref(),
            Some("完成")
        );
        assert!(Reply::default()
            .push(&delta(json!({"content":"partial"}), Some("length")))
            .is_err());
    }
}
