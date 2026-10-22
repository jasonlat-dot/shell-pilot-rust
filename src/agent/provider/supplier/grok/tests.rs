use super::*;

fn config(provider: ProviderKind) -> ModelConfig {
    serde_json::from_value(json!({"supplier":"grok","provider":provider,"baseUrl":"https://proxy.test/custom/","completionsPath":if provider==ProviderKind::OpenAiResponses {"responses"}else{"chat/completions"},"model":"arbitrary-alias","outputTokenParameter":"maxTokens","capabilities":{"developerRole":false,"temperature":true,"reasoningEffort":true},"requireApiKey":false})).unwrap()
}

#[test]
fn grok_defaults_and_efforts_use_the_selected_protocol_without_guessing_model_names() {
    let request = ChatRequest {
        messages: vec![ChatMessage::User {
            content: "hello".into(),
        }],
        tools: vec![],
    };
    for provider in [
        ProviderKind::OpenAiCompatible,
        ProviderKind::OpenAiResponses,
    ] {
        for effort in [
            None,
            Some(ReasoningEffort::Low),
            Some(ReasoningEffort::Medium),
            Some(ReasoningEffort::High),
            Some(ReasoningEffort::Xhigh),
        ] {
            let mut c = config(provider.clone());
            c.reasoning_effort = effort.clone();
            c.temperature = Some(0.7);
            let c = c.normalized().unwrap();
            assert_eq!(c.supplier, Some(SupplierKind::Grok));
            let body: Value =
                serde_json::from_str(&prepare_request(&c, &request).unwrap().body).unwrap();
            assert_eq!(body["model"], "arbitrary-alias");
            assert_eq!(body["temperature"], 0.7);
            assert!(body.get("thinking").is_none());
            assert!(body.get("output_config").is_none());
            assert!(body.get("max_tokens").is_none());
            if provider == ProviderKind::OpenAiResponses {
                assert_eq!(body["max_output_tokens"], 2048);
                assert_eq!(body["store"], false);
                assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
                assert!(body.get("reasoning_effort").is_none());
                if let Some(effort) = effort {
                    assert_eq!(body["reasoning"]["effort"], json!(effort));
                } else {
                    assert!(body.get("reasoning").is_none());
                }
            } else {
                assert_eq!(body["max_completion_tokens"], 2048);
                assert!(body.get("reasoning").is_none());
                if let Some(effort) = effort {
                    assert_eq!(body["reasoning_effort"], json!(effort));
                } else {
                    assert!(body.get("reasoning_effort").is_none());
                }
            }
        }
        for effort in [
            ReasoningEffort::None,
            ReasoningEffort::Enabled,
            ReasoningEffort::Minimal,
            ReasoningEffort::Max,
        ] {
            let mut c = config(provider.clone());
            c.reasoning_effort = Some(effort);
            assert!(c.normalized().is_err());
        }
        let key = SecretKey::new("grok-unit-fixture").unwrap();
        let headers = Grok.headers(&provider, Some(&key)).unwrap();
        assert_eq!(headers[header::AUTHORIZATION], "Bearer grok-unit-fixture");
        assert!(headers[header::AUTHORIZATION].is_sensitive());
        assert!(!headers.contains_key("x-api-key"));
        for temperature in [0.0, 2.0] {
            let mut c = config(provider.clone());
            c.temperature = Some(temperature);
            assert!(c.normalized().is_ok());
        }
    }
    for provider in [ProviderKind::AnthropicMessages, ProviderKind::GoogleGemini] {
        assert!(config(provider).normalized().is_err());
    }
}

#[tokio::test]
async fn local_response_measurement_covers_tools_images_and_reasoning_and_honors_cancellation() {
    use tokio_util::sync::CancellationToken;
    let c = config(ProviderKind::OpenAiResponses).normalized().unwrap();
    let client = ModelClient::new(RuntimeConfig {
        proxy: Default::default(),
        config: c,
        api_key: None,
    })
    .unwrap();
    let cancel = CancellationToken::new();
    let body = json!({"input":[{"role":"user","content":"hello"}],"instructions":"trusted"});
    let small = client.count_body(&body, &cancel).await.unwrap();
    assert!(small > 0);
    let mut big = body.clone();
    big["tools"] =
        json!([{"type":"function","name":"read","parameters":{"description":"x".repeat(2000)}}]);
    assert!(client.count_body(&big, &cancel).await.unwrap() > small);
    big["input"] = json!([{"type":"reasoning","summary":[],"encrypted_content":"x".repeat(2000)}]);
    assert!(client.count_body(&big, &cancel).await.unwrap() > small);
    let image = |n| json!({"input":[{"role":"user","content":[{"type":"input_image","image_url":format!("data:image/jpeg;base64,{}","a".repeat(n))}]}]});
    let image_cost = client.count_body(&image(40), &cancel).await.unwrap();
    assert!(image_cost >= crate::agent::attachments::IMAGE_TOKENS);
    assert_eq!(
        image_cost,
        client.count_body(&image(4000), &cancel).await.unwrap()
    );
    cancel.cancel();
    assert!(matches!(
        client.count_body(&body, &cancel).await,
        Err(ModelError::Cancelled)
    ));
    assert!(Grok.local_response_count());
    assert!(!COMPATIBLE.local_response_count());
}

#[test]
fn grok_identity_persists_and_native_continuations_cannot_cross_suppliers() {
    use std::sync::Arc;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("grok.sqlite");
    let store = Arc::new(crate::store::Store::open(&path).unwrap());
    let repo = ModelRepository::new(store.clone());
    repo.save(
        "model",
        "u",
        0,
        ModelConfigUpdate {
            config: config(ProviderKind::OpenAiResponses),
            api_key: KeyChange::Keep,
        },
    )
    .unwrap();
    let runtime = repo.runtime("model", "u").unwrap();
    let binding = super::super::super::responses::Binding::from_runtime(&runtime).unwrap();
    for supplier in [None, Some(SupplierKind::Kimi), Some(SupplierKind::Qwen)] {
        let mut c = runtime.config.clone();
        c.supplier = supplier;
        assert!(binding.matches_config(&c).is_err());
    }
    drop(repo);
    drop(store);
    let repo = ModelRepository::new(Arc::new(crate::store::Store::open(&path).unwrap()));
    assert_eq!(
        repo.get("model", "u").unwrap().config.supplier,
        Some(SupplierKind::Grok)
    );
}
