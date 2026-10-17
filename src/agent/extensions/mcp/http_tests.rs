use super::*;
use axum::{
    body::{Body, Bytes},
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};

#[derive(Clone, Copy)]
enum Mode {
    Json,
    Events,
    Unauthorized,
    Redirect,
    Malformed,
    WrongId,
    Disconnect,
    Slow,
    Oversized,
}
#[derive(Clone)]
struct Fixture {
    mode: Mode,
    calls: Arc<AtomicUsize>,
    requests: Arc<AtomicUsize>,
    redirects: Arc<AtomicUsize>,
    entered: CancellationToken,
}
async fn redirected(State(f): State<Fixture>) -> StatusCode {
    f.redirects.fetch_add(1, Ordering::SeqCst);
    StatusCode::OK
}
async fn exchange(
    State(f): State<Fixture>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    f.requests.fetch_add(1, Ordering::SeqCst);
    assert_eq!(headers["authorization"], "Bearer fixture-json-token");
    assert_eq!(headers["x-api-key"], "fixture-custom-key");
    assert_eq!(headers["accept"], "application/json, text/event-stream");
    if request["method"] == "initialize" {
        assert!(!headers.contains_key("mcp-session-id"));
        let mut response=Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}}})).into_response();
        response
            .headers_mut()
            .insert("mcp-session-id", "fixture-session".parse().unwrap());
        return response;
    }
    assert_eq!(headers["mcp-session-id"], "fixture-session");
    assert_eq!(headers["mcp-protocol-version"], "2025-03-26");
    if request["method"] == "notifications/initialized" {
        return StatusCode::ACCEPTED.into_response();
    }
    if request["method"] == "tools/list" {
        return Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"tools":[]}}))
            .into_response();
    }
    assert_eq!(request["method"], "tools/call");
    f.calls.fetch_add(1, Ordering::SeqCst);
    f.entered.cancel();
    match f.mode {
        Mode::Unauthorized=>StatusCode::UNAUTHORIZED.into_response(),
        Mode::Redirect=>(StatusCode::TEMPORARY_REDIRECT,[("location","/replayed")]).into_response(),
        Mode::Malformed=>Response::builder().header("content-type","application/json").body(Body::from("{")).unwrap(),
        Mode::WrongId=>Json(json!({"jsonrpc":"2.0","id":999,"result":{}})).into_response(),
        Mode::Disconnect=>Response::builder().header("content-type","application/json").body(Body::from_stream(futures_util::stream::once(async {Err::<Bytes,_>(std::io::Error::other("fixture socket closed"))}))).unwrap(),
        Mode::Slow=>{
            tokio::time::sleep(Duration::from_secs(5)).await;
            StatusCode::NO_CONTENT.into_response()
        },
        Mode::Oversized=>Response::builder().header("content-type","application/json").body(Body::from(vec![b'x';MAX+1])).unwrap(),
        Mode::Json=>Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"content":[{"type":"text","text":"汉🙂result"}],"isError":false}})).into_response(),
        Mode::Events=>{
            let raw=format!("data: {}\r\n\r\nevent: message\r\ndata: {{\"jsonrpc\":\"2.0\",\"id\":{},\r\ndata: \"result\":{{\"content\":[{{\"type\":\"text\",\"text\":\"汉🙂result\"}}],\"isError\":false}}}}\r\n\r\n",json!({"jsonrpc":"2.0","method":"notifications/message","params":{"data":"notice"}}),request["id"]);
            Response::builder().header("content-type","text/event-stream; charset=utf-8").body(Body::from_stream(futures_util::stream::iter(raw.into_bytes().into_iter().map(|byte|Ok::<_,std::io::Error>(Bytes::from(vec![byte])))))).unwrap()
        },
    }
}
struct Server {
    state: Fixture,
    url: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start(mode: Mode) -> Self {
        let state = Fixture {
            mode,
            calls: Arc::default(),
            requests: Arc::default(),
            redirects: Arc::default(),
            entered: CancellationToken::new(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/mcp", listener.local_addr().unwrap());
        let router = Router::new()
            .route("/mcp", post(exchange))
            .route("/replayed", post(redirected))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { state, url, task }
    }
    async fn client(&self) -> Client {
        let server: config::Server = serde_json::from_value(
            json!({"name":"fixture","enabled":true,"transport":"streamableHttp","url":self.url}),
        )
        .unwrap();
        Client::connect(
            &server,
            &CancellationToken::new(),
            None,
            super::super::connection_values::Values {
                headers: BTreeMap::from([
                    ("authorization".into(), "Bearer fixture-json-token".into()),
                    ("x-api-key".into(), "fixture-custom-key".into()),
                ]),
                env: Default::default(),
            },
        )
        .await
        .unwrap()
    }
}
#[tokio::test]
async fn streamable_http_accepts_json_and_fragmented_multiline_events_with_credentials_and_session_headers(
) {
    for mode in [Mode::Json, Mode::Events] {
        let server = Server::start(mode).await;
        let mut client = server.client().await;
        let token = CancellationToken::new();
        assert!(client.rpc("tools/list", json!({}), &token).await.unwrap()["tools"].is_array());
        let result = client
            .rpc(
                "tools/call",
                json!({"name":"lookup","arguments":{}}),
                &token,
            )
            .await
            .unwrap();
        assert_eq!(result["content"][0]["text"], "汉🙂result");
        assert_eq!(server.state.calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.state.requests.load(Ordering::SeqCst), 4);
    }
}
#[tokio::test]
async fn streamable_http_errors_limits_timeout_and_redirects_do_not_replay_a_dispatched_call() {
    for mode in [
        Mode::Unauthorized,
        Mode::Redirect,
        Mode::Malformed,
        Mode::WrongId,
        Mode::Disconnect,
        Mode::Slow,
        Mode::Oversized,
    ] {
        let server = Server::start(mode).await;
        let mut client = server.client().await;
        client.timeout = if matches!(mode, Mode::Slow) {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(2)
        };
        assert!(client
            .rpc(
                "tools/call",
                json!({"name":"lookup","arguments":{}}),
                &CancellationToken::new()
            )
            .await
            .is_err());
        assert_eq!(server.state.calls.load(Ordering::SeqCst), 1);
        assert_eq!(server.state.requests.load(Ordering::SeqCst), 3);
        assert_eq!(server.state.redirects.load(Ordering::SeqCst), 0);
    }
}
#[tokio::test]
async fn streamable_http_cancellation_before_and_after_dispatch_never_retries() {
    let server = Server::start(Mode::Slow).await;
    let mut client = server.client().await;
    let before = CancellationToken::new();
    before.cancel();
    assert!(matches!(
        client.rpc("tools/call", json!({}), &before).await,
        Err(Error::Cancelled)
    ));
    assert_eq!(server.state.calls.load(Ordering::SeqCst), 0);
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let run = tokio::spawn(async move {
        client
            .rpc(
                "tools/call",
                json!({"name":"lookup","arguments":{}}),
                &token,
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(2), server.state.entered.cancelled())
        .await
        .unwrap();
    cancel.cancel();
    assert!(matches!(run.await.unwrap(), Err(Error::Cancelled)));
    assert_eq!(server.state.calls.load(Ordering::SeqCst), 1);
}
