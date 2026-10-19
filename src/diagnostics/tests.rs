use super::*;
fn records(root: &std::path::Path, id: &str) -> Vec<Value> {
    let path = root
        .join(Local::now().format("%Y-%m-%d").to_string())
        .join(format!("{id}.log"));
    fs::read_to_string(path)
        .unwrap()
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}
#[derive(Debug)]
struct FailureFixture;

#[test]
fn retention_removes_expired_trace_files_only_and_updates_live() {
    let root = tempfile::tempdir().unwrap();
    let sink = Diagnostics::default();
    sink.configure(root.path().join("agent"), Level::Info).unwrap();
    assert_eq!(sink.retention_days.load(Ordering::Relaxed), 7);
    let today = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
    let add = |name: &str, file: &str| {
        let path = root.path().join("agent").join(name);
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join(file), b"fixture").unwrap();
        path.join(file)
    };
    let current = add("2026-10-19", "today.log");
    let retained = add("2026-10-13", "six-days.log");
    let expired = add("2026-10-12", "old.log");
    let legacy = add("2026-10-12", "old-format.jsonl");
    let legacy_json = add("2026-10-12", "previous.json");
    let unrelated = add("2026-10-12", "notes.txt");
    let invalid_date = add("not-a-date", "keep.log");
    let future = add("2026-10-20", "future.log");
    let nested = add("2026-10-11/nested", "keep.log");
    let lifecycle = root.path().join("backend.log");
    fs::write(&lifecycle, b"keep").unwrap();
    assert_eq!(sink.cleanup_before(today).unwrap(), 3);
    assert!(!expired.exists());
    assert!(!legacy.exists());
    assert!(!legacy_json.exists());
    for path in [&current, &retained, &unrelated, &invalid_date, &future, &nested, &lifecycle] {
        assert!(path.exists(), "unexpectedly removed {}", path.display());
    }
    sink.set_retention_days(3).unwrap();
    assert_eq!(sink.cleanup_before(today).unwrap(), 1);
    assert!(!retained.exists());
    assert!(!retained.parent().unwrap().exists());
    for invalid in [0, 3651, u32::MAX] { assert!(sink.set_retention_days(invalid).is_err()); }
    assert_eq!(sink.retention_days.load(Ordering::Relaxed), 3);
}

#[test]
fn explicit_conversation_records_preserve_display_text_and_share_trace_without_a_scope() {
    let root = tempfile::tempdir().unwrap();
    let sink = Arc::new(Diagnostics::default());
    sink.configure(root.path().into(), Level::Info).unwrap();
    let trace = sink.trace("dialogue", "chat", "profile");
    let message = "你好\n\"完整正文\" ".repeat(100);
    trace.conversation_message("user", Some("我的服务器"), "model-id", "accepted", &message);
    trace.conversation_message("assistant", Some("我的服务器"), "model-id", "completed", "你好，有什么可以帮你？");
    trace.event(Level::Info, "metadata", json!({"content":"hidden","apiKey":"hidden","password":"hidden"}));
    let rows = records(root.path(), "dialogue");
    assert_eq!(rows[0]["fields"]["message"], message);
    assert_eq!(rows[0]["fields"]["connectionName"], "我的服务器");
    assert_eq!(rows[0]["fields"]["modelId"], "model-id");
    assert_eq!(rows[1]["fields"]["role"], "assistant");
    assert_eq!(rows[1]["fields"]["message"], "你好，有什么可以帮你？");
    assert!(rows.iter().all(|row| row["traceId"] == "dialogue"));
    assert_eq!(rows[2]["fields"]["content"], "[REDACTED]");
    assert_eq!(rows[2]["fields"]["apiKey"], "[REDACTED]");
    assert_eq!(rows[2]["fields"]["password"], "[REDACTED]");
    sink.set_level(Level::Warn);
    trace.conversation_message("user", None, "model-id", "accepted", "filtered");
    assert_eq!(records(root.path(), "dialogue").len(), 3);
}
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
