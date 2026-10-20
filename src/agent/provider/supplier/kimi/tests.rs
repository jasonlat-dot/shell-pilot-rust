use super::*;
fn config(provider: ProviderKind) -> ModelConfig {
    serde_json::from_value(json!({"supplier":"kimi","provider":provider,"baseUrl":"https://proxy.test/custom/","completionsPath":if provider==ProviderKind::AnthropicMessages {"messages"}else{"chat/completions"},"model":"arbitrary-alias","outputTokenParameter":"maxCompletionTokens","capabilities":{"developerRole":false,"temperature":true,"reasoningEffort":true},"requireApiKey":false})).unwrap()
}

#[test]
fn defaults_toggles_and_efforts_are_protocol_specific_and_never_inferred_from_model_names() {
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
            Some(ReasoningEffort::High),
            Some(ReasoningEffort::Max),
        ] {
            let mut c = config(provider.clone());
            c.reasoning_effort = effort.clone();
            if provider == ProviderKind::AnthropicMessages
                && matches!(
                    effort,
                    Some(ReasoningEffort::None | ReasoningEffort::Enabled)
                )
            {
                assert!(c.normalized().is_err());
                continue;
            }
            let c = c.normalized().unwrap();
            let prepared = prepare_request(&c, &request).unwrap();
            let body: Value = serde_json::from_str(&prepared.body).unwrap();
            assert_eq!(body["model"], "arbitrary-alias");
            assert_eq!(body["max_tokens"], 2048);
            for field in [
                "temperature",
                "max_completion_tokens",
                "enable_thinking",
                "extra_body",
                "thinking_budget",
            ] {
                assert!(body.get(field).is_none());
            }
            match effort {
                None => {
                    for field in ["thinking", "reasoning_effort", "output_config"] {
                        assert!(body.get(field).is_none());
                    }
                }
                Some(value @ (ReasoningEffort::None | ReasoningEffort::Enabled)) => {
                    assert_eq!(
                        body["thinking"]["type"],
                        if value == ReasoningEffort::None {
                            "disabled"
                        } else {
                            "enabled"
                        }
                    );
                    assert!(body.get("reasoning_effort").is_none());
                    assert!(body["thinking"].get("keep").is_none());
                }
                Some(value) => {
                    assert!(body.get("thinking").is_none());
                    if provider == ProviderKind::AnthropicMessages {
                        assert_eq!(body["output_config"]["effort"], json!(value));
                        assert!(body.get("reasoning_effort").is_none());
                    } else {
                        assert_eq!(body["reasoning_effort"], json!(value));
                        assert!(body.get("output_config").is_none());
                    }
                }
            }
        }
        for effort in [
            ReasoningEffort::Minimal,
            ReasoningEffort::Medium,
            ReasoningEffort::Xhigh,
        ] {
            let mut c = config(provider.clone());
            c.reasoning_effort = Some(effort);
            assert!(c.normalized().is_err());
        }
        let mut c = config(provider.clone());
        c.temperature = Some(1.0);
        assert!(c.normalized().is_err());
        let key = SecretKey::new("kimi-unit-fixture").unwrap();
        let headers = Kimi.headers(&provider, Some(&key)).unwrap();
        assert_eq!(headers[header::AUTHORIZATION], "Bearer kimi-unit-fixture");
        assert!(!headers.contains_key("x-api-key"));
        assert!(headers[header::AUTHORIZATION].is_sensitive());
        assert!(!Kimi
            .headers(&provider, None)
            .unwrap()
            .contains_key(header::AUTHORIZATION));
    }
    for provider in [ProviderKind::OpenAiResponses, ProviderKind::GoogleGemini] {
        assert!(config(provider).normalized().is_err());
    }
}

#[test]
fn catalog_preserves_origin_and_custom_prefix_and_maps_only_complete_anthropic_suffixes() {
    for (path, expected) in [
        ("/v1/", "/v1/models"),
        ("/proxy/anthropic/v1/", "/proxy/v1/models"),
        ("/proxy/anthropic/", "/proxy/v1/models"),
        ("/proxy/custom/", "/proxy/custom/models"),
        ("/proxy/not-anthropic/v1/", "/proxy/not-anthropic/v1/models"),
    ] {
        let base = Url::parse(&format!("https://proxy.test{path}")).unwrap();
        let url =
            super::super::super::catalog::CatalogAdapter::endpoint(&MODELS, &base, None).unwrap();
        assert_eq!(url.path(), expected);
        assert_eq!(url.origin(), base.origin());
    }
}

#[test]
fn kimi_identity_survives_reopen_and_replay_cannot_cross_suppliers_or_credentials() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("kimi.sqlite");
    let store = std::sync::Arc::new(crate::store::Store::open(&path).unwrap());
    let repo = ModelRepository::new(store.clone());
    let mut c = config(ProviderKind::OpenAiCompatible);
    c.reasoning_effort = Some(ReasoningEffort::Enabled);
    repo.save(
        "model",
        "u",
        0,
        ModelConfigUpdate {
            config: c,
            api_key: KeyChange::Keep,
        },
    )
    .unwrap();
    let runtime = repo.runtime("model", "u").unwrap();
    let binding = super::super::super::responses::Binding::from_runtime(&runtime).unwrap();
    for supplier in [
        None,
        Some(SupplierKind::Deepseek),
        Some(SupplierKind::Glm),
        Some(SupplierKind::Qwen),
    ] {
        let mut c = runtime.config.clone();
        c.supplier = supplier;
        assert!(binding.matches_config(&c).is_err());
    }
    let mut c = runtime.config.clone();
    c.base_url = "https://other.test/v1/".into();
    assert!(binding.matches_config(&c).is_err());
    drop(repo);
    drop(store);
    let reopened = ModelRepository::new(std::sync::Arc::new(
        crate::store::Store::open(&path).unwrap(),
    ));
    let c = reopened.get("model", "u").unwrap().config;
    assert_eq!(c.supplier, Some(SupplierKind::Kimi));
    assert_eq!(c.reasoning_effort, Some(ReasoningEffort::Enabled));
}

#[test]
fn missing_messages_thinking_signature_is_preserved_only_for_kimi_policy() {
    let adapter = super::super::super::adapter::select(&ProviderKind::AnthropicMessages);
    let allowed = std::collections::HashSet::new();
    for signature in [None, Some("")] {
        let mut block = json!({"type":"thinking","thinking":"private"});
        if let Some(signature) = signature {
            block["signature"] = signature.into();
        }
        let bytes=serde_json::to_vec(&json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[block,{"type":"text","text":"hello"}]})).unwrap();
        let mut c = config(ProviderKind::AnthropicMessages);
        assert!(adapter.decode_for(&c, &bytes, &allowed).is_ok());
        c.supplier = None;
        assert!(adapter.decode_for(&c, &bytes, &allowed).is_err());
    }
}
