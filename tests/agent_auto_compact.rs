use axum::{body::Body, extract::State, routing::post, Json, Router};
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        chat::{self, Manager, Request, StreamEvent},
        compact::auto::check_after_tool_batch,
        provider::*,
        *,
    },
    store::Store,
};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy)]
enum Mode {
    Good,
    Invalid,
    Long,
    Http,
}
struct Mock {
    requests: Mutex<Vec<Value>>,
    mode: Mutex<Mode>,
    answer: Mutex<String>,
    hold: AtomicBool,
    arrived: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
async fn handler(
    State(mock): State<Arc<Mock>>,
    Json(body): Json<Value>,
) -> axum::response::Response {
    mock.requests.lock().unwrap().push(body.clone());
    assert!(body.get("tools").is_none());
    if body["stream"] == true {
        let answer = mock.answer.lock().unwrap().clone();
        let data = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            json!({"choices":[{"index":0,"delta":{"content":answer},"finish_reason":null}]}),
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]})
        );
        return axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from(data))
            .unwrap();
    }
    mock.arrived.notify_one();
    if mock.hold.load(Ordering::SeqCst) {
        mock.release.notified().await;
    }
    let mode = *mock.mode.lock().unwrap();
    if matches!(mode, Mode::Http) {
        return axum::response::Response::builder()
            .status(503)
            .body(Body::from("provider-private-body"))
            .unwrap();
    }
    let source: Value =
        serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
    let ids = source["availableEvidenceEventIds"].as_array().unwrap();
    let summary = match mode {
        Mode::Invalid => "{private-invalid".into(),
        Mode::Long => "x".repeat(4000),
        _ => "先前对话讨论日志分析；保留原目标与约束，尚未执行命令。".into(),
    };
    let content = if matches!(mode, Mode::Invalid) {
        summary
    } else {
        json!({"summary":summary,"nextSteps":["继续只读分析"],"evidenceEventIds":[ids.last().unwrap()]}).to_string()
    };
    let reply = json!({"choices":[{"index":0,"message":{"role":"assistant","content":content},"finish_reason":"stop"}]});
    axum::response::Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(reply.to_string()))
        .unwrap()
}
struct Fixture {
    store: Arc<Store>,
    repo: Repository,
    manager: Arc<Manager>,
    session: String,
    mock: Arc<Mock>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        self.manager.shutdown();
    }
}
impl Fixture {
    async fn new() -> Self {
        Self::with_store(Arc::new(Store::in_memory().unwrap())).await
    }
    async fn with_store(store: Arc<Store>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let mock = Arc::new(Mock {
            requests: Mutex::new(vec![]),
            mode: Mutex::new(Mode::Good),
            answer: Mutex::new("a".repeat(1000)),
            hold: AtomicBool::new(false),
            arrived: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Router::new()
            .route("/v1/chat/completions", post(handler))
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        ModelRepository::new(store.clone())
            .save(
                "model",
                "default",
                0,
                ModelConfigUpdate {
                    config: ModelConfig {
                        provider: ProviderKind::OpenAiCompatible,
                        remote_compaction: false,
                        base_url: base,
                        completions_path: "chat/completions".into(),
                        model: "test-text".into(),
                        context_window: 32768,
                        output_reserve: 512,
                        safety_reserve: 256,
                        compact_percent: 75,
                        output_token_parameter: OutputTokenParameter::MaxCompletionTokens,
                        capabilities: ModelCapabilities::default(),
                        temperature: None,
                        reasoning_effort: None,
                        timeout_seconds: 3,
                        max_response_bytes: 262144,
                        require_api_key: false,
                    },
                    api_key: KeyChange::Keep,
                },
            )
            .unwrap();
        let repo = Repository::new(store.clone());
        let manager = Arc::new(Manager::new(store.clone(), CancellationToken::new()));
        let session = manager
            .create_session("model".into(), "default".into())
            .await
            .unwrap();
        Self {
            store,
            repo,
            manager,
            session,
            mock,
            server,
        }
    }
    fn request(&self, message: &str) -> Request {
        Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "model".into(),
            user_id: "default".into(),
            session_id: self.session.clone(),
            terminal_session_id: String::new(),
            message: message.into(),
            attachments: vec![],
        }
    }
    async fn chat(&self, request: Request) -> chat::Result<Vec<Value>> {
        let mut frames = Vec::new();
        self.manager
            .chat(request, None, &CancellationToken::new(), |event| {
                frames.push(serde_json::to_value(event).unwrap());
                async { Ok(()) }
            })
            .await?;
        Ok(frames)
    }
    async fn seed(&self) {
        self.chat(self.request("分析日志，不要重启服务"))
            .await
            .unwrap();
        for _ in 0..7 {
            self.chat(self.request(&"u".repeat(1000))).await.unwrap();
        }
        assert!(self
            .repo
            .checkpoint(&self.session, "default")
            .unwrap()
            .is_none());
        self.mock.requests.lock().unwrap().clear();
    }
    fn config(&self, window: u64, threshold: u8) {
        let repo = ModelRepository::new(self.store.clone());
        let p = repo.get("model", "default").unwrap();
        let mut config = p.config;
        config.context_window = window;
        config.compact_percent = threshold;
        repo.save(
            "model",
            "default",
            p.revision,
            ModelConfigUpdate {
                config,
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
    }
    fn summaries(&self) -> usize {
        self.mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["stream"] == false)
            .count()
    }
    fn streams(&self) -> usize {
        self.mock
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r["stream"] == true)
            .count()
    }
    fn events(&self) -> Vec<Event> {
        self.repo.events(&self.session, "default", 0, 200).unwrap()
    }
    fn snapshot(&self) -> Snapshot {
        self.repo.snapshot(&self.session, "default").unwrap()
    }
}

#[tokio::test]
async fn normal_chat_does_not_summarize_or_create_a_checkpoint() {
    let f = Fixture::new().await;
    let request = f.request("hello");
    assert_eq!(
        f.chat(request).await.unwrap().last().unwrap()["event"],
        "done"
    );
    assert_eq!(f.summaries(), 0);
    assert_eq!(f.streams(), 1);
    assert!(f.repo.checkpoint(&f.session, "default").unwrap().is_none());
}
#[tokio::test]
async fn threshold_compacts_only_complete_prefix_and_keeps_recent_turns_latest_input_and_raw_history(
) {
    let f = Fixture::new().await;
    f.seed().await;
    f.config(8192, 60);
    let original = f.events();
    let request = f.request("最新问题必须原样保留");
    let frames = f.chat(request.clone()).await.unwrap();
    assert_eq!(frames.last().unwrap()["event"], "done");
    assert_eq!(f.summaries(), 1);
    assert_eq!(f.streams(), 1);
    let checkpoint = f.repo.checkpoint(&f.session, "default").unwrap().unwrap();
    assert!(checkpoint.through_event_id < original.last().unwrap().id);
    assert!(matches!(
        original
            .iter()
            .find(|e| e.id == checkpoint.through_event_id)
            .unwrap()
            .payload,
        Payload::AssistantMessage { .. }
    ));
    assert_eq!(&f.events()[..original.len()], &original);
    {
        let requests = f.mock.requests.lock().unwrap();
        let chat = requests.last().unwrap();
        assert_eq!(
            chat["messages"].as_array().unwrap().last().unwrap()["content"],
            request.message
        );
        assert!(chat["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"]
                .as_str()
                .is_some_and(|s| s.contains("历史检查点"))));
        assert!(chat["messages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|m| m["content"] == "a".repeat(1000)));
        assert!(chat["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("不要重启"));
    }
    let history = f
        .manager
        .history("model".into(), "default".into(), f.session.clone(), 0, 20)
        .await
        .unwrap();
    assert_eq!(history.turns.len(), 9);
    assert!(history
        .turns
        .iter()
        .all(|t| t.state == chat::TurnState::Completed));
}
#[tokio::test]
async fn over_hard_window_is_reduced_before_sending_the_ordinary_model_request() {
    let f = Fixture::new().await;
    f.seed().await;
    f.config(6000, 75);
    let frames = f.chat(f.request("继续解释")).await.unwrap();
    assert_eq!(frames.last().unwrap()["event"], "done");
    assert!(f.repo.checkpoint(&f.session, "default").unwrap().is_some());
    assert!(f.summaries() <= 2);
    assert_eq!(f.streams(), 1);
    let requests = f.mock.requests.lock().unwrap();
    let ordinary = requests.last().unwrap();
    // Same heuristic as the adapter; the provider never receives the over-budget original request.
    let cost = shellpilot_server::agent::budget::estimate_text(&ordinary.to_string())
        + ordinary["messages"].as_array().unwrap().len() as u64 * 12;
    assert!(cost <= 6000 - 512 - 256);
}
#[tokio::test]
async fn invalid_summary_retries_at_most_twice_then_uses_only_an_unchanged_fitting_original_context(
) {
    let f = Fixture::new().await;
    f.seed().await;
    f.config(8192, 60);
    *f.mock.mode.lock().unwrap() = Mode::Invalid;
    let original = f.events();
    let cp = f.snapshot().session.checkpoint_id;
    assert_eq!(
        f.chat(f.request("continue")).await.unwrap().last().unwrap()["event"],
        "done"
    );
    assert_eq!(f.summaries(), 2);
    assert_eq!(f.streams(), 1);
    assert_eq!(f.snapshot().session.checkpoint_id, cp);
    assert_eq!(&f.events()[..original.len()], &original);
}
#[tokio::test]
async fn network_summary_error_is_not_retried_and_can_fall_back_without_claiming_compression() {
    let f = Fixture::new().await;
    f.seed().await;
    f.config(8192, 60);
    *f.mock.mode.lock().unwrap() = Mode::Http;
    assert_eq!(
        f.chat(f.request("continue")).await.unwrap().last().unwrap()["event"],
        "done"
    );
    assert_eq!(f.summaries(), 1);
    assert_eq!(f.streams(), 1);
    assert!(f.repo.checkpoint(&f.session, "default").unwrap().is_none());
}
#[tokio::test]
async fn insufficient_reduction_or_invalid_summary_never_commits_when_original_context_cannot_fit()
{
    for mode in [Mode::Long, Mode::Invalid, Mode::Http] {
        let f = Fixture::new().await;
        f.seed().await;
        f.config(6000, 75);
        *f.mock.mode.lock().unwrap() = mode;
        let before = f.snapshot();
        let events = f.events();
        let error = f.chat(f.request("continue")).await.unwrap_err();
        assert!(
            matches!(
                error,
                chat::Error::CompactionFailed | chat::Error::CompactionLimit
            ),
            "{error:?}"
        );
        assert!(f.summaries() <= 2);
        assert_eq!(f.streams(), 0);
        assert_eq!(f.snapshot(), before);
        assert_eq!(f.events(), events);
        assert!(!error.to_string().contains("provider-private-body"));
    }
}
#[tokio::test]
async fn cancellation_during_summary_stops_before_checkpoint_or_ordinary_request_admission() {
    for external in [true, false] {
        let f = Fixture::new().await;
        f.seed().await;
        f.config(8192, 60);
        f.mock.hold.store(true, Ordering::SeqCst);
        let before = f.snapshot();
        let events = f.events();
        let request = f.request("continue");
        let manager = f.manager.clone();
        let token = CancellationToken::new();
        let stop = token.clone();
        let copy = request.clone();
        let run =
            tokio::spawn(
                async move { manager.chat(copy, None, &token, |_| async { Ok(()) }).await },
            );
        tokio::time::timeout(Duration::from_secs(2), f.mock.arrived.notified())
            .await
            .unwrap();
        if external {
            stop.cancel();
        } else {
            assert!(f
                .manager
                .stop_request(
                    "model".into(),
                    "default".into(),
                    f.session.clone(),
                    Some(request.request_id)
                )
                .await
                .unwrap());
        }
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), run)
                .await
                .unwrap()
                .unwrap()
                .unwrap_err()
                .code(),
            "AGENT_CANCELLED"
        );
        assert_eq!(f.snapshot(), before);
        assert_eq!(f.events(), events);
        assert_eq!(f.summaries(), 1);
        assert_eq!(f.streams(), 0);
    }
}
#[tokio::test]
async fn new_input_or_target_during_summary_cannot_be_overwritten_by_a_stale_checkpoint() {
    for change_target in [false, true] {
        let f = Fixture::new().await;
        f.seed().await;
        f.config(8192, 60);
        f.mock.hold.store(true, Ordering::SeqCst);
        let request = f.request("continue");
        let manager = f.manager.clone();
        let run = tokio::spawn(async move {
            manager
                .chat(request, None, &CancellationToken::new(), |_| async {
                    Ok(())
                })
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), f.mock.arrived.notified())
            .await
            .unwrap();
        if change_target {
            f.repo
                .start_task(
                    &f.session,
                    "default",
                    f.snapshot().session.revision,
                    "changed",
                    TaskRequest {
                        goal: "新目标".into(),
                        constraints: vec!["只读".into()],
                        target: Some(Target {
                            connection_id: "other".into(),
                            terminal_session_id: "other-ssh".into(),
                        }),
                    },
                )
                .unwrap();
        } else {
            f.repo
                .append(
                    &f.session,
                    "default",
                    f.snapshot().session.revision,
                    "new-user",
                    Payload::UserMessage {
                        text: "新的约束".into(),
                    },
                )
                .unwrap();
        }
        let current = f.snapshot();
        f.mock.release.notify_one();
        assert_eq!(
            run.await.unwrap().unwrap_err().code(),
            "AGENT_CONTEXT_CHANGED"
        );
        assert_eq!(f.snapshot(), current);
        assert_eq!(f.streams(), 0);
    }
}
#[tokio::test]
async fn oversized_mandatory_latest_input_fails_before_any_summary_or_stream_request() {
    let f = Fixture::new().await;
    f.config(1024, 75);
    assert_eq!(
        f.chat(f.request(&"latest".repeat(1000)))
            .await
            .unwrap_err()
            .code(),
        "MODEL_CONTEXT_LIMIT"
    );
    assert!(f.mock.requests.lock().unwrap().is_empty());
    assert!(f.events().is_empty());
}
#[tokio::test]
async fn checkpoint_projection_survives_reopen_and_completed_request_replay_does_not_resummarize() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("db.sqlite3");
    let f = Fixture::with_store(Arc::new(Store::open(&path).unwrap())).await;
    f.seed().await;
    f.config(8192, 60);
    let request = f.request("continue");
    f.chat(request.clone()).await.unwrap();
    let checkpoint = f.repo.checkpoint(&f.session, "default").unwrap().unwrap();
    let summaries = f.summaries();
    f.chat(request.clone()).await.unwrap();
    assert_eq!(f.summaries(), summaries);
    assert_eq!(f.streams(), 1);
    let id = f.session.clone();
    drop(f);
    let store = Arc::new(Store::open(&path).unwrap());
    let manager = Manager::new(store.clone(), CancellationToken::new());
    let mut done = false;
    manager
        .chat(request, None, &CancellationToken::new(), |event| {
            done = matches!(event, StreamEvent::Done { .. });
            async { Ok(()) }
        })
        .await
        .unwrap();
    assert!(done);
    assert_eq!(
        Repository::new(store)
            .checkpoint(&id, "default")
            .unwrap()
            .unwrap(),
        checkpoint
    );
}
#[tokio::test]
async fn tool_batch_budget_hook_requires_complete_real_results_and_does_not_execute() {
    let f = Fixture::new().await;
    let session = f.repo.session(&f.session, "default").unwrap();
    f.repo
        .start_task(
            &f.session,
            "default",
            session.revision,
            "task",
            TaskRequest {
                goal: "inspect".into(),
                constraints: vec![],
                target: None,
            },
        )
        .unwrap();
    let call = f
        .repo
        .append(
            &f.session,
            "default",
            f.snapshot().session.revision,
            "call",
            Payload::ToolCall {
                call_id: "call".into(),
                tool_name: "ssh".into(),
                arguments: json!({"command":"read logs"}),
            },
        )
        .unwrap()
        .event;
    let config = ModelRepository::new(f.store.clone())
        .get("model", "default")
        .unwrap()
        .config;
    let request = ChatRequest {
        messages: vec![ChatMessage::User {
            content: "explain".into(),
        }],
        tools: vec![],
    };
    assert!(check_after_tool_batch(&config, &request, std::slice::from_ref(&call)).is_err());
    let result = f
        .repo
        .append(
            &f.session,
            "default",
            f.snapshot().session.revision,
            "result",
            Payload::ToolResult {
                call_id: "call".into(),
                outcome: Outcome::Unknown,
                exit_code: None,
                text: "unknown".into(),
                outputs: vec![],
            },
        )
        .unwrap()
        .event;
    let check = check_after_tool_batch(&config, &request, &[call, result]).unwrap();
    assert!(!check.should_compact);
    assert!(check.estimated_input_tokens < check.available_input_tokens);
    assert!(f.mock.requests.lock().unwrap().is_empty());
    assert_eq!(f.snapshot().uncertain_tool_call_ids, vec!["call"]);
}

#[tokio::test]
async fn active_preparation_uses_a_frozen_profile_when_settings_change_during_summary() {
    let f = Fixture::new().await;
    f.seed().await;
    f.config(8192, 60);
    f.mock.hold.store(true, Ordering::SeqCst);
    let request = f.request("continue");
    let manager = f.manager.clone();
    let run = tokio::spawn(async move {
        manager
            .chat(request, None, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), f.mock.arrived.notified())
        .await
        .unwrap();
    // Configuration changes apply to later requests. This active request must not silently reroute.
    let repo = ModelRepository::new(f.store.clone());
    let profile = repo.get("model", "default").unwrap();
    repo.delete("model", "default", profile.revision).unwrap();
    f.mock.release.notify_one();
    run.await.unwrap().unwrap();
    assert_eq!(f.summaries(), 1);
    assert_eq!(f.streams(), 1);
    assert!(f.repo.checkpoint(&f.session, "default").unwrap().is_some());
}

#[tokio::test]
async fn failed_compaction_preserves_an_existing_checkpoint_and_all_tail_evidence() {
    let f = Fixture::new().await;
    f.seed().await;
    f.config(8192, 60);
    f.chat(f.request("continue")).await.unwrap();
    let old = f.repo.checkpoint(&f.session, "default").unwrap().unwrap();
    f.config(32768, 75);
    for _ in 0..3 {
        f.chat(f.request(&"u".repeat(1000))).await.unwrap();
    }
    f.config(4000, 75);
    *f.mock.mode.lock().unwrap() = Mode::Http;
    f.mock.requests.lock().unwrap().clear();
    let before = f.snapshot();
    let events = f.events();
    assert!(matches!(
        f.chat(f.request("continue")).await,
        Err(chat::Error::CompactionFailed)
    ));
    assert_eq!(
        f.repo.checkpoint(&f.session, "default").unwrap().unwrap(),
        old
    );
    assert_eq!(f.snapshot(), before);
    assert_eq!(f.events(), events);
    assert_eq!(f.summaries(), 1);
    assert_eq!(f.streams(), 0);
}
