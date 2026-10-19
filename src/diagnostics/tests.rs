use super::*;
fn records(root: &std::path::Path, id: &str) -> Vec<Value> {
    let path = root
        .join(Local::now().format("%Y-%m-%d").to_string())
        .join(format!("{id}.jsonl"));
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[derive(Debug)]
struct FailureFixture;
impl Failure for FailureFixture {
    fn diagnostic_code(&self) -> &str {
        "TEST_ERROR"
    }
    fn upstream_status(&self) -> Option<u16> {
        Some(429)
    }
}

#[test]
fn levels_update_live_and_fields_are_redacted_with_safe_trace_paths() {
    let root = tempfile::tempdir().unwrap();
    let sink = Arc::new(Diagnostics::default());
    sink.configure(root.path().into(), Level::Info).unwrap();
    let trace = sink.trace("../unsafe/id", "chat", "model");
    assert!(!trace.id().contains('/'));
    assert_ne!(trace.id(), "../unsafe/id");
    trace.event(Level::Debug, "hidden", json!({}));
    trace.event(Level::Info,"visible",json!({"apiKey":"sk-fixture-secret","nested":{"command":"password=private"},"messageBytes":12}));
    sink.set_level(Level::Error);
    trace.event(Level::Warn, "hidden_warn", json!({}));
    trace.event(Level::Error, "failure", json!({"httpStatus":429}));
    sink.set_level(Level::Debug);
    trace.event(Level::Debug, "detail", json!({"round":2}));
    let rows = records(root.path(), trace.id());
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["fields"]["apiKey"], "[REDACTED]");
    assert_eq!(rows[1]["level"], "error");
    assert_eq!(rows[2]["level"], "debug");
    let text = serde_json::to_string(&rows).unwrap();
    assert!(!text.contains("fixture-secret"));
    assert!(!text.contains("password=private"));
    assert!(rows
        .iter()
        .all(|r| r["traceId"] == trace.id() && r["sessionId"] == "chat"));
}

#[tokio::test]
async fn request_trace_propagates_into_spawned_and_blocking_work_without_cross_chat_leaks() {
    let root = tempfile::tempdir().unwrap();
    let sink = Arc::new(Diagnostics::default());
    sink.configure(root.path().into(), Level::Debug).unwrap();
    let first = sink.trace("first", "chat-a", "model");
    let second = sink.trace("second", "chat-b", "model");
    let a = first.request(json!({}), async {
        event(Level::Info, "tool.proposed", json!({"callId":"a"}));
        tokio::spawn(inherit(async {
            tokio::task::spawn_blocking(blocking(|| {
                event(Level::Info, "approval.decided", json!({"allowed":true}))
            }))
            .await
            .unwrap();
        }))
        .await
        .unwrap();
        Ok::<_, FailureFixture>(())
    });
    let b = second.request(json!({}), async {
        event(Level::Error, "model.http_error", json!({"httpStatus":429}));
        Err::<(), _>(FailureFixture)
    });
    let (a, b) = tokio::join!(a, b);
    assert!(a.is_ok());
    assert!(b.is_err());
    let a = records(root.path(), "first");
    let b = records(root.path(), "second");
    assert!(a.iter().any(|r| r["event"] == "approval.decided"));
    assert!(a.iter().all(|r| r["sessionId"] == "chat-a"));
    assert!(b.iter().all(|r| r["sessionId"] == "chat-b"));
    assert_eq!(a.last().unwrap()["event"], "request.completed");
    assert_eq!(b.last().unwrap()["event"], "request.failed");
    assert_eq!(b.last().unwrap()["fields"]["httpStatus"], 429);
}

#[tokio::test]
async fn dropped_request_is_interrupted_and_never_logged_as_completed() {
    let root = tempfile::tempdir().unwrap();
    let sink = Arc::new(Diagnostics::default());
    sink.configure(root.path().into(), Level::Info).unwrap();
    let trace = sink.trace("interrupted", "chat", "model");
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(10),
        trace.request(
            json!({}),
            std::future::pending::<Result<(), FailureFixture>>(),
        ),
    )
    .await;
    assert!(result.is_err());
    let rows = records(root.path(), "interrupted");
    assert_eq!(rows.last().unwrap()["event"], "request.interrupted");
    assert_eq!(rows.last().unwrap()["level"], "warn");
    assert!(!rows.iter().any(|r| r["event"] == "request.completed"));
}
