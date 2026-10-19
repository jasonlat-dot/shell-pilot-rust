use super::*;

fn config() -> ModelConfig {
    serde_json::from_value(json!({
        "provider":"anthropicMessages","baseUrl":"https://api.anthropic.com/v1/",
        "completionsPath":"messages","model":"claude-test","contextWindow":256000,
        "outputTokenParameter":"maxCompletionTokens",
        "capabilities":{"developerRole":true,"temperature":true,"reasoningEffort":false},
        "requireApiKey":false
    }))
    .unwrap()
}
fn tool() -> FunctionDefinition {
    FunctionDefinition {
        name: "read_output".into(),
        description: "Read output".into(),
        parameters: json!({"type":"object","properties":{"id":{"type":"string"}}}),
    }
}
fn response(blocks: Value, reason: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({"type":"message","role":"assistant","content":blocks,"stop_reason":reason,
        "usage":{"input_tokens":10,"cache_creation_input_tokens":4,"cache_read_input_tokens":6,"output_tokens":3}})).unwrap()
}
fn allowed() -> HashSet<String> {
    HashSet::from(["read_output".into()])
}
fn push(stream: &mut Box<dyn ResponseStream>, value: Value) -> super::super::adapter::StreamFrame {
    stream.push(&value.to_string()).unwrap()
}
fn start() -> Box<dyn ResponseStream> {
    let mut stream = AnthropicMessages.stream(true).unwrap();
    push(
        &mut stream,
        json!({"type":"message_start","message":{"type":"message","role":"assistant","content":[],"usage":{"input_tokens":10,"output_tokens":0}}}),
    );
    stream
}
fn stop(stream: &mut Box<dyn ResponseStream>, reason: &str) {
    push(
        stream,
        json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":3}}),
    );
    assert!(push(stream, json!({"type":"message_stop"})).done);
}
#[test]
fn native_request_merges_results_and_keeps_protocol_parameters_separate() {
    let request = ChatRequest {
        messages: vec![
            ChatMessage::System {
                content: "policy".into(),
            },
            ChatMessage::Developer {
                content: "developer".into(),
            },
            ChatMessage::User {
                content: "inspect".into(),
            },
            ChatMessage::Assistant {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "tool-1".into(),
                    kind: "function".into(),
                    function: FunctionCall {
                        name: "read_output".into(),
                        arguments: r#"{"id":"one"}"#.into(),
                    },
                }],
            },
            ChatMessage::Tool {
                tool_call_id: "tool-1".into(),
                content: "output".into(),
            },
            ChatMessage::User {
                content: "continue".into(),
            },
        ],
        tools: vec![tool()],
    };
    let prepared = prepare_request(&config(), &request).unwrap();
    let body: Value = serde_json::from_str(&prepared.body).unwrap();
    assert_eq!(
        prepared.endpoint.as_str(),
        "https://api.anthropic.com/v1/messages"
    );
    assert_eq!(body["system"], "policy\n\ndeveloper");
    assert_eq!(body["max_tokens"], 2048);
    assert!(body.get("max_completion_tokens").is_none());
    assert!(body.get("reasoning_effort").is_none());
    assert_eq!(body["thinking"]["type"], "disabled");
    assert_eq!(body["tools"][0]["input_schema"]["type"], "object");
    assert_eq!(body["messages"].as_array().unwrap().len(), 3);
    assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "tool-1");
    assert_eq!(body["messages"][2]["content"][1]["text"], "continue");
}
#[test]
fn native_vision_cost_does_not_count_base64_as_text() {
    use crate::agent::attachments::{ImageUrl, Part, IMAGE_TOKENS};
    let measure = |size| {
        prepare_request(
            &config(),
            &ChatRequest {
                messages: vec![ChatMessage::UserMedia {
                    content: vec![Part::ImageUrl {
                        image_url: ImageUrl {
                            url: format!("data:image/png;base64,{}", "A".repeat(size)),
                            detail: "high".into(),
                        },
                    }],
                }],
                tools: vec![],
            },
        )
        .unwrap()
    };
    let small = measure(4);
    let large = measure(40000);
    assert_eq!(small.estimated_input_tokens, large.estimated_input_tokens);
    assert!(small.estimated_input_tokens >= IMAGE_TOKENS);
    let body: Value = serde_json::from_str(&small.body).unwrap();
    assert_eq!(
        body["messages"][0]["content"][0]["source"]["media_type"],
        "image/png"
    );
}
#[test]
fn authentication_and_capabilities_are_strategy_owned() {
    let key = SecretKey::new("test-key").unwrap();
    let headers = AnthropicMessages.headers(Some(&key)).unwrap();
    assert_eq!(headers["x-api-key"], "test-key");
    assert!(headers["x-api-key"].is_sensitive());
    assert_eq!(headers["anthropic-version"], "2023-06-01");
    assert!(!headers.contains_key("authorization"));
    let normalized = config().normalized().unwrap();
    assert!(normalized.supports_tools());
    assert!(!normalized.uses_response_windows());
    assert_eq!(
        normalized.output_token_parameter,
        OutputTokenParameter::MaxTokens
    );
    let mut invalid = config();
    invalid.temperature = Some(1.1);
    assert!(invalid.normalized().is_err());
    let mut invalid = config();
    invalid.capabilities.reasoning_effort = true;
    invalid.reasoning_effort = Some(ReasoningEffort::High);
    assert!(invalid.normalized().is_err());
}
#[test]
fn replies_map_usage_refusal_and_strict_finish_reasons() {
    let blocks = json!([{"type":"text","text":"你好"}]);
    let result = AnthropicMessages
        .decode(&response(blocks.clone(), "end_turn"), &allowed())
        .unwrap();
    assert_eq!(result.message.content.as_deref(), Some("你好"));
    let usage = result.usage.unwrap();
    assert_eq!(
        (
            usage.prompt_tokens,
            usage.completion_tokens,
            usage.total_tokens
        ),
        (20, 3, 23)
    );
    let refused = AnthropicMessages
        .decode(&response(blocks.clone(), "refusal"), &allowed())
        .unwrap();
    assert_eq!(refused.message.refusal.as_deref(), Some("你好"));
    for reason in ["max_tokens", "pause_turn", "unknown"] {
        assert!(matches!(
            AnthropicMessages.decode(&response(blocks.clone(), reason), &allowed()),
            Err(ModelError::Incomplete)
        ));
    }
}
#[test]
fn tool_input_raw_json_survives_decode_and_unknown_tools_are_rejected() {
    let bytes=br#"{"type":"message","role":"assistant","content":[{"type":"tool_use","id":"one","name":"read_output","input":{"id":"a","id":"b"}}],"stop_reason":"tool_use"}"#;
    let result = AnthropicMessages.decode(bytes, &allowed()).unwrap();
    assert_eq!(
        result.message.tool_calls[0].function.arguments,
        r#"{"id":"a","id":"b"}"#
    );
    assert!(matches!(
        AnthropicMessages.decode(bytes, &HashSet::new()),
        Err(ModelError::ToolProtocol)
    ));
    let blocks = json!([{"type":"thinking","thinking":"private"},{"type":"tool_use","id":"one","name":"read_output","input":{}}]);
    assert!(matches!(
        AnthropicMessages.decode(&response(blocks, "tool_use"), &allowed()),
        Err(ModelError::ToolProtocol)
    ));
}
#[test]
fn streaming_tool_fragments_remain_private_until_complete() {
    let mut stream = start();
    assert!(push(&mut stream,json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"one","name":"read_output","input":{}}})).text.is_empty());
    for partial in [r#"{"id":""#, "输出", r#""}"#] {
        assert!(push(&mut stream,json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":partial}})).text.is_empty());
    }
    push(&mut stream, json!({"type":"content_block_stop","index":0}));
    stop(&mut stream, "tool_use");
    let result = stream.finish(&allowed()).unwrap();
    assert_eq!(
        result.message.tool_calls[0].function.arguments,
        r#"{"id":"输出"}"#
    );
    assert_eq!(result.usage.unwrap().total_tokens, 13);
}
#[test]
fn streaming_text_is_visible_but_requires_message_stop() {
    let mut stream = start();
    push(
        &mut stream,
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}),
    );
    assert_eq!(push(&mut stream,json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"你好"}})).text,"你好");
    push(&mut stream, json!({"type":"content_block_stop","index":0}));
    assert!(matches!(
        stream.finish(&allowed()),
        Err(ModelError::Incomplete)
    ));
    let mut stream = start();
    push(
        &mut stream,
        json!({"type":"content_block_start","index":0,"content_block":{"type":"text","text":"你好"}}),
    );
    push(&mut stream, json!({"type":"content_block_stop","index":0}));
    stop(&mut stream, "end_turn");
    assert_eq!(
        stream
            .finish(&allowed())
            .unwrap()
            .message
            .content
            .as_deref(),
        Some("你好")
    );
}
#[test]
fn malformed_stream_never_completes_or_dispatches() {
    let mut stream = start();
    assert!(stream
        .push(
            r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":"x"}}"#
        )
        .is_err());
    let mut stream = start();
    push(
        &mut stream,
        json!({"type":"content_block_start","index":0,"content_block":{"type":"tool_use","id":"one","name":"read_output","input":{}}}),
    );
    push(
        &mut stream,
        json!({"type":"content_block_delta","index":0,"delta":{"type":"input_json_delta","partial_json":"{"}}),
    );
    assert!(stream
        .push(r#"{"type":"content_block_stop","index":0}"#)
        .is_err());
    let mut stream = start();
    assert!(stream
        .push(r#"{"type":"error","error":{"message":"secret"}}"#)
        .is_err());
    assert!(stream.finish(&allowed()).is_err());
}
