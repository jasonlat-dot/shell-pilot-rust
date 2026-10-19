use axum::{
    body::{Body, Bytes},
    extract::State,
    http::HeaderMap,
    response::Response,
    routing::post,
    Json, Router,
};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        approvals::{Approvals, Policy},
        provider::*,
        tool_loop::*,
        Repository,
    },
    store::Store,
};
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};
use tokio_util::sync::CancellationToken;

#[derive(Clone)]
struct Wire {
    replies: Arc<Mutex<VecDeque<(bool, String)>>>,
    requests: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
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
async fn respond(
    State(wire): State<Wire>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> Response {
    wire.requests.lock().unwrap().push((headers, body));
    let (sse, body) = wire
        .replies
        .lock()
        .unwrap()
        .pop_front()
        .expect("unexpected request or replay");
    if sse {
        // Deliberately split every UTF-8 character and SSE line across HTTP chunks.
        let chunks = body
            .into_bytes()
            .into_iter()
            .map(|b| Ok::<_, std::io::Error>(Bytes::from(vec![b])));
        Response::builder()
            .header("content-type", "text/event-stream; charset=utf-8")
            .body(Body::from_stream(futures_util::stream::iter(chunks)))
            .unwrap()
    } else {
        Response::builder()
            .header("content-type", "application/json")
            .body(Body::from(body))
            .unwrap()
    }
}
impl Model {
    async fn new(replies: Vec<(bool, String)>) -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config:ModelConfig=serde_json::from_value(json!({"provider":"anthropicMessages","baseUrl":format!("http://{}/proxy/v1/",listener.local_addr().unwrap()),
            "completionsPath":"messages","model":"test","contextWindow":16384,"outputReserve":512,"safetyReserve":256,
            "timeoutSeconds":5,"maxResponseBytes":65536,"outputTokenParameter":"maxTokens",
            "capabilities":{"developerRole":false,"temperature":false,"reasoningEffort":false},"requireApiKey":true})).unwrap();
        let repository = ModelRepository::new(Arc::new(Store::in_memory().unwrap()));
        repository
            .save(
                "model",
                "test",
                0,
                ModelConfigUpdate {
                    config,
                    api_key: KeyChange::Set {
                        value: SecretKey::new("loopback-anthropic-key").unwrap(),
                    },
                },
            )
            .unwrap();
        let client = ModelClient::new(repository.runtime("model", "test").unwrap()).unwrap();
        let wire = Wire {
            replies: Arc::new(Mutex::new(replies.into())),
            requests: Arc::default(),
        };
        let router = Router::new()
            .route("/proxy/v1/messages", post(respond))
            .with_state(wire.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { client, wire, task }
    }
    fn requests(&self) -> Vec<(HeaderMap, Value)> {
        self.wire.requests.lock().unwrap().clone()
    }
}
fn reply(content: Value, reason: &str) -> String {
    json!({"type":"message","role":"assistant","content":content,"stop_reason":reason,"usage":{"input_tokens":12,"output_tokens":4}}).to_string()
}
fn request() -> ChatRequest {
    ChatRequest {
        messages: vec![ChatMessage::User {
            content: "检查一下".into(),
        }],
        tools: vec![],
    }
}
fn events(content: Value, delta: Value, reason: &str) -> String {
    [json!({"type":"message_start","message":{"type":"message","role":"assistant","content":[],"usage":{"input_tokens":12,"output_tokens":0}}}),
    json!({"type":"content_block_start","index":0,"content_block":content}),
    json!({"type":"content_block_delta","index":0,"delta":delta}),
    json!({"type":"content_block_stop","index":0}),
    json!({"type":"message_delta","delta":{"stop_reason":reason},"usage":{"output_tokens":4}}),
    json!({"type":"message_stop"})].iter().map(|event|format!("event: {}\ndata: {}\n\n",event["type"].as_str().unwrap(),event)).collect()
}
#[tokio::test]
async fn json_and_fragmented_sse_use_native_auth_and_normalized_reply() {
    let text = json!([{"type":"text","text":"检查完成"}]);
    let sse = events(
        json!({"type":"text","text":""}),
        json!({"type":"text_delta","text":"检查完成"}),
        "end_turn",
    );
    let model = Model::new(vec![(false, reply(text, "end_turn")), (true, sse)]).await;
    let cancel = CancellationToken::new();
    assert_eq!(
        model
            .client
            .complete(&request(), &cancel)
            .await
            .unwrap()
            .message
            .content
            .as_deref(),
        Some("检查完成")
    );
    let visible = Arc::new(Mutex::new(String::new()));
    let sink = visible.clone();
    let completion = model
        .client
        .stream(&request(), &cancel, move |text| {
            sink.lock().unwrap().push_str(&text);
            async { Ok(()) }
        })
        .await
        .unwrap();
    assert_eq!(completion.message.content.as_deref(), Some("检查完成"));
    assert_eq!(*visible.lock().unwrap(), "检查完成");
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    for (headers, body) in &requests {
        assert_eq!(headers["x-api-key"], "loopback-anthropic-key");
        assert_eq!(headers["anthropic-version"], "2023-06-01");
        assert!(!headers.contains_key("authorization"));
        assert_eq!(body["max_tokens"], 512);
        assert!(body.get("max_completion_tokens").is_none());
    }
    assert_eq!(requests[0].1["stream"], false);
    assert_eq!(requests[1].1["stream"], true);
}
struct Host {
    authorized: AtomicUsize,
    executed: AtomicUsize,
}
impl ToolExecutor for Host {
    fn authorize<'a>(
        &'a self,
        _: &'a ValidatedCall,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.authorized.fetch_add(1, Ordering::SeqCst);
            true
        })
    }
    fn execute<'a>(
        &'a self,
        call: &'a ValidatedCall,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            assert_eq!(call.name(), "inspect");
            assert_eq!(call.arguments()["target"], "测试");
            self.executed.fetch_add(1, Ordering::SeqCst);
            ToolOutcome::Success(json!({"installed":true}))
        })
    }
}
struct StreamingContext;
impl LoopContext for StreamingContext {
    fn streams_replies(&self) -> bool {
        true
    }
    fn before_model<'a>(
        &'a self,
        request: &'a ChatRequest,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, shellpilot_server::agent::tool_loop::Result<ChatRequest>> {
        Box::pin(async move { Ok(request.clone()) })
    }
    fn begin_batch<'a>(
        &'a self,
        _: &'a [ToolCall],
        _: Option<&'a str>,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, shellpilot_server::agent::tool_loop::Result<()>> {
        Box::pin(async { Ok(()) })
    }
    fn started(&self, _: &str) -> shellpilot_server::agent::tool_loop::Result<()> {
        Ok(())
    }
    fn result<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, shellpilot_server::agent::tool_loop::Result<()>> {
        Box::pin(async { Ok(()) })
    }
}
#[tokio::test]
async fn native_tool_use_passes_existing_authorization_and_returns_tool_result_once() {
    let sse = events(
        json!({"type":"tool_use","id":"one","name":"inspect","input":{}}),
        json!({"type":"input_json_delta","partial_json":"{\"target\":\"测试\"}"}),
        "tool_use",
    );
    let model = Model::new(vec![
        (true, sse),
        (
            false,
            reply(json!([{"type":"text","text":"已安装"}]), "end_turn"),
        ),
    ])
    .await;
    let mut registry = ToolRegistry::default();
    registry.register(FunctionDefinition{name:"inspect".into(),description:"inspect".into(),parameters:json!({"type":"object","properties":{"target":{"type":"string"}},"required":["target"],"additionalProperties":false})}).unwrap();
    let host = Host {
        authorized: AtomicUsize::new(0),
        executed: AtomicUsize::new(0),
    };
    let store = Arc::new(Store::in_memory().unwrap());
    let session = Repository::new(store.clone())
        .create_general_session_with_policy("model", "test", Some(Policy::FullAccess))
        .unwrap();
    let context = StreamingContext;
    let mut run = ToolLoop::new(&registry, request().messages, Limits::default())
        .unwrap()
        .with_context(&context)
        .with_approval_boundary(ApprovalBoundary::new(
            Approvals::new(store),
            session.id,
            "test".into(),
        ));
    assert_eq!(
        run.run(&model.client, &host, &CancellationToken::new())
            .await
            .unwrap()
            .content
            .as_deref(),
        Some("已安装")
    );
    assert_eq!(host.authorized.load(Ordering::SeqCst), 1);
    assert_eq!(host.executed.load(Ordering::SeqCst), 1);
    let requests = model.requests();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|(_, body)| body["stream"] == true));
    assert_eq!(
        requests[1].1["messages"][1]["content"][0]["type"],
        "tool_use"
    );
    let result = &requests[1].1["messages"][2]["content"][0];
    assert_eq!(result["type"], "tool_result");
    assert_eq!(result["tool_use_id"], "one");
    assert!(result["content"].as_str().unwrap().contains("installed"));
}
#[tokio::test]
async fn truncated_tool_stream_never_executes_or_retries() {
    let mut sse = events(
        json!({"type":"tool_use","id":"one","name":"inspect","input":{}}),
        json!({"type":"input_json_delta","partial_json":"{\"target\":\"测试\"}"}),
        "tool_use",
    );
    sse.truncate(sse.rfind("event: message_stop").unwrap());
    let model = Model::new(vec![(true, sse)]).await;
    let mut request = request();
    request.tools.push(FunctionDefinition {
        name: "inspect".into(),
        description: "inspect".into(),
        parameters: json!({"type":"object"}),
    });
    assert!(matches!(
        model
            .client
            .stream_tools(&request, &CancellationToken::new(), |_| async { Ok(()) })
            .await,
        Err(ModelError::Incomplete)
    ));
    assert_eq!(model.requests().len(), 1);
}
