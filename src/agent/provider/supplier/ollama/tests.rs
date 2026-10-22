use super::*;

fn config(model: &str) -> ModelConfig {
    serde_json::from_value(json!({"supplier":"ollama","provider":"openAiCompatible","baseUrl":"http://127.0.0.1:11434/proxy/v1/","completionsPath":"chat/completions","model":model,"outputTokenParameter":"maxCompletionTokens","capabilities":{"developerRole":false,"temperature":true,"reasoningEffort":true},"requireApiKey":false})).unwrap()
}

#[test]
fn ollama_uses_its_own_chat_controls_for_every_local_model_alias() {
    let request = ChatRequest { messages: vec![ChatMessage::User { content: "hello".into() }], tools: vec![] };
    for model in ["deepseek-r1:14b", "qwen3:8b", "library/custom:latest"] {
        for effort in [None, Some(ReasoningEffort::None), Some(ReasoningEffort::Low), Some(ReasoningEffort::Medium), Some(ReasoningEffort::High)] {
            let mut c = config(model); c.reasoning_effort = effort.clone(); c.temperature = Some(0.7);
            let c = c.normalized().unwrap();
            assert_eq!(c.supplier, Some(SupplierKind::Ollama));
            let prepared = prepare_request(&c, &request).unwrap();
            let body: Value = serde_json::from_str(&prepared.body).unwrap();
            assert_eq!(body["model"], model); assert_eq!(body["max_tokens"], 2048); assert_eq!(body["temperature"], 0.7);
            assert_eq!(prepared.endpoint.path(), "/proxy/v1/chat/completions");
            for field in ["thinking", "enable_thinking", "think", "options", "max_completion_tokens", "output_config"] { assert!(body.get(field).is_none(), "unexpected field: {field}"); }
            if let Some(effort) = effort { assert_eq!(body["reasoning_effort"], json!(effort)); }
            else { assert!(body.get("reasoning_effort").is_none()); }
        }
    }
    for effort in [ReasoningEffort::Enabled, ReasoningEffort::Minimal, ReasoningEffort::Xhigh, ReasoningEffort::Max] {
        let mut c = config("alias"); c.reasoning_effort = Some(effort); assert!(c.normalized().is_err());
    }
    for provider in [ProviderKind::AnthropicMessages, ProviderKind::OpenAiResponses, ProviderKind::GoogleGemini] {
        let mut c = config("alias"); c.provider = provider; assert!(c.normalized().is_err());
    }
    let headers = Ollama.headers(&ProviderKind::OpenAiCompatible, None).unwrap(); assert!(!headers.contains_key("authorization"));
    let key = SecretKey::new("ollama-proxy-fixture").unwrap();
    let headers = Ollama.headers(&ProviderKind::OpenAiCompatible, Some(&key)).unwrap();
    assert_eq!(headers[header::AUTHORIZATION], "Bearer ollama-proxy-fixture"); assert!(headers[header::AUTHORIZATION].is_sensitive());
}

#[test]
fn ollama_identity_persists_and_continuation_bindings_are_specific_to_the_deployed_model() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap(); let path = dir.path().join("ollama.sqlite");
    let store = Arc::new(crate::store::Store::open(&path).unwrap()); let repo = ModelRepository::new(store.clone());
    repo.save("local", "u", 0, ModelConfigUpdate { config: config("deepseek-r1:14b"), api_key: KeyChange::Keep }).unwrap();
    let runtime = repo.runtime("local", "u").unwrap(); assert!(runtime.api_key.is_none());
    let binding = super::super::super::responses::Binding::from_runtime(&runtime).unwrap();
    let mut next = runtime.config.clone(); next.model = "qwen3:8b".into(); assert!(binding.matches_config(&next).is_err());
    for supplier in [None, Some(SupplierKind::Deepseek), Some(SupplierKind::Qwen)] { next = runtime.config.clone(); next.supplier = supplier; assert!(binding.matches_config(&next).is_err()); }
    drop(repo); drop(store);
    let repo = ModelRepository::new(Arc::new(crate::store::Store::open(&path).unwrap())); assert_eq!(repo.get("local", "u").unwrap().config.supplier, Some(SupplierKind::Ollama));
}

#[test]
fn ollama_reasoning_field_is_private_and_replayed_verbatim_for_json_and_text_streams() {
    use std::collections::HashSet;
    let c=config("qwen3:8b").normalized().unwrap();
    let runtime=RuntimeConfig{config:c.clone(),api_key:None,proxy:Default::default()};
    let binding=super::super::super::responses::Binding::from_runtime(&runtime).unwrap();
    let raw=json!({"choices":[{"index":0,"message":{"role":"assistant","content":"answer","reasoning":"private-思考"},"finish_reason":"stop"}]});
    let mut completions=vec![Ollama.decode_chat(&serde_json::to_vec(&raw).unwrap(),&HashSet::new()).unwrap()];
    assert!(COMPATIBLE.decode_chat(&serde_json::to_vec(&raw).unwrap(),&HashSet::new()).unwrap().message.continuation.is_none());
    for tools in [false,true] {
        let mut stream=Ollama.stream_chat(tools);
        for reasoning in ["private-","思考"] {let frame=json!({"choices":[{"index":0,"delta":{"role":"assistant","reasoning":reasoning},"finish_reason":null}]}).to_string();assert!(stream.push(&frame).unwrap().text.is_empty());}
        assert_eq!(stream.push(&json!({"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":"stop"}]}).to_string()).unwrap().text,"answer");
        assert!(stream.push("[DONE]").unwrap().done);completions.push(stream.finish(&HashSet::new()).unwrap());
    }
    for mut completion in completions {
        assert!(!serde_json::to_string(&completion).unwrap().contains("private-"));
        let state=completion.message.continuation.as_mut().unwrap();state.seal(&binding);
        let wire=state.wire(&c).unwrap();assert_eq!(wire,raw["choices"][0]["message"]);assert!(wire.get("reasoning_content").is_none());
    }
}

#[test]
fn ollama_rejects_malformed_or_conflicting_reasoning_without_loosening_stream_completion() {
    use std::collections::HashSet;
    for extra in [json!({"reasoning":1}),json!({"reasoning":["secret"]}),json!({"reasoning":"one","reasoning_content":"two"})] {
        let mut message=json!({"role":"assistant","content":"answer"});message.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
        let raw=json!({"choices":[{"index":0,"message":message,"finish_reason":"stop"}]});
        assert!(Ollama.decode_chat(&serde_json::to_vec(&raw).unwrap(),&HashSet::new()).is_err());
        for tools in [false,true] {assert!(Ollama.stream_chat(tools).push(&json!({"choices":[{"index":0,"delta":message,"finish_reason":"stop"}]}).to_string()).is_err());}
    }
    for tools in [false,true] {
        let mut stream=Ollama.stream_chat(tools);stream.push(&json!({"choices":[{"index":0,"delta":{"reasoning":"private"},"finish_reason":null}]}).to_string()).unwrap();
        assert!(stream.finish(&HashSet::new()).is_err());
    }
}
