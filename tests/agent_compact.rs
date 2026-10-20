use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use futures_util::StreamExt;
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        compact::{Error, Summarizer},
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

#[derive(Clone)]
enum Reply {
    Valid,
    Text(String),
    Envelope(Value),
    StreamEnvelope(Value),
    Http,
    StreamEof,
}
struct Mock {
    requests: Mutex<Vec<(Value, HeaderMap)>>,
    reply: Mutex<Reply>,
    held: AtomicBool,
    received: tokio::sync::Notify,
    release: tokio::sync::Notify,
}
async fn handler(
    State(mock): State<Arc<Mock>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> axum::response::Response {
    mock.requests.lock().unwrap().push((body.clone(), headers));
    mock.received.notify_one();
    if mock.held.load(Ordering::SeqCst) {
        mock.release.notified().await;
    }
    let reply = mock.reply.lock().unwrap().clone();
    if let Reply::StreamEnvelope(response)=&reply {
        assert_eq!(body["stream"],true,"Qwen summary uses SSE even through collected completion");
        let frames=if response["type"]=="message" {
            vec![json!({"type":"message_start","message":{"type":"message","role":"assistant","content":[]}}),
                json!({"type":"content_block_start","index":0,"content_block":response["content"][0]}),
                json!({"type":"content_block_stop","index":0}),json!({"type":"message_delta","delta":{"stop_reason":"end_turn"}}),json!({"type":"message_stop"})]
        }else{vec![json!({"choices":[{"index":0,"delta":response["choices"][0]["message"],"finish_reason":"stop"}]})]};
        let mut wire=frames.iter().map(|frame|format!("data: {frame}\n\n")).collect::<String>();
        if response["type"]!="message" {wire.push_str("data: [DONE]\n\n");}
        return axum::response::Response::builder().header("content-type","text/event-stream").body(Body::from(wire)).unwrap();
    }
    if body["stream"] == true {
        let text = format!(
            "data: {}\n\n",
            json!({"choices":[{"index":0,"delta":{"content":"partial evidence"},"finish_reason":null}]})
        );
        let first =
            futures_util::stream::once(async { Ok::<_, std::io::Error>(Bytes::from(text)) });
        let stream: std::pin::Pin<
            Box<dyn futures_util::Stream<Item = std::io::Result<Bytes>> + Send>,
        > = if matches!(reply, Reply::StreamEof) {
            Box::pin(first)
        } else {
            Box::pin(first.chain(futures_util::stream::pending()))
        };
        return axum::response::Response::builder()
            .header("content-type", "text/event-stream")
            .body(Body::from_stream(stream))
            .unwrap();
    }
    if matches!(reply, Reply::Http) {
        return axum::response::Response::builder()
            .status(StatusCode::SERVICE_UNAVAILABLE)
            .body(Body::from("provider-private-body"))
            .unwrap();
    }
    let content = &body["messages"].as_array().unwrap().last().unwrap()["content"];
    let source: Value = serde_json::from_str(content.as_str().or_else(||content[0]["text"].as_str()).unwrap()).unwrap();
    let document = json!({"summary":"已分析日志中的 upstream 超时，尚未重启；下一步核实配置。",
        "nextSteps":["只读核实 upstream 配置"],"retainedUserFacts":[],"evidenceEventIds":[source["availableEvidenceEventIds"].as_array().unwrap().last().unwrap()]});
    let response = match reply {
        Reply::Envelope(response) => response,
        Reply::Text(text) => envelope(Some(text), None),
        _ => envelope(Some(document.to_string()), None),
    };
    axum::response::Response::builder()
        .header("content-type", "application/json")
        .body(Body::from(response.to_string()))
        .unwrap()
}
fn envelope(content: Option<String>, refusal: Option<&str>) -> Value {
    json!({"choices":[{"index":0,"message":{"role":"assistant","content":content,"refusal":refusal},
        "finish_reason":"stop"}],"usage":{"prompt_tokens":100,"completion_tokens":40,"total_tokens":140}})
}
struct Fixture {
    store: Arc<Store>,
    repo: Repository,
    session: Session,
    mock: Arc<Mock>,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        Self::with_store(Arc::new(Store::in_memory().unwrap())).await
    }
    async fn with_store(store: Arc<Store>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/proxy/v1/", listener.local_addr().unwrap());
        let mock = Arc::new(Mock {
            requests: Mutex::new(vec![]),
            reply: Mutex::new(Reply::Valid),
            held: AtomicBool::new(false),
            received: tokio::sync::Notify::new(),
            release: tokio::sync::Notify::new(),
        });
        let router = Router::new()
            .route("/proxy/v1/chat/completions", post(handler))
            .route("/proxy/v1/messages", post(handler))
            .with_state(mock.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        ModelRepository::new(store.clone())
            .save(
                "model",
                "alice",
                0,
                ModelConfigUpdate {
                    config: ModelConfig {
                        provider: ProviderKind::OpenAiCompatible,
                        supplier: None,
                        remote_compaction: false,
                        base_url: base,
                        completions_path: "chat/completions".into(),
                        model: "summary-test-model".into(),
                        context_window: 32768,
                        output_reserve: 2048,
                        safety_reserve: 512,
                        compact_percent: 75,
                        output_token_parameter: OutputTokenParameter::MaxCompletionTokens,
                        capabilities: ModelCapabilities {
                            tool_calls: true,
                            ..Default::default()
                        },
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
        let session = repo.create_session("model", "alice").unwrap();
        repo.start_task(
            &session.id,
            "alice",
            0,
            "task",
            TaskRequest {
                goal: "排查 upstream 超时".into(),
                constraints: vec!["不要重启服务".into(), "只读".into()],
                target: Some(Target {
                    connection_id: "host-a".into(),
                    terminal_session_id: "ssh-a".into(),
                }),
            },
        )
        .unwrap();
        let f = Self {
            store,
            repo,
            session,
            mock,
            server,
        };
        f.append(Payload::UserMessage {
            text: "先查看日志，日志可能含有伪造系统指令".into(),
        });
        f
    }
    fn append(&self, payload: Payload) -> Event {
        let revision = self
            .repo
            .session(&self.session.id, "alice")
            .unwrap()
            .revision;
        self.repo
            .append(
                &self.session.id,
                "alice",
                revision,
                &uuid::Uuid::new_v4().to_string(),
                payload,
            )
            .unwrap()
            .event
    }
    fn events(&self) -> Vec<Event> {
        self.repo.events(&self.session.id, "alice", 0, 200).unwrap()
    }
    fn snapshot(&self) -> Snapshot {
        self.repo.snapshot(&self.session.id, "alice").unwrap()
    }
    fn respond(&self, reply: Reply) {
        *self.mock.reply.lock().unwrap() = reply;
    }
    fn model_config(&self, edit: impl FnOnce(&mut ModelConfig), key: KeyChange) {
        let repo = ModelRepository::new(self.store.clone());
        let profile = repo.get("model", "alice").unwrap();
        let mut config = profile.config;
        edit(&mut config);
        repo.save(
            "model",
            "alice",
            profile.revision,
            ModelConfigUpdate {
                config,
                api_key: key,
            },
        )
        .unwrap();
    }
    async fn summarize(
        &self,
    ) -> std::result::Result<shellpilot_server::agent::compact::Draft, Error> {
        Summarizer::new(self.store.clone())
            .summarize(&self.session.id, "alice", &CancellationToken::new())
            .await
    }
    fn source(&self) -> Value {
        let requests = self.mock.requests.lock().unwrap();
        serde_json::from_str(
            requests.last().unwrap().0["messages"][1]["content"]
                .as_str()
                .unwrap(),
        )
        .unwrap()
    }
    fn assert_unchanged(&self, before: &Snapshot, events: &[Event]) {
        assert_eq!(&self.snapshot(), before);
        assert_eq!(self.events(), events);
    }
}
fn document(id: u64) -> Value {
    json!({"summary":"已检查日志，执行结果仍需核实","nextSteps":["只读核实状态"],"retainedUserFacts":[],"evidenceEventIds":[id]})
}

#[tokio::test]
async fn retained_user_fact_survives_five_forgetful_resummaries_and_sqlite_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("facts.sqlite3");
    let f = Fixture::with_store(Arc::new(Store::open(&path).unwrap())).await;
    let quote = "恢复演练的报告编号是 QSB-7042";
    let evidence = f.append(Payload::UserMessage {
        text: format!("补充公开事实：{quote}，请保留。"),
    });
    let mut reply = document(evidence.id);
    reply["retainedUserFacts"] = json!([{"eventId":evidence.id,"quote":quote}]);
    f.respond(Reply::Text(reply.to_string()));
    let draft = f.summarize().await.unwrap();
    assert!(!draft.content.summary.contains("QSB-7042"));
    assert_eq!(draft.content.retained_user_facts[0].quote, quote);
    f.repo
        .save_checkpoint(&draft.snapshot, draft.content)
        .unwrap();

    for _ in 0..5 {
        let next = f.append(Payload::UserMessage {
            text: "只讨论下一轮日志排查，不复述其他话题。".into(),
        });
        // The model again omits the fact from both prose and new facts. The host must retain it.
        f.respond(Reply::Text(document(next.id).to_string()));
        let draft = f.summarize().await.unwrap();
        assert_eq!(draft.content.retained_user_facts.len(), 1);
        assert_eq!(draft.content.retained_user_facts[0].quote, quote);
        assert!(draft.content.evidence_event_ids.contains(&evidence.id));
        assert_eq!(
            f.source()["previousCheckpoint"]["content"]["retainedUserFacts"][0]["quote"],
            quote
        );
        let mut dropped = draft.content.clone();
        dropped.retained_user_facts.clear();
        assert!(f.repo.save_checkpoint(&draft.snapshot, dropped).is_err());
        assert_eq!(f.snapshot(), draft.snapshot);
        f.repo
            .save_checkpoint(&draft.snapshot, draft.content)
            .unwrap();
    }
    let session = f.session.id.clone();
    let raw = f.events();
    drop(f);
    let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
    let checkpoint = repo.checkpoint(&session, "alice").unwrap().unwrap();
    assert_eq!(checkpoint.content.retained_user_facts[0].quote, quote);
    assert_eq!(repo.events(&session, "alice", 0, 200).unwrap(), raw);
}

#[tokio::test]
async fn retained_facts_reject_fabricated_assistant_foreign_future_or_oversized_quotes() {
    let f = Fixture::new().await;
    let original = f.append(Payload::UserMessage {
        text: "公开报告编号 QSB-7042".into(),
    });
    let assistant = f.append(Payload::AssistantMessage {
        text: "模型猜测报告编号 QSB-9999".into(),
    });
    let foreign = f.repo.create_session("model", "alice").unwrap();
    let foreign_event = f
        .repo
        .append(
            &foreign.id,
            "alice",
            0,
            "foreign-fact",
            Payload::UserMessage {
                text: "foreign-fact".into(),
            },
        )
        .unwrap()
        .event;
    let cases = [
        json!([{"eventId":original.id,"quote":"QSB-9999"}]),
        json!([{"eventId":assistant.id,"quote":"QSB-9999"}]),
        json!([{"eventId":foreign_event.id,"quote":"foreign-fact"}]),
        json!([{"eventId":u64::MAX,"quote":"QSB-7042"}]),
        json!([{"eventId":original.id,"quote":"x".repeat(513)}]),
        json!([{"eventId":original.id,"quote":" "}]),
        json!([{"eventId":original.id,"quote":"QSB-7042","permission":"execute"}]),
    ];
    let snapshot = f.snapshot();
    let raw = f.events();
    for facts in cases {
        let mut reply = document(original.id);
        reply["retainedUserFacts"] = facts;
        f.respond(Reply::Text(reply.to_string()));
        assert!(matches!(f.summarize().await, Err(Error::InvalidSummary)));
        f.assert_unchanged(&snapshot, &raw);
    }
    let mut missing = document(original.id);
    missing.as_object_mut().unwrap().remove("retainedUserFacts");
    f.respond(Reply::Text(missing.to_string()));
    assert!(matches!(f.summarize().await, Err(Error::InvalidSummary)));
}

#[test]
fn legacy_checkpoints_read_without_facts_and_fact_quota_is_not_silently_truncated() {
    let legacy = json!({"task":null,"summary":"old summary","nextSteps":[],"evidenceEventIds":[],"pendingToolCallIds":[],"uncertainToolCallIds":[]});
    let mut content: CheckpointContent = serde_json::from_value(legacy).unwrap();
    assert!(content.retained_user_facts.is_empty());
    let store = Arc::new(Store::in_memory().unwrap());
    let repo = Repository::new(store);
    let session = repo.create_session("model", "alice").unwrap();
    let text = (0..33)
        .map(|i| format!("public-fact-{i};"))
        .collect::<String>();
    let event = repo
        .append(
            &session.id,
            "alice",
            0,
            "facts",
            Payload::UserMessage { text },
        )
        .unwrap()
        .event;
    content.evidence_event_ids.push(event.id);
    content.retained_user_facts = (0..32)
        .map(|i| RetainedUserFact {
            event_id: event.id,
            quote: format!("public-fact-{i};"),
        })
        .collect();
    let snap = repo.snapshot(&session.id, "alice").unwrap();
    repo.save_checkpoint(&snap, content.clone()).unwrap();
    content.retained_user_facts.push(RetainedUserFact {
        event_id: event.id,
        quote: "public-fact-32;".into(),
    });
    let snap = repo.snapshot(&session.id, "alice").unwrap();
    assert!(repo.save_checkpoint(&snap, content).is_err());
    assert_eq!(repo.snapshot(&session.id, "alice").unwrap(), snap);
    assert_eq!(
        repo.checkpoint(&session.id, "alice")
            .unwrap()
            .unwrap()
            .content
            .retained_user_facts
            .len(),
        32
    );
}

#[tokio::test]
async fn semantic_draft_uses_owner_model_and_cannot_change_task_or_pending_unknown_bindings() {
    let f = Fixture::new().await;
    f.append(Payload::ToolCall {
        call_id: "unknown".into(),
        tool_name: "ssh".into(),
        arguments: json!({"command":"inspect logs"}),
    });
    f.append(Payload::ToolResult {
        call_id: "unknown".into(),
        outcome: Outcome::Unknown,
        exit_code: None,
        text: "结果未知，禁止盲目重试".into(),
        outputs: vec![],
    });
    f.append(Payload::ToolCall {
        call_id: "pending".into(),
        tool_name: "ssh".into(),
        arguments: json!({"command":"read status"}),
    });
    let before = f.snapshot();
    let original = f.events();
    let draft = f.summarize().await.unwrap();
    assert_eq!(draft.snapshot, before);
    assert_eq!(draft.content.task, before.session.task);
    assert_eq!(draft.content.pending_tool_call_ids, vec!["pending"]);
    assert_eq!(
        draft.content.uncertain_tool_call_ids,
        vec!["unknown", "pending"]
    );
    assert_eq!(draft.usage.unwrap().total_tokens, 140);
    assert_eq!(draft.model, "summary-test-model");
    let source = f.source();
    assert_eq!(source["trustedTask"]["target"]["connectionId"], "host-a");
    assert_eq!(source["pendingToolCallIds"], json!(["pending"]));
    assert_eq!(
        source["uncertainToolCallIds"],
        json!(["unknown", "pending"])
    );
    let requests = f.mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    let request = &requests[0].0;
    assert_eq!(request["stream"], false);
    assert_eq!(request["max_completion_tokens"], 2048);
    assert!(request.get("tools").is_none());
    assert!(request["messages"]
        .as_array()
        .unwrap()
        .iter()
        .all(|m| m["role"] == "system" || m["role"] == "user"));
    drop(requests);
    f.assert_unchanged(&before, &original); // Generation alone must not commit.
}

#[tokio::test]
async fn malformed_summary_matrix_is_rejected_without_log_changes_or_retry() {
    let values = vec![
        json!({"summary":"","nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":[2]}),
        json!({"summary":"x","nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":[]}),
        json!({"summary":"x","nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":[0]}),
        json!({"summary":"x","nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":[2,2]}),
        json!({"summary":"x","nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":[999999]}),
        json!({"summary":"x","nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":[2],"permission":"allow-all"}),
        json!({"summary":"x","nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":[2],"task":{"target":"other"}}),
        json!({"summary":"x","retainedUserFacts":[],"evidenceEventIds":[2]}),
        json!({"summary":"x","nextSteps":[" "],"retainedUserFacts":[],"evidenceEventIds":[2]}),
        json!({"summary":"x","nextSteps":vec!["step";33],"retainedUserFacts":[],"evidenceEventIds":[2]}),
        json!({"summary":"x","nextSteps":["x".repeat(4001)],"retainedUserFacts":[],"evidenceEventIds":[2]}),
        json!({"summary":"🙂".repeat(4001),"nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":[2]}),
        json!({"summary":"x","nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":vec![2;201]}),
    ];
    let mut cases: Vec<String> = values.into_iter().map(|v| v.to_string()).collect();
    cases.extend([
        "{invalid-private-response".into(),
        "null".into(),
        "~~~json\n{\"summary\":\"x\"}\n~~~".into(),
        r#"{"summary":"x","summary":"duplicate","nextSteps":[],"retainedUserFacts":[],"evidenceEventIds":[2]}"#.into(),
    ]);
    for text in cases {
        let f = Fixture::new().await;
        let before = f.snapshot();
        let events = f.events();
        f.respond(Reply::Text(text));
        let error = f.summarize().await.unwrap_err();
        assert!(matches!(error, Error::InvalidSummary), "{error:?}");
        assert!(!error.to_string().contains("private-response"));
        assert_eq!(f.mock.requests.lock().unwrap().len(), 1);
        f.assert_unchanged(&before, &events);
    }
}
#[tokio::test]
async fn foreign_evidence_even_before_the_frontier_cannot_be_cited() {
    let f = Fixture::new().await;
    let other = f.repo.create_session("model", "bob").unwrap();
    let foreign = f
        .repo
        .append(
            &other.id,
            "bob",
            0,
            "foreign",
            Payload::UserMessage {
                text: "another owner".into(),
            },
        )
        .unwrap()
        .event;
    f.append(Payload::UserMessage {
        text: "same owner new observation".into(),
    });
    assert!(foreign.id < f.snapshot().through_event_id);
    f.respond(Reply::Text(document(foreign.id).to_string()));
    assert!(matches!(f.summarize().await, Err(Error::InvalidSummary)));
    assert!(matches!(
        Summarizer::new(f.store.clone())
            .summarize(&f.session.id, "bob", &CancellationToken::new())
            .await,
        Err(Error::Context(shellpilot_server::agent::Error::NotFound))
    ));
    assert_eq!(f.mock.requests.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn refused_tool_or_truncated_provider_reply_is_never_a_checkpoint() {
    for response in [
        envelope(Some(document(2).to_string()), Some("refusal")),
        json!({"choices":[{"index":0,"message":{"role":"assistant","content":document(2).to_string()},"finish_reason":"length"}]}),
        json!({"choices":[{"index":0,"message":{"role":"assistant","tool_calls":[{"id":"call","type":"function","function":{"name":"run","arguments":"{}"}}]},"finish_reason":"tool_calls"}]}),
        json!({"choices":[{"index":0,"message":{"role":"assistant","content":document(2).to_string()},"finish_reason":"stop"}],
            "usage":{"prompt_tokens":100,"completion_tokens":40,"total_tokens":1}}),
    ] {
        let f = Fixture::new().await;
        let before = f.snapshot();
        let events = f.events();
        f.respond(Reply::Envelope(response));
        assert!(f.summarize().await.is_err());
        assert_eq!(f.mock.requests.lock().unwrap().len(), 1);
        f.assert_unchanged(&before, &events);
    }
}
#[tokio::test]
async fn changed_user_input_cancel_or_target_during_generation_invalidates_the_draft() {
    for mode in ["message", "cancel", "target"] {
        let f = Fixture::new().await;
        f.mock.held.store(true, Ordering::SeqCst);
        let store = f.store.clone();
        let id = f.session.id.clone();
        let run = tokio::spawn(async move {
            Summarizer::new(store)
                .summarize(&id, "alice", &CancellationToken::new())
                .await
        });
        tokio::time::timeout(Duration::from_secs(2), f.mock.received.notified())
            .await
            .unwrap();
        match mode {
            "message" => {
                f.append(Payload::UserMessage {
                    text: "增加约束，不要修改配置".into(),
                });
            }
            "cancel" => {
                f.append(Payload::Cancelled {
                    reason: "用户停止".into(),
                });
            }
            _ => {
                f.repo
                    .start_task(
                        &f.session.id,
                        "alice",
                        f.snapshot().session.revision,
                        "new-target",
                        TaskRequest {
                            goal: "另一个目标".into(),
                            constraints: vec!["只读".into()],
                            target: Some(Target {
                                connection_id: "host-b".into(),
                                terminal_session_id: "ssh-b".into(),
                            }),
                        },
                    )
                    .unwrap();
            }
        }
        let newer = f.snapshot();
        let events = f.events();
        f.mock.release.notify_one();
        assert!(matches!(run.await.unwrap(), Err(Error::ContextChanged)));
        f.assert_unchanged(&newer, &events);
    }
}
#[tokio::test]
async fn previous_checkpoint_is_input_and_explicit_cas_commit_keeps_raw_output_after_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("agent.sqlite3");
    let f = Fixture::with_store(Arc::new(Store::open(&path).unwrap())).await;
    f.append(Payload::ToolCall {
        call_id: "output".into(),
        tool_name: "ssh".into(),
        arguments: json!({"command":"read logs"}),
    });
    let mut writer = match f
        .repo
        .begin_output(
            &f.session.id,
            "alice",
            CaptureRequest {
                request_id: "capture".into(),
                call_id: "output".into(),
                channel: OutputChannel::Stdout,
            },
        )
        .unwrap()
    {
        CaptureStart::New(writer) => writer,
        _ => panic!("fresh output expected"),
    };
    let raw = "日志🙂\n".repeat(1000).into_bytes();
    writer.write(&raw).unwrap();
    let output = writer.finish().unwrap();
    f.repo
        .append_captured_result(
            &f.session.id,
            "alice",
            f.snapshot().session.revision,
            "captured-result",
            CapturedResult {
                call_id: "output".into(),
                outcome: Outcome::Unknown,
                exit_code: None,
                output_ids: vec![output.reference.id.clone()],
            },
        )
        .unwrap();
    let snapshot = f.snapshot();
    let evidence = f.events().last().unwrap().id;
    let old = f
        .repo
        .save_checkpoint(
            &snapshot,
            CheckpointContent {
                task: snapshot.session.task.clone(),
                summary: "旧摘要保留未知输出".into(),
                next_steps: vec!["核实结果".into()],
                evidence_event_ids: vec![evidence],
                retained_user_facts: vec![],
                pending_tool_call_ids: snapshot.pending_tool_call_ids.clone(),
                uncertain_tool_call_ids: snapshot.uncertain_tool_call_ids.clone(),
            },
        )
        .unwrap();
    f.append(Payload::UserMessage {
        text: "继续，但不要重复命令".into(),
    });
    let events = f.events();
    let draft = f.summarize().await.unwrap();
    let source = f.source();
    assert_eq!(source["previousCheckpoint"]["id"], old.id);
    assert_eq!(source["events"].as_array().unwrap().len(), 1);
    assert!(source["availableEvidenceEventIds"]
        .as_array()
        .unwrap()
        .contains(&json!(evidence)));
    assert_eq!(
        f.repo.checkpoint(&f.session.id, "alice").unwrap().unwrap(),
        old
    );
    assert_eq!(draft.content.uncertain_tool_call_ids, vec!["output"]);
    assert!(draft.content.evidence_event_ids.contains(&evidence));
    // C1 never calls this. A future C2 caller must check its watermarks and explicitly commit.
    let saved = f
        .repo
        .save_checkpoint(&draft.snapshot, draft.content)
        .unwrap();
    let id = f.session.id.clone();
    drop(f);
    let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
    assert_eq!(repo.checkpoint(&id, "alice").unwrap().unwrap(), saved);
    assert_eq!(repo.events(&id, "alice", 0, 200).unwrap(), events);
    assert_eq!(
        repo.read_output_range(&id, "alice", &output.reference.id, 0, 65536)
            .unwrap()
            .bytes,
        raw
    );
}
#[tokio::test]
async fn later_context_change_still_requires_repository_cas_even_after_valid_generation() {
    let f = Fixture::new().await;
    let draft = f.summarize().await.unwrap();
    f.append(Payload::UserMessage {
        text: "新的请求必须保留".into(),
    });
    let newer = f.snapshot();
    assert!(matches!(
        f.repo.save_checkpoint(&draft.snapshot, draft.content),
        Err(shellpilot_server::agent::Error::Stale)
    ));
    assert_eq!(f.snapshot(), newer);
    assert!(f.repo.checkpoint(&f.session.id, "alice").unwrap().is_none());
}
#[tokio::test]
async fn input_count_serialized_bytes_and_model_window_are_checked_before_network() {
    for mode in ["count", "bytes", "window"] {
        let f = Fixture::new().await;
        match mode {
            "count" => {
                for _ in 0..201 {
                    f.append(Payload::UserMessage {
                        text: "evidence".into(),
                    });
                }
            }
            "bytes" => {
                for _ in 0..34 {
                    f.append(Payload::UserMessage {
                        text: "x".repeat(16000),
                    });
                }
            }
            _ => f.model_config(
                |c| {
                    c.context_window = 256;
                    c.output_reserve = 64;
                    c.safety_reserve = 32;
                },
                KeyChange::Keep,
            ),
        }
        let before = f.snapshot();
        let error = f.summarize().await.unwrap_err();
        assert!(
            matches!(
                error,
                Error::InputLimit | Error::Model(ModelError::BudgetExceeded)
            ),
            "{mode}: {error:?}"
        );
        assert!(f.mock.requests.lock().unwrap().is_empty());
        assert_eq!(f.snapshot(), before);
    }
}
#[tokio::test]
async fn empty_source_and_pre_cancel_never_make_a_model_call() {
    let f = Fixture::new().await;
    let empty = f.repo.create_session("model", "alice").unwrap();
    assert!(matches!(
        Summarizer::new(f.store.clone())
            .summarize(&empty.id, "alice", &CancellationToken::new())
            .await,
        Err(Error::EmptySource)
    ));
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        Summarizer::new(f.store.clone())
            .summarize(&f.session.id, "alice", &token)
            .await,
        Err(Error::Model(ModelError::Cancelled))
    ));
    assert!(f.mock.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn cancellation_timeout_and_http_error_keep_source_and_do_not_retry() {
    for mode in ["cancel", "timeout", "http"] {
        let f = Fixture::new().await;
        let before = f.snapshot();
        let events = f.events();
        if mode == "http" {
            f.respond(Reply::Http);
        } else {
            f.mock.held.store(true, Ordering::SeqCst);
        }
        if mode == "timeout" {
            f.model_config(|c| c.timeout_seconds = 1, KeyChange::Keep);
        }
        let token = CancellationToken::new();
        let cancel = token.clone();
        let store = f.store.clone();
        let id = f.session.id.clone();
        let run =
            tokio::spawn(
                async move { Summarizer::new(store).summarize(&id, "alice", &token).await },
            );
        tokio::time::timeout(Duration::from_secs(2), f.mock.received.notified())
            .await
            .unwrap();
        if mode == "cancel" {
            cancel.cancel();
        }
        let error = tokio::time::timeout(Duration::from_secs(3), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap_err();
        assert!(
            matches!(
                error,
                Error::Model(ModelError::Cancelled | ModelError::Timeout | ModelError::Http(503))
            ),
            "{error:?}"
        );
        assert!(!error.to_string().contains("provider-private-body"));
        assert_eq!(f.mock.requests.lock().unwrap().len(), 1);
        f.assert_unchanged(&before, &events);
    }
}
#[cfg(windows)]
#[tokio::test]
async fn dpapi_key_is_header_only_and_not_added_to_summary_source_draft_or_events() {
    let f = Fixture::new().await;
    let key = "summary-fixture-key";
    f.model_config(
        |c| c.require_api_key = true,
        KeyChange::Set {
            value: SecretKey::new(key).unwrap(),
        },
    );
    let draft = f.summarize().await.unwrap();
    let requests = f.mock.requests.lock().unwrap();
    assert_eq!(requests[0].1["authorization"], "Bearer summary-fixture-key");
    assert!(!requests[0].0.to_string().contains(key));
    assert!(!format!("{draft:?}").contains(key));
    assert!(!serde_json::to_string(&f.events()).unwrap().contains(key));
}
#[tokio::test]
async fn active_text_turn_is_busy_and_failed_partial_is_explicit_evidence_after_it_finishes() {
    let f = Fixture::new().await;
    let manager = Arc::new(chat::Manager::new(
        f.store.clone(),
        CancellationToken::new(),
    ));
    let request = chat::Request {
        request_id: "active".into(),
        agent_id: "model".into(),
        user_id: "alice".into(),
        session_id: f.session.id.clone(),
        terminal_session_id: "ssh-a".into(),
        message: "生成文字".into(),
        attachments: vec![],
    };
    let target = f.snapshot().session.task.unwrap().target;
    let token = CancellationToken::new();
    let cancel = token.clone();
    let copy = manager.clone();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let mut sender = Some(sender);
    let run = tokio::spawn(async move {
        copy.chat(request, target, &token, move |event| {
            if matches!(event, chat::StreamEvent::Text { .. }) {
                if let Some(sender) = sender.take() {
                    let _ = sender.send(());
                }
            }
            async { Ok(()) }
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(2), receiver)
        .await
        .unwrap()
        .unwrap();
    let before = f.snapshot();
    assert!(matches!(f.summarize().await, Err(Error::Busy)));
    assert_eq!(f.mock.requests.lock().unwrap().len(), 1);
    assert_eq!(f.snapshot(), before);
    cancel.cancel();
    run.await.unwrap().unwrap_err();
    let draft = f.summarize().await.unwrap();
    assert!(draft.content.task.is_some());
    let source = f.source();
    assert_eq!(source["failedTextTurns"][0]["state"], "cancelled");
    assert_eq!(
        source["failedTextTurns"][0]["partialText"],
        "partial evidence"
    );
    assert_eq!(source["failedTextTurns"][0]["errorCode"], "AGENT_CANCELLED");
    assert_eq!(f.mock.requests.lock().unwrap().len(), 2);
    manager.shutdown();
}
#[tokio::test]
async fn eof_partial_stays_failed_evidence_and_never_becomes_completed_by_summary() {
    let f = Fixture::new().await;
    f.respond(Reply::StreamEof);
    let manager = chat::Manager::new(f.store.clone(), CancellationToken::new());
    let target = f.snapshot().session.task.unwrap().target;
    let request = chat::Request {
        request_id: "failed".into(),
        agent_id: "model".into(),
        user_id: "alice".into(),
        session_id: f.session.id.clone(),
        terminal_session_id: "ssh-a".into(),
        message: "failed input".into(),
        attachments: vec![],
    };
    assert_eq!(
        manager
            .chat(request, target, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
            .unwrap_err()
            .code(),
        "MODEL_INCOMPLETE"
    );
    f.respond(Reply::Valid);
    f.summarize().await.unwrap();
    let source = f.source();
    assert_eq!(source["failedTextTurns"][0]["state"], "failed");
    assert_eq!(
        source["failedTextTurns"][0]["errorCode"],
        "MODEL_INCOMPLETE"
    );
    assert_eq!(
        source["failedTextTurns"][0]["partialText"],
        "partial evidence"
    );
    let history = manager
        .history("model".into(), "alice".into(), f.session.id.clone(), 0, 20)
        .await
        .unwrap();
    assert_eq!(history.turns[0].state, chat::TurnState::Failed);
    assert!(history.turns[0].result_text.is_none());
}

#[tokio::test]
async fn deepseek_both_protocols_use_local_semantic_compaction_without_tools() {
    for protocol in [ProviderKind::OpenAiCompatible, ProviderKind::AnthropicMessages] {
        let f=Fixture::new().await;
        let evidence=f.append(Payload::UserMessage{text:"请保留测试目标".into()});
        f.model_config(|config|{config.supplier=Some(SupplierKind::Deepseek);config.provider=protocol.clone();config.completions_path=if protocol==ProviderKind::AnthropicMessages {"messages"}else{"chat/completions"}.into();config.reasoning_effort=Some(ReasoningEffort::High);config.capabilities.reasoning_effort=true;},KeyChange::Keep);
        let summary=document(evidence.id).to_string();
        f.respond(Reply::Envelope(if protocol==ProviderKind::AnthropicMessages {json!({"type":"message","role":"assistant","content":[{"type":"text","text":summary}],"stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":40}})}else{envelope(Some(summary),None)}));
        let before=f.events();let draft=f.summarize().await.unwrap();assert_eq!(draft.model,"summary-test-model");assert_eq!(f.events(),before);
        let requests=f.mock.requests.lock().unwrap();assert_eq!(requests.len(),1);let body=&requests[0].0;
        assert_eq!(body["max_tokens"],2048);assert!(body.get("max_completion_tokens").is_none());assert!(body.get("tools").is_none());assert_eq!(body["thinking"]["type"],"enabled");
    }
}

#[tokio::test]
async fn glm_both_protocols_use_local_semantic_compaction_without_tools() {
    for protocol in [ProviderKind::OpenAiCompatible, ProviderKind::AnthropicMessages] {
        let f=Fixture::new().await;
        let evidence=f.append(Payload::UserMessage{text:"保留 GLM 测试目标".into()});
        f.model_config(|config|{config.supplier=Some(SupplierKind::Glm);config.provider=protocol.clone();config.completions_path=if protocol==ProviderKind::AnthropicMessages {"messages"}else{"chat/completions"}.into();config.reasoning_effort=Some(ReasoningEffort::High);config.capabilities.reasoning_effort=true;},KeyChange::Keep);
        let summary=document(evidence.id).to_string();
        f.respond(Reply::Envelope(if protocol==ProviderKind::AnthropicMessages {json!({"type":"message","role":"assistant","content":[{"type":"text","text":summary}],"stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":40}})}else{envelope(Some(summary),None)}));
        let before=f.events();let draft=f.summarize().await.unwrap();assert_eq!(draft.model,"summary-test-model");assert_eq!(f.events(),before);
        let requests=f.mock.requests.lock().unwrap();assert_eq!(requests.len(),1);let body=&requests[0].0;
        assert_eq!(body["max_tokens"],2048);assert!(body.get("max_completion_tokens").is_none());assert!(body.get("tools").is_none());assert_eq!(body["thinking"]["type"],"enabled");
    }
}

#[tokio::test]
async fn qwen_both_protocols_collect_streaming_semantic_summary_without_tools() {
    for protocol in [ProviderKind::OpenAiCompatible,ProviderKind::AnthropicMessages] {
        let f=Fixture::new().await;let evidence=f.append(Payload::UserMessage{text:"保留 Qwen 测试目标".into()});
        f.model_config(|config|{config.supplier=Some(SupplierKind::Qwen);config.provider=protocol.clone();config.completions_path=if protocol==ProviderKind::AnthropicMessages {"messages"}else{"chat/completions"}.into();config.reasoning_effort=Some(ReasoningEffort::Enabled);config.capabilities.reasoning_effort=true;},KeyChange::Keep);
        let summary=document(evidence.id).to_string();
        f.respond(Reply::StreamEnvelope(if protocol==ProviderKind::AnthropicMessages {json!({"type":"message","role":"assistant","content":[{"type":"text","text":summary}],"stop_reason":"end_turn"})}else{envelope(Some(summary),None)}));
        let before=f.events();let draft=f.summarize().await.unwrap();assert_eq!(draft.model,"summary-test-model");assert_eq!(f.events(),before);
        let requests=f.mock.requests.lock().unwrap();assert_eq!(requests.len(),1);let body=&requests[0].0;
        assert_eq!(body["stream"],true);assert_eq!(body["max_tokens"],2048);assert!(body.get("tools").is_none());assert!(body.get("reasoning_effort").is_none());assert!(body.get("output_config").is_none());
        if protocol==ProviderKind::AnthropicMessages {assert_eq!(body["thinking"]["type"],"enabled");}else{assert_eq!(body["enable_thinking"],true);}
    }
}

#[tokio::test]
async fn kimi_both_protocols_use_local_semantic_summary_and_native_effort() {
    for protocol in [ProviderKind::OpenAiCompatible,ProviderKind::AnthropicMessages] {
        let f=Fixture::new().await;let evidence=f.append(Payload::UserMessage{text:"保留 Kimi 测试目标".into()});
        f.model_config(|config|{config.supplier=Some(SupplierKind::Kimi);config.provider=protocol.clone();config.completions_path=if protocol==ProviderKind::AnthropicMessages {"messages"}else{"chat/completions"}.into();config.reasoning_effort=Some(ReasoningEffort::High);config.capabilities.reasoning_effort=true;config.temperature=None;},KeyChange::Keep);
        let summary=document(evidence.id).to_string();
        f.respond(Reply::Envelope(if protocol==ProviderKind::AnthropicMessages {json!({"type":"message","role":"assistant","content":[{"type":"text","text":summary}],"stop_reason":"end_turn","usage":{"input_tokens":100,"output_tokens":40}})}else{envelope(Some(summary),None)}));
        let before=f.events();let draft=f.summarize().await.unwrap();assert_eq!(draft.model,"summary-test-model");assert_eq!(f.events(),before);
        let requests=f.mock.requests.lock().unwrap();assert_eq!(requests.len(),1);let body=&requests[0].0;
        assert_eq!(body["max_tokens"],2048);assert!(body.get("tools").is_none());assert!(body.get("thinking").is_none());assert!(body.get("temperature").is_none());
        if protocol==ProviderKind::AnthropicMessages {assert_eq!(body["output_config"]["effort"],"high");}else{assert_eq!(body["reasoning_effort"],"high");}
    }
}

#[tokio::test]
async fn grok_chat_uses_local_semantic_summary_with_native_parameters() {
    let f=Fixture::new().await;let evidence=f.append(Payload::UserMessage{text:"保留 Grok 测试目标".into()});
    f.model_config(|config|{config.supplier=Some(SupplierKind::Grok);config.reasoning_effort=Some(ReasoningEffort::High);config.capabilities.reasoning_effort=true;},KeyChange::Keep);
    f.respond(Reply::Envelope(envelope(Some(document(evidence.id).to_string()),None)));
    let before=f.events();f.summarize().await.unwrap();assert_eq!(f.events(),before);
    let requests=f.mock.requests.lock().unwrap();assert_eq!(requests.len(),1);let body=&requests[0].0;
    assert_eq!(body["max_completion_tokens"],2048);assert_eq!(body["reasoning_effort"],"high");assert!(body.get("tools").is_none());assert!(body.get("thinking").is_none());
}

#[tokio::test]
async fn ollama_uses_local_semantic_summary_without_vendor_specific_thinking_fields() {
    let f=Fixture::new().await;let evidence=f.append(Payload::UserMessage{text:"保留本地 Ollama 测试目标".into()});
    f.model_config(|config|{config.supplier=Some(SupplierKind::Ollama);config.model="qwen3:8b".into();config.require_api_key=false;config.reasoning_effort=Some(ReasoningEffort::Low);config.capabilities.reasoning_effort=true;},KeyChange::Keep);
    f.respond(Reply::Envelope(envelope(Some(document(evidence.id).to_string()),None)));
    let before=f.events();f.summarize().await.unwrap();assert_eq!(f.events(),before);
    let requests=f.mock.requests.lock().unwrap();assert_eq!(requests.len(),1);let body=&requests[0].0;
    assert_eq!(body["max_tokens"],2048);assert_eq!(body["reasoning_effort"],"low");
    for field in ["tools","thinking","enable_thinking","options","think","max_completion_tokens"] {assert!(body.get(field).is_none());}
}
