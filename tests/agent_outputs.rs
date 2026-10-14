use sha2::{Digest, Sha256};
use shellpilot_server::{
    agent::{
        budget::{estimate_event, group_history},
        *,
    },
    store::Store,
};
use std::{
    io::{self, Read},
    sync::{Arc, Barrier},
};

struct Fixture {
    repo: Repository,
    session: Session,
}
impl Fixture {
    fn new() -> Self {
        Self::from_repo(Repository::new(Arc::new(Store::in_memory().unwrap())))
    }
    fn from_repo(repo: Repository) -> Self {
        let session = repo.create_session("ssh-agent", "default").unwrap();
        repo.start_task(
            &session.id,
            "default",
            0,
            "task",
            TaskRequest {
                goal: "排查服务".into(),
                constraints: vec!["不要重启".into()],
                target: Some(Target {
                    connection_id: "host-a".into(),
                    terminal_session_id: "pty-a".into(),
                }),
            },
        )
        .unwrap();
        Self { repo, session }
    }
    fn revision(&self) -> u64 {
        self.repo
            .session(&self.session.id, "default")
            .unwrap()
            .revision
    }
    fn call(&self, id: &str) {
        self.repo
            .append(
                &self.session.id,
                "default",
                self.revision(),
                &format!("call-{id}"),
                Payload::ToolCall {
                    call_id: id.into(),
                    tool_name: "ssh".into(),
                    arguments: serde_json::json!({"command":"read logs"}),
                },
            )
            .unwrap();
    }
    fn begin(&self, request: &str, call: &str, channel: OutputChannel) -> Result<CaptureStart> {
        self.repo.begin_output(
            &self.session.id,
            "default",
            CaptureRequest {
                request_id: request.into(),
                call_id: call.into(),
                channel,
            },
        )
    }
    fn writer(&self, request: &str, call: &str, channel: OutputChannel) -> OutputWriter {
        match self.begin(request, call, channel).unwrap() {
            CaptureStart::New(writer) => *writer,
            _ => panic!("expected fresh writer"),
        }
    }
    fn capture(
        &self,
        request: &str,
        call: &str,
        channel: OutputChannel,
        bytes: &[u8],
    ) -> StoredOutput {
        let mut writer = self.writer(request, call, channel);
        let mut reader = bytes;
        writer.copy_from(&mut reader).unwrap();
        writer.finish().unwrap()
    }
    fn stats(&self) -> OutputStats {
        self.repo.output_stats(&self.session.id, "default").unwrap()
    }
    fn read(&self, id: &str, offset: u64, limit: usize) -> Result<OutputRange> {
        self.repo
            .read_output_range(&self.session.id, "default", id, offset, limit)
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn small_limits() -> OutputLimits {
    OutputLimits {
        max_output_bytes: 65536,
        max_session_bytes: 131072,
        max_total_bytes: 131072,
        max_outputs: 100,
        max_writers: 4,
        idle_millis: 120000,
    }
}
fn captured(call: &str, ids: Vec<String>) -> CapturedResult {
    CapturedResult {
        call_id: call.into(),
        outcome: Outcome::Success,
        exit_code: Some(0),
        output_ids: ids,
    }
}

#[test]
fn large_unicode_output_streams_in_fixed_chunks_and_exact_ranges() {
    let f = Fixture::new();
    f.call("x");
    let data = "中文日志🙂 upstream 错误\n".repeat(30000).into_bytes();
    struct Reader<'a> {
        data: &'a [u8],
        largest: usize,
    }
    impl Read for Reader<'_> {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.largest = self.largest.max(out.len());
            let count = out.len().min(17003).min(self.data.len());
            out[..count].copy_from_slice(&self.data[..count]);
            self.data = &self.data[count..];
            Ok(count)
        }
    }
    let mut reader = Reader {
        data: &data,
        largest: 0,
    };
    let mut writer = f.writer("capture", "x", OutputChannel::Stdout);
    let id = writer.id().to_string();
    writer.copy_from(&mut reader).unwrap();
    assert_eq!(reader.largest, 65536);
    assert_eq!(writer.captured_bytes(), data.len() as u64);
    assert!(matches!(
        f.repo.output(&f.session.id, "default", &id),
        Err(Error::NotFound)
    ));
    let output = writer.finish().unwrap();
    assert_eq!(output.reference.size, data.len() as u64);
    assert_eq!(output.reference.sha256, hex(&Sha256::digest(&data)));
    assert!(output.preview_truncated);
    assert!(output.preview.len() <= OUTPUT_PREVIEW_BYTES + 80);
    let mut recovered = Vec::new();
    let mut offset = 0;
    loop {
        let range = f.read(&id, offset, 11111).unwrap();
        recovered.extend_from_slice(&range.bytes);
        offset = range.next_offset;
        if range.eof {
            break;
        }
    }
    assert_eq!(recovered, data);
    for offset in [1, 65533, 65535, 65536, 131070] {
        assert_eq!(
            f.read(&id, offset, 37).unwrap().bytes,
            data[offset as usize..offset as usize + 37]
        );
    }
}
#[test]
fn empty_and_binary_outputs_keep_exact_bytes_and_report_preview_loss() {
    let f = Fixture::new();
    f.call("empty");
    f.call("binary");
    let empty = f.capture("empty-cap", "empty", OutputChannel::Stdout, b"");
    assert_eq!(empty.reference.sha256, hex(&Sha256::digest([])));
    let range = f.read(&empty.reference.id, 0, 1).unwrap();
    assert!(range.eof);
    assert!(range.bytes.is_empty());
    let bytes = vec![0xff; 8000];
    let output = f.capture("binary-cap", "binary", OutputChannel::Stderr, &bytes);
    assert!(output.preview_lossy);
    assert!(output.preview_truncated);
    assert!(output.preview.len() <= OUTPUT_PREVIEW_BYTES + 80);
    assert_eq!(f.read(&output.reference.id, 0, 8000).unwrap().bytes, bytes);
}
#[test]
fn scope_checks_reject_other_users_sessions_and_unknown_calls() {
    let f = Fixture::new();
    f.call("x");
    let output = f.capture("cap", "x", OutputChannel::Stdout, b"secret evidence");
    let other = f.repo.create_session("ssh-agent", "default").unwrap();
    assert!(matches!(
        f.repo.output(&f.session.id, "other", &output.reference.id),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        f.repo
            .read_output_range(&other.id, "default", &output.reference.id, 0, 20),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        f.begin("orphan", "unknown", OutputChannel::Stdout),
        Err(Error::ToolProtocol)
    ));
    assert!(matches!(f.read("../outside", 0, 20), Err(Error::NotFound)));
}
#[test]
fn duplicate_capture_returns_read_only_receipt_without_replacing_evidence() {
    let f = Fixture::new();
    f.call("x");
    let writer = f.writer("cap", "x", OutputChannel::Stdout);
    assert!(matches!(
        f.begin("cap", "x", OutputChannel::Stdout),
        Err(Error::OutputBusy)
    ));
    assert!(matches!(
        f.begin("cap", "x", OutputChannel::Stderr),
        Err(Error::Conflict)
    ));
    drop(writer);
    let output = f.capture("cap", "x", OutputChannel::Stdout, b"original");
    match f.begin("cap", "x", OutputChannel::Stdout).unwrap() {
        CaptureStart::Existing(prior) => assert_eq!(*prior, output),
        _ => panic!("duplicate completion must not get a new writer"),
    }
    assert!(matches!(
        f.begin("new-cap", "x", OutputChannel::Stdout),
        Err(Error::Conflict)
    ));
    assert_eq!(f.stats().outputs, 1);
}
#[test]
fn cancellation_drop_and_limit_errors_cleanup_unpublished_chunks() {
    let f = Fixture::from_repo(
        Repository::with_output_limits(Arc::new(Store::in_memory().unwrap()), small_limits())
            .unwrap(),
    );
    for (n, mode) in ["drop", "cancel", "large"].iter().enumerate() {
        let call = format!("x-{n}");
        f.call(&call);
        let mut writer = f.writer(&format!("cap-{n}"), &call, OutputChannel::Stdout);
        let id = writer.id().to_string();
        writer.write(&vec![b'x'; 65536]).unwrap();
        assert!(matches!(f.read(&id, 0, 1), Err(Error::NotFound)));
        match *mode {
            "drop" => drop(writer),
            "cancel" => writer.cancel(),
            _ => {
                assert!(matches!(
                    writer.write(b"overflow"),
                    Err(Error::OutputTooLarge)
                ));
                assert!(matches!(writer.finish(), Err(Error::OutputUnavailable)));
            }
        }
        assert_eq!(f.stats().outputs, 0);
        assert_eq!(f.stats().reserved_bytes, 0);
    }
    assert_eq!(
        f.repo
            .snapshot(&f.session.id, "default")
            .unwrap()
            .uncertain_tool_call_ids
            .len(),
        3
    );
}
#[test]
fn failing_reader_aborts_all_chunks_and_cannot_finish_as_success() {
    let f = Fixture::new();
    f.call("x");
    struct Broken {
        once: bool,
    }
    impl Read for Broken {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            if self.once {
                return Err(io::Error::other("test input failure"));
            }
            self.once = true;
            out.fill(b'x');
            Ok(out.len())
        }
    }
    let mut writer = f.writer("cap", "x", OutputChannel::Stdout);
    assert!(matches!(
        writer.copy_from(&mut Broken { once: false }),
        Err(Error::OutputInput(_))
    ));
    assert!(matches!(writer.finish(), Err(Error::OutputUnavailable)));
    assert_eq!(f.stats().outputs, 0);
}
#[test]
fn quotas_reserve_before_capture_and_unlinked_discard_releases_capacity() {
    let f = Fixture::from_repo(
        Repository::with_output_limits(Arc::new(Store::in_memory().unwrap()), small_limits())
            .unwrap(),
    );
    for id in ["a", "b", "c"] {
        f.call(id)
    }
    let a = f.writer("cap-a", "a", OutputChannel::Stdout);
    let b = f.writer("cap-b", "b", OutputChannel::Stdout);
    assert_eq!(f.stats().reserved_bytes, 131072);
    assert!(matches!(
        f.begin("cap-c", "c", OutputChannel::Stdout),
        Err(Error::OutputQuota)
    ));
    let mut a = a;
    a.write(b"one").unwrap();
    let a = a.finish().unwrap();
    assert_eq!(f.stats().used_bytes, 3);
    assert_eq!(f.stats().reserved_bytes, 65536);
    assert!(matches!(
        f.begin("cap-c", "c", OutputChannel::Stdout),
        Err(Error::OutputQuota)
    ));
    f.repo
        .discard_output(&f.session.id, "default", &a.reference.id)
        .unwrap();
    let c = f.writer("cap-c", "c", OutputChannel::Stdout);
    drop(b);
    drop(c);
    assert_eq!(f.stats().outputs, 0);
}
#[test]
fn writer_and_count_limits_are_enforced_independently() {
    let mut limits = small_limits();
    limits.max_writers = 1;
    limits.max_outputs = 1;
    let f = Fixture::from_repo(
        Repository::with_output_limits(Arc::new(Store::in_memory().unwrap()), limits).unwrap(),
    );
    f.call("a");
    f.call("b");
    let a = f.writer("a", "a", OutputChannel::Stdout);
    assert!(matches!(
        f.begin("b", "b", OutputChannel::Stdout),
        Err(Error::OutputBusy)
    ));
    let output = a.finish().unwrap();
    assert!(matches!(
        f.begin("b", "b", OutputChannel::Stdout),
        Err(Error::OutputQuota)
    ));
    f.repo
        .discard_output(&f.session.id, "default", &output.reference.id)
        .unwrap();
    drop(f.writer("b", "b", OutputChannel::Stdout));
}
#[test]
fn concurrent_captures_cannot_overbook_global_quota() {
    let store = Arc::new(Store::in_memory().unwrap());
    let limits = OutputLimits {
        max_output_bytes: 65536,
        max_session_bytes: 65536,
        max_total_bytes: 65536,
        ..small_limits()
    };
    let repo = Repository::with_output_limits(store, limits).unwrap();
    let a = Fixture::from_repo(repo.clone());
    let b = Fixture::from_repo(repo.clone());
    a.call("a");
    b.call("b");
    let gate = Arc::new(Barrier::new(3));
    let workers = [(a.session.id.clone(), "a"), (b.session.id.clone(), "b")]
        .into_iter()
        .map(|(id, call)| {
            let repo = repo.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                repo.begin_output(
                    &id,
                    "default",
                    CaptureRequest {
                        request_id: "cap".into(),
                        call_id: call.into(),
                        channel: OutputChannel::Stdout,
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    gate.wait();
    let results = workers
        .into_iter()
        .map(|w| w.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Ok(CaptureStart::New(_))))
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::OutputQuota)))
            .count(),
        1
    );
    drop(results);
    assert_eq!(a.stats().reserved_bytes + b.stats().reserved_bytes, 0);
}
#[test]
fn event_link_keeps_only_preview_and_references_and_protects_full_evidence() {
    let f = Fixture::new();
    f.call("x");
    let stdout = f.capture(
        "out",
        "x",
        OutputChannel::Stdout,
        &vec![b'x'; 2 * 1024 * 1024],
    );
    let stderr = f.capture("err", "x", OutputChannel::Stderr, b"upstream refused");
    let rev = f.revision();
    let receipt = f
        .repo
        .append_captured_result(
            &f.session.id,
            "default",
            rev,
            "result",
            captured(
                "x",
                vec![stdout.reference.id.clone(), stderr.reference.id.clone()],
            ),
        )
        .unwrap();
    assert!(estimate_event(&receipt.event).unwrap() < 6000);
    match &receipt.event.payload {
        Payload::ToolResult { text, outputs, .. } => {
            assert!(text.len() < 10000);
            assert_eq!(outputs.len(), 2);
            assert!(text.contains("upstream refused"));
        }
        _ => panic!("missing tool result"),
    }
    assert!(matches!(
        f.repo
            .discard_output(&f.session.id, "default", &stdout.reference.id),
        Err(Error::Conflict)
    ));
    let replay = f
        .repo
        .append_captured_result(
            &f.session.id,
            "default",
            rev,
            "result",
            captured(
                "x",
                vec![stdout.reference.id.clone(), stderr.reference.id.clone()],
            ),
        )
        .unwrap();
    assert!(!replay.inserted);
    assert_eq!(replay.event.id, receipt.event.id);
    let events = f.repo.events(&f.session.id, "default", 0, 200).unwrap();
    assert!(group_history(&events).unwrap().iter().all(|g| g.complete));
    let snapshot = f.repo.snapshot(&f.session.id, "default").unwrap();
    f.repo
        .save_checkpoint(
            &snapshot,
            CheckpointContent {
                task: snapshot.session.task.clone(),
                summary: "已查证 upstream 拒绝连接，不重启服务".into(),
                next_steps: vec!["查看 upstream 配置".into()],
                evidence_event_ids: vec![receipt.event.id],
                retained_user_facts: vec![],
                pending_tool_call_ids: vec![],
                uncertain_tool_call_ids: vec![],
            },
        )
        .unwrap();
    assert_eq!(f.read(&stdout.reference.id, 65535, 2).unwrap().bytes, b"xx");
    assert_eq!(
        f.repo
            .output(&f.session.id, "default", &stdout.reference.id)
            .unwrap()
            .event_id,
        Some(receipt.event.id)
    );
}
#[test]
fn forged_wrong_call_and_cross_session_references_are_rejected_atomically() {
    let f = Fixture::new();
    f.call("a");
    f.call("b");
    let out = f.capture("a-cap", "a", OutputChannel::Stdout, b"evidence");
    let rev = f.revision();
    let mut reference = out.reference.clone();
    reference.size += 1;
    let bad = Payload::ToolResult {
        call_id: "a".into(),
        outcome: Outcome::Success,
        exit_code: Some(0),
        text: "preview".into(),
        outputs: vec![reference],
    };
    assert!(matches!(
        f.repo.append(&f.session.id, "default", rev, "forged", bad),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        f.repo.append_captured_result(
            &f.session.id,
            "default",
            rev,
            "wrong",
            captured("b", vec![out.reference.id.clone()])
        ),
        Err(Error::ToolProtocol)
    ));
    let other = Fixture::from_repo(f.repo.clone());
    other.call("a");
    let cross = Payload::ToolResult {
        call_id: "a".into(),
        outcome: Outcome::Success,
        exit_code: Some(0),
        text: "preview".into(),
        outputs: vec![out.reference.clone()],
    };
    assert!(matches!(
        f.repo.append(
            &other.session.id,
            "default",
            other.revision(),
            "cross",
            cross
        ),
        Err(Error::NotFound)
    ));
    assert_eq!(f.revision(), rev);
    assert!(f
        .repo
        .output(&f.session.id, "default", &out.reference.id)
        .unwrap()
        .event_id
        .is_none());
}
#[test]
fn late_captured_result_keeps_original_task_and_target() {
    let f = Fixture::new();
    f.call("x");
    let writer = f.writer("out", "x", OutputChannel::Stdout);
    let old = f
        .repo
        .session(&f.session.id, "default")
        .unwrap()
        .task
        .unwrap();
    f.repo
        .start_task(
            &f.session.id,
            "default",
            f.revision(),
            "new",
            TaskRequest {
                goal: "另一台服务器".into(),
                constraints: vec![],
                target: Some(Target {
                    connection_id: "host-b".into(),
                    terminal_session_id: "pty-b".into(),
                }),
            },
        )
        .unwrap();
    let mut writer = writer;
    writer.write(b"late host-a evidence").unwrap();
    let out = writer.finish().unwrap();
    let event = f
        .repo
        .append_captured_result(
            &f.session.id,
            "default",
            f.revision(),
            "late",
            captured("x", vec![out.reference.id]),
        )
        .unwrap()
        .event;
    assert_eq!(event.task_id, Some(old.id));
    assert_eq!(
        f.repo
            .session(&f.session.id, "default")
            .unwrap()
            .task
            .unwrap()
            .target
            .unwrap()
            .connection_id,
        "host-b"
    );
}
#[test]
fn range_limits_and_eof_are_explicit() {
    let f = Fixture::new();
    f.call("x");
    let out = f.capture("cap", "x", OutputChannel::Stdout, "中文abc".as_bytes());
    for (offset, limit) in [(0, 0), (0, 65537), (10, 1), (u64::MAX, 1)] {
        assert!(matches!(
            f.read(&out.reference.id, offset, limit),
            Err(Error::Invalid(_))
        ));
    }
    let range = f.read(&out.reference.id, 9, 5).unwrap();
    assert!(range.eof);
    assert!(range.bytes.is_empty());
    assert_eq!(
        f.read(&out.reference.id, 1, 2).unwrap().bytes,
        &"中文abc".as_bytes()[1..3]
    );
}
#[test]
fn restart_restores_complete_chunks_and_does_not_reap_another_live_writer() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.sqlite3");
    let (id, out_id) = {
        let f = Fixture::from_repo(Repository::new(Arc::new(Store::open(&path).unwrap())));
        f.call("x");
        f.call("live");
        let out = f.capture("cap", "x", OutputChannel::Stdout, &vec![b'z'; 100000]);
        let mut live = f.writer("live-cap", "live", OutputChannel::Stdout);
        live.write(&vec![b'l'; 65536]).unwrap();
        let other = Repository::new(Arc::new(Store::open(&path).unwrap()));
        assert_eq!(other.recover_outputs().unwrap(), 0);
        live.write(b"tail").unwrap();
        let live = live.finish().unwrap();
        assert_eq!(
            other
                .read_output_range(&f.session.id, "default", &live.reference.id, 65536, 4)
                .unwrap()
                .bytes,
            b"tail"
        );
        (f.session.id, out.reference.id)
    };
    let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
    assert_eq!(
        repo.read_output_range(&id, "default", &out_id, 65530, 20)
            .unwrap()
            .bytes,
        vec![b'z'; 20]
    );
    assert_eq!(
        repo.output(&id, "default", &out_id).unwrap().reference.size,
        100000
    );
}
#[test]
fn expired_capture_is_recovered_but_stale_owner_cannot_publish_or_delete_a_replacement() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.sqlite3");
    let f = Fixture::from_repo(Repository::new(Arc::new(Store::open(&path).unwrap())));
    f.call("x");
    let mut old = f.writer("cap", "x", OutputChannel::Stdout);
    old.write(&vec![b'x'; 65536]).unwrap();
    let old_id = old.id().to_string();
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE agent_outputs SET expires_at=0 WHERE id=?1",
        [&old_id],
    )
    .unwrap();
    assert_eq!(f.repo.recover_outputs().unwrap(), 1);
    let mut replacement = f.writer("cap", "x", OutputChannel::Stdout);
    replacement.write(b"replacement").unwrap();
    assert!(matches!(old.write(b"late"), Err(Error::OutputUnavailable)));
    let output = replacement.finish().unwrap();
    assert_ne!(output.reference.id, old_id);
    assert_eq!(
        f.read(&output.reference.id, 0, 20).unwrap().bytes,
        b"replacement"
    );
    assert_eq!(
        db.query_row(
            "SELECT COUNT(*) FROM agent_output_chunks WHERE output_id=?1",
            [old_id],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
}
#[test]
fn corrupt_or_missing_chunk_returns_error_instead_of_partial_success() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.sqlite3");
    let f = Fixture::from_repo(Repository::new(Arc::new(Store::open(&path).unwrap())));
    f.call("x");
    let out = f.capture("cap", "x", OutputChannel::Stdout, &vec![b'x'; 100000]);
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute(
        "UPDATE agent_output_chunks SET data=zeroblob(65536) WHERE output_id=?1 AND chunk_index=0",
        [&out.reference.id],
    )
    .unwrap();
    assert!(matches!(
        f.read(&out.reference.id, 0, 20),
        Err(Error::OutputUnavailable)
    ));
    db.execute(
        "DELETE FROM agent_output_chunks WHERE output_id=?1 AND chunk_index=1",
        [&out.reference.id],
    )
    .unwrap();
    assert!(matches!(
        f.read(&out.reference.id, 65536, 20),
        Err(Error::OutputUnavailable)
    ));
}
#[test]
fn legacy_tool_results_without_references_remain_readable() {
    let legacy = r#"{"type":"toolResult","callId":"x","outcome":"unknown","exitCode":null,"text":"legacy result"}"#;
    let payload: Payload = serde_json::from_str(legacy).unwrap();
    assert!(matches!(payload,Payload::ToolResult{outputs,..} if outputs.is_empty()));
    let limits = OutputLimits {
        max_output_bytes: MAX_OUTPUT_BYTES + 1,
        ..Default::default()
    };
    assert!(matches!(
        Repository::with_output_limits(Arc::new(Store::in_memory().unwrap()), limits),
        Err(Error::Invalid(_))
    ));
}
