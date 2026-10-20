use super::*;

fn config(provider: ProviderKind) -> ModelConfig {
    serde_json::from_value(json!({"supplier":"glm","provider":provider,"baseUrl":"https://proxy.test/custom/","completionsPath":if provider==ProviderKind::AnthropicMessages {"messages"}else{"chat/completions"},"model":"arbitrary-alias","outputTokenParameter":"maxCompletionTokens","capabilities":{"developerRole":false,"temperature":true,"reasoningEffort":true},"requireApiKey":false})).unwrap()
}

#[test]
fn glm_parameters_follow_supplier_policy_without_model_name_guesses() {
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
            Some(ReasoningEffort::Low),
            Some(ReasoningEffort::High),
            Some(ReasoningEffort::Max),
        ] {
            let mut c = config(provider.clone());
            c.reasoning_effort = effort.clone();
            c.temperature = Some(0.7);
            let c = c.normalized().unwrap();
            assert_eq!(c.supplier, Some(SupplierKind::Glm));
            let prepared = prepare_request(&c, &request).unwrap();
            assert!(prepared
                .endpoint
                .as_str()
                .starts_with("https://proxy.test/custom/"));
            let body: Value = serde_json::from_str(&prepared.body).unwrap();
            assert_eq!(body["model"], "arbitrary-alias");
            assert_eq!(body["max_tokens"], 2048);
            assert_eq!(body["temperature"], 0.7);
            assert!(body.get("max_completion_tokens").is_none());
            match effort {
                None => {
                    for field in ["thinking", "reasoning_effort", "output_config"] {
                        assert!(body.get(field).is_none());
                    }
                }
                Some(ReasoningEffort::None) => {
                    assert_eq!(body["thinking"]["type"], "disabled");
                    assert!(body.get("reasoning_effort").is_none());
                    assert!(body.get("output_config").is_none());
                }
                Some(effort) => {
                    assert_eq!(body["thinking"]["type"], "enabled");
                    if provider == ProviderKind::AnthropicMessages {
                        assert_eq!(body["output_config"]["effort"], json!(effort));
                        assert!(body.get("reasoning_effort").is_none());
                    } else {
                        assert_eq!(body["reasoning_effort"], json!(effort));
                        assert!(body.get("output_config").is_none());
                    }
                }
            }
        }
        for temperature in [0.0, 1.0] {
            let mut c = config(provider.clone());
            c.temperature = Some(temperature);
            assert!(c.normalized().is_ok());
        }
        let mut c = config(provider.clone());
        c.temperature = Some(1.01);
        assert!(c.normalized().is_err());
        for effort in [
            ReasoningEffort::Minimal,
            ReasoningEffort::Medium,
            ReasoningEffort::Xhigh,
        ] {
            let mut c = config(provider.clone());
            c.reasoning_effort = Some(effort);
            assert!(c.normalized().is_err());
        }
    }
    for provider in [ProviderKind::OpenAiResponses, ProviderKind::GoogleGemini] {
        assert!(config(provider).normalized().is_err());
    }
}

#[test]
fn glm_identity_survives_reopen_and_replay_cannot_cross_supplier() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("glm.sqlite");
    let store = std::sync::Arc::new(crate::store::Store::open(&path).unwrap());
    let repository = ModelRepository::new(store.clone());
    repository
        .save(
            "model",
            "user",
            0,
            ModelConfigUpdate {
                config: config(ProviderKind::OpenAiCompatible),
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
    let runtime = repository.runtime("model", "user").unwrap();
    let binding = super::super::responses::Binding::from_runtime(&runtime).unwrap();
    for supplier in [None, Some(SupplierKind::Deepseek)] {
        let mut other = runtime.config.clone();
        other.supplier = supplier;
        assert!(binding.matches_config(&other).is_err());
    }
    drop(repository);
    drop(store);
    let reopened = ModelRepository::new(std::sync::Arc::new(
        crate::store::Store::open(&path).unwrap(),
    ));
    assert_eq!(
        reopened.get("model", "user").unwrap().config.supplier,
        Some(SupplierKind::Glm)
    );
}

#[test]
fn glm_accepts_unsigned_thinking_without_weakening_anthropic() {
    let adapter = super::super::adapter::select(&ProviderKind::AnthropicMessages);
    let bytes = serde_json::to_vec(&json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"hello"}]})).unwrap();
    let allowed = std::collections::HashSet::new();
    let mut c = config(ProviderKind::AnthropicMessages);
    assert!(adapter.decode_for(&c, &bytes, &allowed).is_ok());
    c.supplier = None;
    assert!(adapter.decode_for(&c, &bytes, &allowed).is_err());
}
