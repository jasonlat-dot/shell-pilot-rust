use axum::{
    extract::{OriginalUri, State},
    http::HeaderMap,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        completion::{self, Request},
        provider::*,
    },
    store::Store,
};
use std::sync::{Arc, Mutex};
use tokio_util::sync::CancellationToken;

struct Mock {
    calls: Mutex<Vec<(String, Value, HeaderMap)>>,
    reply: Mutex<String>,
    stall: bool,
}
async fn upstream(
    State(state): State<Arc<Mock>>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Json<Value> {
    state
        .calls
        .lock()
        .unwrap()
        .push((uri.path().into(), body, headers));
    if state.stall {
        std::future::pending::<()>().await;
    }
    let text = state.reply.lock().unwrap().clone();
    Json(if uri.path().ends_with("/input_tokens") {
        json!({"object":"response.input_tokens","input_tokens":120})
    } else if uri.path().ends_with("/messages") {
        json!({"id":"test","type":"message","role":"assistant","model":"test","content":[{"type":"text","text":text}],"stop_reason":"end_turn","usage":{"input_tokens":20,"output_tokens":4}})
    } else if uri.path().contains(":generateContent") {
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":text}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":20,"candidatesTokenCount":4,"totalTokenCount":24}})
    } else if uri.path().ends_with("/responses") {
        json!({"id":"r","object":"response","status":"completed","output":[{"id":"m","type":"message","role":"assistant","status":"completed","content":[{"type":"output_text","text":text,"annotations":[]}]}],"usage":{"input_tokens":20,"output_tokens":4,"total_tokens":24}})
    } else {
        json!({"choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"}],"usage":{"prompt_tokens":20,"completion_tokens":4,"total_tokens":24}})
    })
}
async fn fixture(
    provider: &str,
    supplier: Option<&str>,
    stall: bool,
) -> (ModelRepository, Arc<Mock>, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/proxy/v1/", listener.local_addr().unwrap());
    let state = Arc::new(Mock {
        calls: Mutex::new(vec![]),
        reply: Mutex::new(" logs -f".into()),
        stall,
    });
    let router = Router::new()
        .route("/proxy/v1/chat/completions", post(upstream))
        .route("/proxy/v1/messages", post(upstream))
        .route("/proxy/v1/responses/input_tokens", post(upstream))
        .route("/proxy/v1/models/test:generateContent", post(upstream))
        .route("/proxy/v1/responses", post(upstream))
        .with_state(state.clone());
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let repo = ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    let path = match provider {
        "anthropicMessages" => "messages",
        "googleGemini" => "models",
        "openAiResponses" => "responses",
        _ => "chat/completions",
    };
    let update:ModelConfigUpdate=serde_json::from_value(json!({"config":{"provider":provider,"supplier":supplier,"baseUrl":base,"completionsPath":path,"model":"test",
        "contextWindow":8192,"outputReserve":2048,"safetyReserve":512,"compactPercent":75,"outputTokenParameter":"maxCompletionTokens",
        "capabilities":{"toolCalls":true,"developerRole":false,"temperature":false,"reasoningEffort":false,"vision":true},
        "temperature":null,"reasoningEffort":null,"timeoutSeconds":120,"maxResponseBytes":1048576,"requireApiKey":true},
        "apiKey":{"action":"set","value":"synthetic-completion-key"}})).unwrap();
    repo.save("completion-model", "owner", 0, update).unwrap();
    (repo, state, server)
}
fn context() -> Request {
    Request {
        prefix: "docker compose".into(),
        cwd: "/opt/app".into(),
    }
}

#[tokio::test]
async fn four_protocols_use_saved_credentials_and_ephemeral_options_without_mutating_profile() {
    for provider in [
        "openAiCompatible",
        "anthropicMessages",
        "googleGemini",
        "openAiResponses",
    ] {
        let (repo, state, server) = fixture(provider, None, false).await;
        let before = serde_json::to_value(repo.get("completion-model", "owner").unwrap()).unwrap();
        let reply = completion::suggest(
            repo.runtime("completion-model", "owner").unwrap(),
            context(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(reply.suffix, " logs -f", "{provider}");
        let calls = state.calls.lock().unwrap();
        assert_eq!(
            calls.len(),
            if provider == "openAiResponses" { 2 } else { 1 }
        );
        let (_, body, headers) = calls.last().unwrap();
        assert!(!body.to_string().contains("synthetic-completion-key"));
        assert!(
            headers.get("authorization").is_some()
                || headers.get("x-api-key").is_some()
                || headers.get("x-goog-api-key").is_some()
        );
        assert!(body
            .get("tools")
            .is_none_or(|value| value.as_array().is_some_and(Vec::is_empty)));
        let limit = body
            .get("max_completion_tokens")
            .or_else(|| body.get("max_output_tokens"))
            .or_else(|| body.get("max_tokens"))
            .or_else(|| body.pointer("/generationConfig/maxOutputTokens"));
        assert_eq!(limit, Some(&json!(64)), "{body}");
        drop(calls);
        assert_eq!(
            serde_json::to_value(repo.get("completion-model", "owner").unwrap()).unwrap(),
            before
        );
        assert!(repo.runtime("completion-model", "other-user").is_err());
        server.abort();
    }
}
#[tokio::test]
async fn deepseek_disables_thinking_and_rejects_multiline_suggestions() {
    let (repo, state, server) = fixture("openAiCompatible", Some("deepseek"), false).await;
    *state.reply.lock().unwrap() = "\nrm -rf /".into();
    let result = completion::suggest(
        repo.runtime("completion-model", "owner").unwrap(),
        context(),
        &CancellationToken::new(),
    )
    .await
    .unwrap();
    assert_eq!(result.suffix, "");
    let calls = state.calls.lock().unwrap();
    assert_eq!(calls[0].1["thinking"]["type"], "disabled");
    assert_eq!(calls[0].1["max_tokens"], 64);
    server.abort();
}
#[tokio::test]
async fn cancellation_and_timeout_stop_request_without_retries() {
    let (repo, state, server) = fixture("openAiCompatible", None, true).await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        completion::suggest(
            repo.runtime("completion-model", "owner").unwrap(),
            context(),
            &cancel
        )
        .await,
        Err(ModelError::Cancelled)
    ));
    assert!(state.calls.lock().unwrap().is_empty());
    let cancel = CancellationToken::new();
    let future = completion::suggest(
        repo.runtime("completion-model", "owner").unwrap(),
        context(),
        &cancel,
    );
    assert!(matches!(future.await, Err(ModelError::Timeout)));
    assert_eq!(state.calls.lock().unwrap().len(), 1);
    server.abort();
}
