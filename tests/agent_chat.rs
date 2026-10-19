use axum::{
    body::{Body, Bytes},
    extract::State,
    routing::post,
    Json, Router,
};
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        chat::{Error, Manager, Request, StreamEvent},
        provider::*,
        Payload, Repository, Target,
    },
    store::Store,
};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Mock {
    bodies: Arc<Mutex<Vec<Value>>>,
    stream: Vec<u8>,
    delay: Duration,
    status: axum::http::StatusCode,
    content_type: String,
}
struct Fixture {
    store: Arc<Store>,
    manager: Arc<Manager>,
    mock: Mock,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
        self.manager.shutdown();
    }
}
fn chunk(content: Option<&str>, finish: Option<&str>) -> String {
    format!(
        "data: {}\r\n\r\n",
        json!({"choices":[{"index":0,"delta":{"role":"assistant","content":content},"finish_reason":finish}]})
    )
}
fn stream() -> String {
    format!(
        "{}{}{}data: [DONE]\r\n\r\n",
        chunk(Some("汉🙂 "), None),
        chunk(Some("\"quote\"\\n"), None),
        chunk(None, Some("stop"))
    )
}
async fn handler(State(mock): State<Mock>, Json(body): Json<Value>) -> axum::response::Response {
    mock.bodies.lock().unwrap().push(body);
    let body = Body::from_stream(async_stream::stream! {
        // One-byte frames deliberately split both CRLF and multi-byte Unicode.
        for byte in mock.stream {
            yield Ok::<_,std::io::Error>(Bytes::from(vec![byte]));
        }
        tokio::time::sleep(mock.delay).await;
    });
    axum::response::Response::builder()
        .status(mock.status)
        .header("content-type", mock.content_type)
        .body(body)
        .unwrap()
}
impl Fixture {
    async fn new(stream: String, delay: Duration) -> Self {
        Self::response(
            stream,
            delay,
            axum::http::StatusCode::OK,
            "text/event-stream",
        )
        .await
    }
    async fn response(
        stream: String,
        delay: Duration,
        status: axum::http::StatusCode,
        content_type: &str,
    ) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let mock = Mock {
            bodies: Arc::new(Mutex::new(vec![])),
            stream: stream.into_bytes(),
            delay,
            status,
            content_type: content_type.into(),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(handler))
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let store = Arc::new(Store::in_memory().unwrap());
        let config = ModelConfig {
            provider: ProviderKind::OpenAiCompatible,
            remote_compaction: false,
            base_url: base,
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
            timeout_seconds: 3,
            max_response_bytes: 65536,
            require_api_key: false,
        };
        ModelRepository::new(store.clone())
            .save(
                "agent",
                "default",
                0,
                ModelConfigUpdate {
                    config,
                    api_key: KeyChange::Keep,
                },
            )
            .unwrap();
        let manager = Arc::new(Manager::new(store.clone(), CancellationToken::new()));
        Self {
            store,
            manager,
            mock,
            server,
        }
    }
    async fn request(&self) -> Request {
        Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "agent".into(),
            user_id: "default".into(),
            session_id: self
                .manager
                .create_session("agent".into(), "default".into())
                .await
                .unwrap(),
            terminal_session_id: String::new(),
            message: "只返回文字，不要执行命令".into(),
            attachments: vec![],
        }
    }
    fn events(&self, session: &str) -> Vec<shellpilot_server::agent::Event> {
        Repository::new(self.store.clone())
            .events(session, "default", 0, 200)
            .unwrap()
    }
}
#[tokio::test]
async fn list_create_stream_and_next_turn_preserve_protocol_and_saved_history() {
    let f = Fixture::new(stream(), Duration::ZERO).await;
    assert_eq!(
        f.manager.list("default".into()).await.unwrap()[0].agent_id,
        "agent"
    );
    assert!(f.manager.list("other".into()).await.unwrap().is_empty());
    let mut r = f.request().await;
    let id = r.session_id.clone();
    let frames = Arc::new(Mutex::new(vec![]));
    f.manager
        .chat(r.clone(), None, &CancellationToken::new(), |e| {
            frames
                .lock()
                .unwrap()
                .push(serde_json::to_value(e).unwrap());
            async { Ok(()) }
        })
        .await
        .unwrap();
    {
        let data = frames.lock().unwrap();
        assert_eq!(
            data.iter()
                .filter(|e| e["event"] == "text")
                .map(|e| e["content"].as_str().unwrap())
                .collect::<String>(),
            "汉🙂 \"quote\"\\n"
        );
        assert_eq!(
            data.last().unwrap(),
            &json!({"event":"done","content":"汉🙂 \"quote\"\\n"})
        );
    }
    assert!(matches!(
        f.events(&id).last().unwrap().payload,
        Payload::AssistantMessage { .. }
    ));
    r.message = "继续解释".into();
    r.request_id = uuid::Uuid::new_v4().to_string();
    f.manager
        .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
        .await
        .unwrap();
    {
        let bodies = f.mock.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert_eq!(bodies[0]["stream"], true);
        assert_eq!(bodies[1]["messages"][2]["role"], "assistant");
        assert_eq!(bodies[1]["messages"][2]["content"], "汉🙂 \"quote\"\\n");
        assert!(bodies[0].get("tools").is_none());
    }
    assert!(!f
        .manager
        .stop_chat("agent".into(), "default".into(), id)
        .await
        .unwrap());
}
#[tokio::test]
async fn eof_even_after_finish_reason_is_error_and_not_saved_as_success() {
    for stream in [
        chunk(Some("partial"), None),
        format!(
            "{}{}",
            chunk(Some("partial"), None),
            chunk(None, Some("stop"))
        ),
    ] {
        let f = Fixture::new(stream, Duration::ZERO).await;
        let r = f.request().await;
        let id = r.session_id.clone();
        Repository::new(f.store.clone())
            .start_task(
                &id,
                "default",
                0,
                "initial-task",
                shellpilot_server::agent::TaskRequest {
                    goal: "original goal".into(),
                    constraints: vec!["never-restart".into()],
                    target: None,
                },
            )
            .unwrap();
        let frames = Arc::new(Mutex::new(vec![]));
        assert!(matches!(
            f.manager
                .chat(r.clone(), None, &CancellationToken::new(), |e| {
                    frames.lock().unwrap().push(e);
                    async { Ok(()) }
                })
                .await,
            Err(Error::Model(ModelError::Incomplete))
        ));
        assert!(!frames
            .lock()
            .unwrap()
            .iter()
            .any(|e| matches!(e, StreamEvent::Done { .. })));
        assert!(!f
            .events(&id)
            .iter()
            .any(|e| matches!(e.payload, Payload::AssistantMessage { .. })));
        assert!(matches!(
            f.events(&id).last().unwrap().payload,
            Payload::Cancelled { .. }
        ));
        let mut retry = r;
        retry.message = "retry explanation".into();
        retry.request_id = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            f.manager
                .chat(retry, None, &CancellationToken::new(), |_| async { Ok(()) })
                .await,
            Err(Error::Model(ModelError::Incomplete))
        ));
        assert_eq!(
            Repository::new(f.store.clone())
                .session(&id, "default")
                .unwrap()
                .task
                .unwrap()
                .constraints,
            vec!["never-restart"]
        );
        assert!(f.mock.bodies.lock().unwrap()[1]["messages"][0]["content"]
            .as_str()
            .unwrap()
            .contains("never-restart"));
    }
}
#[tokio::test]
async fn stop_interrupts_stream_and_same_session_cannot_overlap() {
    let f = Fixture::new(chunk(Some("partial"), None), Duration::from_secs(30)).await;
    let r = f.request().await;
    let id = r.session_id.clone();
    let manager = f.manager.clone();
    let first = r.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    let running = tokio::spawn(async move {
        manager
            .chat(first, None, &CancellationToken::new(), |e| {
                let sender = sender.clone();
                async move { sender.send(e).await.map_err(|_| Error::Stopped) }
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(receiver.recv().await.unwrap(), StreamEvent::Text { content } if content == "partial") {
                break;
            }
        }
    }).await.unwrap();
    assert!(matches!(
        f.manager
            .chat(r.clone(), None, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await,
        Err(Error::Busy)
    ));
    assert!(matches!(
        f.manager
            .stop_chat("agent".into(), "other".into(), id.clone())
            .await,
        Err(Error::NotFound)
    ));
    assert!(f
        .manager
        .stop_chat("agent".into(), "default".into(), id.clone())
        .await
        .unwrap());
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap(),
        Err(Error::Model(ModelError::Cancelled))
    ));
    assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
    assert!(matches!(
        f.events(&id).last().unwrap().payload,
        Payload::Cancelled { .. }
    ));
}
#[tokio::test]
async fn caller_cancellation_and_shutdown_release_active_turn() {
    let f = Fixture::new(chunk(Some("partial"), None), Duration::from_secs(30)).await;
    let r = f.request().await;
    let manager = f.manager.clone();
    let cancel = CancellationToken::new();
    let signal = cancel.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    let running = tokio::spawn(async move {
        manager
            .chat(r, None, &signal, |e| {
                let sender = sender.clone();
                async move { sender.send(e).await.map_err(|_| Error::Stopped) }
            })
            .await
    });
    receiver.recv().await.unwrap();
    cancel.cancel();
    assert!(matches!(
        running.await.unwrap(),
        Err(Error::Model(ModelError::Cancelled))
    ));
    let r = f.request().await;
    let manager = f.manager.clone();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(4);
    let running = tokio::spawn(async move {
        manager
            .chat(r, None, &CancellationToken::new(), |e| {
                let sender = sender.clone();
                async move { sender.send(e).await.map_err(|_| Error::Stopped) }
            })
            .await
    });
    receiver.recv().await.unwrap();
    f.manager.shutdown();
    assert!(matches!(
        running.await.unwrap(),
        Err(Error::Model(ModelError::Cancelled))
    ));
    assert!(matches!(
        f.manager
            .create_session("agent".into(), "default".into())
            .await,
        Err(Error::Stopped)
    ));
}
#[tokio::test]
async fn wrong_agent_target_and_attachments_are_rejected_before_model_request() {
    let f = Fixture::new(stream(), Duration::ZERO).await;
    let mut r = f.request().await;
    r.agent_id = "other".into();
    assert!(matches!(
        f.manager
            .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
            .await,
        Err(Error::NotFound)
    ));
    let mut r = f.request().await;
    r.attachments
        .push(shellpilot_server::agent::chat::Attachment {
            file_id: "file".into(),
        });
    assert!(matches!(
        f.manager
            .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
            .await,
        Err(Error::Attachment(_))
    ));
    let mut r = f.request().await;
    r.terminal_session_id = "pty-1".into();
    let target = Target {
        connection_id: "host-1".into(),
        terminal_session_id: "pty-1".into(),
    };
    f.manager
        .chat(
            r.clone(),
            Some(target),
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    let other = Target {
        connection_id: "host-2".into(),
        terminal_session_id: "pty-1".into(),
    };
    r.request_id = uuid::Uuid::new_v4().to_string();
    assert!(matches!(
        f.manager
            .chat(r, Some(other), &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await,
        Err(Error::TargetMismatch)
    ));
    assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn streamed_tool_call_filtered_finish_and_provider_error_are_not_text_success() {
    let tool = format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"id":"call"}]},"finish_reason":null}]})
    );
    for data in [
        tool,
        format!(
            "{}data: [DONE]\n\n",
            chunk(Some("truncated"), Some("length"))
        ),
        "data: {\"error\":{\"message\":\"secret-echo\"}}\n\n".into(),
    ] {
        let f = Fixture::new(data, Duration::ZERO).await;
        let error = f
            .manager
            .chat(
                f.request().await,
                None,
                &CancellationToken::new(),
                |_| async { Ok(()) },
            )
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("secret-echo"));
    }
}

#[tokio::test]
async fn budget_or_json_event_limit_rejection_does_not_start_a_model_request() {
    let f = Fixture::new(stream(), Duration::ZERO).await;
    let mut r = f.request().await;
    let id = r.session_id.clone();
    r.message = "x".repeat(15000);
    assert!(matches!(
        f.manager
            .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
            .await,
        Err(Error::Model(ModelError::BudgetExceeded))
    ));
    assert!(f.events(&id).is_empty());
    let mut r = f.request().await;
    let id = r.session_id.clone();
    r.message = "\0".repeat(16000);
    assert!(matches!(
        f.manager
            .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
            .await,
        Err(Error::Invalid)
    ));
    assert!(f.events(&id).is_empty());
    assert!(f.mock.bodies.lock().unwrap().is_empty());
}
#[tokio::test]
async fn escaping_cannot_overflow_the_persistent_assistant_event_or_channel_frame() {
    let response = format!(
        "{}{}data: [DONE]\n\n",
        chunk(Some(&"\0".repeat(10000)), None),
        chunk(None, Some("stop"))
    );
    let f = Fixture::new(response, Duration::ZERO).await;
    let r = f.request().await;
    let id = r.session_id.clone();
    assert!(matches!(
        f.manager
            .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
            .await,
        Err(Error::Model(ModelError::ResponseTooLarge))
    ));
    assert!(!f
        .events(&id)
        .iter()
        .any(|e| matches!(e.payload, Payload::AssistantMessage { .. })));
}

#[tokio::test]
async fn completed_request_replay_survives_model_deletion_without_another_network_call() {
    let f = Fixture::new(stream(), Duration::ZERO).await;
    let r = f.request().await;
    f.manager
        .chat(r.clone(), None, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
        .unwrap();
    ModelRepository::new(f.store.clone())
        .delete("agent", "default", 1)
        .unwrap();
    let output = Arc::new(Mutex::new(vec![]));
    f.manager
        .chat(r.clone(), None, &CancellationToken::new(), |e| {
            output.lock().unwrap().push(e);
            async { Ok(()) }
        })
        .await
        .unwrap();
    assert_eq!(output.lock().unwrap().len(), 1);
    assert!(matches!(
        output.lock().unwrap()[0],
        StreamEvent::Done { .. }
    ));
    assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
    let mut conflict = r.clone();
    conflict.message = "changed input".into();
    assert!(matches!(
        f.manager
            .chat(conflict, None, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await,
        Err(Error::Conflict)
    ));
    let history = f
        .manager
        .history(
            "agent".into(),
            "default".into(),
            r.session_id.clone(),
            0,
            20,
        )
        .await
        .unwrap();
    assert_eq!(history.turns.len(), 1);
    assert_eq!(
        history.turns[0].state,
        shellpilot_server::agent::chat::TurnState::Completed
    );
    assert_eq!(
        f.manager
            .sessions("agent".into(), "default".into(), String::new(), 20)
            .await
            .unwrap()[0]
            .id,
        r.session_id
    );
}
#[tokio::test]
async fn failed_request_replay_does_not_auto_retry_and_retains_partial_evidence() {
    let f = Fixture::new(chunk(Some("partial🙂"), None), Duration::ZERO).await;
    let r = f.request().await;
    assert_eq!(
        f.manager
            .chat(r.clone(), None, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
            .unwrap_err()
            .code(),
        "MODEL_INCOMPLETE"
    );
    assert_eq!(
        f.manager
            .chat(r.clone(), None, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
            .unwrap_err()
            .code(),
        "MODEL_INCOMPLETE"
    );
    assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
    let history = f
        .manager
        .history(
            "agent".into(),
            "default".into(),
            r.session_id.clone(),
            0,
            20,
        )
        .await
        .unwrap();
    assert_eq!(history.turns[0].partial_text, "partial🙂");
    assert!(history.turns[0].result_text.is_none());
    assert_eq!(
        history.turns[0].state,
        shellpilot_server::agent::chat::TurnState::Failed
    );
}
#[tokio::test]
async fn independent_sessions_and_managers_do_not_share_cancellation_or_reexecute_active_requests()
{
    let f = Fixture::new(chunk(Some("partial"), None), Duration::from_secs(30)).await;
    let a = f.request().await;
    let b = f.request().await;
    let a_id = a.session_id.clone();
    let b_id = b.session_id.clone();
    let second = Arc::new(Manager::new(f.store.clone(), CancellationToken::new()));
    let (sender, mut events) = tokio::sync::mpsc::channel(4);
    let manager = f.manager.clone();
    let running_a = a.clone();
    let one = sender.clone();
    let task_a = tokio::spawn(async move {
        manager
            .chat(running_a, None, &CancellationToken::new(), |e| {
                let sender = one.clone();
                async move { sender.send(e).await.map_err(|_| Error::Stopped) }
            })
            .await
    });
    let manager = second.clone();
    let running_b = b.clone();
    let task_b = tokio::spawn(async move {
        manager
            .chat(running_b, None, &CancellationToken::new(), |e| {
                let sender = sender.clone();
                async move { sender.send(e).await.map_err(|_| Error::Stopped) }
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    tokio::time::timeout(Duration::from_secs(2), events.recv())
        .await
        .unwrap()
        .unwrap();
    let mut duplicate = a.clone();
    duplicate.request_id = uuid::Uuid::new_v4().to_string();
    assert!(matches!(
        second
            .chat(duplicate, None, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await,
        Err(Error::Busy)
    ));
    // Persisted cancellation can reach a turn owned by another Manager via its heartbeat.
    assert!(second
        .stop_request(
            "agent".into(),
            "default".into(),
            a_id.clone(),
            Some(a.request_id)
        )
        .await
        .unwrap());
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(3), task_a)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err()
            .code(),
        "AGENT_CANCELLED"
    );
    assert!(!task_b.is_finished());
    assert_eq!(
        second
            .history("agent".into(), "default".into(), b_id.clone(), 0, 20)
            .await
            .unwrap()
            .turns[0]
            .state,
        shellpilot_server::agent::chat::TurnState::Running
    );
    second
        .stop_request("agent".into(), "default".into(), b_id, Some(b.request_id))
        .await
        .unwrap();
    task_b.await.unwrap().unwrap_err();
    assert_eq!(f.mock.bodies.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn aborting_a_future_recovers_partial_as_interrupted_instead_of_completing_or_replaying() {
    let f = Fixture::new(chunk(Some("partial"), None), Duration::from_secs(30)).await;
    let r = f.request().await;
    let manager = f.manager.clone();
    let request = r.clone();
    let (sender, mut output) = tokio::sync::mpsc::channel(1);
    let run = tokio::spawn(async move {
        manager
            .chat(request, None, &CancellationToken::new(), |e| {
                let sender = sender.clone();
                async move { sender.send(e).await.map_err(|_| Error::Stopped) }
            })
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if matches!(output.recv().await.unwrap(), StreamEvent::Text { content } if content == "partial") {
                break;
            }
        }
    }).await.unwrap();
    run.abort();
    let _ = run.await;
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let history = f
                .manager
                .history(
                    "agent".into(),
                    "default".into(),
                    r.session_id.clone(),
                    0,
                    20,
                )
                .await
                .unwrap();
            if history.turns[0].state == shellpilot_server::agent::chat::TurnState::Interrupted {
                assert_eq!(history.turns[0].partial_text, "partial");
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        f.manager
            .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
            .await
            .unwrap_err()
            .code(),
        "AGENT_INTERRUPTED"
    );
    assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn before_and_after_first_delta_timeouts_keep_failed_turns_and_do_not_retry() {
    for data in [String::new(), chunk(Some("partial"), None)] {
        let f = Fixture::new(data, Duration::from_secs(30)).await;
        let repo = ModelRepository::new(f.store.clone());
        let mut profile = repo.get("agent", "default").unwrap();
        profile.config.timeout_seconds = 1;
        repo.save(
            "agent",
            "default",
            1,
            ModelConfigUpdate {
                config: profile.config,
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
        let r = f.request().await;
        assert_eq!(
            f.manager
                .chat(r.clone(), None, &CancellationToken::new(), |_| async {
                    Ok(())
                })
                .await
                .unwrap_err()
                .code(),
            "MODEL_TIMEOUT"
        );
        assert_eq!(
            f.manager.receipt(r.clone()).await.unwrap().unwrap().state,
            shellpilot_server::agent::chat::TurnState::Failed
        );
        assert_eq!(
            f.manager
                .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
                .await
                .unwrap_err()
                .code(),
            "MODEL_TIMEOUT"
        );
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn protocol_http_content_type_and_usage_errors_are_redacted_and_never_persisted_as_completed()
{
    let invalid_usage = format!(
        "{}{}data: {}\n\ndata: [DONE]\n\n",
        chunk(Some("x"), None),
        chunk(None, Some("stop")),
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"total_tokens":1}})
    );
    for (data, status, kind) in [
        (
            "secret-echo".to_owned(),
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            "text/event-stream",
        ),
        (stream(), axum::http::StatusCode::OK, "application/json"),
        (
            "data: {broken\n\n".into(),
            axum::http::StatusCode::OK,
            "text/event-stream",
        ),
        (
            "data: [DONE]\n\n".into(),
            axum::http::StatusCode::OK,
            "text/event-stream",
        ),
        (
            invalid_usage,
            axum::http::StatusCode::OK,
            "text/event-stream",
        ),
    ] {
        let f = Fixture::response(data, Duration::ZERO, status, kind).await;
        let r = f.request().await;
        let error = f
            .manager
            .chat(r.clone(), None, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("secret-echo"));
        assert_eq!(
            f.manager.receipt(r).await.unwrap().unwrap().state,
            shellpilot_server::agent::chat::TurnState::Failed
        );
    }
}
#[tokio::test]
async fn delivery_failure_after_complete_commit_does_not_retroactively_cancel_the_reply() {
    let f = Fixture::new(stream(), Duration::ZERO).await;
    let r = f.request().await;
    let error = f
        .manager
        .chat(
            r.clone(),
            None,
            &CancellationToken::new(),
            |event| async move {
                if matches!(event, StreamEvent::Done { .. }) {
                    Err(Error::Stopped)
                } else {
                    Ok(())
                }
            },
        )
        .await
        .unwrap_err();
    assert_eq!(error.code(), "BACKEND_UNAVAILABLE");
    assert!(!f
        .manager
        .stop_request(
            "agent".into(),
            "default".into(),
            r.session_id.clone(),
            Some(r.request_id.clone())
        )
        .await
        .unwrap());
    assert_eq!(
        f.manager.receipt(r.clone()).await.unwrap().unwrap().state,
        shellpilot_server::agent::chat::TurnState::Completed
    );
    f.manager
        .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
        .await
        .unwrap();
    assert_eq!(f.mock.bodies.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn fragmented_multiline_sse_with_comments_and_usage_preserves_raw_text() {
    let text = "中文🙂\n~~~json\n{\"content\":\"literal\",\"path\":\"C:\\\\tmp\"}\n~~~";
    let multiline = serde_json::to_string_pretty(&json!({
        "choices":[{"index":0,"delta":{"role":"assistant","content":text},"finish_reason":null}]
    }))
    .unwrap()
    .lines()
    .map(|line| format!("data: {line}\r\n"))
    .collect::<String>();
    let data = format!(
        ": keepalive\r\nevent: message\r\nid: ignored\r\n{multiline}\r\n{}data: {}\r\n\r\ndata: [DONE]\r\n\r\n",
        chunk(None, Some("stop")),
        json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":8,"total_tokens":20}})
    );
    let f = Fixture::response(
        data,
        Duration::ZERO,
        axum::http::StatusCode::OK,
        "TEXT/EVENT-STREAM; charset=utf-8",
    )
    .await;
    let r = f.request().await;
    let mut received = Vec::new();
    f.manager
        .chat(r.clone(), None, &CancellationToken::new(), |event| {
            received.push(event);
            async { Ok(()) }
        })
        .await
        .unwrap();
    assert!(matches!(received.last(), Some(StreamEvent::Done { content }) if content == text));
    let turn = f.manager.receipt(r).await.unwrap().unwrap();
    assert_eq!(turn.result_text.as_deref(), Some(text));
    assert_eq!(turn.usage.unwrap().total_tokens, 20);
    let bodies = f.mock.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 1);
    assert_eq!(bodies[0]["stream"], true);
    assert_eq!(bodies[0]["max_completion_tokens"], 512);
    assert!(bodies[0].get("tools").is_none());
}

#[tokio::test]
async fn stream_protocol_matrix_keeps_partial_evidence_without_success_or_automatic_retry() {
    let cases = [
        ("multiple choices", json!({"choices":[
            {"index":0,"delta":{},"finish_reason":"stop"},
            {"index":1,"delta":{},"finish_reason":"stop"}]}).to_string(), "MODEL_PROTOCOL_FAILED"),
        ("wrong index", json!({"choices":[{"index":1,"delta":{},"finish_reason":"stop"}]}).to_string(), "MODEL_PROTOCOL_FAILED"),
        ("wrong role", json!({"choices":[{"index":0,"delta":{"role":"user"},"finish_reason":null}]}).to_string(), "MODEL_PROTOCOL_FAILED"),
        ("tool delta", json!({"choices":[{"index":0,"delta":{"tool_calls":[{"id":"tool"}]},"finish_reason":null}]}).to_string(), "MODEL_TOOL_PROTOCOL_FAILED"),
        ("legacy function", json!({"choices":[{"index":0,"delta":{"function_call":{"name":"run"}},"finish_reason":null}]}).to_string(), "MODEL_TOOL_PROTOCOL_FAILED"),
        ("refusal", json!({"choices":[{"index":0,"delta":{"refusal":"refused"},"finish_reason":null}]}).to_string(), "MODEL_INCOMPLETE"),
        ("truncated", json!({"choices":[{"index":0,"delta":{},"finish_reason":"length"}]}).to_string(), "MODEL_INCOMPLETE"),
        ("filtered", json!({"choices":[{"index":0,"delta":{},"finish_reason":"content_filter"}]}).to_string(), "MODEL_INCOMPLETE"),
        ("invalid JSON", "{provider-secret:broken".into(), "MODEL_PROTOCOL_FAILED"),
        ("premature marker", "[DONE]".into(), "MODEL_INCOMPLETE"),
    ];
    for (name, bad, code) in cases {
        let f = Fixture::new(
            format!("{}data: {bad}\n\n", chunk(Some("证据🙂"), None)),
            Duration::ZERO,
        )
        .await;
        let r = f.request().await;
        let mut frames = Vec::new();
        let error = f
            .manager
            .chat(r.clone(), None, &CancellationToken::new(), |event| {
                frames.push(event);
                async { Ok(()) }
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), code, "{name}");
        assert!(!error.to_string().contains("provider-secret"), "{name}");
        assert!(
            !frames
                .iter()
                .any(|event| matches!(event, StreamEvent::Done { .. })),
            "{name}"
        );
        let turn = f.manager.receipt(r.clone()).await.unwrap().unwrap();
        assert_eq!(
            turn.state,
            shellpilot_server::agent::chat::TurnState::Failed,
            "{name}"
        );
        assert_eq!(turn.partial_text, "证据🙂", "{name}");
        assert!(turn.result_text.is_none(), "{name}");
        assert!(
            !f.events(&r.session_id)
                .iter()
                .any(|event| matches!(event.payload, Payload::AssistantMessage { .. })),
            "{name}"
        );
        assert_eq!(
            f.manager
                .chat(r, None, &CancellationToken::new(), |_| async { Ok(()) })
                .await
                .unwrap_err()
                .code(),
            code,
            "{name}"
        );
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1, "{name}");
    }
}

#[tokio::test]
async fn response_and_sse_frame_limits_keep_the_delivered_prefix_without_retry() {
    for (name, suffix, limit) in [
        ("line", format!("data: {}\n\n", "x".repeat(65537)), 131072),
        (
            "multiline frame",
            format!(
                "{}\n",
                "data: xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\n".repeat(2100)
            ),
            131072,
        ),
        (
            "response without content length",
            format!(": {}\n\n", "x".repeat(8192)),
            4096,
        ),
    ] {
        let f = Fixture::new(
            format!("{}{suffix}", chunk(Some("bounded prefix"), None)),
            Duration::ZERO,
        )
        .await;
        let repository = ModelRepository::new(f.store.clone());
        let profile = repository.get("agent", "default").unwrap();
        let mut config = profile.config;
        config.max_response_bytes = limit;
        repository
            .save(
                "agent",
                "default",
                profile.revision,
                ModelConfigUpdate {
                    config,
                    api_key: KeyChange::Keep,
                },
            )
            .unwrap();
        let r = f.request().await;
        let error = f
            .manager
            .chat(r.clone(), None, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
            .unwrap_err();
        assert_eq!(error.code(), "MODEL_RESPONSE_LIMIT", "{name}");
        let turn = f.manager.receipt(r).await.unwrap().unwrap();
        assert_eq!(turn.partial_text, "bounded prefix", "{name}");
        assert_eq!(
            turn.error_code.as_deref(),
            Some("MODEL_RESPONSE_LIMIT"),
            "{name}"
        );
        assert!(turn.result_text.is_none(), "{name}");
        assert_eq!(f.mock.bodies.lock().unwrap().len(), 1, "{name}");
    }
}

#[tokio::test]
async fn confirmed_memory_is_rebuilt_per_turn_without_becoming_conversation_history() {
    use shellpilot_server::agent::memory::{Kind, Scope, Write, View};
    let f = Fixture::new(stream(), Duration::ZERO).await;
    let mut request = f.request().await;
    let write = Write {
        id: uuid::Uuid::new_v4().to_string(),
        revision: 0,
        title: "偏好来源测试".into(),
        content: "记忆测试唯一内容：优先简洁中文。".into(),
        kind: Kind::Preference,
        scope: Scope::Global,
        enabled: true,
    };
    let note = f
        .manager
        .save_memory(
            "default".into(),
            View {agent_id:"agent".into(),session_id:request.session_id.clone(),project_id:String::new()},
            write,
        )
        .await
        .unwrap();
    f.manager
        .chat(
            request.clone(),
            None,
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    let body = f.mock.bodies.lock().unwrap()[0].clone();
    assert!(body["messages"].to_string().contains("记忆测试唯一内容"));
    assert_eq!(body["messages"][1]["role"], "system");
    assert_eq!(
        body["messages"].as_array().unwrap().last().unwrap()["content"],
        request.message
    );
    let events = serde_json::to_string(&f.events(&request.session_id)).unwrap();
    assert!(!events.contains("记忆测试唯一内容"));
    f.manager
        .delete_memory(
            "default".into(),
            View {agent_id:"agent".into(),session_id:request.session_id.clone(),project_id:String::new()},
            note.id,
            note.revision,
        )
        .await
        .unwrap();
    request.request_id = uuid::Uuid::new_v4().to_string();
    request.message = "下一轮".into();
    f.manager
        .chat(request, None, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
        .unwrap();
    assert!(!f.mock.bodies.lock().unwrap().last().unwrap()["messages"]
        .to_string()
        .contains("记忆测试唯一内容"));
}

#[tokio::test]
async fn diagnostic_trace_keeps_http_status_and_provider_error_without_credentials_or_body() {
    use shellpilot_server::diagnostics::Level;
    let body=json!({"error":{"type":"invalid_request_error","code":"invalid_request_error","message":"The reasoning_content must be passed back. Bearer sk-private-fixture"}}).to_string();
    let f=Fixture::response(body,Duration::ZERO,axum::http::StatusCode::BAD_REQUEST,"application/json").await;
    let logs=tempfile::tempdir().unwrap();f.store.diagnostics.configure(logs.path().into(),Level::Debug).unwrap();
    let request=f.request().await;let trace=request.request_id.clone();
    let result=f.manager.chat(request,None,&CancellationToken::new(),|_|async{Ok(())}).await;
    assert!(matches!(result,Err(Error::Model(ModelError::Http(400)))));
    let path=logs.path().join(chrono::Local::now().format("%Y-%m-%d").to_string()).join(format!("{trace}.jsonl"));
    let raw=std::fs::read_to_string(path).unwrap();
    assert!(!raw.contains("sk-private-fixture"));assert!(!raw.contains("只返回文字"));
    let rows:Vec<Value>=raw.lines().map(|line|serde_json::from_str(line).unwrap()).collect();
    assert!(rows.iter().all(|row|row["traceId"]==trace));
    assert!(rows.iter().any(|row|row["event"]=="model.http_error"&&row["fields"]["httpStatus"]==400));
    assert!(rows.iter().any(|row|row["event"]=="model.http_error_detail"&&row["fields"]["reason"]=="reasoning_content_required"));
    assert_eq!(rows.last().unwrap()["event"],"request.failed");
    assert_eq!(rows.last().unwrap()["fields"]["httpStatus"],400);
}
