#![cfg(windows)]
#[allow(dead_code)]
mod support;
use shellpilot_server::{
    agent::{
        approvals::{Approvals, Decision, Policy, Receipt, State},
        ssh_tools::{self, Command, SshExecutor},
        Payload, Repository, Session, Target, TaskRequest,
    },
    store::Store,
    terminal::{TerminalManager, TerminalSettings},
};
use std::{
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use support::*;
use tokio_util::sync::CancellationToken;
struct Bound {
    _directory: tempfile::TempDir,
    path: PathBuf,
    fixture: Fixture,
    store: Arc<Store>,
    terminals: Arc<TerminalManager>,
    repo: Repository,
    session: Session,
    target: Target,
    approvals: Approvals,
}
impl Bound {
    async fn new(spec: Spec) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent.sqlite");
        let fixture = Fixture::new(spec).await;
        let store = Arc::new(Store::open(&path).unwrap());
        let conn = store.save(fixture.request(false), false).unwrap();
        let terminals = Arc::new(TerminalManager::with_settings(
            store.clone(),
            TerminalSettings::default(),
        ));
        let terminal = terminals.connect(open(&conn.connection_id)).await.unwrap();
        let target = Target {
            connection_id: conn.connection_id,
            terminal_session_id: terminal.session_id,
        };
        let repo = Repository::new(store.clone());
        let session = repo.create_session("model", "default").unwrap();
        repo.start_task(
            &session.id,
            "default",
            0,
            "task",
            TaskRequest {
                goal: "诊断 A，保持原约束".into(),
                constraints: vec!["只执行用户批准的命令".into()],
                target: Some(target.clone()),
            },
        )
        .unwrap();
        let session = repo.session(&session.id, "default").unwrap();
        let approvals = Approvals::new(store.clone());
        Self {
            _directory: directory,
            path,
            fixture,
            store,
            terminals,
            repo,
            session,
            target,
            approvals,
        }
    }
    async fn executor(&self) -> SshExecutor {
        SshExecutor::bind(
            self.store.clone(),
            self.terminals.clone(),
            &self.session.id,
            "default",
            &self.session.task.as_ref().unwrap().id,
        )
        .await
        .unwrap()
        .with_approvals(self.approvals.clone())
        .unwrap()
    }
    async fn pending(&self, call: &str) -> Receipt {
        tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let rows = self
                    .approvals
                    .list(self.session.id.clone(), "default".into(), "".into())
                    .await
                    .unwrap();
                if let Some(row) = rows.into_iter().find(|r| r.call_id == call) {
                    return row;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }
    async fn decide(&self, r: &Receipt, d: Decision) {
        self.approvals
            .decide(r.id.clone(), "default".into(), d)
            .await
            .unwrap();
    }
    fn execs(&self) -> usize {
        self.fixture.stats.execs.lock().unwrap().len()
    }
    async fn state(&self, r: &Receipt) -> State {
        self.approvals
            .receipt(r.id.clone(), "default".into())
            .await
            .unwrap()
            .state
    }
    fn cancel_task(&self) {
        let s = self.repo.session(&self.session.id, "default").unwrap();
        self.repo
            .append(
                &s.id,
                "default",
                s.revision,
                "stop",
                Payload::Cancelled {
                    reason: "停止任务".into(),
                },
            )
            .unwrap();
    }
}
fn command(text: &str) -> Command {
    Command {
        command: text.into(),
        timeout_seconds: 30,
        explanation: None,
    }
}
fn run(
    executor: SshExecutor,
    call: &str,
    text: &str,
    cancel: CancellationToken,
) -> tokio::task::JoinHandle<ssh_tools::Result<ssh_tools::CommandResult>> {
    let call = call.to_owned();
    let cmd = command(text);
    tokio::spawn(async move { executor.execute_command(&call, &cmd, &cancel).await })
}
#[tokio::test]
async fn explicit_denial_wrong_owner_and_immutable_approval_never_send_exec() {
    let b = Bound::new(Spec::default()).await;
    let job = run(
        b.executor().await,
        "deny",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    let pending = b.pending("deny").await;
    assert_eq!(pending.state, State::Pending);
    assert_eq!(pending.command.command, "__exec_unicode__");
    assert_eq!(pending.binding.tool.target, b.target);
    assert_eq!(b.execs(), 0);
    assert!(b
        .approvals
        .decide(pending.id.clone(), "other".into(), Decision::ApproveOnce)
        .await
        .is_err());
    assert!(b
        .approvals
        .list(b.session.id.clone(), "other".into(), "".into())
        .await
        .is_err());
    b.decide(&pending, Decision::Deny).await;
    assert!(matches!(job.await.unwrap(), Err(ssh_tools::Error::Denied)));
    assert_eq!(b.state(&pending).await, State::Denied);
    assert!(matches!(
        b.executor()
            .await
            .execute_command(
                "deny",
                &command("__exec_unicode__"),
                &CancellationToken::new()
            )
            .await,
        Err(ssh_tools::Error::Denied)
    ));
    assert_eq!(b.execs(), 0);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn approve_once_receipt_replay_is_persistent_offline_and_never_authorizes_a_second_call() {
    let b = Bound::new(Spec::default()).await;
    let executor = b.executor().await;
    let job = run(
        b.executor().await,
        "once",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    let pending = b.pending("once").await;
    b.decide(&pending, Decision::ApproveOnce).await;
    let result = job.await.unwrap().unwrap();
    assert_eq!(result.stdout, "执行完成\n");
    assert_eq!(b.execs(), 1);
    assert_eq!(b.state(&pending).await, State::Completed);
    let replay = executor
        .execute_command(
            "once",
            &command("__exec_unicode__"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(replay.stdout, result.stdout);
    assert_eq!(b.execs(), 1);
    assert!(matches!(
        executor
            .execute_command("once", &command("__exec_fail__"), &CancellationToken::new())
            .await,
        Err(ssh_tools::Error::Conflict)
    ));
    let token = CancellationToken::new();
    let next = run(
        b.executor().await,
        "new-call",
        "__exec_unicode__",
        token.clone(),
    );
    let second = b.pending("new-call").await;
    assert_eq!(second.state, State::Pending);
    assert_eq!(b.execs(), 1);
    token.cancel();
    assert!(next.await.unwrap().is_err());
    b.terminals.close(&b.target.terminal_session_id);
    assert!(executor
        .execute_command(
            "once",
            &command("__exec_unicode__"),
            &CancellationToken::new()
        )
        .await
        .is_ok());
    assert_eq!(b.execs(), 1);
    let reopened = Approvals::new(Arc::new(Store::open(&b.path).unwrap()));
    let saved = reopened
        .receipt(pending.id.clone(), "default".into())
        .await
        .unwrap();
    assert_eq!(saved.state, State::Completed);
    assert_eq!(saved.result.unwrap().stdout, result.stdout);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn duplicate_concurrent_calls_have_one_waiter_and_one_dispatch() {
    let b = Bound::new(Spec::default()).await;
    let first = run(
        b.executor().await,
        "same",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    let pending = b.pending("same").await;
    let second = run(
        b.executor().await,
        "same",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    assert!(matches!(second.await.unwrap(), Err(ssh_tools::Error::Busy)));
    b.decide(&pending, Decision::ApproveOnce).await;
    first.await.unwrap().unwrap();
    assert_eq!(b.execs(), 1);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn task_grant_is_exact_including_timeout_not_prefix_or_different_target_and_can_be_revoked() {
    let b = Bound::new(Spec::default()).await;
    let first = run(
        b.executor().await,
        "grant",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    let pending = b.pending("grant").await;
    b.decide(&pending, Decision::ApproveTaskCommand { seconds: 60 })
        .await;
    first.await.unwrap().unwrap();
    // Persisted task grants do not turn the default-deny executor into an enabled host.
    let default = SshExecutor::bind(
        b.store.clone(),
        b.terminals.clone(),
        &b.session.id,
        "default",
        &b.session.task.as_ref().unwrap().id,
    )
    .await
    .unwrap();
    assert!(matches!(
        default
            .execute_command(
                "still-default-deny",
                &command("__exec_unicode__"),
                &CancellationToken::new()
            )
            .await,
        Err(ssh_tools::Error::Denied)
    ));
    assert_eq!(b.execs(), 1);
    // A fresh native controller over the same persisted DB can use the exact task-scoped grant.
    let controller = Approvals::new(b.store.clone());
    let executor = b.executor().await.with_approvals(controller).unwrap();
    executor
        .execute_command(
            "granted",
            &command("__exec_unicode__"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(b.execs(), 2);
    for (call, cmd) in [
        (
            "changed-timeout",
            Command {
                command: "__exec_unicode__".into(),
                timeout_seconds: 31,
                explanation: None,
            },
        ),
        ("compound", command("__exec_unicode__; unexpected")),
    ] {
        let executor = b.executor().await;
        let token = CancellationToken::new();
        let stop = token.clone();
        let job = tokio::spawn(async move { executor.execute_command(call, &cmd, &token).await });
        let r = b.pending(call).await;
        assert_eq!(r.state, State::Pending);
        assert_eq!(b.execs(), 2);
        stop.cancel();
        assert!(job.await.unwrap().is_err());
    }
    b.approvals
        .revoke_grant(pending.id.clone(), "default".into())
        .await
        .unwrap();
    let token = CancellationToken::new();
    let last = run(
        b.executor().await,
        "revoked",
        "__exec_unicode__",
        token.clone(),
    );
    assert_eq!(b.pending("revoked").await.state, State::Pending);
    token.cancel();
    assert!(last.await.unwrap().is_err());
    assert_eq!(b.execs(), 2);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn revoking_grant_while_exec_channel_opens_prevents_dispatch() {
    let b = Bound::new(Spec {
        open_delay: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    let first = run(
        b.executor().await,
        "grant",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    let r = b.pending("grant").await;
    b.decide(&r, Decision::ApproveTaskCommand { seconds: 60 })
        .await;
    first.await.unwrap().unwrap();
    eventually(|| b.fixture.stats.channels.load(Ordering::SeqCst) == 1).await;
    let second = run(
        b.executor().await,
        "after-grant",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    eventually(|| b.fixture.stats.channels.load(Ordering::SeqCst) == 2).await;
    b.approvals
        .revoke_grant(r.id.clone(), "default".into())
        .await
        .unwrap();
    assert!(matches!(
        second.await.unwrap(),
        Err(ssh_tools::Error::Denied)
    ));
    assert_eq!(b.execs(), 1);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn policy_cas_deny_and_revocation_override_approved_requests() {
    let b = Bound::new(Spec {
        open_delay: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    let task = b.session.task.as_ref().unwrap().id.clone();
    assert!(b
        .approvals
        .set_policy(
            b.session.id.clone(),
            "other".into(),
            task.clone(),
            0,
            Policy::Confirm
        )
        .await
        .is_err());
    assert_eq!(
        b.approvals
            .set_policy(
                b.session.id.clone(),
                "default".into(),
                task.clone(),
                0,
                Policy::Confirm
            )
            .await
            .unwrap(),
        1
    );
    assert!(b
        .approvals
        .set_policy(
            b.session.id.clone(),
            "default".into(),
            task.clone(),
            0,
            Policy::Deny
        )
        .await
        .is_err());
    let job = run(
        b.executor().await,
        "approved",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    let r = b.pending("approved").await;
    b.decide(&r, Decision::ApproveOnce).await;
    eventually(|| b.fixture.stats.channels.load(Ordering::SeqCst) == 2).await;
    b.approvals
        .set_policy(
            b.session.id.clone(),
            "default".into(),
            task.clone(),
            1,
            Policy::Deny,
        )
        .await
        .unwrap();
    assert!(matches!(job.await.unwrap(), Err(ssh_tools::Error::Denied)));
    assert_eq!(b.execs(), 0);
    assert!(matches!(
        b.executor()
            .await
            .execute_command(
                "policy-block",
                &command("__exec_unicode__"),
                &CancellationToken::new()
            )
            .await,
        Err(ssh_tools::Error::Denied)
    ));
    assert_eq!(b.execs(), 0);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn pending_cancel_drop_task_change_and_disconnect_invalidate_late_decisions() {
    for mode in 0..4 {
        let b = Bound::new(Spec::default()).await;
        let token = CancellationToken::new();
        let job = run(
            b.executor().await,
            "pending",
            "__exec_unicode__",
            token.clone(),
        );
        let r = b.pending("pending").await;
        match mode {
            0 => token.cancel(),
            1 => job.abort(),
            2 => b.cancel_task(),
            _ => {
                b.terminals.close(&b.target.terminal_session_id);
            }
        }
        assert!(tokio::time::timeout(Duration::from_secs(3), job)
            .await
            .unwrap()
            .map_or(true, |r| r.is_err()));
        let saved = b
            .approvals
            .receipt(r.id.clone(), "default".into())
            .await
            .unwrap();
        assert_eq!(saved.state, State::Cancelled);
        assert!(b
            .approvals
            .decide(r.id.clone(), "default".into(), Decision::ApproveOnce)
            .await
            .is_err());
        assert_eq!(b.execs(), 0);
        b.terminals.shutdown().await;
    }
}
#[tokio::test]
async fn expired_request_cannot_execute_and_a_new_request_does_not_inherit_permission() {
    let b = Bound::new(Spec::default()).await;
    let approvals = Approvals::with_lifetime(b.store.clone(), Duration::from_millis(80)).unwrap();
    let executor = b.executor().await.with_approvals(approvals).unwrap();
    let job = run(
        executor,
        "expire",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    let r = b.pending("expire").await;
    assert!(tokio::time::timeout(Duration::from_secs(2), job)
        .await
        .unwrap()
        .unwrap()
        .is_err());
    assert_eq!(b.state(&r).await, State::Expired);
    assert!(b
        .approvals
        .decide(r.id.clone(), "default".into(), Decision::ApproveOnce)
        .await
        .is_err());
    assert_eq!(b.execs(), 0);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn unknown_after_send_cancellation_or_drop_blocks_new_ids_and_survives_reopen() {
    for abort in [false, true] {
        let b = Bound::new(Spec::default()).await;
        let token = CancellationToken::new();
        let job = run(
            b.executor().await,
            "unknown",
            "__exec_quiet__",
            token.clone(),
        );
        let r = b.pending("unknown").await;
        b.decide(&r, Decision::ApproveTaskCommand { seconds: 60 })
            .await;
        eventually(|| b.execs() == 1).await;
        if abort {
            job.abort();
        } else {
            token.cancel();
        }
        assert!(job
            .await
            .map_or(true, |r| matches!(r, Err(ssh_tools::Error::OutcomeUnknown))));
        assert_eq!(b.state(&r).await, State::Unknown);
        let executor = b.executor().await;
        assert!(matches!(
            executor
                .execute_command(
                    "unknown",
                    &command("__exec_quiet__"),
                    &CancellationToken::new()
                )
                .await,
            Err(ssh_tools::Error::OutcomeUnknown)
        ));
        assert!(matches!(
            executor
                .execute_command(
                    "changed-call",
                    &Command {
                        command: "__exec_quiet__".into(),
                        timeout_seconds: 5,
                        explanation: None,
                    },
                    &CancellationToken::new()
                )
                .await,
            Err(ssh_tools::Error::OutcomeUnknown)
        ));
        let reopened = Approvals::new(Arc::new(Store::open(&b.path).unwrap()));
        assert_eq!(
            reopened
                .receipt(r.id.clone(), "default".into())
                .await
                .unwrap()
                .state,
            State::Unknown
        );
        assert_eq!(b.execs(), 1);
        b.terminals.shutdown().await;
    }
}
#[tokio::test]
async fn changed_task_profile_and_new_terminal_never_inherit_task_command_grant() {
    let b = Bound::new(Spec::default()).await;
    let first = run(
        b.executor().await,
        "grant",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    let r = b.pending("grant").await;
    b.decide(&r, Decision::ApproveTaskCommand { seconds: 60 })
        .await;
    first.await.unwrap().unwrap();
    let reopened = b
        .terminals
        .connect(open(&b.target.connection_id))
        .await
        .unwrap();
    let s = b.repo.session(&b.session.id, "default").unwrap();
    b.repo
        .start_task(
            &s.id,
            "default",
            s.revision,
            "next-task",
            TaskRequest {
                goal: "new target".into(),
                constraints: vec![],
                target: Some(Target {
                    connection_id: b.target.connection_id.clone(),
                    terminal_session_id: reopened.session_id,
                }),
            },
        )
        .unwrap();
    let current = b.repo.session(&b.session.id, "default").unwrap();
    let executor = SshExecutor::bind(
        b.store.clone(),
        b.terminals.clone(),
        &b.session.id,
        "default",
        &current.task.unwrap().id,
    )
    .await
    .unwrap()
    .with_approvals(b.approvals.clone())
    .unwrap();
    let token = CancellationToken::new();
    let second = run(executor, "new-task", "__exec_unicode__", token.clone());
    assert_eq!(b.pending("new-task").await.state, State::Pending);
    assert_eq!(b.execs(), 1);
    token.cancel();
    assert!(second.await.unwrap().is_err());
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn native_withdrawal_and_expired_grants_require_new_approval() {
    let b = Bound::new(Spec::default()).await;
    let job = run(
        b.executor().await,
        "withdraw",
        "__exec_unicode__",
        CancellationToken::new(),
    );
    let r = b.pending("withdraw").await;
    b.approvals
        .cancel_pending(r.id.clone(), "default".into())
        .await
        .unwrap();
    assert!(job.await.unwrap().is_err());
    assert_eq!(b.state(&r).await, State::Cancelled);
    assert!(b
        .approvals
        .decide(r.id, "default".into(), Decision::ApproveOnce)
        .await
        .is_err());
    assert_eq!(b.execs(), 0);
    b.terminals.shutdown().await;
}
