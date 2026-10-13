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
        .header("content-type", "text/event-stream")
        .body(body)
        .unwrap()
}
impl Fixture {
    async fn new(stream: String, delay: Duration) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let mock = Mock {
            bodies: Arc::new(Mutex::new(vec![])),
            stream: stream.into_bytes(),
            delay,
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
    tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
        .unwrap();
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
        Err(Error::Unsupported)
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
