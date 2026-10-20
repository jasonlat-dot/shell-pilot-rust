//! Model discovery uses loopback-only fixtures and disposable encrypted credentials.
use axum::{
    body::Body,
    extract::State,
    http::{HeaderMap, Uri},
    response::Response,
    routing::get,
    Router,
};
use serde_json::{json, Value};
use shellpilot_server::{agent::provider::*, store::Store};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
#[derive(Clone)]
enum Reply {
    Json(Value),
    Http(u16),
    Large,
    Hold,
}
#[derive(Clone, Default)]
struct Wire {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    calls: Arc<Mutex<Vec<(Uri, HeaderMap)>>>,
    received: Arc<tokio::sync::Notify>,
}
async fn respond(State(w): State<Wire>, uri: Uri, headers: HeaderMap) -> Response {
    w.calls.lock().unwrap().push((uri, headers));
    w.received.notify_one();
    let reply = w
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected retry");
    match reply {
        Reply::Json(value) => Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        Reply::Http(code) => Response::builder()
            .status(code)
            .header("location", "http://127.0.0.1:1/secret")
            .body(Body::from("private upstream diagnostic"))
            .unwrap(),
        Reply::Large => Response::builder()
            .header("content-length", (9 * 1024 * 1024).to_string())
            .body(Body::from(vec![b' '; 9 * 1024 * 1024]))
            .unwrap(),
        Reply::Hold => {
            std::future::pending::<()>().await;
            unreachable!()
        }
    }
}
struct Fixture {
    _dir: tempfile::TempDir,
    repository: ModelRepository,
    wire: Wire,
    base: String,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(replies: Vec<Reply>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let repository = ModelRepository::new(Arc::new(
            Store::open(&dir.path().join("catalog.sqlite")).unwrap(),
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/proxy/v1/", listener.local_addr().unwrap());
        let wire = Wire::default();
        wire.replies.lock().unwrap().extend(replies);
        let app = Router::new()
            .route("/proxy/v1/models", get(respond))
            .route("/proxy/models", get(respond))
            .with_state(wire.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            _dir: dir,
            repository,
            wire,
            base,
            server,
        }
    }
    fn query(&self, provider: ProviderKind) -> CatalogRequest {
        CatalogRequest {
            provider,
            supplier: None,
            base_url: self.base.clone(),
            require_api_key: true,
            api_key: KeyChange::Set {
                value: SecretKey::new("catalog-test-key").unwrap(),
            },
            channel_id: None,
            revision: None,
        }
    }
    async fn list(&self, query: CatalogRequest) -> Result<Vec<CatalogModel>> {
        self.repository
            .catalog_client("user", query)?
            .list(&CancellationToken::new())
            .await
    }
}
#[tokio::test]
async fn openai_protocols_share_catalog_and_preserve_proxy_prefix_without_saving() {
    let f = Fixture::new(vec![
        Reply::Json(json!({"data":[{"id":"one"},{"id":"one"},{"id":"vendor/two"}]})),
        Reply::Json(json!({"data":[]})),
    ])
    .await;
    let models = f
        .list(f.query(ProviderKind::OpenAiCompatible))
        .await
        .unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["one", "vendor/two"]
    );
    assert!(!serde_json::to_string(&models)
        .unwrap()
        .contains("catalog-test-key"));
    assert!(f
        .list(f.query(ProviderKind::OpenAiResponses))
        .await
        .unwrap()
        .is_empty());
    for (uri, headers) in f.wire.calls.lock().unwrap().iter() {
        assert_eq!(uri.path(), "/proxy/v1/models");
        assert!(uri.query().is_none());
        assert_eq!(headers["authorization"], "Bearer catalog-test-key");
        assert!(!headers.contains_key("x-api-key"));
    }
    assert!(f.repository.channels("user").unwrap().is_empty());
}
#[tokio::test]
async fn anthropic_pages_use_native_headers_and_escaped_cursor() {
    let f=Fixture::new(vec![Reply::Json(json!({"data":[{"id":"claude-one","display_name":"Claude One"}],"has_more":true,"last_id":"cursor +&/"})),Reply::Json(json!({"data":[{"id":"claude-two"}],"has_more":false}))]).await;
    let models = f
        .list(f.query(ProviderKind::AnthropicMessages))
        .await
        .unwrap();
    assert_eq!(models.len(), 2);
    assert_eq!(models[0].display_name.as_deref(), Some("Claude One"));
    let calls = f.wire.calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].1["x-api-key"], "catalog-test-key");
    assert_eq!(calls[0].1["anthropic-version"], "2023-06-01");
    assert!(!calls[0].1.contains_key("authorization"));
    let url = url::Url::parse(&format!("http://localhost{}", calls[1].0)).unwrap();
    assert!(url
        .query_pairs()
        .any(|(k, v)| k == "after_id" && v == "cursor +&/"));
}
#[tokio::test]
async fn google_pages_filter_generation_methods_and_normalize_model_ids() {
    let f=Fixture::new(vec![Reply::Json(json!({"models":[{"name":"models/gemini-one","displayName":"Gemini One","supportedGenerationMethods":["generateContent","countTokens"]},{"name":"models/embedding-one","supportedGenerationMethods":["embedContent"]}],"nextPageToken":"page+2"})),Reply::Json(json!({"models":[{"name":"models/gemini-two","supportedGenerationMethods":["generateContent"]}]}))]).await;
    let models = f.list(f.query(ProviderKind::GoogleGemini)).await.unwrap();
    assert_eq!(
        models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
        ["gemini-one", "gemini-two"]
    );
    let calls = f.wire.calls.lock().unwrap();
    assert_eq!(calls[0].1["x-goog-api-key"], "catalog-test-key");
    assert!(!calls[0].1.contains_key("authorization"));
    assert!(calls[1].0.query().unwrap().contains("pageToken=page%2B2"));
    assert!(!calls[1].0.to_string().contains("catalog-test-key"));
}
#[tokio::test]
async fn saved_channel_key_survives_removing_original_model_and_queries_do_not_mutate() {
    let f = Fixture::new(vec![
        Reply::Json(json!({"data":[{"id":"available"}]})),
        Reply::Json(json!({"data":[]})),
        Reply::Json(json!({"data":[]})),
    ])
    .await;
    let config:ModelConfig=serde_json::from_value(json!({"provider":"openAiCompatible","baseUrl":f.base,"completionsPath":"chat/completions","model":"old","outputTokenParameter":"maxCompletionTokens","capabilities":{"toolCalls":true,"developerRole":false,"temperature":false,"reasoningEffort":false,"vision":true},"requireApiKey":true})).unwrap();
    let saved = f
        .repository
        .save_channel(
            "channel",
            "user",
            0,
            ModelConfigUpdate {
                config: config.clone(),
                api_key: KeyChange::Set {
                    value: SecretKey::new("saved-test-key").unwrap(),
                },
            },
            "Proxy",
            vec!["old".into(), "new".into()],
        )
        .unwrap();
    let saved = f
        .repository
        .save_channel(
            "channel",
            "user",
            saved.profile.revision,
            ModelConfigUpdate {
                config,
                api_key: KeyChange::Keep,
            },
            "Proxy",
            vec!["new".into()],
        )
        .unwrap();
    let mut query = f.query(ProviderKind::OpenAiCompatible);
    query.api_key = KeyChange::Keep;
    query.channel_id = Some("channel".into());
    query.revision = Some(saved.profile.revision);
    assert_eq!(f.list(query.clone()).await.unwrap()[0].id, "available");
    assert!(matches!(
        f.repository.catalog_client("other", query.clone()),
        Err(ModelError::NotFound)
    ));
    let mut stale = query.clone();
    stale.revision = Some(1);
    assert!(matches!(
        f.repository.catalog_client("user", stale),
        Err(ModelError::Stale)
    ));
    query.require_api_key = false;
    f.list(query.clone()).await.unwrap();
    query.require_api_key = true;
    query.api_key = KeyChange::Set {
        value: SecretKey::new("replacement-test-key").unwrap(),
    };
    f.list(query).await.unwrap();
    let calls = f.wire.calls.lock().unwrap();
    assert_eq!(calls[0].1["authorization"], "Bearer saved-test-key");
    assert!(!calls[1].1.contains_key("authorization"));
    assert_eq!(calls[2].1["authorization"], "Bearer replacement-test-key");
    let after = f.repository.channels("user").unwrap();
    assert_eq!(after[0].profile.revision, saved.profile.revision);
    assert_eq!(after[0].models.len(), 1);
    assert_eq!(after[0].models[0].id, saved.models[0].id);
}
#[tokio::test]
async fn http_errors_redirects_and_size_limits_do_not_retry_or_expose_upstream_body() {
    for reply in [
        Reply::Http(401),
        Reply::Http(404),
        Reply::Http(429),
        Reply::Http(302),
        Reply::Large,
    ] {
        let f = Fixture::new(vec![reply]).await;
        let error = f
            .list(f.query(ProviderKind::OpenAiCompatible))
            .await
            .unwrap_err();
        assert!(matches!(
            error,
            ModelError::Http(401 | 404 | 429 | 302) | ModelError::ResponseTooLarge
        ));
        assert!(!error.to_string().contains("private"));
        assert_eq!(f.wire.calls.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn malformed_pages_and_repeated_cursors_fail_instead_of_returning_partial_catalog() {
    for replies in [
        vec![json!({"data":[{"id":"ok"}],"has_more":true})],
        vec![
            json!({"data":[{"id":"ok"}],"has_more":true,"last_id":"same"}),
            json!({"data":[],"has_more":true,"last_id":"same"}),
        ],
        vec![json!({"data":[{"id":"bad\nmodel"}]})],
        vec![json!({"error":"not a model list"})],
    ] {
        let f = Fixture::new(replies.into_iter().map(Reply::Json).collect()).await;
        assert!(matches!(
            f.list(f.query(ProviderKind::AnthropicMessages)).await,
            Err(ModelError::Response)
        ));
    }
}
#[tokio::test]
async fn cancellation_stops_an_inflight_catalog_request() {
    let f = Fixture::new(vec![Reply::Hold]).await;
    let client = f
        .repository
        .catalog_client("user", f.query(ProviderKind::OpenAiCompatible))
        .unwrap();
    let cancel = CancellationToken::new();
    let stopped = cancel.clone();
    let received = f.wire.received.clone();
    let trigger = tokio::spawn(async move {
        received.notified().await;
        stopped.cancel();
    });
    assert!(matches!(
        tokio::time::timeout(std::time::Duration::from_secs(2), client.list(&cancel))
            .await
            .unwrap(),
        Err(ModelError::Cancelled)
    ));
    trigger.await.unwrap();
}
#[tokio::test]
async fn invalid_urls_and_missing_keys_fail_before_network_access() {
    let f = Fixture::new(vec![]).await;
    for base in [
        "https://user:password@example.com/v1/",
        "https://example.com/v1/?key=secret",
        "https://example.com/v1/#fragment",
        "file:///secret",
        "https://example.com/a%2fb/",
    ] {
        let mut query = f.query(ProviderKind::OpenAiCompatible);
        query.base_url = base.into();
        assert!(matches!(
            f.repository.catalog_client("user", query),
            Err(ModelError::Invalid(_))
        ));
    }
    let mut query = f.query(ProviderKind::OpenAiCompatible);
    query.api_key = KeyChange::Keep;
    assert!(matches!(
        f.repository.catalog_client("user", query),
        Err(ModelError::KeyMissing)
    ));
    assert!(f.wire.calls.lock().unwrap().is_empty());
}

fn read_catalog_logs(directory: &std::path::Path) -> (String, Vec<Value>) {
    let text = std::fs::read_dir(directory)
        .unwrap()
        .flat_map(|day| std::fs::read_dir(day.unwrap().path()).unwrap())
        .map(|file| std::fs::read_to_string(file.unwrap().path()).unwrap())
        .collect::<String>();
    let rows = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    (text, rows)
}
#[tokio::test]
async fn catalog_logs_link_pages_and_redact_credentials_cursors_and_raw_response() {
    use shellpilot_server::diagnostics::{Diagnostics, Level};
    let secret_cursor = "opaque-cursor-do-not-log";
    let f = Fixture::new(vec![Reply::Json(json!({"models":[{"name":"models/gemini-one","description":"raw-body-do-not-log","supportedGenerationMethods":["generateContent"]}],"nextPageToken":secret_cursor})),Reply::Json(json!({"models":[]}))]).await;
    let log_dir = tempfile::tempdir().unwrap();
    let sink = Arc::new(Diagnostics::default());
    sink.configure(log_dir.path().into(), Level::Debug).unwrap();
    let trace = sink.trace("catalog-log-success", "", "");
    let client = f
        .repository
        .catalog_client("user", f.query(ProviderKind::GoogleGemini))
        .unwrap();
    let result = trace
        .scope(client.list(&CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(result.len(), 1);
    let (text, rows) = read_catalog_logs(log_dir.path());
    assert!(!text.contains("catalog-test-key"));
    assert!(!text.contains(secret_cursor));
    assert!(!text.contains("raw-body-do-not-log"));
    assert!(rows
        .iter()
        .all(|row| row["traceId"] == "catalog-log-success"));
    let requests = rows
        .iter()
        .filter(|row| row["event"] == "model.catalog.http_request")
        .collect::<Vec<_>>();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0]["fields"]["method"], "GET");
    assert_eq!(requests[0]["fields"]["authentication"], "x-goog-api-key");
    assert_eq!(requests[0]["fields"]["hasBody"], false);
    assert!(requests[0]["fields"]["url"]
        .as_str()
        .unwrap()
        .ends_with("/proxy/v1/models?pageSize=1000"));
    assert!(requests[1]["fields"]["url"]
        .as_str()
        .unwrap()
        .contains("pageToken=REDACTED"));
    assert!(rows
        .iter()
        .any(|row| row["event"] == "model.catalog.http_response"
            && row["fields"]["httpStatus"] == 200));
    assert!(rows
        .iter()
        .any(|row| row["event"] == "model.catalog.completed" && row["fields"]["modelCount"] == 1));
}
#[tokio::test]
async fn catalog_logs_explain_invalid_methods_and_prefix_errors() {
    use shellpilot_server::diagnostics::{Diagnostics, Level};
    for (body, path, expected, actual_type) in [
        (
            json!({"models":[{"name":"gemini-3-flash","supportedGenerationMethods":"invalid"}]}),
            "/models/0/supportedGenerationMethods",
            "array",
            "string",
        ),
        (
            json!({"models":[{"name":"gemini-3-flash","supportedGenerationMethods":["generateContent"]}]}),
            "/models/0/name",
            "models/<model-id>",
            "string",
        ),
        (
            json!({"data":[{"id":"gemini-3-flash"}]}),
            "/models",
            "array",
            "missing",
        ),
    ] {
        let f = Fixture::new(vec![Reply::Json(body)]).await;
        let log_dir = tempfile::tempdir().unwrap();
        let sink = Arc::new(Diagnostics::default());
        sink.configure(log_dir.path().into(), Level::Info).unwrap();
        let trace = sink.trace("catalog-schema-error", "", "");
        let client = f
            .repository
            .catalog_client("user", f.query(ProviderKind::GoogleGemini))
            .unwrap();
        assert!(matches!(
            trace.scope(client.list(&CancellationToken::new())).await,
            Err(ModelError::Response)
        ));
        let (text, rows) = read_catalog_logs(log_dir.path());
        assert!(!text.contains("catalog-test-key"));
        assert!(!text.contains("gemini-3-flash"));
        assert!(rows.iter().any(|row| row["level"] == "error"
            && row["event"] == "model.catalog.parse_failed"
            && row["fields"]["jsonPath"] == path
            && row["fields"]["expected"] == expected
            && row["fields"]["actualType"] == actual_type));
        assert!(rows.iter().any(|row| row["event"] == "model.catalog.failed"
            && row["fields"]["errorCode"] == "MODEL_RESPONSE_INVALID"));
        assert!(!rows
            .iter()
            .any(|row| row["event"] == "model.catalog.completed"));
    }
}
#[tokio::test]
async fn catalog_failure_and_cancellation_levels_obey_the_configured_log_level() {
    use shellpilot_server::diagnostics::{Diagnostics, Level};
    for (reply, cancelled, level) in [
        (Reply::Http(429), false, Level::Error),
        (Reply::Hold, true, Level::Warn),
    ] {
        let f = Fixture::new(vec![reply]).await;
        let log_dir = tempfile::tempdir().unwrap();
        let sink = Arc::new(Diagnostics::default());
        sink.configure(log_dir.path().into(), level).unwrap();
        let trace = sink.trace("catalog-log-level", "", "");
        let client = f
            .repository
            .catalog_client("user", f.query(ProviderKind::OpenAiCompatible))
            .unwrap();
        let cancel = CancellationToken::new();
        if cancelled {
            cancel.cancel();
        }
        let error = trace.scope(client.list(&cancel)).await.unwrap_err();
        assert!(matches!(
            error,
            ModelError::Http(429) | ModelError::Cancelled
        ));
        let (text, rows) = read_catalog_logs(log_dir.path());
        assert!(!text.contains("private upstream diagnostic"));
        assert!(rows.iter().all(|row| row["level"] == level.text()));
        assert!(rows
            .iter()
            .any(|row| row["event"] == "model.catalog.failed"));
        assert!(!rows
            .iter()
            .any(|row| row["event"] == "model.catalog.completed"));
    }
}

#[tokio::test]
async fn google_mixed_proxy_catalog_skips_empty_capabilities_and_finishes_on_null_cursor() {
    use shellpilot_server::diagnostics::{Diagnostics, Level};
    let incomplete = [
        "gemini-3-flash",
        "gemini-3.8-flash-high",
        "gemini-3.8-flash-low",
        "gemini-3.8-flash-medium",
        "gemini-3.8-flash-tiered",
    ];
    let expected = [
        "gemini-3-flash",
        "gemini-3.1-flash-image",
        "gemini-3.8-flash",
        "gemini-3.8-flash-high",
        "gemini-3.8-flash-low",
        "gemini-3.8-flash-medium",
        "gemini-3.8-flash-tiered",
    ];
    let mut models = incomplete.iter().map(|name|json!({"name":name,"displayName":name,"baseModelId":null,"version":null,"description":null,"inputTokenLimit":null,"outputTokenLimit":null,"supportedGenerationMethods":null,"thinking":null,"temperature":null,"maxTemperature":null,"topP":null,"topK":null})).collect::<Vec<_>>();
    models.extend(expected.iter().map(|name|json!({"name":format!("models/{name}"),"supportedGenerationMethods":["generateContent","streamGenerateContent"]})));
    // A missing/empty declaration is also unadvertised, not evidence of chat support.
    models.extend([
        json!({"name":"unknown-methods"}),
        json!({"name":"empty-methods","supportedGenerationMethods":[]}),
        json!({"name":"models/embedding-only","supportedGenerationMethods":["embedContent"]}),
        json!({"name":"models/gemini-3-flash","supportedGenerationMethods":["generateContent"]}),
    ]);
    let f = Fixture::new(vec![Reply::Json(
        json!({"models":models,"nextPageToken":null}),
    )])
    .await;
    let logs = tempfile::tempdir().unwrap();
    let sink = Arc::new(Diagnostics::default());
    sink.configure(logs.path().into(), Level::Debug).unwrap();
    let trace = sink.trace("catalog-mixed-proxy", "", "");
    let client = f
        .repository
        .catalog_client("user", f.query(ProviderKind::GoogleGemini))
        .unwrap();
    let result = trace
        .scope(client.list(&CancellationToken::new()))
        .await
        .unwrap();
    assert_eq!(
        result
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>(),
        expected
    );
    assert_eq!(
        f.wire.calls.lock().unwrap().len(),
        1,
        "null cursor must not request another page"
    );
    let (raw, rows) = read_catalog_logs(logs.path());
    assert!(!raw.contains("catalog-test-key"));
    assert!(rows
        .iter()
        .any(|row| row["event"] == "model.catalog.entries_skipped" && row["fields"]["count"] == 7));
    assert!(rows
        .iter()
        .any(|row| row["event"] == "model.catalog.completed" && row["fields"]["modelCount"] == 7));
    assert!(!rows.iter().any(|row| row["level"] == "error"));
}
#[tokio::test]
async fn google_empty_capability_page_follows_cursor_and_empty_markers_end_cleanly() {
    let f=Fixture::new(vec![Reply::Json(json!({"models":[{"name":"unadvertised","supportedGenerationMethods":null}],"nextPageToken":"next"})),Reply::Json(json!({"models":[{"name":"models/valid","supportedGenerationMethods":["generateContent"]}],"nextPageToken":null}))]).await;
    let result = f.list(f.query(ProviderKind::GoogleGemini)).await.unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].id, "valid");
    assert_eq!(f.wire.calls.lock().unwrap().len(), 2);
    for marker in [None, Some(Value::Null), Some(json!(""))] {
        let mut body =
            json!({"models":[{"name":"unadvertised","supportedGenerationMethods":null}]});
        if let Some(marker) = marker {
            body["nextPageToken"] = marker;
        }
        let f = Fixture::new(vec![Reply::Json(body)]).await;
        assert!(f
            .list(f.query(ProviderKind::GoogleGemini))
            .await
            .unwrap()
            .is_empty());
        assert_eq!(f.wire.calls.lock().unwrap().len(), 1);
    }
    for invalid in [
        Value::Null,
        json!(42),
        json!("invalid"),
        json!({"name":"bad","supportedGenerationMethods":false}),
    ] {
        let f = Fixture::new(vec![Reply::Json(
            json!({"models":[invalid],"nextPageToken":null}),
        )])
        .await;
        assert!(matches!(
            f.list(f.query(ProviderKind::GoogleGemini)).await,
            Err(ModelError::Response)
        ));
    }
    for marker in [json!(42), json!(false), json!({})] {
        let f = Fixture::new(vec![Reply::Json(
            json!({"models":[],"nextPageToken":marker}),
        )])
        .await;
        assert!(matches!(
            f.list(f.query(ProviderKind::GoogleGemini)).await,
            Err(ModelError::Response)
        ));
    }
}

#[tokio::test]
async fn deepseek_catalog_uses_shared_models_and_bearer_for_both_protocols() {
    let f = Fixture::new(vec![Reply::Json(json!({"data":[{"id":"deepseek-model","name":"DeepSeek model"}]})), Reply::Json(json!({"data":[{"id":"deepseek-model"}]}))]).await;
    for protocol in [ProviderKind::OpenAiCompatible, ProviderKind::AnthropicMessages] {
        let mut query = f.query(protocol.clone()); query.supplier = Some(SupplierKind::Deepseek);
        if protocol == ProviderKind::AnthropicMessages { query.base_url = f.base.replace("/proxy/v1/", "/proxy/anthropic/v1/"); }
        let models = f.list(query).await.unwrap(); assert_eq!(models[0].id, "deepseek-model");
        if protocol == ProviderKind::OpenAiCompatible { assert_eq!(models[0].display_name.as_deref(), Some("DeepSeek model")); }
    }
    let calls = f.wire.calls.lock().unwrap();
    assert_eq!(calls[0].0.path(), "/proxy/v1/models"); assert_eq!(calls[1].0.path(), "/proxy/models");
    for (url, headers) in calls.iter() { assert!(url.query().is_none()); assert_eq!(headers["authorization"], "Bearer catalog-test-key"); assert!(!headers.contains_key("x-api-key")); }
    assert!(f.repository.list("user").unwrap().is_empty());
}
