use axum::{extract::State, routing::post, Json, Router};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        provider::*,
        tool_loop::{self, *},
    },
    store::Store,
};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Wire {
    replies: Arc<Mutex<VecDeque<Value>>>,
    requests: Arc<Mutex<Vec<Value>>>,
}
struct Model {
    client: ModelClient,
    wire: Wire,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Model {
    fn drop(&mut self) {
        self.task.abort();
    }
}
async fn respond(State(wire): State<Wire>, Json(body): Json<Value>) -> Json<Value> {
    wire.requests.lock().unwrap().push(body);
    Json(
        wire.replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected model retry"),
    )
}
impl Model {
    async fn new(replies: Vec<Value>, change: impl FnOnce(&mut ModelConfig)) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut config = ModelConfig {
            provider: ProviderKind::OpenAiCompatible,
            remote_compaction: false,
            base_url: format!("http://{}/v1/", listener.local_addr().unwrap()),
            completions_path: "chat/completions".into(),
            model: "loopback-test".into(),
            context_window: 16384,
            output_reserve: 512,
            safety_reserve: 256,
            compact_percent: 75,
            output_token_parameter: OutputTokenParameter::MaxCompletionTokens,
            capabilities: ModelCapabilities {
                tool_calls: true,
                ..Default::default()
            },
            temperature: None,
            reasoning_effort: None,
            timeout_seconds: 3,
            max_response_bytes: 65536,
            require_api_key: false,
        };
        change(&mut config);
        let repository = ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
        repository
            .save(
                "model",
                "test",
                0,
                ModelConfigUpdate {
                    config,
                    api_key: KeyChange::Keep,
                },
            )
            .unwrap();
        let client = ModelClient::new(repository.runtime("model", "test").unwrap()).unwrap();
        let wire = Wire {
            replies: Arc::new(Mutex::new(replies.into())),
            requests: Arc::default(),
        };
        let router = Router::new()
            .route("/v1/chat/completions", post(respond))
            .with_state(wire.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { client, wire, task }
    }
    fn requests(&self) -> Vec<Value> {
        self.wire.requests.lock().unwrap().clone()
    }
}
fn definition() -> FunctionDefinition {
    FunctionDefinition {
        name: "inspect".into(),
        description: "Inspect a bounded test item".into(),
        parameters: json!({"type":"object","properties":{
            "count":{"type":"integer","minimum":1,"maximum":4},
            "mode":{"type":"string","enum":["safe"]},
            "options":{"type":"object","properties":{"enabled":{"type":"boolean"}},"required":["enabled"],"additionalProperties":false},
            "labels":{"type":"array","items":{"type":"string","minLength":1,"maxLength":4},"maxItems":2}
        },"required":["count","mode","options","labels"],"additionalProperties":false}),
    }
}
fn registry() -> ToolRegistry {
    let mut r = ToolRegistry::default();
    r.register(definition()).unwrap();
    r
}
fn args() -> Value {
    json!({"count":1,"mode":"safe","options":{"enabled":true},"labels":["测试"]})
}
fn call(id: &str, arguments: String) -> Value {
    json!({"id":id,"type":"function","function":{"name":"inspect","arguments":arguments}})
}
fn calls(values: Vec<Value>) -> Value {
    json!({"choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":values},"finish_reason":"tool_calls"}]})
}
fn final_reply() -> Value {
    json!({"choices":[{"index":0,"message":{"role":"assistant","content":"检查完成"},"finish_reason":"stop"}]})
}
fn input() -> Vec<ChatMessage> {
    vec![ChatMessage::User {
        content: "请检查测试项".into(),
    }]
}
enum Behavior {
    Success(Value),
    Failed,
    Unknown,
    Cancel(CancellationToken),
    Hang,
}
struct Host {
    ids: Mutex<Vec<String>>,
    behavior: Behavior,
}
impl Host {
    fn new(behavior: Behavior) -> Self {
        Self {
            ids: Mutex::new(vec![]),
            behavior,
        }
    }
}
impl ToolExecutor for Host {
    fn authorize<'a>(
        &'a self,
        _call: &'a ValidatedCall,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        // This test host explicitly authorizes its synthetic in-memory tools.
        Box::pin(async move { !cancel.is_cancelled() })
    }

    fn execute<'a>(
        &'a self,
        call: &'a ValidatedCall,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            assert_eq!(call.name(), "inspect");
            assert_eq!(call.arguments()["count"], 1);
            self.ids.lock().unwrap().push(call.id().into());
            match &self.behavior {
                Behavior::Success(data) => ToolOutcome::Success(data.clone()),
                Behavior::Failed => ToolOutcome::Failed,
                Behavior::Unknown => ToolOutcome::OutcomeUnknown,
                Behavior::Cancel(cancel) => {
                    cancel.cancel();
                    std::future::pending().await
                }
                Behavior::Hang => std::future::pending().await,
            }
        })
    }
}
fn results(loop_: &ToolLoop<'_>) -> Vec<Value> {
    loop_
        .transcript()
        .messages
        .iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { content, .. } => Some(serde_json::from_str(content).unwrap()),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn managed_batch_can_cross_budget_before_host_rebuild_but_oversized_requests_are_never_sent() {
    struct Rebuild { batches: std::sync::atomic::AtomicUsize }
    impl LoopContext for Rebuild {
        fn before_model<'a>(&'a self,r:&'a ChatRequest,_:&'a CancellationToken)->BoxFuture<'a,tool_loop::Result<ChatRequest>> {
            Box::pin(async move {
                if self.batches.load(std::sync::atomic::Ordering::SeqCst)>0 {
                    Ok(ChatRequest{messages:vec![ChatMessage::User{content:"Host checkpoint: inspect completed once; report the result.".into()}],tools:r.tools.clone()})
                }else{Ok(r.clone())}
            })
        }
        fn begin_batch<'a>(&'a self,_:&'a[ToolCall],_:Option<&'a str>,_:&'a CancellationToken)->BoxFuture<'a,tool_loop::Result<()>> {
            Box::pin(async move{self.batches.fetch_add(1,std::sync::atomic::Ordering::SeqCst);Ok(())})
        }
        fn started(&self,_:&str)->tool_loop::Result<()>{Ok(())}
        fn result<'a>(&'a self,_:&'a str,_:&'a str)->BoxFuture<'a,tool_loop::Result<()>>{Box::pin(async{Ok(())})}
    }
    let mut first=calls(vec![call("near-full",args().to_string())]);
    first["choices"][0]["message"]["content"]=json!("x".repeat(6000));
    let model=Model::new(vec![first,final_reply()],|c|c.context_window=2048).await;
    let registry=registry();
    let host=Host::new(Behavior::Success(json!({"value":1})));
    let context=Rebuild{batches:Default::default()};
    let store=Arc::new(Store::in_memory().unwrap());
    let session=shellpilot_server::agent::Repository::new(store.clone())
        .create_general_session_with_policy("model","test",Some(shellpilot_server::agent::approvals::Policy::FullAccess)).unwrap();
    let mut loop_=ToolLoop::new(&registry,input(),Limits::default()).unwrap().with_context(&context)
        .with_approval_boundary(ApprovalBoundary::new(shellpilot_server::agent::approvals::Approvals::new(store),session.id,"test".into()));
    loop_.run(&model.client,&host,&CancellationToken::new()).await.unwrap();
    assert_eq!(host.ids.lock().unwrap().as_slice(),["near-full"]);
    assert_eq!(model.requests().len(),2);
    assert!(model.requests()[1]["messages"][0]["content"].as_str().unwrap().starts_with("Host checkpoint"));
}

struct ConcurrentHost {
    active: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
    ids: Mutex<Vec<String>>,
    unknown: bool,
}
impl ToolExecutor for ConcurrentHost {
    fn supports_parallel(&self, call: &ValidatedCall) -> bool {
        call.id() != "fence"
    }
    fn authorize<'a>(
        &'a self,
        _: &'a ValidatedCall,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { true })
    }
    fn execute<'a>(
        &'a self,
        call: &'a ValidatedCall,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            use std::sync::atomic::Ordering;
            let active = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(active, Ordering::SeqCst);
            self.ids.lock().unwrap().push(call.id().into());
            if call.id() == "fence" {
                assert_eq!(active, 1, "exclusive tools must run alone");
            }
            struct Guard<'a>(&'a std::sync::atomic::AtomicUsize);
            impl Drop for Guard<'_> {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _guard = Guard(&self.active);
            if self.unknown {
                if call.id() == "a" {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    return ToolOutcome::OutcomeUnknown;
                }
                cancel.cancelled().await;
                return ToolOutcome::OutcomeUnknown;
            }
            tokio::time::sleep(Duration::from_millis(if call.id() == "a" { 30 } else { 5 })).await;
            ToolOutcome::Success(json!({"id":call.id()}))
        })
    }
}
#[tokio::test]
async fn independent_calls_overlap_with_a_bound_and_exclusive_fences_preserve_protocol_order() {
    let ids = ["a", "b", "c", "d", "fence", "e", "f", "g", "h", "i"];
    let model = Model::new(
        vec![
            calls(ids.iter().map(|id| call(id, args().to_string())).collect()),
            final_reply(),
        ],
        |_| {},
    )
    .await;
    let registry = registry();
    let mut loop_ = ToolLoop::new(&registry, input(), Limits::default()).unwrap();
    let host = ConcurrentHost {
        active: 0.into(),
        peak: 0.into(),
        ids: Mutex::new(vec![]),
        unknown: false,
    };
    loop_
        .run(&model.client, &host, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(host.peak.load(std::sync::atomic::Ordering::SeqCst), 4);
    let actual = host.ids.lock().unwrap();
    assert_eq!(actual[4], "fence");
    let returned = results(&loop_);
    assert_eq!(
        returned
            .iter()
            .map(|r| r["data"]["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ids
    );
}
#[tokio::test]
async fn unknown_parallel_call_cancels_active_siblings_without_starting_queued_work() {
    let model = Model::new(
        vec![calls(
            ["a", "b", "c", "d", "e", "f"]
                .iter()
                .map(|id| call(id, args().to_string()))
                .collect(),
        )],
        |_| {},
    )
    .await;
    let registry = registry();
    let mut loop_ = ToolLoop::new(&registry, input(), Limits::default()).unwrap();
    let host = ConcurrentHost {
        active: 0.into(),
        peak: 0.into(),
        ids: Mutex::new(vec![]),
        unknown: true,
    };
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(2),
            loop_.run(&model.client, &host, &CancellationToken::new())
        )
        .await
        .unwrap(),
        Err(tool_loop::Error::OutcomeUnknown)
    ));
    assert_eq!(host.ids.lock().unwrap().len(), 4);
    assert_eq!(host.active.load(std::sync::atomic::Ordering::SeqCst), 0);
    assert_eq!(model.requests().len(), 1);
}

#[tokio::test]
async fn main_model_selects_multi_round_tools_and_receives_paired_results() {
    let model = Model::new(
        vec![
            calls(vec![
                call("a", args().to_string()),
                call("b", args().to_string()),
            ]),
            calls(vec![call("c", args().to_string())]),
            final_reply(),
        ],
        |_| {},
    )
    .await;
    let r = registry();
    let host = Host::new(Behavior::Success(json!({"status":"healthy"})));
    let mut loop_ = ToolLoop::new(&r, input(), Limits::default()).unwrap();
    let reply = loop_
        .run(&model.client, &host, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(reply.content.as_deref(), Some("检查完成"));
    assert_eq!(*host.ids.lock().unwrap(), ["a", "b", "c"]);
    assert_eq!(
        (loop_.model_rounds(), loop_.tool_calls(), loop_.state()),
        (3, 3, tool_loop::State::Finished)
    );
    let requests = model.requests();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[1]["messages"][1]["tool_calls"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(requests[1]["messages"][2]["tool_call_id"], "a");
    assert_eq!(requests[1]["messages"][3]["tool_call_id"], "b");
    assert!(requests[1]["messages"][2]["content"]
        .as_str()
        .unwrap()
        .contains("healthy"));
    assert_eq!(
        requests[2]["tools"][0]["function"]["parameters"],
        definition().parameters
    );
    prepare_request(model.client.config(), loop_.transcript()).unwrap();
    assert!(matches!(
        loop_
            .run(&model.client, &host, &CancellationToken::new())
            .await,
        Err(tool_loop::Error::AlreadyStarted)
    ));
}

#[tokio::test]
async fn invalid_arguments_never_dispatch_and_model_can_correct_them() {
    let mut bad = vec!["{}".to_string(), "{\"count\":1,\"count\":2}".into()];
    for (field, value) in [
        ("count", json!("1")),
        ("count", json!(5)),
        ("count", json!(1.0)),
        ("mode", json!("raw-secret")),
        ("extra", json!(true)),
        ("options", json!({"enabled":true,"extra":true})),
        ("labels", json!(["too long"])),
    ] {
        let mut a = args();
        a[field] = value;
        bad.push(a.to_string());
    }
    bad.push("{\"count\":1,\"mode\":\"safe\",\"options\":{\"enabled\":true,\"enabled\":false},\"labels\":[]}".into());
    let invalid_count = bad.len();
    let batch = bad
        .into_iter()
        .enumerate()
        .map(|(i, a)| call(&format!("bad-{i}"), a))
        .collect();
    let model = Model::new(
        vec![
            calls(batch),
            calls(vec![call("fixed", args().to_string())]),
            final_reply(),
        ],
        |_| {},
    )
    .await;
    let r = registry();
    let host = Host::new(Behavior::Success(json!("ok")));
    let mut loop_ = ToolLoop::new(&r, input(), Limits::default()).unwrap();
    loop_
        .run(&model.client, &host, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(*host.ids.lock().unwrap(), ["fixed"]);
    assert!(results(&loop_)[..invalid_count]
        .iter()
        .all(|r| r == &json!({"ok":false,"error":"invalid_arguments"})));
    assert_eq!(model.requests().len(), 3);
}

#[test]
fn unsupported_or_ambiguous_schemas_fail_at_registration() {
    for mutate in [0, 1, 2, 3, 4, 5] {
        let mut d = definition();
        match mutate {
            0 => d.parameters["additionalProperties"] = true.into(),
            1 => d.parameters["required"] = json!(["count"]),
            2 => d.parameters["properties"]["count"]["oneOf"] = json!([]),
            3 => d.parameters["properties"]["options"]["additionalProperties"] = true.into(),
            4 => d.parameters["properties"]["labels"]["maxItems"] = (-1).into(),
            _ => d.parameters["properties"]["count"]["minimum"] = 5.into(),
        }
        assert!(matches!(
            ToolRegistry::default().register(d),
            Err(tool_loop::Error::Invalid)
        ));
    }
    let mut r = registry();
    assert!(matches!(
        r.register(definition()),
        Err(tool_loop::Error::Invalid)
    ));
}

#[tokio::test]
async fn reused_call_id_fails_before_second_execution() {
    let response = calls(vec![call("same", args().to_string())]);
    let model = Model::new(vec![response.clone(), response], |_| {}).await;
    let r = registry();
    let host = Host::new(Behavior::Success(json!(true)));
    let mut loop_ = ToolLoop::new(&r, input(), Limits::default()).unwrap();
    assert!(matches!(
        loop_
            .run(&model.client, &host, &CancellationToken::new())
            .await,
        Err(tool_loop::Error::Model(ModelError::ToolProtocol))
    ));
    assert_eq!(host.ids.lock().unwrap().len(), 1);
    assert_eq!(model.requests().len(), 2);
    prepare_request(model.client.config(), loop_.transcript()).unwrap();
    assert_eq!(loop_.state(), tool_loop::State::Halted);
}

#[tokio::test]
async fn round_and_call_limits_do_not_start_a_batch_they_cannot_finish() {
    for limits in [
        Limits {
            model_rounds: 1,
            ..Default::default()
        },
        Limits {
            tool_calls: 1,
            ..Default::default()
        },
    ] {
        let model = Model::new(
            vec![calls(vec![
                call("a", args().to_string()),
                call("b", args().to_string()),
            ])],
            |_| {},
        )
        .await;
        let r = registry();
        let host = Host::new(Behavior::Success(json!(true)));
        let rounds = limits.model_rounds == 1;
        let mut loop_ = ToolLoop::new(&r, input(), limits).unwrap();
        let result = loop_
            .run(&model.client, &host, &CancellationToken::new())
            .await;
        assert!(if rounds {
            matches!(result, Err(tool_loop::Error::ModelRoundLimit))
        } else {
            matches!(result, Err(tool_loop::Error::ToolCallLimit))
        });
        assert!(host.ids.lock().unwrap().is_empty());
        assert!(results(&loop_).iter().all(|r| r["error"] == "not_executed"));
        assert_eq!(model.requests().len(), 1);
    }
}

#[tokio::test]
async fn tool_results_and_definitions_count_toward_each_model_boundary() {
    let model = Model::new(vec![calls(vec![call("a", args().to_string())])], |c| {
        c.context_window = 2048
    })
    .await;
    let r = registry();
    let host = Host::new(Behavior::Success(json!("界".repeat(1800))));
    let mut loop_ = ToolLoop::new(&r, input(), Limits::default()).unwrap();
    assert!(matches!(
        loop_
            .run(&model.client, &host, &CancellationToken::new())
            .await,
        Err(tool_loop::Error::NeedsCompaction)
    ));
    assert_eq!(model.requests().len(), 1);
    assert_eq!(results(&loop_)[0]["ok"], true);
    // No hidden pruning or automatic tool repeat after the boundary stops the turn.
    assert!(matches!(
        loop_
            .run(&model.client, &host, &CancellationToken::new())
            .await,
        Err(tool_loop::Error::AlreadyStarted)
    ));
    let mut huge = definition();
    huge.description = "界".repeat(2000);
    let mut huge_registry = ToolRegistry::default();
    huge_registry.register(huge).unwrap();
    let mut loop_ = ToolLoop::new(&huge_registry, input(), Limits::default()).unwrap();
    assert!(matches!(
        loop_
            .run(&model.client, &host, &CancellationToken::new())
            .await,
        Err(tool_loop::Error::NeedsCompaction)
    ));
    assert_eq!(model.requests().len(), 1);
}

#[tokio::test]
async fn cancellation_timeout_and_unknown_stop_without_dispatching_remaining_calls() {
    for mode in 0..4 {
        let cancel = CancellationToken::new();
        let behavior = match mode {
            0 => Behavior::Cancel(cancel.clone()),
            1 => Behavior::Hang,
            _ => Behavior::Unknown,
        };
        let model = Model::new(
            vec![calls(vec![
                call("a", args().to_string()),
                call("b", args().to_string()),
            ])],
            |_| {},
        )
        .await;
        let r = registry();
        let host = Host::new(behavior);
        let mut loop_ = ToolLoop::new(
            &r,
            input(),
            Limits {
                tool_timeout: Duration::from_millis(10),
                ..Default::default()
            },
        )
        .unwrap();
        if mode == 3 {
            cancel.cancel();
        }
        let result = loop_.run(&model.client, &host, &cancel).await;
        if mode == 3 {
            assert!(matches!(result, Err(tool_loop::Error::Cancelled)));
            assert!(model.requests().is_empty());
            assert!(host.ids.lock().unwrap().is_empty());
        } else {
            assert!(matches!(result, Err(tool_loop::Error::OutcomeUnknown)));
            assert_eq!(*host.ids.lock().unwrap(), ["a"]);
            assert_eq!(results(&loop_)[0]["error"], "result_unknown");
            assert_eq!(results(&loop_)[1]["error"], "not_executed");
            prepare_request(model.client.config(), loop_.transcript()).unwrap();
            assert_eq!(model.requests().len(), 1);
        }
    }
}

#[tokio::test]
async fn execution_failure_and_oversized_output_are_bounded_model_visible_results() {
    for behavior in [
        Behavior::Failed,
        Behavior::Success(json!("x".repeat(10000))),
    ] {
        let model = Model::new(
            vec![calls(vec![call("a", args().to_string())]), final_reply()],
            |_| {},
        )
        .await;
        let r = registry();
        let host = Host::new(behavior);
        let mut loop_ = ToolLoop::new(&r, input(), Limits::default()).unwrap();
        loop_
            .run(&model.client, &host, &CancellationToken::new())
            .await
            .unwrap();
        let error = results(&loop_)[0]["error"].as_str().unwrap().to_owned();
        assert!(matches!(error.as_str(), "tool_failed" | "output_too_large"));
        assert_eq!(model.requests().len(), 2);
        assert_eq!(host.ids.lock().unwrap().len(), 1);
        assert!(model.requests()[1].to_string().len() < 5000);
    }
}

#[tokio::test]
async fn legacy_tool_capability_is_enabled_and_responses_stay_unsupported() {
    for responses in [false, true] {
        let model = Model::new(vec![], |c| {
            if responses {
                c.provider = ProviderKind::OpenAiResponses;
                c.completions_path = "responses".into();
            } else {
                c.capabilities.tool_calls = false;
            }
        })
        .await;
        if !responses {
            assert!(model.client.config().capabilities.tool_calls);
            assert!(model.requests().is_empty());
            continue;
        }
        let r = registry();
        let host = Host::new(Behavior::Success(json!(true)));
        let mut loop_ = ToolLoop::new(&r, input(), Limits::default()).unwrap();
        assert!(matches!(
            loop_
                .run(&model.client, &host, &CancellationToken::new())
                .await,
            Err(tool_loop::Error::Model(ModelError::Invalid(_)))
        ));
        assert!(model.requests().is_empty());
        assert!(host.ids.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn diagnostic_trace_covers_tools_and_model_followup_without_raw_arguments() {
    use shellpilot_server::diagnostics::{Diagnostics,Level};
    let model=Model::new(vec![calls(vec![call("trace-call",args().to_string())]),final_reply()],|_|{}).await;
    let logs=tempfile::tempdir().unwrap();let sink=Arc::new(Diagnostics::default());sink.configure(logs.path().into(),Level::Debug).unwrap();
    let trace=sink.trace("tool-trace","chat","model");let r=registry();let host=Host::new(Behavior::Success(json!({"exitCode":0,"status":"healthy"})));
    let mut loop_=ToolLoop::new(&r,input(),Limits::default()).unwrap();
    trace.scope(loop_.run(&model.client,&host,&CancellationToken::new())).await.unwrap();
    let path=logs.path().join(chrono::Local::now().format("%Y-%m-%d").to_string()).join("tool-trace.jsonl");
    let raw=std::fs::read_to_string(path).unwrap();let rows:Vec<Value>=raw.lines().map(|line|serde_json::from_str(line).unwrap()).collect();
    for name in ["tool.proposed","tool.authorization_started","tool.execution_started","tool.execution_finished","tool.result_recorded"] {assert!(rows.iter().any(|row|row["event"]==name),"missing {name}");}
    assert_eq!(rows.iter().filter(|row|row["event"]=="model.request_started").count(),2);
    assert!(rows.iter().all(|row|row["traceId"]=="tool-trace"));
    assert!(rows.iter().any(|row|row["event"]=="tool.execution_finished"&&row["fields"]["exitCode"]==0));
}
