use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode, Uri},
    response::Response,
    routing::post,
    Router,
};
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{provider::*, Repository},
    store::Store,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

const TEST_KEY: &str = "unit-test-model-key";
fn config() -> ModelConfig {
    ModelConfig {
        provider: ProviderKind::OpenAiCompatible,
        remote_compaction: false,
        base_url: "https://model.example/v1/".into(),
        completions_path: "chat/completions".into(),
        model: "test-model".into(),
        context_window: 8192,
        output_reserve: 512,
        safety_reserve: 256,
        compact_percent: 75,
        output_token_parameter: OutputTokenParameter::MaxCompletionTokens,
        capabilities: ModelCapabilities::default(),
        temperature: None,
        reasoning_effort: None,
        timeout_seconds: 5,
        max_response_bytes: 65536,
        require_api_key: false,
    }
}
fn update(config: ModelConfig, api_key: KeyChange) -> ModelConfigUpdate {
    ModelConfigUpdate { config, api_key }
}
fn request() -> ChatRequest {
    ChatRequest {
        messages: vec![ChatMessage::User {
            content: "你好，请只返回文字".into(),
        }],
        tools: vec![],
    }
}
fn tool() -> FunctionDefinition {
    FunctionDefinition {
        name: "read_output".into(),
        description: "Read bounded output".into(),
        parameters: json!({"type":"object","properties":{"id":{"type":"string"}},"required":["id"],"additionalProperties":false}),
    }
}
fn call() -> ToolCall {
    ToolCall {
        id: "call-1".into(),
        kind: "function".into(),
        function: FunctionCall {
            name: "read_output".into(),
            arguments: "{\"id\":\"output-1\"}".into(),
        },
    }
}
fn reply() -> Value {
    json!({"choices":[{"index":0,"message":{"role":"assistant","content":"你好"},"finish_reason":"stop"}],
        "usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":12}})
}

#[test]
fn api_url_normalization_preserves_prefix_and_rejects_ambiguous_endpoints() {
    let mut c = config();
    c.base_url = "https://model.example/proxy/v1".into();
    c.completions_path = "/chat/completions".into();
    let c = c.normalized().unwrap();
    assert_eq!(
        c.endpoint().unwrap().as_str(),
        "https://model.example/proxy/v1/chat/completions"
    );
    for path in [
        "https://evil.example/path",
        "//evil.example/path",
        "../chat",
        "a/../b",
        "a//b",
        "a?key=secret",
        "a#fragment",
        "a%2fb",
        "a\\b",
    ] {
        let mut c = config();
        c.completions_path = path.into();
        assert!(
            matches!(c.normalized(), Err(ModelError::Invalid(_))),
            "{path}"
        );
    }
    for base in [
        "file:///tmp/key",
        "https://user:secret@model.example",
        "https://model.example?key=secret",
        "https://model.example/#x",
        "https://model.example/%2f",
    ] {
        let mut c = config();
        c.base_url = base.into();
        assert!(
            matches!(c.normalized(), Err(ModelError::Invalid(_))),
            "{base}"
        );
    }
}
#[test]
fn model_limits_and_capabilities_are_checked_before_sampling() {
    for change in [0, 1, 2, 3, 4, 5, 6] {
        let mut c = config();
        match change {
            0 => c.model.clear(),
            1 => c.context_window = 128,
            2 => c.output_reserve = c.context_window,
            3 => c.compact_percent = 100,
            4 => c.temperature = Some(f64::NAN),
            5 => {
                c.temperature = Some(0.5);
                c.capabilities.temperature = false;
            }
            _ => c.reasoning_effort = Some(ReasoningEffort::High),
        }
        assert!(matches!(c.normalized(), Err(ModelError::Invalid(_))));
    }
    let mut c = config();
    c.capabilities.reasoning_effort = true;
    c.reasoning_effort = Some(ReasoningEffort::High);
    c.output_token_parameter = OutputTokenParameter::MaxTokens;
    let body: Value = serde_json::from_str(&prepare_request(&c, &request()).unwrap().body).unwrap();
    assert_eq!(body["max_tokens"], 512);
    assert!(body.get("max_completion_tokens").is_none());
    assert_eq!(body["reasoning_effort"], "high");
    assert!(body.get("temperature").is_none());
    let mut r = request();
    r.messages.insert(
        0,
        ChatMessage::Developer {
            content: "trusted".into(),
        },
    );
    assert!(matches!(
        prepare_request(&c, &r),
        Err(ModelError::Invalid(_))
    ));
    c.capabilities.developer_role = true;
    assert!(prepare_request(&c, &r).is_ok());
}
#[test]
fn incomplete_duplicate_or_unknown_tool_history_is_rejected() {
    let mut c = config();
    c.capabilities.tool_calls = true;
    let mut r = request();
    r.tools.push(tool());
    r.messages.push(ChatMessage::Assistant {
        content: None,
        tool_calls: vec![call()],
    });
    assert!(matches!(
        prepare_request(&c, &r),
        Err(ModelError::ToolProtocol)
    ));
    r.messages.push(ChatMessage::Tool {
        tool_call_id: "call-1".into(),
        content: "evidence".into(),
    });
    let prepared = prepare_request(&c, &r).unwrap();
    let body: Value = serde_json::from_str(&prepared.body).unwrap();
    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(
        body["messages"][1]["tool_calls"][0]["function"]["name"],
        "read_output"
    );
    r.messages.push(ChatMessage::Tool {
        tool_call_id: "call-1".into(),
        content: "duplicate".into(),
    });
    assert!(matches!(
        prepare_request(&c, &r),
        Err(ModelError::ToolProtocol)
    ));
    r.messages.pop();
    r.messages.push(ChatMessage::Assistant {
        content: None,
        tool_calls: vec![call()],
    });
    assert!(matches!(
        prepare_request(&c, &r),
        Err(ModelError::ToolProtocol)
    ));
    c.capabilities.tool_calls = false;
    assert!(matches!(
        prepare_request(&c, &r),
        Err(ModelError::Invalid(_))
    ));
}
#[test]
fn oversized_context_is_not_silently_truncated() {
    let mut c = config();
    c.context_window = 1024;
    c.output_reserve = 256;
    c.safety_reserve = 64;
    let mut r = request();
    r.messages[0] = ChatMessage::User {
        content: "证据".repeat(1000),
    };
    assert!(matches!(
        prepare_request(&c, &r),
        Err(ModelError::BudgetExceeded)
    ));
    r.messages[0] = ChatMessage::User {
        content: "x".repeat(1450),
    };
    let p = prepare_request(&c, &r).unwrap();
    assert!(p.should_compact);
    assert!(p.body.contains(&"x".repeat(1450)));
}
#[test]
fn profiles_are_owner_scoped_and_saved_with_revision_checks() {
    let repo = ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    let mut c = config();
    c.require_api_key = true;
    let p = repo
        .save("profile", "alice", 0, update(c.clone(), KeyChange::Keep))
        .unwrap();
    assert_eq!(p.revision, 1);
    assert!(!p.ready);
    assert!(!p.api_key_configured);
    assert!(matches!(
        repo.runtime("profile", "alice"),
        Err(ModelError::KeyMissing)
    ));
    assert!(matches!(
        repo.get("profile", "bob"),
        Err(ModelError::NotFound)
    ));
    assert!(matches!(
        repo.delete("profile", "bob", 1),
        Err(ModelError::NotFound)
    ));
    assert!(matches!(
        repo.save("profile", "alice", 0, update(c.clone(), KeyChange::Keep)),
        Err(ModelError::Stale)
    ));
    c.require_api_key = false;
    let p = repo
        .save("profile", "alice", 1, update(c, KeyChange::Clear))
        .unwrap();
    assert_eq!(p.revision, 2);
    assert!(p.ready);
    assert!(repo.runtime("profile", "alice").is_ok());
    assert_eq!(repo.list("alice").unwrap().len(), 1);
    assert!(repo.list("bob").unwrap().is_empty());
    assert!(matches!(
        repo.delete("profile", "alice", 1),
        Err(ModelError::Stale)
    ));
    repo.delete("profile", "alice", 2).unwrap();
    assert!(repo.list("alice").unwrap().is_empty());
}
#[test]
fn profile_limit_and_secret_input_validation_are_enforced() {
    let repo = ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    for id in 0..32 {
        repo.save(
            &id.to_string(),
            "alice",
            0,
            update(config(), KeyChange::Keep),
        )
        .unwrap();
    }
    assert!(matches!(
        repo.save("overflow", "alice", 0, update(config(), KeyChange::Keep)),
        Err(ModelError::Invalid(_))
    ));
    assert!(repo
        .save("overflow", "bob", 0, update(config(), KeyChange::Keep))
        .is_ok());
    for key in ["", "key with spaces", "key\r\nheader"] {
        assert!(SecretKey::new(key).is_err());
    }
    assert!(!format!("{:?}", SecretKey::new(TEST_KEY).unwrap()).contains(TEST_KEY));
    // Deserializing an IPC-shaped update cannot bypass the native validation.
    let change: KeyChange =
        serde_json::from_value(json!({"action":"set","value":"bad\nkey"})).unwrap();
    assert!(matches!(
        repo.save("bad", "bob", 0, update(config(), change)),
        Err(ModelError::Invalid(_))
    ));
}
#[test]
fn schema_upgrade_preserves_existing_agent_sessions() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("upgrade.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(include_str!("../migrations/001_connections.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/002_files.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/003_agent_context.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/004_agent_outputs.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/005_agent_models.sql"))
        .unwrap();
    db.execute_batch(include_str!("../migrations/006_agent_turns.sql"))
        .unwrap();
    db.execute("INSERT INTO agent_model_profiles(id,user_id,revision,config_json,updated_at) VALUES('legacy-profile','alice',3,?1,0)", [serde_json::to_string(&config()).unwrap()]).unwrap();
    db.execute("INSERT INTO agent_sessions(id,user_id,agent_id,created_at) VALUES('existing','alice','ssh-agent',0)",[]).unwrap();
    drop(db);
    let store = Arc::new(Store::open(&path).unwrap());
    let legacy = ModelRepository::new(store.clone())
        .get("legacy-profile", "alice")
        .unwrap();
    assert_eq!(legacy.display_name, "legacy-profile");
    assert_eq!(legacy.revision, 3);
    assert_eq!(legacy.config.model, "test-model");
    assert_eq!(
        Repository::new(store.clone())
            .session("existing", "alice")
            .unwrap()
            .revision,
        0
    );
    ModelRepository::new(store)
        .save("model", "alice", 0, update(config(), KeyChange::Keep))
        .unwrap();
    let db = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        db.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        13
    );
    assert_eq!(
        db.query_row("SELECT count(*) FROM agent_sessions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn profile_names_preserve_identity_sessions_and_survive_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("names.sqlite");
    let store = Arc::new(Store::open(&path).unwrap());
    let repo = ModelRepository::new(store.clone());
    let original = repo
        .save(
            "legacy-model",
            "alice",
            0,
            update(config(), KeyChange::Keep),
        )
        .unwrap();
    assert_eq!(original.display_name, "legacy-model");
    let session = Repository::new(store.clone())
        .create_session("legacy-model", "alice")
        .unwrap();
    let renamed = repo
        .save_named(
            "legacy-model",
            "alice",
            1,
            update(config(), KeyChange::Keep),
            Some(" 日常问答 "),
        )
        .unwrap();
    assert_eq!(renamed.id, original.id);
    assert_eq!(renamed.display_name, "日常问答");
    assert_eq!(renamed.revision, 2);
    assert_eq!(repo.list("alice").unwrap().len(), 1);
    assert_eq!(
        Repository::new(store.clone())
            .session(&session.id, "alice")
            .unwrap()
            .agent_id,
        original.id
    );
    assert!(matches!(
        repo.save_named(
            "legacy-model",
            "alice",
            1,
            update(config(), KeyChange::Keep),
            Some("过期修改")
        ),
        Err(ModelError::Stale)
    ));
    for invalid in ["", "\n", "bad\nname"] {
        assert!(matches!(
            repo.save_named(
                "legacy-model",
                "alice",
                2,
                update(config(), KeyChange::Keep),
                Some(invalid)
            ),
            Err(ModelError::Invalid(_))
        ));
    }
    let saved = repo
        .save(
            "legacy-model",
            "alice",
            2,
            update(config(), KeyChange::Keep),
        )
        .unwrap();
    assert_eq!(saved.display_name, "日常问答"); // Older callers cannot reset the label.
    assert!(repo.list("bob").unwrap().is_empty());
    drop(repo);
    drop(store);
    let reopened = ModelRepository::new(Arc::new(Store::open(&path).unwrap()));
    assert_eq!(
        reopened.get("legacy-model", "alice").unwrap().display_name,
        "日常问答"
    );
    reopened.delete("legacy-model", "alice", 3).unwrap();
    let recreated = reopened
        .save(
            "legacy-model",
            "alice",
            0,
            update(config(), KeyChange::Keep),
        )
        .unwrap();
    assert_eq!(recreated.display_name, "legacy-model");
}

#[cfg(windows)]
#[test]
fn renaming_profile_keeps_encrypted_key_usable() {
    let store = Arc::new(Store::in_memory().unwrap());
    let repo = ModelRepository::new(store);
    let saved = repo
        .save(
            "stable-id",
            "alice",
            0,
            update(
                config(),
                KeyChange::Set {
                    value: SecretKey::new(TEST_KEY).unwrap(),
                },
            ),
        )
        .unwrap();
    let renamed = repo
        .save_named(
            &saved.id,
            "alice",
            saved.revision,
            update(config(), KeyChange::Keep),
            Some("新名称"),
        )
        .unwrap();
    assert!(renamed.api_key_configured);
    assert!(!serde_json::to_string(&renamed).unwrap().contains(TEST_KEY));
    // Runtime decrypts with the original ID-bound entropy after renaming.
    assert!(ModelClient::new(repo.runtime("stable-id", "alice").unwrap()).is_ok());
}
#[cfg(windows)]
#[test]
fn encrypted_keys_survive_restart_without_becoming_public_and_require_explicit_destination_change()
{
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("keys.sqlite");
    let repo = ModelRepository::new(Arc::new(Store::open(&path).unwrap()));
    let mut c = config();
    c.require_api_key = true;
    let p = repo
        .save(
            "model",
            "alice",
            0,
            update(
                c.clone(),
                KeyChange::Set {
                    value: SecretKey::new(TEST_KEY).unwrap(),
                },
            ),
        )
        .unwrap();
    assert!(p.api_key_configured && p.ready);
    assert!(!serde_json::to_string(&p).unwrap().contains(TEST_KEY));
    assert!(!format!("{:?}", repo.runtime("model", "alice").unwrap()).contains(TEST_KEY));
    let db = rusqlite::Connection::open(&path).unwrap();
    let (metadata, key): (String, Vec<u8>) = db
        .query_row(
            "SELECT config_json,api_key FROM agent_model_profiles",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert!(!metadata.contains(TEST_KEY));
    assert!(!key
        .windows(TEST_KEY.len())
        .any(|s| s == TEST_KEY.as_bytes()));
    drop(repo);
    let repo = ModelRepository::new(Arc::new(Store::open(&path).unwrap()));
    assert!(repo.runtime("model", "alice").is_ok());
    c.model = "second-model".into();
    repo.save("model", "alice", 1, update(c.clone(), KeyChange::Keep))
        .unwrap();
    c.base_url = "https://different.example/v1".into();
    assert!(matches!(
        repo.save("model", "alice", 2, update(c.clone(), KeyChange::Keep)),
        Err(ModelError::Invalid(_))
    ));
    assert_eq!(repo.get("model", "alice").unwrap().revision, 2);
    let p = repo
        .save("model", "alice", 2, update(c, KeyChange::Clear))
        .unwrap();
    assert!(!p.api_key_configured && !p.ready);
    assert!(matches!(
        repo.runtime("model", "alice"),
        Err(ModelError::KeyMissing)
    ));
}
#[cfg(windows)]
#[test]
fn copying_or_corrupting_key_ciphertext_does_not_reassign_credentials() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("keys.sqlite");
    let repo = ModelRepository::new(Arc::new(Store::open(&path).unwrap()));
    repo.save(
        "a",
        "alice",
        0,
        update(
            config(),
            KeyChange::Set {
                value: SecretKey::new(TEST_KEY).unwrap(),
            },
        ),
    )
    .unwrap();
    repo.save("b", "alice", 0, update(config(), KeyChange::Keep))
        .unwrap();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute("UPDATE agent_model_profiles SET api_key=(SELECT api_key FROM agent_model_profiles WHERE id='a') WHERE id='b'",[]).unwrap();
    assert!(matches!(
        repo.runtime("b", "alice"),
        Err(ModelError::Credentials)
    ));
    db.execute(
        "UPDATE agent_model_profiles SET api_key=x'010203' WHERE id='a'",
        [],
    )
    .unwrap();
    assert!(matches!(
        repo.runtime("a", "alice"),
        Err(ModelError::Credentials)
    ));
}

#[derive(Clone)]
struct MockState {
    requests: Arc<Mutex<Vec<(String, HeaderMap, Value)>>>,
    body: String,
    status: StatusCode,
    delay: Duration,
    location: Option<String>,
    stream_body: bool,
}
struct MockModel {
    base: String,
    state: MockState,
    handle: tokio::task::JoinHandle<()>,
}
impl Drop for MockModel {
    fn drop(&mut self) {
        self.handle.abort();
    }
}
async fn handler(
    State(state): State<MockState>,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    state.requests.lock().unwrap().push((
        uri.to_string(),
        headers,
        serde_json::from_slice(&body).unwrap(),
    ));
    if !state.stream_body {
        tokio::time::sleep(state.delay).await;
    }
    let mut response = Response::builder()
        .status(state.status)
        .header("content-type", "application/json");
    if let Some(location) = &state.location {
        response = response.header("location", location);
    }
    let body = if state.stream_body {
        let bytes = Bytes::from(state.body);
        let middle = bytes.len() / 2;
        Body::from_stream(async_stream::stream! {
            yield Ok::<_,std::io::Error>(bytes.slice(..middle));
            tokio::time::sleep(state.delay).await;
            yield Ok::<_,std::io::Error>(bytes.slice(middle..));
        })
    } else {
        Body::from(state.body)
    };
    response.body(body).unwrap()
}
impl MockModel {
    async fn start(
        body: String,
        status: StatusCode,
        delay: Duration,
        location: Option<String>,
    ) -> Self {
        let state = MockState {
            requests: Arc::new(Mutex::new(vec![])),
            body,
            status,
            delay,
            location,
            stream_body: false,
        };
        Self::serve(state).await
    }
    async fn chunked(body: String, delay: Duration) -> Self {
        Self::serve(MockState {
            requests: Arc::new(Mutex::new(vec![])),
            body,
            status: StatusCode::OK,
            delay,
            location: None,
            stream_body: true,
        })
        .await
    }
    async fn serve(state: MockState) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/v1/chat/completions", post(handler))
            .with_state(state.clone());
        let handle = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            base,
            state,
            handle,
        }
    }
    fn client(&self, mut c: ModelConfig) -> ModelClient {
        c.base_url = self.base.clone();
        let repo = ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
        repo.save("model", "alice", 0, update(c, KeyChange::Keep))
            .unwrap();
        ModelClient::new(repo.runtime("model", "alice").unwrap()).unwrap()
    }
    fn count(&self) -> usize {
        self.state.requests.lock().unwrap().len()
    }
}
#[tokio::test]
async fn chunked_response_without_length_still_obeys_size_and_total_read_timeout() {
    let mock = MockModel::chunked("x".repeat(2048), Duration::from_millis(10)).await;
    let mut c = config();
    c.max_response_bytes = 1024;
    assert!(matches!(
        mock.client(c)
            .complete(&request(), &CancellationToken::new())
            .await,
        Err(ModelError::ResponseTooLarge)
    ));
    let mock = MockModel::chunked(reply().to_string(), Duration::from_secs(10)).await;
    let mut c = config();
    c.timeout_seconds = 1;
    assert!(matches!(
        mock.client(c)
            .complete(&request(), &CancellationToken::new())
            .await,
        Err(ModelError::Timeout)
    ));
    assert_eq!(mock.count(), 1);
}
#[tokio::test]
async fn local_model_request_and_usage_round_trip() {
    let mock = MockModel::start(reply().to_string(), StatusCode::OK, Duration::ZERO, None).await;
    let result = mock
        .client(config())
        .complete(&request(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.message.content.as_deref(), Some("你好"));
    assert_eq!(result.usage.unwrap().total_tokens, 12);
    let requests = mock.state.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].0, "/v1/chat/completions");
    assert!(requests[0].1.get("authorization").is_none());
    assert_eq!(requests[0].2["model"], "test-model");
    assert_eq!(requests[0].2["stream"], false);
    assert_eq!(requests[0].2["max_completion_tokens"], 512);
}
#[cfg(windows)]
#[tokio::test]
async fn native_credentials_are_sent_as_bearer_header_only() {
    let mock = MockModel::start(reply().to_string(), StatusCode::OK, Duration::ZERO, None).await;
    let mut c = config();
    c.base_url = mock.base.clone();
    c.require_api_key = true;
    let repo = ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
    let profile = repo
        .save(
            "model",
            "alice",
            0,
            update(
                c,
                KeyChange::Set {
                    value: SecretKey::new(TEST_KEY).unwrap(),
                },
            ),
        )
        .unwrap();
    assert!(!serde_json::to_string(&profile).unwrap().contains(TEST_KEY));
    ModelClient::new(repo.runtime("model", "alice").unwrap())
        .unwrap()
        .complete(&request(), &CancellationToken::new())
        .await
        .unwrap();
    let requests = mock.state.requests.lock().unwrap();
    assert_eq!(requests[0].1["authorization"], format!("Bearer {TEST_KEY}"));
    assert!(!requests[0].2.to_string().contains(TEST_KEY));
}
#[tokio::test]
async fn model_function_calls_are_validated_without_executing_a_tool() {
    let mut body = reply();
    body["choices"][0]["message"] =
        json!({"role":"assistant","content":null,"tool_calls":[call()]});
    body["choices"][0]["finish_reason"] = "tool_calls".into();
    let mock = MockModel::start(body.to_string(), StatusCode::OK, Duration::ZERO, None).await;
    let mut c = config();
    c.capabilities.tool_calls = true;
    let mut r = request();
    r.tools.push(tool());
    let result = mock
        .client(c.clone())
        .complete(&r, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result.message.tool_calls[0].function.name, "read_output");
    assert!(matches!(
        mock.client(c)
            .complete(&request(), &CancellationToken::new())
            .await,
        Err(ModelError::ToolProtocol)
    ));
}
#[tokio::test]
async fn malformed_filtered_or_truncated_model_response_is_not_success() {
    let mut length = reply();
    length["choices"][0]["finish_reason"] = "length".into();
    let mut filtered = reply();
    filtered["choices"][0]["finish_reason"] = "content_filter".into();
    let mut usage = reply();
    usage["usage"]["total_tokens"] = 1.into();
    for body in [
        "{".into(),
        json!({"choices":[]}).to_string(),
        length.to_string(),
        filtered.to_string(),
        usage.to_string(),
    ] {
        let mock = MockModel::start(body, StatusCode::OK, Duration::ZERO, None).await;
        let result = mock
            .client(config())
            .complete(&request(), &CancellationToken::new())
            .await;
        assert!(matches!(
            result,
            Err(ModelError::Response | ModelError::Incomplete)
        ));
    }
}
#[tokio::test]
async fn provider_errors_are_redacted_and_not_retried() {
    let mock = MockModel::start(
        format!("secret: {TEST_KEY}"),
        StatusCode::TOO_MANY_REQUESTS,
        Duration::ZERO,
        None,
    )
    .await;
    let error = mock
        .client(config())
        .complete(&request(), &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(matches!(error, ModelError::Http(429)));
    assert!(!format!("{error:?} {error}").contains(TEST_KEY));
    assert_eq!(mock.count(), 1);
}
#[tokio::test]
async fn redirect_is_not_followed_to_another_model_endpoint() {
    let destination =
        MockModel::start(reply().to_string(), StatusCode::OK, Duration::ZERO, None).await;
    let mock = MockModel::start(
        String::new(),
        StatusCode::TEMPORARY_REDIRECT,
        Duration::ZERO,
        Some(format!("{}chat/completions", destination.base)),
    )
    .await;
    assert!(matches!(
        mock.client(config())
            .complete(&request(), &CancellationToken::new())
            .await,
        Err(ModelError::Http(307))
    ));
    assert_eq!(mock.count(), 1);
    assert_eq!(destination.count(), 0);
}
#[tokio::test]
async fn cancellation_prevents_or_interrupts_local_model_request() {
    let mock = MockModel::start(
        reply().to_string(),
        StatusCode::OK,
        Duration::from_secs(10),
        None,
    )
    .await;
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(matches!(
        mock.client(config()).complete(&request(), &cancel).await,
        Err(ModelError::Cancelled)
    ));
    assert_eq!(mock.count(), 0);
    let cancel = CancellationToken::new();
    let client = mock.client(config());
    let r = request();
    let operation = client.complete(&r, &cancel);
    let cancellation = async {
        tokio::time::timeout(Duration::from_secs(3), async {
            while mock.count() == 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        cancel.cancel();
    };
    let (result, ()) = tokio::join!(operation, cancellation);
    assert!(matches!(result, Err(ModelError::Cancelled)));
    assert_eq!(mock.count(), 1);
}
#[tokio::test]
async fn total_timeout_and_response_size_are_bounded() {
    let mock = MockModel::start(
        reply().to_string(),
        StatusCode::OK,
        Duration::from_secs(10),
        None,
    )
    .await;
    let mut c = config();
    c.timeout_seconds = 1;
    assert!(matches!(
        mock.client(c)
            .complete(&request(), &CancellationToken::new())
            .await,
        Err(ModelError::Timeout)
    ));
    assert_eq!(mock.count(), 1);
    let mock = MockModel::start("x".repeat(2048), StatusCode::OK, Duration::ZERO, None).await;
    let mut c = config();
    c.max_response_bytes = 1024;
    assert!(matches!(
        mock.client(c)
            .complete(&request(), &CancellationToken::new())
            .await,
        Err(ModelError::ResponseTooLarge)
    ));
}
