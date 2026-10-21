use super::*;
fn config(provider: ProviderKind) -> ModelConfig {
    serde_json::from_value(json!({"supplier":"deepseek","provider":provider,"baseUrl":"https://api.deepseek.com/","completionsPath":if provider==ProviderKind::AnthropicMessages {"anthropic/v1/messages"}else{"chat/completions"},"model":"arbitrary-model","outputTokenParameter":"maxCompletionTokens","capabilities":{"developerRole":false,"temperature":true,"reasoningEffort":true},"requireApiKey":false})).unwrap()
}
fn request() -> ChatRequest {
    ChatRequest {
        messages: vec![ChatMessage::User {
            content: "hello".into(),
        }],
        tools: vec![],
    }
}
#[test]
fn policy_keeps_legacy_configs_and_only_valid_combinations() {
    let mut legacy = serde_json::to_value(config(ProviderKind::OpenAiCompatible)).unwrap();
    legacy.as_object_mut().unwrap().remove("supplier");
    let old: ModelConfig = serde_json::from_value(legacy).unwrap();
    assert_eq!(old.normalized().unwrap().supplier, None);
    for kind in [ProviderKind::OpenAiResponses, ProviderKind::GoogleGemini] {
        assert!(config(kind).normalized().is_err());
    }
    for effort in [
        ReasoningEffort::Minimal,
        ReasoningEffort::Medium,
        ReasoningEffort::Xhigh,
    ] {
        let mut c = config(ProviderKind::OpenAiCompatible);
        c.reasoning_effort = Some(effort);
        assert!(c.normalized().is_err());
    }
    let mut generic = config(ProviderKind::OpenAiCompatible);
    generic.supplier = Some(SupplierKind::Openai);
    assert_eq!(generic.normalized().unwrap().supplier, None);
}
#[test]
fn dual_protocol_max_tokens_temperature_and_thinking_policy() {
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
            c.temperature = Some(1.8);
            let c = c.normalized().unwrap();
            let body: Value =
                serde_json::from_str(&prepare_request(&c, &request()).unwrap().body).unwrap();
            assert_eq!(body["max_tokens"], 2048);
            assert!(body.get("max_completion_tokens").is_none());
            assert_eq!(body["temperature"], 1.8);
            match effort {
                None => assert!(body.get("thinking").is_none()),
                Some(ReasoningEffort::None) => {
                    assert_eq!(body["thinking"]["type"], "disabled");
                    assert!(body.get("output_config").is_none());
                    assert!(body.get("reasoning_effort").is_none());
                }
                Some(_) => assert_eq!(body["thinking"]["type"], "enabled"),
            }
        }
    }
}
#[test]
fn supplier_identity_persists_and_isolates_replay_binding() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("supplier.sqlite");
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
    let mut changed = runtime.config.clone();
    changed.supplier = None;
    assert!(binding.matches_config(&changed).is_err());
    drop(repository);
    drop(store);
    let reopened = ModelRepository::new(std::sync::Arc::new(
        crate::store::Store::open(&path).unwrap(),
    ));
    assert_eq!(
        reopened.get("model", "user").unwrap().config.supplier,
        Some(SupplierKind::Deepseek)
    );
}
#[test]
fn unsigned_messages_thinking_is_only_accepted_for_deepseek() {
    let bytes=serde_json::to_vec(&json!({"type":"message","role":"assistant","stop_reason":"end_turn","content":[{"type":"thinking","thinking":"private"},{"type":"text","text":"hello"}]})).unwrap();
    let adapter = super::super::adapter::select(&ProviderKind::AnthropicMessages);
    let allowed = std::collections::HashSet::new();
    assert!(adapter
        .decode_for(&config(ProviderKind::AnthropicMessages), &bytes, &allowed)
        .is_ok());
    let mut generic = config(ProviderKind::AnthropicMessages);
    generic.supplier = None;
    assert!(adapter.decode_for(&generic, &bytes, &allowed).is_err());
}

#[test]
fn messages_catalog_strips_only_complete_known_segments_and_keeps_proxy_origin() {
    use super::super::catalog::CatalogAdapter;
    for (path, expected) in [
        ("/anthropic/v1/", "/models"),
        ("/proxy/anthropic/v1/", "/proxy/models"),
        ("/proxy/anthropic/", "/proxy/models"),
        ("/proxy/myanthropic/v1/", "/proxy/myanthropic/v1/models"),
        ("/proxy/v1/", "/proxy/v1/models"),
    ] {
        let base = Url::parse(&format!("https://proxy.example{path}")).unwrap();
        let endpoint = DEEPSEEK_MODELS.endpoint(&base, None).unwrap();
        assert_eq!(endpoint.origin(), base.origin());
        assert_eq!(endpoint.path(), expected);
    }
}


#[test]
fn completion_presets_are_owned_by_supplier_policies() {
    for supplier in [SupplierKind::Deepseek, SupplierKind::Glm, SupplierKind::Qwen, SupplierKind::Ollama] {
        assert_eq!(select(Some(&supplier)).completion_effort(&ProviderKind::OpenAiCompatible), Some(ReasoningEffort::None));
    }
    assert_eq!(select(Some(&SupplierKind::Kimi)).completion_effort(&ProviderKind::AnthropicMessages), None);
    assert_eq!(select(Some(&SupplierKind::Grok)).completion_effort(&ProviderKind::OpenAiResponses), None);
    assert_eq!(select(None).completion_effort(&ProviderKind::GoogleGemini), None);
    assert_eq!(select(None).completion_effort(&ProviderKind::OpenAiCompatible), None);
}
