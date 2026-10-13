//! Combined context-kernel scenarios. No model requests or SSH commands are executed.
use shellpilot_server::{
    agent::{budget::*, *},
    store::Store,
};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Barrier,
};

const USER: &str = "default";
fn task(host: &str) -> TaskRequest {
    TaskRequest {
        goal: "排查日志，继续原任务".into(),
        constraints: vec!["不要重启服务".into()],
        target: Some(Target {
            connection_id: host.into(),
            terminal_session_id: format!("pty-{host}"),
        }),
    }
}
fn call(id: &str) -> Payload {
    Payload::ToolCall {
        call_id: id.into(),
        tool_name: "ssh_execute".into(),
        arguments: serde_json::json!({"command":"read logs"}),
    }
}
fn checkpoint(snapshot: &Snapshot, evidence: Vec<u64>) -> CheckpointContent {
    CheckpointContent {
        task: snapshot.session.task.clone(),
        summary: "保留原目标及不要重启的约束".into(),
        next_steps: vec!["回查输出并确认未完成操作".into()],
        evidence_event_ids: evidence,
        pending_tool_call_ids: snapshot.pending_tool_call_ids.clone(),
        uncertain_tool_call_ids: snapshot.uncertain_tool_call_ids.clone(),
    }
}
fn start(repo: &Repository, session: &str, request: &str, host: &str) -> Snapshot {
    let revision = repo.session(session, USER).unwrap().revision;
    repo.start_task(session, USER, revision, request, task(host))
        .unwrap();
    repo.snapshot(session, USER).unwrap()
}
fn append(repo: &Repository, session: &str, request: &str, payload: Payload) -> AppendReceipt {
    let revision = repo.session(session, USER).unwrap().revision;
    repo.append(session, USER, revision, request, payload)
        .unwrap()
}
fn captured(repo: &Repository, session: &str, call_id: &str, bytes: &[u8]) -> StoredOutput {
    let capture = repo
        .begin_output(
            session,
            USER,
            CaptureRequest {
                request_id: format!("output-{call_id}"),
                call_id: call_id.into(),
                channel: OutputChannel::Stdout,
            },
        )
        .unwrap();
    let CaptureStart::New(mut writer) = capture else {
        panic!("expected new capture")
    };
    writer.write(bytes).unwrap();
    writer.finish().unwrap()
}

#[test]
fn bound_admission_rejects_wrong_task_host_terminal_and_model_routing_atomically() {
    let repo = Repository::new(Arc::new(Store::in_memory().unwrap()));
    let session = repo.create_session("ssh-agent", USER).unwrap();
    let snapshot = start(&repo, &session.id, "start", "host-a");
    let binding = ToolBinding::from_snapshot(&snapshot).unwrap();
    for field in ["task", "host", "terminal"] {
        let mut wrong = binding.clone();
        match field {
            "task" => wrong.task_id = "another-task".into(),
            "host" => wrong.target.connection_id = "host-b".into(),
            _ => wrong.target.terminal_session_id = "pty-other".into(),
        }
        assert!(matches!(
            repo.append_tool_call(
                &session.id,
                USER,
                snapshot.session.revision,
                field,
                &wrong,
                call(field)
            ),
            Err(Error::TargetMismatch)
        ));
    }
    for (field, value) in [
        ("connectionId", "host-b"),
        ("terminalSessionId", "pty-other"),
        ("taskId", "other"),
    ] {
        let mut payload = call(field);
        if let Payload::ToolCall { arguments, .. } = &mut payload {
            arguments[field] = value.into();
        }
        assert!(matches!(
            repo.append_tool_call(
                &session.id,
                USER,
                snapshot.session.revision,
                field,
                &binding,
                payload
            ),
            Err(Error::TargetMismatch)
        ));
    }
    assert_eq!(repo.snapshot(&session.id, USER).unwrap(), snapshot);
    let receipt = repo
        .append_tool_call(
            &session.id,
            USER,
            snapshot.session.revision,
            "valid",
            &binding,
            call("valid"),
        )
        .unwrap();
    assert!(receipt.inserted);
    assert_eq!(
        repo.tool_call_binding(&session.id, USER, "valid").unwrap(),
        binding
    );
}

#[test]
fn duplicate_admission_has_one_winner_and_restart_returns_only_a_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("agent.sqlite3");
    let (session_id, binding, revision, original) = {
        let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
        let session = repo.create_session("ssh-agent", USER).unwrap();
        let snapshot = start(&repo, &session.id, "start", "host-a");
        let binding = ToolBinding::from_snapshot(&snapshot).unwrap();
        let gate = Arc::new(Barrier::new(3));
        let counts = Arc::new(AtomicUsize::new(0));
        let workers = (0..2)
            .map(|_| {
                let repo = repo.clone();
                let session_id = session.id.clone();
                let binding = binding.clone();
                let gate = gate.clone();
                let counts = counts.clone();
                std::thread::spawn(move || {
                    gate.wait();
                    let receipt = repo
                        .append_tool_call(
                            &session_id,
                            USER,
                            snapshot.session.revision,
                            "same-request",
                            &binding,
                            call("same-call"),
                        )
                        .unwrap();
                    // Simulated dispatch only: this does not prove actual SSH execution deduplication.
                    if receipt.inserted {
                        counts.fetch_add(1, Ordering::SeqCst);
                    }
                    receipt
                })
            })
            .collect::<Vec<_>>();
        gate.wait();
        let receipts = workers
            .into_iter()
            .map(|worker| worker.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(counts.load(Ordering::SeqCst), 1);
        assert_eq!(
            receipts.iter().filter(|receipt| receipt.inserted).count(),
            1
        );
        assert_eq!(receipts[0].event, receipts[1].event);
        (
            session.id,
            binding,
            snapshot.session.revision,
            receipts[0].event.clone(),
        )
    };
    let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
    let replay = repo
        .append_tool_call(
            &session_id,
            USER,
            revision,
            "same-request",
            &binding,
            call("same-call"),
        )
        .unwrap();
    assert!(!replay.inserted);
    assert_eq!(replay.event, original);
    assert_eq!(
        repo.snapshot(&session_id, USER)
            .unwrap()
            .uncertain_tool_call_ids,
        vec!["same-call"]
    );
    let current = repo.snapshot(&session_id, USER).unwrap();
    assert!(matches!(
        ModelBudget {
            context_window: 10000,
            output_reserve: 1000,
            safety_reserve: 100,
            compact_percent: 75
        }
        .plan(
            &InputCosts::default(),
            &group_history(&repo.events(&session_id, USER, 0, 200).unwrap()).unwrap()
        ),
        Err(Error::ToolProtocol)
    ));
    assert_eq!(repo.snapshot(&session_id, USER).unwrap(), current);
}

#[test]
fn continuation_to_another_host_preserves_old_receipts_and_late_captured_evidence() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("agent.sqlite3");
    let bytes = "旧主机完整输出🙂\n".repeat(8000).into_bytes();
    let (session_id, old_binding, new_binding, output_id, result_id, checkpoint_id) = {
        let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
        let session = repo.create_session("ssh-agent", USER).unwrap();
        let old = start(&repo, &session.id, "task-a", "host-a");
        let old_binding = ToolBinding::from_snapshot(&old).unwrap();
        repo.append_tool_call(
            &session.id,
            USER,
            old.session.revision,
            "original-call",
            &old_binding,
            call("old-call"),
        )
        .unwrap();
        let output = captured(&repo, &session.id, "old-call", &bytes);
        let new = start(&repo, &session.id, "task-b", "host-b");
        let new_binding = ToolBinding::from_snapshot(&new).unwrap();
        assert!(matches!(
            repo.append_tool_call(
                &session.id,
                USER,
                new.session.revision,
                "old-host-new-request",
                &old_binding,
                call("unsafe")
            ),
            Err(Error::TargetMismatch)
        ));
        assert!(matches!(
            repo.append_tool_call(
                &session.id,
                USER,
                new.session.revision,
                "original-call",
                &new_binding,
                call("old-call")
            ),
            Err(Error::TargetMismatch)
        ));
        let replay = repo
            .append_tool_call(
                &session.id,
                USER,
                old.session.revision,
                "original-call",
                &old_binding,
                call("old-call"),
            )
            .unwrap();
        assert!(!replay.inserted);
        let late = repo
            .append_captured_result(
                &session.id,
                USER,
                new.session.revision,
                "late-result",
                CapturedResult {
                    call_id: "old-call".into(),
                    outcome: Outcome::Success,
                    exit_code: Some(0),
                    output_ids: vec![output.reference.id.clone()],
                },
            )
            .unwrap();
        assert_eq!(
            late.event.task_id.as_deref(),
            Some(old_binding.task_id.as_str())
        );
        let snapshot = repo.snapshot(&session.id, USER).unwrap();
        let saved = repo
            .save_checkpoint(&snapshot, checkpoint(&snapshot, vec![late.event.id]))
            .unwrap();
        (
            session.id,
            old_binding,
            new_binding,
            output.reference.id,
            late.event.id,
            saved.id,
        )
    };
    let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
    assert_eq!(
        repo.tool_call_binding(&session_id, USER, "old-call")
            .unwrap(),
        old_binding
    );
    let snapshot = repo.snapshot(&session_id, USER).unwrap();
    assert_eq!(ToolBinding::from_snapshot(&snapshot).unwrap(), new_binding);
    let saved = repo.checkpoint(&session_id, USER).unwrap().unwrap();
    assert_eq!(saved.id, checkpoint_id);
    assert_eq!(saved.content.evidence_event_ids, vec![result_id]);
    assert_eq!(
        saved.content.task.unwrap().constraints,
        vec!["不要重启服务"]
    );
    let output = repo.output(&session_id, USER, &output_id).unwrap();
    assert_eq!(output.task_id, old_binding.task_id);
    assert!(output.preview_truncated);
    let mut restored = Vec::new();
    let mut offset = 0;
    loop {
        let range = repo
            .read_output_range(&session_id, USER, &output_id, offset, 65536)
            .unwrap();
        restored.extend_from_slice(&range.bytes);
        offset = range.next_offset;
        if range.eof {
            break;
        }
    }
    assert_eq!(restored, bytes);
    assert!(matches!(
        repo.discard_output(&session_id, USER, &output_id),
        Err(Error::Conflict)
    ));
}

#[test]
fn cancellation_and_new_constraints_reject_new_calls_and_stale_checkpoints_without_losing_evidence()
{
    let repo = Repository::new(Arc::new(Store::in_memory().unwrap()));
    let session = repo.create_session("ssh-agent", USER).unwrap();
    let start = start(&repo, &session.id, "start", "host-a");
    let binding = ToolBinding::from_snapshot(&start).unwrap();
    let receipt = repo
        .append_tool_call(
            &session.id,
            USER,
            start.session.revision,
            "call",
            &binding,
            call("pending"),
        )
        .unwrap();
    let snapshot = repo.snapshot(&session.id, USER).unwrap();
    repo.save_checkpoint(&snapshot, checkpoint(&snapshot, vec![receipt.event.id]))
        .unwrap();
    let before_cancel = repo.snapshot(&session.id, USER).unwrap();
    append(
        &repo,
        &session.id,
        "constraint",
        Payload::UserMessage {
            text: "不要删除文件".into(),
        },
    );
    append(
        &repo,
        &session.id,
        "cancel",
        Payload::Cancelled {
            reason: "用户停止".into(),
        },
    );
    assert!(matches!(
        repo.save_checkpoint(&before_cancel, checkpoint(&before_cancel, vec![])),
        Err(Error::Stale)
    ));
    let cancelled = repo.snapshot(&session.id, USER).unwrap();
    assert!(matches!(
        ToolBinding::from_snapshot(&cancelled),
        Err(Error::TargetMismatch)
    ));
    assert!(matches!(
        repo.append_tool_call(
            &session.id,
            USER,
            cancelled.session.revision,
            "new-call",
            &binding,
            call("new")
        ),
        Err(Error::TargetMismatch)
    ));
    let replay = repo
        .append_tool_call(
            &session.id,
            USER,
            start.session.revision,
            "call",
            &binding,
            call("pending"),
        )
        .unwrap();
    assert!(!replay.inserted);
    append(
        &repo,
        &session.id,
        "unknown",
        Payload::ToolResult {
            call_id: "pending".into(),
            outcome: Outcome::Unknown,
            exit_code: None,
            text: "尚不能确认是否执行，禁止自动重试".into(),
            outputs: vec![],
        },
    );
    let latest = repo.snapshot(&session.id, USER).unwrap();
    assert!(latest.pending_tool_call_ids.is_empty());
    assert_eq!(latest.uncertain_tool_call_ids, vec!["pending"]);
    assert_eq!(latest.session.task.unwrap().status, TaskStatus::Cancelled);
    let events = repo.events(&session.id, USER, 0, 200).unwrap();
    assert_eq!(events.len(), 5);
    assert!(events.iter().any(
        |event| matches!(&event.payload, Payload::UserMessage { text } if text == "不要删除文件")
    ));
}

#[test]
fn session_user_and_output_ownership_are_enforced_together() {
    let repo = Repository::new(Arc::new(Store::in_memory().unwrap()));
    let a = repo.create_session("ssh-agent", USER).unwrap();
    let b = repo.create_session("ssh-agent", USER).unwrap();
    let sa = start(&repo, &a.id, "a", "host-a");
    let sb = start(&repo, &b.id, "b", "host-b");
    let binding = ToolBinding::from_snapshot(&sa).unwrap();
    repo.append_tool_call(
        &a.id,
        USER,
        sa.session.revision,
        "call",
        &binding,
        call("call-a"),
    )
    .unwrap();
    let output = captured(&repo, &a.id, "call-a", b"evidence");
    assert!(matches!(
        repo.append_tool_call(
            &b.id,
            USER,
            sb.session.revision,
            "foreign",
            &binding,
            call("foreign")
        ),
        Err(Error::TargetMismatch)
    ));
    assert!(matches!(
        repo.tool_call_binding(&a.id, "other-user", "call-a"),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        repo.read_output_range(&b.id, USER, &output.reference.id, 0, 10),
        Err(Error::NotFound)
    ));
    assert!(repo
        .append_captured_result(
            &b.id,
            USER,
            sb.session.revision,
            "foreign-result",
            CapturedResult {
                call_id: "call-a".into(),
                outcome: Outcome::Success,
                exit_code: Some(0),
                output_ids: vec![output.reference.id]
            }
        )
        .is_err());
    assert_eq!(repo.snapshot(&b.id, USER).unwrap(), sb);
}

#[test]
fn smaller_model_windows_keep_tool_groups_atomic_and_reject_pinned_overflow() {
    let repo = Repository::new(Arc::new(Store::in_memory().unwrap()));
    let session = repo.create_session("ssh-agent", USER).unwrap();
    let snapshot = start(&repo, &session.id, "task", "host-a");
    let binding = ToolBinding::from_snapshot(&snapshot).unwrap();
    repo.append_tool_call(
        &session.id,
        USER,
        snapshot.session.revision,
        "call",
        &binding,
        call("read"),
    )
    .unwrap();
    let output = captured(&repo, &session.id, "read", &vec![b'x'; 100000]);
    let revision = repo.session(&session.id, USER).unwrap().revision;
    repo.append_captured_result(
        &session.id,
        USER,
        revision,
        "result",
        CapturedResult {
            call_id: "read".into(),
            outcome: Outcome::Success,
            exit_code: Some(0),
            output_ids: vec![output.reference.id],
        },
    )
    .unwrap();
    let events = repo.events(&session.id, USER, 0, 200).unwrap();
    let groups = group_history(&events).unwrap();
    assert_eq!(
        groups.last().unwrap().event_ids,
        vec![events[1].id, events[2].id]
    );
    let large = ModelBudget {
        context_window: 10000,
        output_reserve: 1000,
        safety_reserve: 100,
        compact_percent: 75,
    };
    assert!(
        !large
            .plan(&InputCosts::default(), &groups)
            .unwrap()
            .should_compact
    );
    let small = ModelBudget {
        context_window: 1200,
        ..large
    };
    let plan = small.plan(&InputCosts::default(), &groups).unwrap();
    assert!(plan.should_compact);
    assert!(plan.selected_event_ids.is_empty());
    assert!(matches!(
        small.plan(
            &InputCosts {
                fixed_tokens: 50,
                pinned_tokens: 30,
                media_tokens: 21
            },
            &groups
        ),
        Err(Error::BudgetExceeded)
    ));
    assert_eq!(repo.events(&session.id, USER, 0, 200).unwrap(), events);
}
