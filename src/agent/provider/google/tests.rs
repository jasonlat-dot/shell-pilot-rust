use super::*;
fn config() -> ModelConfig {
    serde_json::from_value(json!({"provider":"googleGemini","baseUrl":"https://proxy.invalid/gemini/v1beta/","completionsPath":"models","model":"models/arbitrary-model","outputTokenParameter":"maxCompletionTokens","capabilities":{"developerRole":true,"temperature":true,"reasoningEffort":true},"requireApiKey":false})).unwrap()
}
fn request() -> ChatRequest {
    ChatRequest {
        messages: vec![
            ChatMessage::System {
                content: "policy".into(),
            },
            ChatMessage::Developer {
                content: "instruction".into(),
            },
            ChatMessage::User {
                content: "inspect".into(),
            },
        ],
        tools: vec![FunctionDefinition {
            name: "inspect".into(),
            description: "inspect".into(),
            parameters: json!({"type":"object","properties":{},"additionalProperties":false}),
        }],
    }
}
fn response(parts: Value) -> Value {
    json!({"candidates":[{"index":0,"content":{"role":"model","parts":parts},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":3,"thoughtsTokenCount":2,"totalTokenCount":15}})
}
fn decode(value: &Value) -> Result<Completion> {
    GoogleGemini.decode(
        &serde_json::to_vec(value).unwrap(),
        &HashSet::from(["inspect".into()]),
    )
}
#[test]
fn endpoint_auth_and_generation_parameters_are_native_and_prefix_safe() {
    let mut c = config();
    c.reasoning_effort = Some(ReasoningEffort::High);
    let c = c.normalized().unwrap();
    assert_eq!(c.model, "arbitrary-model");
    assert_eq!(
        c.endpoint().unwrap().as_str(),
        "https://proxy.invalid/gemini/v1beta/models/arbitrary-model:generateContent"
    );
    let mut prepared = GoogleGemini.measure(&c, &request()).unwrap();
    let mut body: Value = serde_json::from_str(&prepared.body).unwrap();
    assert_eq!(
        body["generationConfig"]["thinkingConfig"]["thinkingLevel"],
        "HIGH"
    );
    assert_eq!(body["generationConfig"]["maxOutputTokens"], 2048);
    assert_eq!(
        body["systemInstruction"]["parts"].as_array().unwrap().len(),
        2
    );
    assert!(body["tools"][0]["functionDeclarations"][0]["parametersJsonSchema"].is_object());
    GoogleGemini
        .prepare_stream(&mut prepared.endpoint, &mut body)
        .unwrap();
    assert!(prepared
        .endpoint
        .as_str()
        .ends_with("models/arbitrary-model:streamGenerateContent?alt=sse"));
    assert!(body.get("stream").is_none());
    assert!(body.get("model").is_none());
    let headers = GoogleGemini
        .headers(Some(&SecretKey::new("fixture-key").unwrap()))
        .unwrap();
    assert_eq!(headers["x-goog-api-key"], "fixture-key");
    assert!(headers["x-goog-api-key"].is_sensitive());
    assert!(!headers.contains_key("authorization"));
    assert!(!format!("{headers:?}").contains("fixture-key"));
    for model in [
        "../escape",
        "x?key=secret",
        "x/y",
        "https://host",
        "x:generateContent",
    ] {
        let mut bad = c.clone();
        bad.model = model.into();
        assert!(bad.normalized().is_err());
    }
}
#[test]
fn levels_are_explicit_default_is_omitted_and_remote_compaction_is_rejected() {
    let c = config().normalized().unwrap();
    assert!(encode(&c, &request()).unwrap()["generationConfig"]
        .get("thinkingConfig")
        .is_none());
    for effort in [
        ReasoningEffort::Minimal,
        ReasoningEffort::Low,
        ReasoningEffort::Medium,
        ReasoningEffort::High,
    ] {
        let mut c = c.clone();
        c.reasoning_effort = Some(effort);
        assert!(c.normalized().is_ok());
    }
    for effort in [
        ReasoningEffort::None,
        ReasoningEffort::Xhigh,
        ReasoningEffort::Max,
    ] {
        let mut c = c.clone();
        c.reasoning_effort = Some(effort);
        assert!(c.normalized().is_err());
    }
    let mut c = c;
    c.remote_compaction = true;
    assert!(c.normalized().is_err());
}
#[test]
fn native_thoughts_signatures_and_optional_call_ids_replay_without_becoming_visible() {
    for id in [None, Some("provider-call-id")] {
        let c = config().normalized().unwrap();
        let mut call = json!({"name":"inspect","args":{}});
        if let Some(id) = id {
            call["id"] = id.into();
        }
        let parts = json!([{"thought":true,"text":"private thinking"},{"functionCall":call,"thoughtSignature":"opaque-signature"}]);
        let mut reply = decode(&response(parts.clone())).unwrap();
        assert!(reply.message.content.is_none());
        assert_eq!(reply.usage.as_ref().unwrap().completion_tokens, 5);
        assert!(!serde_json::to_string(&reply)
            .unwrap()
            .contains("opaque-signature"));
        reply.message.continuation.as_mut().unwrap().seal(
            &super::super::responses::Binding::from_runtime(&RuntimeConfig {
                proxy: Default::default(),
                config: c.clone(),
                api_key: None,
            })
            .unwrap(),
        );
        let call_id = reply.message.tool_calls[0].id.clone();
        let mut r = request();
        r.messages.push(reply.message.history_message());
        r.messages.push(ChatMessage::Tool {
            tool_call_id: call_id,
            content: "plain result".into(),
        });
        let body = encode(&c, &r).unwrap();
        assert_eq!(body["contents"][1]["parts"], parts);
        let result = &body["contents"][2]["parts"][0]["functionResponse"];
        assert_eq!(result["name"], "inspect");
        assert_eq!(result["response"]["result"], "plain result");
        assert_eq!(result.get("id").and_then(Value::as_str), id);
        let mut changed = c.clone();
        changed.model = "another".into();
        assert!(encode(&changed, &r).is_err());
    }
}
#[test]
fn images_use_inline_data_without_budgeting_base64_as_text() {
    let c = config().normalized().unwrap();
    let measure = |length| {
        let mut r = request();
        r.messages.push(ChatMessage::UserMedia {
            content: vec![Part::ImageUrl {
                image_url: crate::agent::attachments::ImageUrl {
                    detail: "high".into(),
                    url: format!("data:image/png;base64,{}", "A".repeat(length)),
                },
            }],
        });
        GoogleGemini.measure(&c, &r).unwrap()
    };
    let small = measure(4);
    let large = measure(40000);
    assert_eq!(small.estimated_input_tokens, large.estimated_input_tokens);
    let body: Value = serde_json::from_str(&small.body).unwrap();
    assert_eq!(
        body["contents"][0]["parts"][1]["inlineData"]["mimeType"],
        "image/png"
    );
}
#[test]
fn sse_requires_explicit_success_and_preserves_usage_after_finish() {
    let mut stream = GoogleGemini.stream(false).unwrap();
    let frame = json!({"candidates":[{"index":0,"content":{"parts":[{"thought":true,"text":"hidden"},{"text":"你好"}],"role":"model"}}]});
    assert_eq!(stream.push(&frame.to_string()).unwrap().text, "你好");
    assert!(
        !stream
            .push(&json!({"candidates":[{"finishReason":"STOP"}]}).to_string())
            .unwrap()
            .done
    );
    stream.push(&json!({"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":3,"totalTokenCount":13}}).to_string()).unwrap();
    let result = stream.finish(&HashSet::new()).unwrap();
    assert_eq!(result.message.content.as_deref(), Some("你好"));
    assert_eq!(result.usage.unwrap().total_tokens, 13);
    let mut truncated = GoogleGemini.stream(true).unwrap();
    truncated.push(&frame.to_string()).unwrap();
    assert!(matches!(
        truncated.finish(&HashSet::new()),
        Err(ModelError::Incomplete)
    ));
}
#[test]
fn malformed_blocked_oversized_and_unknown_tools_are_not_executable() {
    for reason in [
        "MAX_TOKENS",
        "SAFETY",
        "MALFORMED_FUNCTION_CALL",
        "CONTINUATION",
    ] {
        let mut value = response(json!([{"functionCall":{"name":"inspect","args":{}}}]));
        value["candidates"][0]["finishReason"] = reason.into();
        assert!(matches!(decode(&value), Err(ModelError::Incomplete)));
    }
    assert!(decode(&json!({"promptFeedback":{"blockReason":"SAFETY"}})).is_err());
    assert!(decode(&response(
        json!([{"functionCall":{"name":"unknown","args":{}}}])
    ))
    .is_err());
    assert!(decode(&response(
        json!([{"functionCall":{"name":"inspect","args":"{}"}}])
    ))
    .is_err());
    assert!(decode(&response(json!([{"text":"x".repeat(16001)}]))).is_err());
    assert!(decode(&response(json!([{"functionCall":{"id":"same","name":"inspect","args":{}}},{"functionCall":{"id":"same","name":"inspect","args":{}}}]))).is_err());
}

#[test]
fn duplicate_tool_argument_keys_are_rejected_before_json_normalization() {
    let bytes=br#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"inspect","args":{"target":"safe","target":"unsafe"}}}]},"finishReason":"STOP"}]}"#;
    assert!(matches!(
        GoogleGemini.decode(bytes, &HashSet::from(["inspect".into()])),
        Err(ModelError::ToolProtocol)
    ));
    let mut stream = GoogleGemini.stream(true).unwrap();
    assert!(matches!(
        stream.push(std::str::from_utf8(bytes).unwrap()),
        Err(ModelError::ToolProtocol)
    ));
}
