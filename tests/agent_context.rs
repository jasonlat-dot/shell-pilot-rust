use shellpilot_server::{
    agent::{
        budget::*, CheckpointContent, Error, Event, Outcome, Payload, Repository, Session, Target,
        TaskRequest, TaskStatus,
    },
    store::Store,
};
use std::sync::{Arc, Barrier};

struct Fixture {
    repo: Repository,
    session: Session,
}
impl Fixture {
    fn new() -> Self {
        let repo = Repository::new(Arc::new(Store::in_memory().unwrap()));
        let session = repo.create_session("ssh-agent", "default").unwrap();
        Self { repo, session }
    }
    fn current(&self) -> Session {
        self.repo.session(&self.session.id, "default").unwrap()
    }
    fn start(&self, request: &str, host: &str) -> Event {
        self.repo
            .start_task(
                &self.session.id,
                "default",
                self.current().revision,
                request,
                task(host),
            )
            .unwrap()
    }
    fn append(&self, request: &str, payload: Payload) -> Event {
        let receipt = self
            .repo
            .append(
                &self.session.id,
                "default",
                self.current().revision,
                request,
                payload,
            )
            .unwrap();
        assert!(receipt.inserted);
        receipt.event
    }
    fn events(&self) -> Vec<Event> {
        self.repo
            .events(&self.session.id, "default", 0, 200)
            .unwrap()
    }
}
fn task(host: &str) -> TaskRequest {
    TaskRequest {
        goal: "排查 nginx 502".into(),
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
        arguments: serde_json::json!({"command":"tail -n 20 /var/log/nginx/error.log"}),
    }
}
fn result(id: &str, outcome: Outcome) -> Payload {
    Payload::ToolResult {
        call_id: id.into(),
        exit_code: if outcome == Outcome::Success {
            Some(0)
        } else {
            None
        },
        outcome,
        text: "evidence".into(),
        outputs: vec![],
    }
}
fn content(snapshot: &shellpilot_server::agent::Snapshot) -> CheckpointContent {
    CheckpointContent {
        task: snapshot.session.task.clone(),
        summary: "已检查错误日志，下一步检查 upstream，不要重启服务".into(),
        next_steps: vec!["读取 upstream 配置".into()],
        evidence_event_ids: vec![],
        pending_tool_call_ids: snapshot.pending_tool_call_ids.clone(),
        uncertain_tool_call_ids: snapshot.uncertain_tool_call_ids.clone(),
    }
}
fn model() -> ModelBudget {
    ModelBudget {
        context_window: 1000,
        output_reserve: 200,
        safety_reserve: 100,
        compact_percent: 75,
    }
}

#[test]
fn sessions_isolate_users_events_and_targets() {
    let a = Fixture::new();
    a.start("task-a", "host-a");
    let b = a.repo.create_session("ssh-agent", "other").unwrap();
    a.repo
        .start_task(&b.id, "other", 0, "task-b", task("host-b"))
        .unwrap();
    assert_eq!(
        a.current().task.unwrap().target.unwrap().connection_id,
        "host-a"
    );
    assert_eq!(
        a.repo
            .session(&b.id, "other")
            .unwrap()
            .task
            .unwrap()
            .target
            .unwrap()
            .connection_id,
        "host-b"
    );
    assert!(matches!(
        a.repo.session(&b.id, "default"),
        Err(Error::NotFound)
    ));
    assert!(matches!(
        a.repo.events(&b.id, "default", 0, 20),
        Err(Error::NotFound)
    ));
    assert_eq!(a.events().len(), 1);
}
#[test]
fn retries_return_receipts_without_appending_or_executing_again() {
    let f = Fixture::new();
    f.start("task", "host-a");
    let rev = f.current().revision;
    let first = f
        .repo
        .append(&f.session.id, "default", rev, "request-1", call("call-1"))
        .unwrap();
    let replay = f
        .repo
        .append(&f.session.id, "default", rev, "request-1", call("call-1"))
        .unwrap();
    assert!(first.inserted);
    assert!(!replay.inserted);
    assert_eq!(first.event, replay.event);
    assert_eq!(f.current().revision, rev + 1);
    assert_eq!(f.events().len(), 2);
    assert!(matches!(
        f.repo.append(
            &f.session.id,
            "default",
            rev,
            "request-1",
            call("different")
        ),
        Err(Error::Conflict)
    ));
    assert!(matches!(
        f.repo.append(
            &f.session.id,
            "default",
            rev + 1,
            "request-2",
            call("call-1")
        ),
        Err(Error::Conflict)
    ));
}
#[test]
fn late_result_stays_on_original_task_after_target_changes() {
    let f = Fixture::new();
    let original = f.start("task-a", "host-a");
    f.append("call", call("late"));
    f.start("task-b", "host-b");
    let late = f.append("result", result("late", Outcome::Success));
    assert_eq!(late.task_id, original.task_id);
    assert_ne!(
        late.task_id,
        f.current().task.as_ref().map(|task| task.id.clone())
    );
    assert_eq!(
        f.current().task.unwrap().target.unwrap().connection_id,
        "host-b"
    );
    assert!(f
        .repo
        .snapshot(&f.session.id, "default")
        .unwrap()
        .pending_tool_call_ids
        .is_empty());
}
#[test]
fn tool_outcomes_require_calls_and_cancellation_never_fakes_success() {
    let f = Fixture::new();
    f.start("task", "host");
    assert!(matches!(
        f.repo.append(
            &f.session.id,
            "default",
            1,
            "orphan",
            result("missing", Outcome::Success)
        ),
        Err(Error::ToolProtocol)
    ));
    f.append("call", call("unknown"));
    f.append(
        "cancel",
        Payload::Cancelled {
            reason: "用户停止".into(),
        },
    );
    assert_eq!(f.current().task.unwrap().status, TaskStatus::Cancelled);
    let snapshot = f.repo.snapshot(&f.session.id, "default").unwrap();
    assert_eq!(snapshot.pending_tool_call_ids, vec!["unknown"]);
    assert!(matches!(
        f.repo.append(
            &f.session.id,
            "default",
            f.current().revision,
            "new-call",
            call("new")
        ),
        Err(Error::Invalid(_))
    ));
    f.append("unknown-result", result("unknown", Outcome::Unknown));
    assert!(f
        .repo
        .snapshot(&f.session.id, "default")
        .unwrap()
        .pending_tool_call_ids
        .is_empty());
    assert!(matches!(
        f.events().last().unwrap().payload,
        Payload::ToolResult {
            outcome: Outcome::Unknown,
            ..
        }
    ));
}
#[test]
fn checkpoint_keeps_original_events_constraints_and_pending_actions() {
    let f = Fixture::new();
    f.start("task", "host");
    f.append(
        "user",
        Payload::UserMessage {
            text: "先看日志，不要重启".into(),
        },
    );
    f.append("call", call("uncertain"));
    let original = f.events();
    let snapshot = f.repo.snapshot(&f.session.id, "default").unwrap();
    let mut input = content(&snapshot);
    input.evidence_event_ids = vec![original[1].id];
    let checkpoint = f.repo.save_checkpoint(&snapshot, input).unwrap();
    assert_eq!(
        checkpoint.content.task.unwrap().constraints,
        vec!["不要重启服务"]
    );
    assert_eq!(checkpoint.content.pending_tool_call_ids, vec!["uncertain"]);
    assert_eq!(checkpoint.source_revision, snapshot.session.revision);
    assert_eq!(f.current().revision, snapshot.session.revision + 1);
    assert_eq!(f.events(), original);
    assert_eq!(
        f.repo
            .checkpoint(&f.session.id, "default")
            .unwrap()
            .unwrap()
            .id,
        checkpoint.id
    );
}
#[test]
fn checkpoint_cannot_drop_constraints_target_or_uncertain_calls() {
    let f = Fixture::new();
    f.start("task", "host");
    f.append("call", call("uncertain"));
    let snapshot = f.repo.snapshot(&f.session.id, "default").unwrap();
    let mut input = content(&snapshot);
    input.task.as_mut().unwrap().constraints.clear();
    assert!(matches!(
        f.repo.save_checkpoint(&snapshot, input),
        Err(Error::Invalid(_))
    ));
    let mut input = content(&snapshot);
    input.task.as_mut().unwrap().target = Some(task("other").target.unwrap());
    assert!(matches!(
        f.repo.save_checkpoint(&snapshot, input),
        Err(Error::Invalid(_))
    ));
    let mut input = content(&snapshot);
    input.pending_tool_call_ids.clear();
    assert!(matches!(
        f.repo.save_checkpoint(&snapshot, input),
        Err(Error::Invalid(_))
    ));
    assert!(f
        .repo
        .checkpoint(&f.session.id, "default")
        .unwrap()
        .is_none());
    assert_eq!(f.current().revision, snapshot.session.revision);
}
#[test]
fn newer_messages_target_changes_and_cancellation_invalidate_compression_snapshot() {
    for action in ["message", "cancel", "target"] {
        let f = Fixture::new();
        f.start("task", "host");
        let snapshot = f.repo.snapshot(&f.session.id, "default").unwrap();
        match action {
            "message" => {
                f.append(
                    "steer",
                    Payload::UserMessage {
                        text: "不要修改配置".into(),
                    },
                );
            }
            "cancel" => {
                f.append(
                    "stop",
                    Payload::Cancelled {
                        reason: "停止".into(),
                    },
                );
            }
            _ => {
                f.start("new-target", "host-b");
            }
        }
        assert!(matches!(
            f.repo.save_checkpoint(&snapshot, content(&snapshot)),
            Err(Error::Stale)
        ));
        assert!(f
            .repo
            .checkpoint(&f.session.id, "default")
            .unwrap()
            .is_none());
    }
}
#[test]
fn checkpoint_evidence_cannot_reference_other_sessions_or_future_events() {
    let f = Fixture::new();
    let other = f.repo.create_session("ssh-agent", "default").unwrap();
    let alien = f
        .repo
        .append(
            &other.id,
            "default",
            0,
            "other",
            Payload::UserMessage {
                text: "other".into(),
            },
        )
        .unwrap()
        .event;
    f.start("task", "host");
    let snapshot = f.repo.snapshot(&f.session.id, "default").unwrap();
    for id in [0, alien.id, snapshot.through_event_id + 1] {
        let mut input = content(&snapshot);
        input.evidence_event_ids = vec![id];
        assert!(matches!(
            f.repo.save_checkpoint(&snapshot, input),
            Err(Error::Invalid(_))
        ));
    }
}
#[test]
fn restart_restores_checkpoint_raw_events_and_uncertain_actions() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("test.sqlite3");
    let (id, events, checkpoint_id) = {
        let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
        let s = repo.create_session("ssh-agent", "default").unwrap();
        repo.start_task(&s.id, "default", 0, "task", task("host-a"))
            .unwrap();
        repo.append(&s.id, "default", 1, "call", call("pending"))
            .unwrap();
        let snapshot = repo.snapshot(&s.id, "default").unwrap();
        let checkpoint = repo.save_checkpoint(&snapshot, content(&snapshot)).unwrap();
        let events = repo.events(&s.id, "default", 0, 200).unwrap();
        (s.id, events, checkpoint.id)
    };
    let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
    assert_eq!(repo.events(&id, "default", 0, 200).unwrap(), events);
    assert_eq!(
        repo.snapshot(&id, "default").unwrap().pending_tool_call_ids,
        vec!["pending"]
    );
    assert_eq!(
        repo.checkpoint(&id, "default").unwrap().unwrap().id,
        checkpoint_id
    );
    let schema = rusqlite::Connection::open(path).unwrap();
    assert_eq!(
        schema
            .query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        6
    );
}
#[test]
fn concurrent_appends_have_one_winner_and_no_partial_event() {
    let f = Fixture::new();
    let gate = Arc::new(Barrier::new(3));
    let workers = (0..2)
        .map(|n| {
            let repo = f.repo.clone();
            let id = f.session.id.clone();
            let gate = gate.clone();
            std::thread::spawn(move || {
                gate.wait();
                repo.append(
                    &id,
                    "default",
                    0,
                    &format!("req-{n}"),
                    Payload::UserMessage {
                        text: format!("message-{n}"),
                    },
                )
            })
        })
        .collect::<Vec<_>>();
    gate.wait();
    let results = workers
        .into_iter()
        .map(|worker| worker.join().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(Error::Stale)))
            .count(),
        1
    );
    assert_eq!(f.events().len(), 1);
    assert_eq!(f.current().revision, 1);
}
#[test]
fn pending_call_admission_is_bounded_and_unknown_result_releases_a_slot() {
    let f = Fixture::new();
    f.start("task", "host");
    for n in 0..64 {
        f.append(&format!("req-{n}"), call(&format!("call-{n}")));
    }
    assert!(matches!(
        f.repo.append(
            &f.session.id,
            "default",
            f.current().revision,
            "full",
            call("full")
        ),
        Err(Error::Invalid(_))
    ));
    f.append("unknown", result("call-0", Outcome::Unknown));
    f.append("new", call("new"));
    assert_eq!(
        f.repo
            .snapshot(&f.session.id, "default")
            .unwrap()
            .pending_tool_call_ids
            .len(),
        64
    );
}
#[test]
fn validation_rejects_oversized_payload_and_conflicting_exit_status_atomically() {
    let f = Fixture::new();
    f.start("task", "host");
    let revision = f.current().revision;
    let bad = Payload::ToolResult {
        call_id: "x".into(),
        outcome: Outcome::Success,
        exit_code: Some(1),
        text: "bad".into(),
        outputs: vec![],
    };
    assert!(matches!(
        f.repo
            .append(&f.session.id, "default", revision, "bad", bad),
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        f.repo.append(
            &f.session.id,
            "default",
            revision,
            "large",
            Payload::UserMessage {
                text: "x".repeat(16001)
            }
        ),
        Err(Error::Invalid(_))
    ));
    let mut request = task("host");
    request.constraints = vec!["x".repeat(4000); 32];
    assert!(matches!(
        f.repo
            .start_task(&f.session.id, "default", revision, "huge-task", request),
        Err(Error::Invalid(_))
    ));
    assert_eq!(f.current().revision, revision);
    assert_eq!(f.events().len(), 1);
}

#[test]
fn request_budget_accounts_for_fixed_pinned_media_output_and_safety() {
    let costs = InputCosts {
        fixed_tokens: 100,
        pinned_tokens: 150,
        media_tokens: 50,
    };
    let groups = vec![
        HistoryGroup {
            event_ids: vec![1],
            tokens: 250,
            complete: true,
        },
        HistoryGroup {
            event_ids: vec![2, 3],
            tokens: 200,
            complete: true,
        },
    ];
    let plan = model().plan(&costs, &groups).unwrap();
    assert_eq!(plan.available_input_tokens, 700);
    assert_eq!(plan.selected_event_ids, vec![2, 3]);
    assert_eq!(plan.estimated_input_tokens, 500);
    assert_eq!(plan.estimated_full_input_tokens, 750);
    assert!(plan.should_compact);
    assert_eq!(plan.dropped_groups, 1);
    assert!(matches!(
        model().plan(
            &InputCosts {
                pinned_tokens: 701,
                ..Default::default()
            },
            &[]
        ),
        Err(Error::BudgetExceeded)
    ));
    assert!(matches!(
        model().plan(
            &InputCosts {
                fixed_tokens: u64::MAX,
                pinned_tokens: 1,
                ..Default::default()
            },
            &[]
        ),
        Err(Error::BudgetExceeded)
    ));
}
#[test]
fn model_downshift_forces_compaction_and_utf8_arguments_are_counted() {
    let f = Fixture::new();
    f.start("task", "host");
    let event = f.append(
        "call",
        Payload::ToolCall {
            call_id: "x".into(),
            tool_name: "ssh".into(),
            arguments: serde_json::json!({"command":"中".repeat(100)}),
        },
    );
    assert!(estimate_event(&event).unwrap() > 100);
    assert_eq!(estimate_text("中文"), 2);
    let groups = vec![HistoryGroup {
        event_ids: vec![1, 2],
        tokens: 600,
        complete: true,
    }];
    assert!(
        model()
            .plan(
                &InputCosts {
                    fixed_tokens: 200,
                    ..Default::default()
                },
                &groups
            )
            .unwrap()
            .should_compact
    );
    let larger = ModelBudget {
        context_window: 4000,
        ..model()
    };
    assert!(
        !larger
            .plan(
                &InputCosts {
                    fixed_tokens: 200,
                    ..Default::default()
                },
                &groups
            )
            .unwrap()
            .should_compact
    );
}
#[test]
fn parallel_tool_calls_and_results_are_retained_as_one_atomic_group() {
    let f = Fixture::new();
    f.start("task", "host");
    f.append("a", call("a"));
    f.append("b", call("b"));
    f.append("result-b", result("b", Outcome::Error));
    f.append("result-a", result("a", Outcome::Success));
    let groups = group_history(&f.events()).unwrap();
    assert_eq!(groups.len(), 2);
    assert_eq!(groups[1].event_ids.len(), 4);
    assert!(groups[1].complete);
    let plan = model()
        .plan(
            &InputCosts {
                pinned_tokens: 690,
                ..Default::default()
            },
            &groups,
        )
        .unwrap();
    assert!(plan.selected_event_ids.is_empty());
    assert!(plan.should_compact); // The host must compact before trying to sample.
}
#[test]
fn partial_or_orphan_tool_protocol_cannot_be_sent_to_model() {
    let f = Fixture::new();
    f.start("task", "host");
    f.append("call", call("x"));
    let groups = group_history(&f.events()).unwrap();
    assert!(!groups.last().unwrap().complete);
    assert!(matches!(
        model().plan(&InputCosts::default(), &groups),
        Err(Error::ToolProtocol)
    ));
    let mut orphan = f.events()[1].clone();
    orphan.payload = result("missing", Outcome::Success);
    assert!(matches!(group_history(&[orphan]), Err(Error::ToolProtocol)));
    let mut unordered = f.events();
    unordered.reverse();
    assert!(matches!(group_history(&unordered), Err(Error::Invalid(_))));
}

#[test]
fn explicit_unknown_output_remains_uncertain_in_checkpoint_and_after_restart() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("uncertain.sqlite3");
    let id = {
        let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
        let s = repo.create_session("ssh-agent", "default").unwrap();
        repo.start_task(&s.id, "default", 0, "task", task("host"))
            .unwrap();
        repo.append(&s.id, "default", 1, "call", call("x")).unwrap();
        repo.append(
            &s.id,
            "default",
            2,
            "unknown",
            result("x", Outcome::Unknown),
        )
        .unwrap();
        let snapshot = repo.snapshot(&s.id, "default").unwrap();
        assert!(snapshot.pending_tool_call_ids.is_empty());
        assert_eq!(snapshot.uncertain_tool_call_ids, vec!["x"]);
        let mut dropped = content(&snapshot);
        dropped.uncertain_tool_call_ids.clear();
        assert!(matches!(
            repo.save_checkpoint(&snapshot, dropped),
            Err(Error::Invalid(_))
        ));
        repo.save_checkpoint(&snapshot, content(&snapshot)).unwrap();
        s.id
    };
    let repo = Repository::new(Arc::new(Store::open(&path).unwrap()));
    assert_eq!(
        repo.snapshot(&id, "default")
            .unwrap()
            .uncertain_tool_call_ids,
        vec!["x"]
    );
    assert_eq!(
        repo.checkpoint(&id, "default")
            .unwrap()
            .unwrap()
            .content
            .uncertain_tool_call_ids,
        vec!["x"]
    );
}
