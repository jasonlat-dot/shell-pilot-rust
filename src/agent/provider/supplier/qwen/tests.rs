use super::*;
fn config(provider: ProviderKind) -> ModelConfig {
    serde_json::from_value(json!({"supplier":"qwen","provider":provider,"baseUrl":"https://proxy.test/custom/","completionsPath":if provider==ProviderKind::AnthropicMessages {"messages"}else{"chat/completions"},"model":"arbitrary-alias","outputTokenParameter":"maxCompletionTokens","capabilities":{"developerRole":false,"temperature":true,"reasoningEffort":true},"requireApiKey":false})).unwrap()
}
#[test]
fn thinking_switch_and_efforts_use_the_correct_wire_fields_without_invented_budget() {
    let request = ChatRequest {
        messages: vec![ChatMessage::User {
            content: "hello".into(),
        }],
        tools: vec![],
    };
    for provider in [
        ProviderKind::OpenAiCompatible,
        ProviderKind::AnthropicMessages,
    ] {
        for effort in [
            None,
            Some(ReasoningEffort::None),
            Some(ReasoningEffort::Enabled),
            Some(ReasoningEffort::Low),
            Some(ReasoningEffort::Medium),
            Some(ReasoningEffort::Xhigh),
        ] {
            let mut config = config(provider.clone());
            config.reasoning_effort = effort.clone();
            config.temperature = Some(1.8);
            let config = config.normalized().unwrap();
            assert_eq!(config.supplier, Some(SupplierKind::Qwen));
            let prepared = prepare_request(&config, &request).unwrap();
            assert!(prepared
                .endpoint
                .as_str()
                .starts_with("https://proxy.test/custom/"));
            let body: Value = serde_json::from_str(&prepared.body).unwrap();
            assert_eq!(body["model"], "arbitrary-alias");
            assert_eq!(body["max_tokens"], 2048);
            assert_eq!(body["temperature"], 1.8);
            for field in ["max_completion_tokens", "thinking_budget", "extra_body"] {
                assert!(body.get(field).is_none());
            }
            assert!(body["thinking"].get("budget_tokens").is_none());
            if let Some(effort) = effort {
                let enabled = effort != ReasoningEffort::None;
                if provider == ProviderKind::AnthropicMessages {
                    assert_eq!(
                        body["thinking"]["type"],
                        if enabled { "enabled" } else { "disabled" }
                    );
                    assert!(body.get("enable_thinking").is_none());
                } else {
                    assert_eq!(body["enable_thinking"], enabled);
                    assert!(body.get("thinking").is_none());
                }
                if matches!(effort, ReasoningEffort::None | ReasoningEffort::Enabled) {
                    assert!(body.get("reasoning_effort").is_none());
                    assert!(body.get("output_config").is_none());
                } else if provider == ProviderKind::AnthropicMessages {
                    assert_eq!(body["output_config"]["effort"], json!(effort));
                } else {
                    assert_eq!(body["reasoning_effort"], json!(effort));
                }
            } else {
                for field in [
                    "enable_thinking",
                    "thinking",
                    "reasoning_effort",
                    "output_config",
                ] {
                    assert!(body.get(field).is_none());
                }
            }
        }
    }
}
#[test]
fn native_validation_rejects_invalid_combinations_and_toggle_does_not_leak_to_other_suppliers() {
    for provider in [
        ProviderKind::OpenAiCompatible,
        ProviderKind::AnthropicMessages,
    ] {
        for effort in [
            ReasoningEffort::Minimal,
            ReasoningEffort::High,
            ReasoningEffort::Max,
        ] {
            let mut c = config(provider.clone());
            c.reasoning_effort = Some(effort);
            assert!(c.normalized().is_err());
        }
        let mut c = config(provider.clone());
        c.temperature = Some(2.0);
        assert!(c.normalized().is_err());
        for temperature in [0.0, 1.999] {
            let mut c = config(provider.clone());
            c.temperature = Some(temperature);
            assert!(c.normalized().is_ok());
        }
    }
    for supplier in [
        None,
        Some(SupplierKind::Openai),
        Some(SupplierKind::Deepseek),
        Some(SupplierKind::Glm),
        Some(SupplierKind::Anthropic),
        Some(SupplierKind::Google),
    ] {
        let provider = match supplier {
            Some(SupplierKind::Anthropic) => ProviderKind::AnthropicMessages,
            Some(SupplierKind::Google) => ProviderKind::GoogleGemini,
            _ => ProviderKind::OpenAiCompatible,
        };
        let mut c = config(provider);
        c.supplier = supplier;
        c.reasoning_effort = Some(ReasoningEffort::Enabled);
        assert!(c.normalized().is_err());
    }
    for provider in [ProviderKind::OpenAiResponses, ProviderKind::GoogleGemini] {
        assert!(config(provider).normalized().is_err());
    }
}
#[test]
fn qwen_identity_and_toggle_persist_and_cannot_replay_across_suppliers() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("qwen.sqlite");
    let store = std::sync::Arc::new(crate::store::Store::open(&path).unwrap());
    let repository = ModelRepository::new(store.clone());
    let mut config = config(ProviderKind::OpenAiCompatible);
    config.reasoning_effort = Some(ReasoningEffort::Enabled);
    repository
        .save(
            "model",
            "user",
            0,
            ModelConfigUpdate {
                config,
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
    let runtime = repository.runtime("model", "user").unwrap();
    let binding = super::super::super::responses::Binding::from_runtime(&runtime).unwrap();
    for supplier in [None, Some(SupplierKind::Deepseek), Some(SupplierKind::Glm)] {
        let mut c = runtime.config.clone();
        c.supplier = supplier;
        assert!(binding.matches_config(&c).is_err());
    }
    drop(repository);
    drop(store);
    let reopened = ModelRepository::new(std::sync::Arc::new(
        crate::store::Store::open(&path).unwrap(),
    ));
    let config = reopened.get("model", "user").unwrap().config;
    assert_eq!(config.supplier, Some(SupplierKind::Qwen));
    assert_eq!(config.reasoning_effort, Some(ReasoningEffort::Enabled));
}
#[test]
fn documented_empty_messages_signature_is_accepted_only_by_supplier_policy() {
    let adapter = super::super::super::adapter::select(&ProviderKind::AnthropicMessages);
    let bytes=serde_json::to_vec(&json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"private","signature":""},{"type":"text","text":"hello"}]})).unwrap();
    let allowed = std::collections::HashSet::new();
    let mut c = config(ProviderKind::AnthropicMessages);
    assert!(adapter.decode_for(&c, &bytes, &allowed).is_ok());
    c.supplier = None;
    assert!(adapter.decode_for(&c, &bytes, &allowed).is_err());
}
