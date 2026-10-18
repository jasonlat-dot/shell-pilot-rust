use axum::{
    body::{Body, Bytes},
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        approvals::Policy,
        chat::{Manager, Request, StreamEvent},
        extensions::{
            self,
            config::{Config, Server, Settings, Transport},
        },
        provider::*,
        Payload, Repository,
    },
    store::Store,
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[tokio::test]
async fn late_compaction_finds_a_fitting_older_prefix_and_preserves_mcp_evidence() {
    check_late_compaction(false).await;
}

#[tokio::test]
async fn late_compaction_retry_explains_oversized_fact_rejection_without_relaxing_validation() {
    check_late_compaction(true).await;
}

async fn check_late_compaction(reject_first: bool) {
    let mut first=calls("catalog","list_mcp_tools",json!({"server":"mock"}));
    first["choices"][0]["message"]["content"]=json!("Planning the read-only inventory query. ".repeat(30));
    let f=Fixture::new(vec![first,
        calls("lookup","call_mcp_tool",json!({"server":"mock","tool":"lookup","argumentsJson":"{\"query\":\"hello\"}"})),
        final_reply("beta 的查询完成，未重启。")]).await;
    f.wire.auto_summary.store(true,Ordering::SeqCst);
    f.wire.reject_first_summary.store(reject_first, Ordering::SeqCst);
    let models=ModelRepository::new(f.store.clone());
    let old=models.get("agent","default").unwrap();
    let mut config=old.config;
    config.context_window=16384;config.output_reserve=8192;config.safety_reserve=512;config.compact_percent=99;
    models.save("agent","default",old.revision,ModelConfigUpdate{config,api_key:KeyChange::Keep}).unwrap();
    let request=f.request(Policy::FullAccess).await;
    let repo=Repository::new(f.store.clone());
    let mut texts=vec!["目标 beta；禁止重启服务。".to_string()];
    texts.extend((0..10).map(|i|format!("历史背景 {i}: {}","archived telemetry row=ok latency=12ms batch=closed; ".repeat(26))));
    for (i,text) in texts.into_iter().enumerate() {
        let revision=repo.session(&request.session_id,"default").unwrap().revision;
        repo.append(&request.session_id,"default",revision,&format!("seed-user-{i}"),Payload::UserMessage{text}).unwrap();
        let revision=repo.session(&request.session_id,"default").unwrap().revision;
        repo.append(&request.session_id,"default",revision,&format!("seed-answer-{i}"),Payload::AssistantMessage{text:"已记录背景。".into()}).unwrap();
    }
    f.manager.chat_desktop(request.clone(),None,true,&CancellationToken::new(),|_|async{Ok(())}).await.unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst),1);
    let checkpoint=repo.checkpoint(&request.session_id,"default").unwrap().unwrap();
    assert!(checkpoint.content.retained_user_facts.iter().any(|f|f.quote.contains("目标 beta")));
    let bodies=f.wire.bodies.lock().unwrap();
    let summaries=bodies.iter().filter(|b|b["tools"].as_array().is_none_or(Vec::is_empty)).count();
    assert!((1..=2).contains(&summaries));
    if reject_first {
        let attempts: Vec<_> = bodies.iter().filter(|b| b["tools"].as_array().is_none_or(Vec::is_empty)).collect();
        assert_eq!(attempts.len(), 2);
        assert!(attempts[1]["messages"][0]["content"].as_str().unwrap().contains("fact_quote_byte_limit"), "retry must explain the actual validation failure");
        assert!(checkpoint.content.retained_user_facts.iter().all(|fact| fact.quote.len() <= 512));
    }
    drop(bodies);
    let history=repo.events(&request.session_id,"default",0,200).unwrap();
    assert!(history.iter().any(|e|matches!(&e.payload,Payload::ToolResult{call_id,..} if call_id=="lookup")));
    assert!(shellpilot_server::agent::budget::group_history(&history).unwrap().iter().all(|g|g.complete));
}

fn calls(id: &str, name: &str, args: Value) -> Value {
    json!({"choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}}]},"finish_reason":"tool_calls"}]})
}
fn final_reply(text: &str) -> Value {
    json!({"choices":[{"index":0,"message":{"role":"assistant","content":text},"finish_reason":"stop"}]})
}
#[derive(Clone)]
struct Wire {
    auto_summary: Arc<AtomicBool>,
    reject_first_summary: Arc<AtomicBool>,
    replies: Arc<Mutex<VecDeque<Value>>>,
    bodies: Arc<Mutex<Vec<Value>>>,
    executed: Arc<AtomicUsize>,
    children_entered: Arc<AtomicUsize>,
    child_release: Arc<Mutex<Option<CancellationToken>>>,
    mcp_release: Arc<Mutex<Option<CancellationToken>>>,
    legacy: Arc<Mutex<HashMap<String, tokio::sync::mpsc::Sender<Bytes>>>>,
    bearer_headers: Arc<Mutex<Vec<Option<String>>>>,
    custom_headers: Arc<Mutex<Vec<Option<String>>>>,
}
async fn model(State(w): State<Wire>, Json(body): Json<Value>) -> Json<Value> {
    let child = body["messages"].as_array().is_some_and(|m| {
        m.iter().any(|m| {
            m["role"] == "user"
                && m["content"]
                    .as_str()
                    .is_some_and(|s| s.starts_with("委派子任务："))
        })
    });
    w.bodies.lock().unwrap().push(body.clone());
    if w.auto_summary.load(Ordering::SeqCst) && body["tools"].as_array().is_none_or(Vec::is_empty) {
        let source:Value=serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
        let mut facts:Vec<_>=source["events"].as_array().unwrap().iter().filter_map(|e| {
            let text=e["payload"]["text"].as_str()?;
            (e["payload"]["type"]=="userMessage"&&text.starts_with("目标 beta"))
                .then(||json!({"eventId":e["id"],"quote":text}))
        }).collect();
        if w.reject_first_summary.swap(false, Ordering::SeqCst) {
            let event = source["events"].as_array().unwrap().iter().find(|event|
                event["payload"]["type"] == "userMessage" && event["payload"]["text"].as_str().is_some_and(|text| text.len() > 512)).unwrap();
            facts.push(json!({"eventId":event["id"],"quote":event["payload"]["text"]}));
        }
        return Json(final_reply(&json!({"summary":"已确认目标 beta，禁止重启，历史遥测是背景证据。","nextSteps":["继续只读查询"],"retainedUserFacts":facts,"evidenceEventIds":source["availableEvidenceEventIds"]}).to_string()));
    }
    let response = w
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected model replay");
    let release = w.child_release.lock().unwrap().clone();
    if child {
        if let Some(release) = release {
            w.children_entered.fetch_add(1, Ordering::SeqCst);
            release.cancelled().await;
        }
    }
    Json(response)
}
fn record_mcp_headers(w: &Wire, headers: &HeaderMap) {
    w.custom_headers.lock().unwrap().push(
        headers
            .get("x-api-key")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    );
    w.bearer_headers.lock().unwrap().push(
        headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned),
    );
}
async fn mcp(State(w): State<Wire>, headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    record_mcp_headers(&w, &headers);
    if body["method"] == "tools/call" && body["params"]["arguments"]["query"] == "malformed" {
        w.executed.fetch_add(1, Ordering::SeqCst);
        return Json(json!({"jsonrpc":"2.0","id":body["id"],"result":{"isError":"false"}}));
    }
    if body["method"] == "tools/call" && body["params"]["arguments"]["query"] == "unknown" {
        w.executed.fetch_add(1, Ordering::SeqCst);
        return Json(
            json!({"jsonrpc":"2.0","id":body["id"],"error":{"code":-32000,"message":"remote outcome unavailable"}}),
        );
    }
    let result = match body["method"].as_str().unwrap_or("") {
        "initialize" => {
            json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"mock","version":"1"}})
        }
        "tools/list" => {
            json!({"tools":[{"name":"lookup","description":"sample","annotations":{"readOnlyHint":true},"inputSchema":{"type":"object","properties":{"query":{"type":"string"},"limit":{"type":"integer","minimum":1}},"required":["query"],"additionalProperties":false}}]})
        }
        "tools/call" => {
            w.executed.fetch_add(1, Ordering::SeqCst);
            let release = w.mcp_release.lock().unwrap().clone();
            if let Some(release) = release {
                release.cancelled().await;
            }
            json!({"content":[{"type":"text","text":if body["params"]["arguments"]["query"]=="large"{"汉🙂".repeat(5000)}else{"lookup-result".into()}}],"isError":false})
        }
        _ => json!({}),
    };
    Json(json!({"jsonrpc":"2.0","id":body["id"],"result":result}))
}
async fn legacy_get(State(w): State<Wire>, headers: HeaderMap) -> Response {
    record_mcp_headers(&w, &headers);
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    tx.send(Bytes::from(format!(
        "event: endpoint\ndata: /messages?session={id}&api_key=fixture\n\n"
    )))
    .await
    .unwrap();
    w.legacy.lock().unwrap().insert(id, tx);
    let stream = futures_util::stream::unfold(rx, |mut rx| async move {
        rx.recv()
            .await
            .map(|bytes| (Ok::<_, std::io::Error>(bytes), rx))
    });
    Response::builder()
        .header("content-type", "text/event-stream")
        .body(Body::from_stream(stream))
        .unwrap()
}
async fn legacy_post(
    State(w): State<Wire>,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    assert_eq!(query["api_key"], "fixture");
    let tx = w
        .legacy
        .lock()
        .unwrap()
        .get(&query["session"])
        .unwrap()
        .clone();
    if body.get("id").is_some() {
        let response = mcp(State(w), headers, Json(body)).await.0;
        let _ = tx
            .send(Bytes::from(format!("event: message\ndata: {response}\n\n")))
            .await;
    }
    StatusCode::ACCEPTED.into_response()
}
struct Fixture {
    store: Arc<Store>,
    manager: Arc<Manager>,
    wire: Wire,
    server: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.manager.shutdown();
        self.server.abort();
    }
}
impl Fixture {
    async fn new(replies: Vec<Value>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let wire = Wire {
            auto_summary: Arc::new(AtomicBool::new(false)),
            reject_first_summary: Arc::new(AtomicBool::new(false)),
            replies: Arc::new(Mutex::new(replies.into())),
            bodies: Arc::new(Mutex::new(vec![])),
            executed: Arc::new(AtomicUsize::new(0)),
            children_entered: Arc::new(AtomicUsize::new(0)),
            child_release: Arc::new(Mutex::new(None)),
            mcp_release: Arc::new(Mutex::new(None)),
            legacy: Arc::new(Mutex::new(HashMap::new())),
            bearer_headers: Arc::default(),
            custom_headers: Arc::default(),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(model))
            .route("/mcp", post(mcp))
            .route("/sse", get(legacy_get))
            .route("/messages", post(legacy_post))
            .with_state(wire.clone());
        let server = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        let store = Arc::new(Store::in_memory().unwrap());
        ModelRepository::new(store.clone())
            .save(
                "agent",
                "default",
                0,
                ModelConfigUpdate {
                    config: ModelConfig {
                        provider: ProviderKind::OpenAiCompatible,
                        remote_compaction: false,
                        base_url: format!("{base}/v1/"),
                        completions_path: "chat/completions".into(),
                        model: "mock".into(),
                        context_window: 32768,
                        output_reserve: 512,
                        safety_reserve: 256,
                        compact_percent: 90,
                        output_token_parameter: OutputTokenParameter::MaxCompletionTokens,
                        capabilities: ModelCapabilities::default(),
                        temperature: None,
                        reasoning_effort: None,
                        timeout_seconds: 3,
                        max_response_bytes: 65536,
                        require_api_key: false,
                    },
                    api_key: KeyChange::Keep,
                },
            )
            .unwrap();
        let manager = Arc::new(Manager::new(store.clone(), CancellationToken::new()));
        extensions::save(
            &store,
            "default",
            Settings {
                revision: 0,
                config: Config {
                    servers: vec![Server {
                        name: "mock".into(),
                        description: String::new(),
                        connection_values: None,
                        saved_headers: vec![],
                        saved_env: vec![],
                        bearer_token: Default::default(),
                        bearer_token_configured: false,
                        enabled: true,
                        transport: Transport::StreamableHttp {
                            url: format!("{base}/mcp"),
                            bearer_env: String::new(),
                        },
                    }],
                    skill_roots: vec![],
                    sub_agents: true,
                    ..Config::default()
                },
            },
        )
        .unwrap();
        Self {
            store,
            manager,
            wire,
            server,
        }
    }
    async fn request(&self, mode: Policy) -> Request {
        let session = self
            .manager
            .create_general_session_with_policy("agent".into(), "default".into(), Some(mode))
            .await
            .unwrap();
        Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "agent".into(),
            user_id: "default".into(),
            session_id: session.id,
            terminal_session_id: String::new(),
            message: "按当前任务查询数据".into(),
            attachments: vec![],
        }
    }
    fn use_legacy_sse(&self) {
        let mut settings = extensions::settings(&self.store, "default").unwrap();
        let Transport::StreamableHttp { url, .. } = &settings.config.servers[0].transport else {
            panic!("expected HTTP fixture");
        };
        settings.config.servers[0].transport = Transport::Sse {
            url: format!("{}/sse?api_key=fixture", url.strip_suffix("/mcp").unwrap()),
            bearer_env: String::new(),
            timeout_seconds: 3,
        };
        extensions::save(&self.store, "default", settings).unwrap();
    }
}
#[tokio::test]
async fn ordinary_chat_executes_mcp_under_full_access_without_registering_ssh_and_replays_no_side_effects(
) {
    let f = Fixture::new(vec![
        calls(
            "mcp1",
            "call_mcp_tool",
            json!({"server":"mock","tool":"lookup","argumentsJson":"{\"query\":\"hello\"}"}),
        ),
        final_reply("done"),
    ])
    .await;
    let request = f.request(Policy::FullAccess).await;
    f.manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
    {
        let bodies = f.wire.bodies.lock().unwrap();
        assert_eq!(bodies.len(), 2);
        assert!(!bodies[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|t| t["function"]["name"] == "ssh_execute"));
    }
    f.manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
    let events = Repository::new(f.store.clone())
        .events(&request.session_id, "default", 0, 200)
        .unwrap();
    assert!(events.iter().any(|e| matches!(
        &e.payload,
        Payload::ToolResult {
            outcome: shellpilot_server::agent::Outcome::Success,
            ..
        }
    )));
}
#[tokio::test]
async fn confirm_waits_for_exact_human_decision_and_deny_never_sends_mcp_call() {
    let f = Fixture::new(vec![
        calls(
            "mcp1",
            "call_mcp_tool",
            json!({"server":"mock","tool":"lookup","argumentsJson":"{\"query\":\"hello\"}"}),
        ),
        final_reply("denied"),
    ])
    .await;
    let request = f.request(Policy::Confirm).await;
    let m = f.manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
    });
    let receipt = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let rows = f
                .manager
                .extension_requests("agent".into(), "default".into(), request.session_id.clone())
                .await
                .unwrap();
            if let Some(r) = rows.into_iter().find(|r| r.state == "pending") {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
    assert!(f
        .manager
        .extension_decision(
            "agent".into(),
            "other".into(),
            request.session_id.clone(),
            receipt.id.clone(),
            true
        )
        .await
        .is_err());
    f.manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            receipt.id,
            false,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn child_inherits_general_target_and_live_tools_and_keeps_nested_raw_calls_paired() {
    let f = Fixture::new(vec![
        calls(
            "parent",
            "delegate_agent",
            json!({"task":"查询工具目录","context":"保持当前目标"}),
        ),
        calls("child1", "list_mcp_tools", json!({"server":""})),
        calls("child2", "list_skills", json!({})),
        final_reply("child report"),
        final_reply("parent report"),
    ])
    .await;
    let request = f.request(Policy::Confirm).await;
    let events = Arc::new(Mutex::new(vec![]));
    let out = events.clone();
    f.manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            move |e| {
                out.lock().unwrap().push(e);
                async { Ok(()) }
            },
        )
        .await
        .unwrap();
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, StreamEvent::AgentStart { .. })));
    assert!(events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e,StreamEvent::AgentResult{status,..} if status=="success")));
    let snapshot = Repository::new(f.store.clone())
        .snapshot(&request.session_id, "default")
        .unwrap();
    assert!(snapshot.pending_tool_call_ids.is_empty());
    assert!(snapshot.session.task.unwrap().target.is_none());
    let bodies = f.wire.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 5);
    assert!(!bodies[1]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .any(|t| t["function"]["name"] == "delegate_agent"));
}
#[test]
fn settings_compare_and_swap_and_roundtrip_preserve_transports() {
    let store = Store::in_memory().unwrap();
    let value = Settings {
        revision: 0,
        config: Config {
            servers: vec![Server {
                name: "local".into(),
                description: String::new(),
                connection_values: None,
                saved_headers: vec![],
                saved_env: vec![],
                bearer_token: Default::default(),
                bearer_token_configured: false,
                enabled: false,
                transport: Transport::Stdio {
                    command: "example".into(),
                    args: vec!["--stdio".into()],
                    env: vec!["TOKEN".into()],
                },
            }],
            skill_roots: vec![],
            sub_agents: false,
            ..Config::default()
        },
    };
    let encoded = serde_json::to_value(&value).unwrap();
    let decoded: Settings = serde_json::from_value(encoded).unwrap();
    let saved = extensions::save(&store, "default", decoded).unwrap();
    assert_eq!(saved.revision, 1);
    assert!(extensions::save(&store, "default", value).is_err());
    let loaded = extensions::settings(&store, "default").unwrap();
    assert!(loaded.config == saved.config);
}

#[cfg(windows)]
#[tokio::test]
async fn mcp_client_token_flows_from_encrypted_storage_to_http_headers_without_entering_model_context(
) {
    let f = Fixture::new(vec![
        calls("direct-token", "call_mcp_tool", lookup("hello")),
        final_reply("done"),
    ])
    .await;
    let settings = extensions::settings(&f.store, "default").unwrap();
    let mut raw = serde_json::to_value(settings).unwrap();
    raw["config"]["servers"][0]["bearerToken"] =
        json!({"action":"set","value":"fixture-client-token/a+b.c"});
    let saved =
        extensions::save(&f.store, "default", serde_json::from_value(raw).unwrap()).unwrap();
    assert!(saved.config.servers[0].bearer_token_configured);
    assert!(!serde_json::to_string(&saved)
        .unwrap()
        .contains("fixture-client-token"));
    let request = f.request(Policy::FullAccess).await;
    f.manager
        .chat_desktop(request, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
    let headers = f.wire.bearer_headers.lock().unwrap();
    assert!(headers.len() >= 3);
    assert!(headers
        .iter()
        .all(|value| value.as_deref() == Some("Bearer fixture-client-token/a+b.c")));
    assert!(!serde_json::to_string(&*f.wire.bodies.lock().unwrap())
        .unwrap()
        .contains("fixture-client-token"));
}

#[cfg(windows)]
#[tokio::test]
async fn json_mcp_headers_use_saved_values_for_http_and_sse_without_leaking_into_model_messages() {
    for legacy in [false, true] {
        let f = Fixture::new(vec![
            calls("json-header", "call_mcp_tool", lookup("hello")),
            final_reply("done"),
        ])
        .await;
        if legacy {
            f.use_legacy_sse();
        }
        let mut settings =
            serde_json::to_value(extensions::settings(&f.store, "default").unwrap()).unwrap();
        settings["config"]["servers"][0]["description"] = json!("实时搜索");
        settings["config"]["servers"][0]["connectionValues"] = json!({"headers":{"Authorization":"Bearer fixture-json-token","X-API-Key":"fixture-custom-key"},"env":{}});
        let saved = extensions::save(
            &f.store,
            "default",
            serde_json::from_value(settings).unwrap(),
        )
        .unwrap();
        let rendered = serde_json::to_string(&saved).unwrap();
        assert!(!rendered.contains("fixture-json-token"));
        assert!(!rendered.contains("fixture-custom-key"));
        assert_eq!(
            saved.config.servers[0].saved_headers,
            vec!["authorization", "x-api-key"]
        );
        let request = f.request(Policy::FullAccess).await;
        f.manager
            .chat_desktop(request, None, true, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
            .unwrap();
        assert!(f
            .wire
            .bearer_headers
            .lock()
            .unwrap()
            .iter()
            .all(|v| v.as_deref() == Some("Bearer fixture-json-token")));
        assert!(f
            .wire
            .custom_headers
            .lock()
            .unwrap()
            .iter()
            .all(|v| v.as_deref() == Some("fixture-custom-key")));
        let bodies = serde_json::to_string(&*f.wire.bodies.lock().unwrap()).unwrap();
        assert!(!bodies.contains("fixture-json-token"));
        assert!(!bodies.contains("fixture-custom-key"));
        assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
    }
}

#[test]
fn legacy_sse_config_preserves_literal_key_and_validates_timeout_and_transport_rules() {
    let store = Store::in_memory().unwrap();
    let raw = json!({"revision":0,"config":{"servers":[{"name":"baidu-search","enabled":true,"transport":"sse","url":"http://appbuilder.baidu.com/v2/ai_search/mcp/sse?api_key=Bearer+fixture"}],"skillRoots":[],"subAgents":false}});
    let value: Settings = serde_json::from_value(raw.clone()).unwrap();
    let saved = extensions::save(&store, "default", value).unwrap();
    let encoded = serde_json::to_value(extensions::settings(&store, "default").unwrap()).unwrap();
    assert_eq!(
        encoded["config"]["servers"][0]["url"],
        raw["config"]["servers"][0]["url"]
    );
    assert_eq!(encoded["config"]["servers"][0]["timeoutSeconds"], 45);
    assert_eq!(saved.revision, 1);
    for timeout in [0, 301] {
        let mut bad = raw.clone();
        bad["config"]["servers"][0]["timeoutSeconds"] = json!(timeout);
        assert!(serde_json::from_value::<Settings>(bad)
            .unwrap()
            .config
            .validate()
            .is_err());
    }
    let mut modern = raw.clone();
    modern["config"]["servers"][0]["transport"] = json!("streamableHttp");
    assert!(serde_json::from_value::<Settings>(modern)
        .unwrap()
        .config
        .validate()
        .is_err());
    for url in [
        "file:///sse",
        "https://user:secret@example.com/sse",
        "https://example.com/sse#fragment",
    ] {
        let mut bad = raw.clone();
        bad["config"]["servers"][0]["url"] = json!(url);
        assert!(serde_json::from_value::<Settings>(bad)
            .unwrap()
            .config
            .validate()
            .is_err());
    }
}

#[tokio::test]
async fn legacy_sse_call_requires_exact_approval_and_executes_only_once() {
    let f = Fixture::new(vec![
        calls("legacy", "call_mcp_tool", lookup("hello")),
        final_reply("done"),
    ])
    .await;
    f.use_legacy_sse();
    let request = f.request(Policy::Confirm).await;
    let manager = f.manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        manager
            .chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
    });
    let approval = pending(&f, &request).await;
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
    f.manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id.clone(),
            true,
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
    assert!(f
        .manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id,
            true
        )
        .await
        .is_err());
    assert_eq!(f.wire.bodies.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn legacy_sse_lost_result_is_unknown_without_replay() {
    let f = Fixture::new(vec![calls("legacy", "call_mcp_tool", lookup("unknown"))]).await;
    f.use_legacy_sse();
    let request = f.request(Policy::FullAccess).await;
    assert!(f
        .manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) }
        )
        .await
        .is_err());
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
    let rows = f
        .manager
        .extension_requests("agent".into(), "default".into(), request.session_id.clone())
        .await
        .unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].state, "unknown");
    assert_eq!(f.wire.bodies.lock().unwrap().len(), 1);
}

async fn pending(f: &Fixture, request: &Request) -> extensions::Receipt {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(r) = f
                .manager
                .extension_requests("agent".into(), "default".into(), request.session_id.clone())
                .await
                .unwrap()
                .into_iter()
                .find(|r| r.state == "pending")
            {
                break r;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap()
}
fn lookup(query: &str) -> Value {
    json!({"server":"mock","tool":"lookup","argumentsJson":json!({"query":query}).to_string()})
}

fn batch(items: Vec<Value>) -> Value {
    let mut result = items[0].clone();
    for item in items.into_iter().skip(1) {
        result["choices"][0]["message"]["tool_calls"]
            .as_array_mut()
            .unwrap()
            .extend(
                item["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .unwrap()
                    .clone(),
            );
    }
    result
}

#[tokio::test]
async fn discovered_readonly_mcp_services_overlap_but_each_call_still_needs_exact_approval() {
    let mut other = lookup("second");
    other["server"] = json!("other");
    let f = Fixture::new(vec![
        batch(vec![
            calls("catalog-a", "list_mcp_tools", json!({"server":"mock"})),
            calls("catalog-b", "list_mcp_tools", json!({"server":"other"})),
        ]),
        batch(vec![
            calls("read-a", "call_mcp_tool", lookup("first")),
            calls("read-b", "call_mcp_tool", other),
        ]),
        final_reply("done"),
    ])
    .await;
    let mut settings = extensions::settings(&f.store, "default").unwrap();
    let mut second = settings.config.servers[0].clone();
    second.name = "other".into();
    settings.config.servers.push(second);
    extensions::save(&f.store, "default", settings).unwrap();
    let release = CancellationToken::new();
    *f.wire.mcp_release.lock().unwrap() = Some(release.clone());
    let request = f.request(Policy::Confirm).await;
    let m = f.manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
    });
    let approvals = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let rows = f
                .manager
                .extension_requests("agent".into(), "default".into(), request.session_id.clone())
                .await
                .unwrap();
            if rows.iter().filter(|r| r.state == "pending").count() == 2 {
                break rows;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("independent services must both reach approval before either is approved");
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
    assert_ne!(approvals[0].id, approvals[1].id);
    for approval in approvals {
        f.manager
            .extension_decision(
                "agent".into(),
                "default".into(),
                request.session_id.clone(),
                approval.id,
                true,
            )
            .await
            .unwrap();
    }
    tokio::time::timeout(Duration::from_secs(4), async {
        while f.wire.executed.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("both transports must enter before either completes");
    release.cancel();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let receipts = f
        .manager
        .extension_requests("agent".into(), "default".into(), request.session_id)
        .await
        .unwrap();
    assert_eq!(receipts.len(), 2);
    assert!(receipts.iter().all(|r| r.state == "completed"));
}

#[tokio::test]
async fn same_mcp_service_queues_transport_work_without_showing_the_second_call_as_running() {
    let f = Fixture::new(vec![
        calls("catalog", "list_mcp_tools", json!({"server":"mock"})),
        batch(vec![
            calls("first", "call_mcp_tool", lookup("one")),
            calls("second", "call_mcp_tool", lookup("two")),
        ]),
        final_reply("done"),
    ])
    .await;
    let release = CancellationToken::new();
    *f.wire.mcp_release.lock().unwrap() = Some(release.clone());
    let request = f.request(Policy::FullAccess).await;
    let m = f.manager.clone();
    let r = request.clone();
    let events = Arc::new(Mutex::new(vec![]));
    let frames = events.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), move |e| {
            frames.lock().unwrap().push(e);
            async { Ok(()) }
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(4), async {
        while f.wire.executed.load(Ordering::SeqCst) < 1 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(
                |e| matches!(e,StreamEvent::ToolCall{tool_name,..} if tool_name=="call_mcp_tool")
            )
            .count(),
        1
    );
    let history = f
        .manager
        .history(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            0,
            20,
        )
        .await
        .unwrap();
    assert!(history.turns[0]
        .started_tool_call_ids
        .iter()
        .any(|id| id == "first"));
    assert!(!history.turns[0]
        .started_tool_call_ids
        .iter()
        .any(|id| id == "second"));
    release.cancel();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn children_overlap_queued_tools_stay_hidden_and_local_call_ids_do_not_collide() {
    let mut batch = calls(
        "left",
        "delegate_agent",
        json!({"task":"Docker 状态","context":"只分析"}),
    );
    for next in [
        calls(
            "right",
            "delegate_agent",
            json!({"task":"磁盘状态","context":"只分析"}),
        ),
        calls(
            "plan",
            "update_plan",
            json!({"explanation":"后续计划","plan":[{"step":"记录结果","status":"completed"}]}),
        ),
        calls("skills", "list_skills", json!({})),
        calls("mcp", "list_mcp_tools", json!({"server":""})),
    ] {
        batch["choices"][0]["message"]["tool_calls"]
            .as_array_mut()
            .unwrap()
            .extend(
                next["choices"][0]["message"]["tool_calls"]
                    .as_array()
                    .unwrap()
                    .clone(),
            );
    }
    let f = Fixture::new(vec![
        batch,
        calls("same-local-id", "list_skills", json!({})),
        calls("same-local-id", "list_skills", json!({})),
        final_reply("child report"),
        final_reply("child report"),
        final_reply("parent report"),
    ])
    .await;
    let release = CancellationToken::new();
    *f.wire.child_release.lock().unwrap() = Some(release.clone());
    let request = f.request(Policy::Confirm).await;
    let m = f.manager.clone();
    let r = request.clone();
    let events = Arc::new(Mutex::new(vec![]));
    let frames = events.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), move |e| {
            frames.lock().unwrap().push(e);
            async { Ok(()) }
        })
        .await
    });
    tokio::time::timeout(Duration::from_secs(3), async {
        while f.wire.children_entered.load(Ordering::SeqCst) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("both children must start before either finishes");
    {
        let frames = events.lock().unwrap();
        assert_eq!(
            frames
                .iter()
                .filter(|e| matches!(e, StreamEvent::AgentStart { .. }))
                .count(),
            2
        );
        assert!(
            !frames.iter().any(
                |e| matches!(e,StreamEvent::ToolCall{tool_name,..} if tool_name!="delegate_agent")
            ),
            "queued plan/Skills/MCP must not be announced as running"
        );
    }
    let history = f
        .manager
        .history(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            0,
            20,
        )
        .await
        .unwrap();
    assert_eq!(history.turns[0].started_tool_call_ids.len(), 2);
    release.cancel();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let events = Repository::new(f.store.clone())
        .events(&request.session_id, "default", 0, 200)
        .unwrap();
    let child_ids = events
        .iter()
        .filter_map(|e| match &e.payload {
            Payload::ToolCall {
                call_id, arguments, ..
            } if arguments.get("parentToolCallId").is_some() => Some(call_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(child_ids.len(), 2);
    assert_ne!(child_ids[0], child_ids[1]);
    assert!(child_ids.iter().all(|id| id.len() <= 128));
    assert!(Repository::new(f.store.clone())
        .snapshot(&request.session_id, "default")
        .unwrap()
        .pending_tool_call_ids
        .is_empty());
}

#[tokio::test]
async fn child_mcp_call_inherits_human_approval_and_parent_cancellation() {
    let f = Fixture::new(vec![
        calls(
            "parent",
            "delegate_agent",
            json!({"task":"查询数据","context":"使用 mock lookup"}),
        ),
        calls("child", "call_mcp_tool", lookup("hello")),
    ])
    .await;
    let request = f.request(Policy::Confirm).await;
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let m = f.manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &token, |_| async { Ok(()) })
            .await
    });
    let approval = pending(&f, &request).await;
    assert!(approval.call_id.starts_with("child_"));
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
    cancel.cancel();
    assert!(tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert!(f
        .manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id,
            true
        )
        .await
        .is_err());
    let snapshot = Repository::new(f.store.clone())
        .snapshot(&request.session_id, "default")
        .unwrap();
    assert!(snapshot.pending_tool_call_ids.is_empty());
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn child_mcp_cannot_outlive_a_parent_policy_revocation() {
    let f = Fixture::new(vec![
        calls(
            "parent",
            "delegate_agent",
            json!({"task":"查询数据","context":"使用 mock lookup"}),
        ),
        calls("child", "call_mcp_tool", lookup("hello")),
        final_reply("child denied"),
        final_reply("parent denied"),
    ])
    .await;
    let request = f.request(Policy::Confirm).await;
    let m = f.manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
    });
    let approval = pending(&f, &request).await;
    let policy = f
        .manager
        .execution_policy(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            None,
        )
        .await
        .unwrap();
    f.manager
        .execution_policy(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            Some((policy["revision"].as_u64().unwrap(), Policy::Deny)),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(f
        .manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id,
            true
        )
        .await
        .is_err());
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
    assert!(Repository::new(f.store.clone())
        .snapshot(&request.session_id, "default")
        .unwrap()
        .pending_tool_call_ids
        .is_empty());
}

#[tokio::test]
async fn malformed_mcp_success_envelope_is_unknown_and_never_claims_success() {
    let f = Fixture::new(vec![calls(
        "malformed",
        "call_mcp_tool",
        lookup("malformed"),
    )])
    .await;
    let request = f.request(Policy::FullAccess).await;
    assert!(f
        .manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) }
        )
        .await
        .is_err());
    let receipts = f
        .manager
        .extension_requests("agent".into(), "default".into(), request.session_id)
        .await
        .unwrap();
    assert_eq!(receipts[0].state, "unknown");
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn disabling_delegation_keeps_existing_child_history_readable_but_denies_new_children() {
    let f = Fixture::new(vec![
        calls(
            "first-child",
            "delegate_agent",
            json!({"task":"简短分析","context":"无需工具"}),
        ),
        final_reply("child report"),
        final_reply("parent report"),
        calls(
            "disabled-child",
            "delegate_agent",
            json!({"task":"不能启动","context":""}),
        ),
        final_reply("delegation disabled"),
    ])
    .await;
    let mut request = f.request(Policy::Confirm).await;
    f.manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    let mut settings = extensions::settings(&f.store, "default").unwrap();
    settings.config.sub_agents = false;
    extensions::save(&f.store, "default", settings).unwrap();
    request.request_id = uuid::Uuid::new_v4().to_string();
    request.message = "继续对话".into();
    f.manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    assert_eq!(
        f.wire.bodies.lock().unwrap().len(),
        5,
        "a disabled child must not make an independent model request"
    );
    let events = Repository::new(f.store.clone())
        .events(&request.session_id, "default", 0, 200)
        .unwrap();
    assert!(events.iter().any(|e|matches!(&e.payload,Payload::ToolResult{call_id,text,..} if call_id=="disabled-child"&&text.contains("denied"))));
}
#[tokio::test]
async fn human_approval_is_exact_and_consumed_once() {
    let f = Fixture::new(vec![
        calls("mcp1", "call_mcp_tool", lookup("hello")),
        final_reply("done"),
    ])
    .await;
    let request = f.request(Policy::Confirm).await;
    let m = f.manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
    });
    let approval = pending(&f, &request).await;
    assert_eq!(approval.arguments["argumentsJson"]["query"], "hello");
    f.manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id.clone(),
            true,
        )
        .await
        .unwrap();
    assert!(f
        .manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id,
            true
        )
        .await
        .is_err());
    run.await.unwrap().unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
}
#[tokio::test]
async fn cancelling_while_waiting_cannot_leave_a_reusable_pending_approval() {
    let f = Fixture::new(vec![calls("mcp1", "call_mcp_tool", lookup("hello"))]).await;
    let request = f.request(Policy::Confirm).await;
    let m = f.manager.clone();
    let r = request.clone();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &token, |_| async { Ok(()) })
            .await
    });
    let approval = pending(&f, &request).await;
    cancel.cancel();
    assert!(run.await.unwrap().is_err());
    let rows = f
        .manager
        .extension_requests("agent".into(), "default".into(), request.session_id.clone())
        .await
        .unwrap();
    assert_eq!(rows[0].state, "cancelled");
    assert!(f
        .manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id,
            true
        )
        .await
        .is_err());
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn changing_policy_revokes_pending_extension_call_before_dispatch() {
    let f = Fixture::new(vec![
        calls("mcp1", "call_mcp_tool", lookup("hello")),
        final_reply("not executed"),
    ])
    .await;
    let request = f.request(Policy::Confirm).await;
    let m = f.manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
    });
    let approval = pending(&f, &request).await;
    let policy = f
        .manager
        .execution_policy(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            None,
        )
        .await
        .unwrap();
    f.manager
        .execution_policy(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            Some((policy["revision"].as_u64().unwrap(), Policy::Deny)),
        )
        .await
        .unwrap();
    run.await.unwrap().unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
    assert!(f
        .manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id,
            approval.id,
            true
        )
        .await
        .is_err());
}
#[tokio::test]
async fn changing_server_configuration_cannot_reuse_an_old_call_approval() {
    let f = Fixture::new(vec![
        calls("mcp1", "call_mcp_tool", lookup("hello")),
        final_reply("not executed"),
    ])
    .await;
    let request = f.request(Policy::Confirm).await;
    let m = f.manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
    });
    let approval = pending(&f, &request).await;
    let mut settings = extensions::settings(&f.store, "default").unwrap();
    settings.config.servers[0].enabled = false;
    extensions::save(&f.store, "default", settings).unwrap();
    assert!(f
        .manager
        .extension_decision(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id,
            true
        )
        .await
        .is_err());
    run.await.unwrap().unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn mcp_unknown_outcome_halts_the_turn_and_cannot_be_replayed() {
    let f = Fixture::new(vec![calls("mcp1", "call_mcp_tool", lookup("unknown"))]).await;
    let request = f.request(Policy::FullAccess).await;
    assert!(f
        .manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) }
        )
        .await
        .is_err());
    assert!(f
        .manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) }
        )
        .await
        .is_err());
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
    assert_eq!(f.wire.bodies.lock().unwrap().len(), 1);
    let rows = f
        .manager
        .extension_requests("agent".into(), "default".into(), request.session_id)
        .await
        .unwrap();
    assert_eq!(rows[0].state, "unknown");
}
#[tokio::test]
async fn large_mcp_result_is_saved_as_original_task_output_with_a_bounded_preview() {
    let f = Fixture::new(vec![
        calls("mcp1", "call_mcp_tool", lookup("large")),
        final_reply("done"),
    ])
    .await;
    let request = f.request(Policy::FullAccess).await;
    f.manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    let events = Repository::new(f.store.clone())
        .events(&request.session_id, "default", 0, 200)
        .unwrap();
    let result = events
        .iter()
        .find_map(|e| match &e.payload {
            Payload::ToolResult { text, outputs, .. } if !outputs.is_empty() => {
                Some((text, outputs))
            }
            _ => None,
        })
        .unwrap();
    assert!(result.0.len() < 8192);
    assert!(result.1.iter().any(|o| o.size > 10000));
    let output = f
        .manager
        .execution_output(
            "agent".into(),
            "default".into(),
            request.session_id.clone(),
            result.1[0].id.clone(),
            0,
            65536,
        )
        .await
        .unwrap();
    assert!(output["bytes"].as_array().unwrap().len() > 10000);
}
#[tokio::test]
async fn auto_review_uses_the_same_isolated_reviewer_and_then_dispatches() {
    let f = Fixture::new(vec![
        calls("mcp1", "call_mcp_tool", lookup("hello")),
        final_reply(r#"{"decision":"allow","risk":"low","reason":"只读取当前任务相关的数据"}"#),
        final_reply("done"),
    ])
    .await;
    let request = f.request(Policy::Auto).await;
    f.manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    assert_eq!(f.wire.executed.load(Ordering::SeqCst), 1);
    let bodies = f.wire.bodies.lock().unwrap();
    assert_eq!(bodies.len(), 3);
    assert!(bodies[1].get("tools").is_none());
}

#[test]
fn execution_settings_upgrade_legacy_json_and_validate_all_configured_bounds() {
    let legacy: Settings = serde_json::from_value(
        json!({"revision":0,"config":{"servers":[],"skillRoots":[],"subAgents":true}}),
    )
    .unwrap();
    assert_eq!(legacy.config.execution_limits.model_rounds, 32);
    assert_eq!(legacy.config.execution_limits.tool_calls, 128);
    assert_eq!(legacy.config.execution_limits.child_model_rounds, 24);
    assert_eq!(legacy.config.execution_limits.child_tool_calls, 64);
    assert_eq!(legacy.config.execution_limits.max_child_tasks, 6);
    for (key, bad) in [
        ("modelRounds", 1),
        ("modelRounds", 257),
        ("toolCalls", 0),
        ("toolCalls", 1025),
        ("childModelRounds", 0),
        ("childModelRounds", 257),
        ("childToolCalls", 0),
        ("childToolCalls", 1025),
        ("maxChildTasks", 0),
        ("maxChildTasks", 17),
    ] {
        let mut value = serde_json::to_value(&legacy).unwrap();
        value["config"]["executionLimits"][key] = json!(bad);
        let decoded: Settings = serde_json::from_value(value).unwrap();
        assert!(
            extensions::save(&Store::in_memory().unwrap(), "default", decoded).is_err(),
            "{key}={bad}"
        );
    }
    let store = Store::in_memory().unwrap();
    let mut custom = legacy;
    custom.config.execution_limits.model_rounds = 80;
    custom.config.execution_limits.tool_calls = 300;
    custom.config.execution_limits.child_model_rounds = 48;
    extensions::save(&store, "default", custom).unwrap();
    assert_eq!(
        extensions::settings(&store, "default")
            .unwrap()
            .config
            .execution_limits
            .model_rounds,
        80
    );
    assert_eq!(
        extensions::settings(&store, "other")
            .unwrap()
            .config
            .execution_limits
            .model_rounds,
        32
    );
}

#[tokio::test]
async fn desktop_uses_saved_main_budgets_and_distinguishes_round_and_call_exhaustion() {
    for rounds in [true, false] {
        let replies = if rounds {
            vec![
                calls("a", "list_skills", json!({})),
                calls("b", "list_skills", json!({})),
            ]
        } else {
            vec![batch(vec![
                calls("a", "list_skills", json!({})),
                calls("b", "list_skills", json!({})),
            ])]
        };
        let f = Fixture::new(replies).await;
        let mut settings = extensions::settings(&f.store, "default").unwrap();
        if rounds {
            settings.config.execution_limits.model_rounds = 2;
        } else {
            settings.config.execution_limits.tool_calls = 1;
        }
        extensions::save(&f.store, "default", settings).unwrap();
        let request = f.request(Policy::Confirm).await;
        let error = f
            .manager
            .chat_desktop(request, None, true, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
            .unwrap_err();
        assert_eq!(
            error.code(),
            if rounds {
                "AGENT_MODEL_ROUND_LIMIT"
            } else {
                "AGENT_TOOL_CALL_LIMIT"
            }
        );
        assert_eq!(
            f.wire.bodies.lock().unwrap().len(),
            if rounds { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn child_uses_its_saved_quota_independently_of_the_parent() {
    let f = Fixture::new(vec![
        calls(
            "parent",
            "delegate_agent",
            json!({"task":"inspect","context":""}),
        ),
        batch(vec![
            calls("a", "list_skills", json!({})),
            calls("b", "list_skills", json!({})),
        ]),
        final_reply("child exceeded its own tool quota"),
    ])
    .await;
    let mut settings = extensions::settings(&f.store, "default").unwrap();
    settings.config.execution_limits.child_tool_calls = 1;
    extensions::save(&f.store, "default", settings).unwrap();
    let request = f.request(Policy::Confirm).await;
    let events = Arc::new(Mutex::new(vec![]));
    let frames = events.clone();
    f.manager
        .chat_desktop(
            request,
            None,
            true,
            &CancellationToken::new(),
            move |event| {
                frames.lock().unwrap().push(event);
                async { Ok(()) }
            },
        )
        .await
        .unwrap();
    assert!(events.lock().unwrap().iter().any(|e|matches!(e,StreamEvent::AgentResult{status,content,..} if status=="error"&&content.contains("工具调用次数上限"))));
    assert_eq!(f.wire.bodies.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn default_main_budget_allows_more_than_eight_model_requests() {
    let mut replies = (0..10)
        .map(|i| calls(&format!("inspect-{i}"), "list_skills", json!({})))
        .collect::<Vec<_>>();
    replies.push(final_reply("finished"));
    let f = Fixture::new(replies).await;
    let request = f.request(Policy::Confirm).await;
    f.manager
        .chat_desktop(request, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
        .unwrap();
    assert_eq!(f.wire.bodies.lock().unwrap().len(), 11);
}

#[tokio::test]
async fn configured_child_count_is_enforced_without_starting_extra_children() {
    let f = Fixture::new(vec![
        batch(vec![
            calls("a", "delegate_agent", json!({"task":"one","context":""})),
            calls("b", "delegate_agent", json!({"task":"two","context":""})),
        ]),
        final_reply("one child"),
        final_reply("parent"),
    ])
    .await;
    let mut settings = extensions::settings(&f.store, "default").unwrap();
    settings.config.execution_limits.max_child_tasks = 1;
    extensions::save(&f.store, "default", settings).unwrap();
    let request = f.request(Policy::Confirm).await;
    let events = Arc::new(Mutex::new(vec![]));
    let frames = events.clone();
    f.manager
        .chat_desktop(
            request,
            None,
            true,
            &CancellationToken::new(),
            move |event| {
                frames.lock().unwrap().push(event);
                async { Ok(()) }
            },
        )
        .await
        .unwrap();
    assert_eq!(
        events
            .lock()
            .unwrap()
            .iter()
            .filter(|e| matches!(e, StreamEvent::AgentStart { .. }))
            .count(),
        1
    );
    assert_eq!(f.wire.bodies.lock().unwrap().len(), 3);
}
