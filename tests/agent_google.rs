//! Google native HTTP contracts. Only loopback services and disposable databases/keys.
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, Uri},
    response::Response,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        chat::{Manager, Request},
        compact::Summarizer,
        provider::*,
        Payload, Repository,
    },
    store::Store,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
#[derive(Clone)]
enum Reply {
    Json(Value),
    Sse(String),
    Http(u16),
    Hold,
    Summary,
}
#[derive(Clone, Default)]
struct Wire {
    replies: Arc<Mutex<VecDeque<Reply>>>,
    requests: Arc<Mutex<Vec<(Uri, HeaderMap, Value)>>>,
    received: Arc<tokio::sync::Notify>,
}
fn response(parts: Value) -> Value {
    json!({"candidates":[{"content":{"role":"model","parts":parts},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":10,"candidatesTokenCount":3,"totalTokenCount":13}})
}
async fn respond(
    State(w): State<Wire>,
    uri: Uri,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    w.requests
        .lock()
        .unwrap()
        .push((uri, headers, body.clone()));
    w.received.notify_one();
    let reply = w
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected retry");
    let reply = match reply {
        Reply::Summary => {
            let source: Value =
                serde_json::from_str(body["contents"][0]["parts"][0]["text"].as_str().unwrap())
                    .unwrap();
            let summary = json!({"summary":"记忆保留：端口为 12233，尚未执行命令。","nextSteps":["继续只读检查"],"retainedUserFacts":[],"evidenceEventIds":source["availableEvidenceEventIds"]});
            Reply::Json(response(json!([{"text":summary.to_string()}])))
        }
        other => other,
    };
    match reply {
        Reply::Hold => {
            std::future::pending::<()>().await;
            unreachable!()
        }
        Reply::Http(status) => Response::builder()
            .status(status)
            .body(Body::from("private-upstream-body"))
            .unwrap(),
        Reply::Json(value) => Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(value.to_string()))
            .unwrap(),
        Reply::Sse(value) => {
            let chunks = value
                .into_bytes()
                .into_iter()
                .map(|b| Ok::<_, std::io::Error>(Bytes::from(vec![b])));
            Response::builder()
                .header("content-type", "text/event-stream")
                .body(Body::from_stream(futures_util::stream::iter(chunks)))
                .unwrap()
        }
        Reply::Summary => unreachable!(),
    }
}
struct Fixture {
    client: ModelClient,
    store: Arc<Store>,
    wire: Wire,
    server: tokio::task::JoinHandle<()>,
    directory: tempfile::TempDir,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new(replies: Vec<Reply>, timeout: u64, max_response: usize) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&directory.path().join("google.sqlite")).unwrap());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config:ModelConfig=serde_json::from_value(json!({"provider":"googleGemini","baseUrl":format!("http://{}/proxy/v1beta/",listener.local_addr().unwrap()),"completionsPath":"models","model":"arbitrary-model","outputTokenParameter":"maxCompletionTokens","timeoutSeconds":timeout,"maxResponseBytes":max_response,"capabilities":{"developerRole":false,"temperature":false,"reasoningEffort":false},"requireApiKey":true})).unwrap();
        let models = ModelRepository::new(store.clone());
        models
            .save(
                "model",
                "test",
                0,
                ModelConfigUpdate {
                    config,
                    api_key: KeyChange::Set {
                        value: SecretKey::new("test-google-only-key").unwrap(),
                    },
                },
            )
            .unwrap();
        let client = ModelClient::new(models.runtime("model", "test").unwrap()).unwrap();
        let wire = Wire {
            replies: Arc::new(Mutex::new(replies.into())),
            ..Default::default()
        };
        let app = Router::new()
            .route("/{*path}", post(respond))
            .with_state(wire.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self {
            client,
            store,
            wire,
            server,
            directory,
        }
    }
}
fn request() -> ChatRequest {
    ChatRequest {
        messages: vec![ChatMessage::User {
            content: "检查一下".into(),
        }],
        tools: vec![],
    }
}
#[tokio::test]
async fn native_json_and_fragmented_sse_use_model_url_header_key_and_final_usage() {
    let first = json!({"candidates":[{"content":{"role":"model","parts":[{"thought":true,"text":"private-thought"},{"text":"你好🌍"}]}}]});
    let finish = json!({"candidates":[{"finishReason":"STOP"}]});
    let usage = json!({"usageMetadata":{"promptTokenCount":9,"candidatesTokenCount":2,"thoughtsTokenCount":1,"totalTokenCount":12}});
    let events = format!("data: {first}\n\ndata: {finish}\n\ndata: {usage}\n\n");
    let f = Fixture::new(
        vec![
            Reply::Json(response(json!([{"text":"你好🌍"}]))),
            Reply::Sse(events),
        ],
        3,
        65536,
    )
    .await;
    assert_eq!(
        f.client
            .complete(&request(), &CancellationToken::new())
            .await
            .unwrap()
            .message
            .content
            .as_deref(),
        Some("你好🌍")
    );
    let mut visible = String::new();
    let reply = f
        .client
        .stream(&request(), &CancellationToken::new(), |text| {
            visible.push_str(&text);
            async { Ok(()) }
        })
        .await
        .unwrap();
    assert_eq!(visible, "你好🌍");
    assert_eq!(reply.usage.unwrap().total_tokens, 12);
    let requests = f.wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for (uri, headers, body) in requests.iter() {
        assert_eq!(headers["x-goog-api-key"], "test-google-only-key");
        assert!(!headers.contains_key("authorization"));
        assert!(!uri.to_string().contains("key"));
        assert!(body.get("stream").is_none());
        assert!(body.get("model").is_none());
    }
    assert_eq!(
        requests[0].0.path(),
        "/proxy/v1beta/models/arbitrary-model:generateContent"
    );
    assert_eq!(
        requests[1].0.path(),
        "/proxy/v1beta/models/arbitrary-model:streamGenerateContent"
    );
    assert_eq!(requests[1].0.query(), Some("alt=sse"));
}
#[tokio::test]
async fn http_error_limits_and_truncated_sse_are_not_retried() {
    let events = format!(
        "data: {}\n\n",
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"partial"}]}}]})
    );
    let unfinished = format!(
        "data: {}\n\ndata: {{",
        response(json!([{"text":"visible"}]))
    );
    let f = Fixture::new(
        vec![
            Reply::Http(429),
            Reply::Sse(events),
            Reply::Sse(unfinished),
            Reply::Json(response(json!([{"text":"x".repeat(5000)}]))),
        ],
        3,
        1024,
    )
    .await;
    assert!(matches!(
        f.client
            .complete(&request(), &CancellationToken::new())
            .await,
        Err(ModelError::Http(429))
    ));
    for _ in 0..2 {
        assert!(matches!(
            f.client
                .stream(&request(), &CancellationToken::new(), |_| async { Ok(()) })
                .await,
            Err(ModelError::Incomplete)
        ));
    }
    assert!(matches!(
        f.client
            .complete(&request(), &CancellationToken::new())
            .await,
        Err(ModelError::ResponseTooLarge)
    ));
    assert_eq!(f.wire.requests.lock().unwrap().len(), 4);
}
#[tokio::test]
async fn timeout_and_cancellation_keep_the_existing_request_boundary() {
    let f = Fixture::new(vec![Reply::Hold, Reply::Hold], 1, 65536).await;
    assert!(matches!(
        f.client
            .complete(&request(), &CancellationToken::new())
            .await,
        Err(ModelError::Timeout)
    ));
    // Consume the first request notification before waiting for the second in-flight request.
    f.wire.received.notified().await;
    let cancel = CancellationToken::new();
    let request = request();
    let call = f.client.complete(&request, &cancel);
    let stop = async {
        f.wire.received.notified().await;
        cancel.cancel();
    };
    let (result, _) = tokio::join!(call, stop);
    assert!(matches!(result, Err(ModelError::Cancelled)));
    assert_eq!(f.wire.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn local_summary_validates_commits_and_continues_with_original_history() {
    let f = Fixture::new(
        vec![
            Reply::Json(response(json!([{"text":"invalid summary"}]))),
            Reply::Summary,
            Reply::Json(response(json!([{"text":"端口为 12233"}]))),
        ],
        3,
        65536,
    )
    .await;
    let repo = Repository::new(f.store.clone());
    let session = repo.create_general_session("model", "test").unwrap();
    let user = repo
        .append(
            &session.id,
            "test",
            0,
            "seed",
            Payload::UserMessage {
                text: "记住端口为 12233".into(),
            },
        )
        .unwrap()
        .event;
    let summarizer = Summarizer::new(f.store.clone());
    assert!(summarizer
        .summarize(&session.id, "test", &CancellationToken::new())
        .await
        .is_err());
    assert!(repo.checkpoint(&session.id, "test").unwrap().is_none());
    let draft = summarizer
        .summarize(&session.id, "test", &CancellationToken::new())
        .await
        .unwrap();
    assert!(draft.content.summary.contains("12233"));
    assert!(draft.content.evidence_event_ids.contains(&user.id));
    repo.save_checkpoint(&draft.snapshot, draft.content)
        .unwrap();
    let manager = Manager::new(f.store.clone(), CancellationToken::new());
    manager
        .chat(
            Request {
                agent_id: "model".into(),
                user_id: "test".into(),
                session_id: session.id.clone(),
                request_id: "after-compact".into(),
                terminal_session_id: String::new(),
                message: "端口是多少？".into(),
                attachments: vec![],
            },
            None,
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    assert_eq!(repo.events(&session.id, "test", 0, 100).unwrap()[0], user);
    let requests = f.wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[0].2.get("tools").is_none());
    assert!(requests[1].2.get("tools").is_none());
    assert!(requests[2].2.to_string().contains("12233"));
}
#[tokio::test]
async fn saved_google_profile_and_encrypted_key_survive_database_reopen() {
    let response = Reply::Json(response(json!([{"text":"reopened"}])));
    let f = Fixture::new(vec![response.clone(), response], 3, 65536).await;
    let models = ModelRepository::new(f.store.clone());
    let profile = models.get("model", "test").unwrap();
    models
        .save_channel(
            "model",
            "test",
            profile.revision,
            ModelConfigUpdate {
                config: profile.config,
                api_key: KeyChange::Keep,
            },
            "Google",
            vec!["models/arbitrary-model".into(), "alternate-model".into()],
        )
        .unwrap();
    let reopened = Arc::new(Store::open(&f.directory.path().join("google.sqlite")).unwrap());
    let repository = ModelRepository::new(reopened);
    let channel = repository.channels("test").unwrap().remove(0);
    assert_eq!(channel.models.len(), 2);
    for member in &channel.models {
        let profile = repository.get(&member.id, "test").unwrap();
        assert_eq!(profile.config.provider, ProviderKind::GoogleGemini);
        assert!(profile.api_key_configured);
        assert!(!serde_json::to_string(&profile)
            .unwrap()
            .contains("test-google-only-key"));
        let client = ModelClient::new(repository.runtime(&member.id, "test").unwrap()).unwrap();
        assert_eq!(
            client
                .complete(&request(), &CancellationToken::new())
                .await
                .unwrap()
                .message
                .content
                .as_deref(),
            Some("reopened")
        );
        assert!(repository.runtime(&member.id, "other-user").is_err());
    }
    let requests = f.wire.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for (request, member) in requests.iter().zip(&channel.models) {
        assert!(request
            .0
            .path()
            .ends_with(&format!("/{}:generateContent", member.model)));
        assert_eq!(request.1["x-goog-api-key"], "test-google-only-key");
    }
}
