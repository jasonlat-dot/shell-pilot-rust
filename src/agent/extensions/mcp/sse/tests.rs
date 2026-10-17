use super::*;
use axum::{
    body::{Body, Bytes},
    extract::{OriginalUri, Query, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

#[test]
fn decoder_handles_fragmented_utf8_bom_crlf_cr_and_multiline_data_without_replay() {
    let raw="\u{feff}: heartbeat\r\nevent: endpoint\r\ndata: /messages?session=x\r\n\r\nevent: message\rdata: {\"text\":\rdata: \"汉🙂\"}\r\rid: ignored\nretry: 1\n\n";
    let mut decoder = Decoder::default();
    let mut events = vec![];
    for byte in raw.as_bytes() {
        events.extend(decoder.feed(&[*byte]).unwrap());
    }
    assert_eq!(events.len(), 2);
    assert_eq!(events[0].kind, "endpoint");
    assert_eq!(events[0].data, "/messages?session=x");
    assert_eq!(
        serde_json::from_str::<Value>(&events[1].data).unwrap()["text"],
        "汉🙂"
    );
    assert!(decoder.feed(&vec![b'x'; MAX + 1]).is_err());
}
#[derive(Clone, Copy)]
enum Mode {
    Normal,
    Headers,
    Foreign,
    Redirect,
    Slow,
    WrongId,
    Malformed,
    Disconnect,
}
#[derive(Clone)]
struct Fixture {
    mode: Mode,
    streams: Arc<Mutex<HashMap<String, mpsc::Sender<Bytes>>>>,
    requests: Arc<Mutex<Vec<(String, Value)>>>,
    gets: Arc<AtomicUsize>,
    active: Arc<AtomicUsize>,
    entered: CancellationToken,
}
struct Active(Arc<AtomicUsize>);
impl Drop for Active {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}
async fn connect(
    State(f): State<Fixture>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    f.gets.fetch_add(1, Ordering::SeqCst);
    if matches!(f.mode, Mode::Headers) {
        assert_eq!(headers["authorization"], "Bearer fixture-json-token");
        assert_eq!(headers["x-api-key"], "fixture-custom-key");
    }
    if matches!(f.mode, Mode::Redirect) {
        return (StatusCode::FOUND, [("location", "/sse")]).into_response();
    }
    let id = uuid::Uuid::new_v4().to_string();
    let (tx, mut rx) = mpsc::channel(32);
    f.streams.lock().unwrap().insert(id.clone(), tx);
    let endpoint = if matches!(f.mode, Mode::Foreign) {
        "http://127.0.0.1:9/collect".into()
    } else {
        format!("/messages?session={id}&api_key=Bearer+fixture")
    };
    assert_eq!(uri.query(), Some("api_key=Bearer+fixture"));
    let first = format!("\u{feff}: heartbeat\r\nevent: endpoint\r\ndata: {endpoint}\r\n\r\n");
    f.active.fetch_add(1, Ordering::SeqCst);
    let active = Active(f.active.clone());
    Response::builder()
        .header("content-type", "text/event-stream; charset=utf-8")
        .body(Body::from_stream(async_stream::stream! {
            let _active=active;
            for byte in first.into_bytes() {yield Ok::<_,std::io::Error>(Bytes::from(vec![byte]));}
            while let Some(chunk)=rx.recv().await {yield Ok::<_,std::io::Error>(chunk);}
        }))
        .unwrap()
}
async fn message(
    State(f): State<Fixture>,
    OriginalUri(uri): OriginalUri,
    Query(query): Query<HashMap<String, String>>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> StatusCode {
    if matches!(f.mode, Mode::Headers) {
        assert_eq!(headers["authorization"], "Bearer fixture-json-token");
        assert_eq!(headers["x-api-key"], "fixture-custom-key");
    }
    f.requests
        .lock()
        .unwrap()
        .push((uri.to_string(), body.clone()));
    let id = query.get("session").unwrap();
    assert_eq!(
        query.get("api_key").map(String::as_str),
        Some("Bearer fixture")
    );
    let tx = f.streams.lock().unwrap().get(id).unwrap().clone();
    if body.get("id").is_none() {
        return StatusCode::ACCEPTED;
    }
    if body["method"] == "tools/call" {
        f.entered.cancel();
        if matches!(f.mode, Mode::Slow) {
            return StatusCode::ACCEPTED;
        }
        if matches!(f.mode, Mode::Disconnect) {
            f.streams.lock().unwrap().remove(id);
            return StatusCode::ACCEPTED;
        }
    }
    let result = match body["method"].as_str().unwrap() {
        "initialize" => {
            assert_eq!(body["params"]["protocolVersion"], "2024-11-05");
            json!({"protocolVersion":"2024-11-05","capabilities":{"tools":{}},"serverInfo":{"name":"fixture","version":"1"}})
        }
        "tools/list" => {
            json!({"tools":[{"name":"lookup","inputSchema":{"type":"object","properties":{},"additionalProperties":false}}]})
        }
        "tools/call" => json!({"content":[{"type":"text","text":"汉🙂result"}],"isError":false}),
        _ => json!({}),
    };
    let data = if body["method"] == "tools/call" && matches!(f.mode, Mode::Malformed) {
        "{".into()
    } else {
        let id = if body["method"] == "tools/call" && matches!(f.mode, Mode::WrongId) {
            json!(999)
        } else {
            body["id"].clone()
        };
        json!({"jsonrpc":"2.0","id":id,"result":result}).to_string()
    };
    let frame = format!(
        "event: message\ndata: {}\n\nevent: message\ndata: {data}\n\n",
        json!({"jsonrpc":"2.0","method":"notifications/message","params":{"data":"notice"}})
    );
    for byte in frame.into_bytes() {
        if tx.send(Bytes::from(vec![byte])).await.is_err() {
            break;
        }
    }
    StatusCode::ACCEPTED
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
            streams: Arc::default(),
            requests: Arc::default(),
            gets: Arc::default(),
            active: Arc::default(),
            entered: CancellationToken::new(),
        };
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/sse?api_key=Bearer+fixture",
            listener.local_addr().unwrap()
        );
        let router = Router::new()
            .route("/sse", get(connect))
            .route("/messages", post(message))
            .with_state(state.clone());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { state, url, task }
    }
    async fn client(&self, timeout: u64) -> Result<Client> {
        Client::connect(
            &config::Server {
                name: "legacy-search".into(),
                description: String::new(),
                connection_values: None,
                saved_headers: vec![],
                saved_env: vec![],
                bearer_token: Default::default(),
                bearer_token_configured: false,
                enabled: true,
                transport: Transport::Sse {
                    url: self.url.clone(),
                    bearer_env: String::new(),
                    timeout_seconds: timeout,
                },
            },
            &CancellationToken::new(),
            None,
            if matches!(self.state.mode, Mode::Headers) {
                super::super::super::connection_values::Values {
                    headers: BTreeMap::from([
                        ("authorization".into(), "Bearer fixture-json-token".into()),
                        ("x-api-key".into(), "fixture-custom-key".into()),
                    ]),
                    env: Default::default(),
                }
            } else {
                Default::default()
            },
        )
        .await
    }
    fn calls(&self) -> usize {
        self.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, v)| v["method"] == "tools/call")
            .count()
    }
    async fn closed(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while self.state.active.load(Ordering::SeqCst) != 0 {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .expect("SSE socket must close after cancellation/error/drop");
    }
}
#[tokio::test]
async fn legacy_get_endpoint_and_post_keep_literal_query_key_and_match_fragmented_responses() {
    let server = Server::start(Mode::Normal).await;
    let mut client = server.client(3).await.unwrap();
    let catalog = client
        .rpc("tools/list", json!({}), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(catalog["tools"][0]["name"], "lookup");
    let result = client
        .rpc(
            "tools/call",
            json!({"name":"lookup","arguments":{}}),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result["content"][0]["text"], "汉🙂result");
    assert_eq!(server.state.gets.load(Ordering::SeqCst), 1);
    assert_eq!(server.calls(), 1);
    assert!(server
        .state
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|(uri, _)| uri.contains("api_key=Bearer+fixture")));
    drop(client);
    server.closed().await;
}
#[tokio::test]
async fn foreign_endpoint_and_redirect_are_rejected_before_any_post() {
    for mode in [Mode::Foreign, Mode::Redirect] {
        let server = Server::start(mode).await;
        assert!(server.client(2).await.is_err());
        assert_eq!(server.state.gets.load(Ordering::SeqCst), 1);
        assert!(server.state.requests.lock().unwrap().is_empty());
        server.closed().await;
    }
}

#[tokio::test]
async fn legacy_sse_json_headers_reach_both_get_and_every_post_with_query_key_preserved() {
    let server = Server::start(Mode::Headers).await;
    let mut client = server.client(3).await.unwrap();
    let token = CancellationToken::new();
    assert!(client.rpc("tools/list", json!({}), &token).await.unwrap()["tools"].is_array());
    assert_eq!(
        client
            .rpc(
                "tools/call",
                json!({"name":"lookup","arguments":{}}),
                &token
            )
            .await
            .unwrap()["content"][0]["text"],
        "汉🙂result"
    );
    assert_eq!(server.calls(), 1);
    assert_eq!(server.state.gets.load(Ordering::SeqCst), 1);
    drop(client);
    server.closed().await;
}
#[tokio::test]
async fn cancelling_a_dispatched_call_closes_the_reader_and_never_posts_the_call_again() {
    let server = Server::start(Mode::Slow).await;
    let mut client = server.client(3).await.unwrap();
    let cancel = CancellationToken::new();
    let token = cancel.clone();
    let run = tokio::spawn(async move {
        let result = client
            .rpc(
                "tools/call",
                json!({"name":"lookup","arguments":{}}),
                &token,
            )
            .await;
        (client, result)
    });
    tokio::time::timeout(Duration::from_secs(2), server.state.entered.cancelled())
        .await
        .unwrap();
    cancel.cancel();
    let (mut client, result) = run.await.unwrap();
    assert!(matches!(result, Err(Error::Cancelled)));
    server.closed().await;
    assert!(client
        .rpc("tools/call", json!({}), &CancellationToken::new())
        .await
        .is_err());
    assert_eq!(server.calls(), 1);
}
#[tokio::test]
async fn timeout_malformed_wrong_id_and_disconnect_do_not_reconnect_or_replay() {
    for mode in [Mode::Slow, Mode::Malformed, Mode::WrongId, Mode::Disconnect] {
        let server = Server::start(mode).await;
        let mut client = server.client(1).await.unwrap();
        assert!(client
            .rpc(
                "tools/call",
                json!({"name":"lookup","arguments":{}}),
                &CancellationToken::new()
            )
            .await
            .is_err());
        server.closed().await;
        assert!(client
            .rpc("tools/call", json!({}), &CancellationToken::new())
            .await
            .is_err());
        assert_eq!(server.state.gets.load(Ordering::SeqCst), 1);
        assert_eq!(server.calls(), 1);
    }
}
