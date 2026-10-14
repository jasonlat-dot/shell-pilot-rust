#![cfg(windows)]
#[allow(dead_code)]
mod support;
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        provider::*,
        ssh_tools::{self, AuthorizationRequest, Authorizer, Command, SshExecutor},
        tool_loop::{Limits, ToolLoop, ToolRegistry},
        Payload, Repository, Session, Target, TaskRequest,
    },
    store::Store,
    terminal::{TerminalManager, TerminalSettings},
};
use std::{
    sync::{atomic::Ordering, Arc, Mutex},
    time::Duration,
};
use support::*;
use tokio_util::sync::CancellationToken;

struct Permit;
impl Authorizer for Permit {
    fn authorize<'a>(
        &'a self,
        r: AuthorizationRequest<'a>,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        assert!(!r.binding.tool.task_id.is_empty());
        assert!(!r.call_id.is_empty());
        assert!(r.command.command.starts_with("__exec_"));
        Box::pin(async { true })
    }
}
struct Gate {
    entered: CancellationToken,
    release: CancellationToken,
}
impl Gate {
    fn new() -> Self {
        Self {
            entered: CancellationToken::new(),
            release: CancellationToken::new(),
        }
    }
}
impl Authorizer for Gate {
    fn authorize<'a>(
        &'a self,
        _: AuthorizationRequest<'a>,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async move {
            self.entered.cancel();
            self.release.cancelled().await;
            true
        })
    }
}
struct Bound {
    fixture: Fixture,
    store: Arc<Store>,
    terminals: Arc<TerminalManager>,
    repo: Repository,
    session: Session,
    target: Target,
}
impl Bound {
    async fn new(spec: Spec, settings: TerminalSettings) -> Self {
        let fixture = Fixture::new(spec).await;
        let (store, connection) = fixture.saved(false);
        let terminals = Arc::new(TerminalManager::with_settings(store.clone(), settings));
        let terminal = terminals
            .connect(open(&connection.connection_id))
            .await
            .unwrap();
        let target = Target {
            connection_id: connection.connection_id,
            terminal_session_id: terminal.session_id,
        };
        let repo = Repository::new(store.clone());
        let session = repo.create_session("model", "default").unwrap();
        start(&repo, &session.id, target.clone());
        let session = repo.session(&session.id, "default").unwrap();
        Self {
            fixture,
            store,
            terminals,
            repo,
            session,
            target,
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
    }
}
fn start(repo: &Repository, sid: &str, target: Target) {
    let s = repo.session(sid, "default").unwrap();
    repo.start_task(
        sid,
        "default",
        s.revision,
        &uuid::Uuid::new_v4().to_string(),
        TaskRequest {
            goal: "SSH protocol test".into(),
            constraints: vec![],
            target: Some(target),
        },
    )
    .unwrap();
}
fn cancel_task(b: &Bound) {
    let s = b.repo.session(&b.session.id, "default").unwrap();
    b.repo
        .append(
            &s.id,
            "default",
            s.revision,
            &uuid::Uuid::new_v4().to_string(),
            Payload::Cancelled {
                reason: "test stop".into(),
            },
        )
        .unwrap();
}
fn command(text: &str) -> Command {
    Command {
        command: text.into(),
        timeout_seconds: 2,
    }
}

#[tokio::test]
async fn bound_exec_uses_original_transport_and_preserves_terminal_with_bounded_outputs() {
    let b = Bound::new(Spec::default(), TerminalSettings::default()).await;
    let executor = b.executor().await.with_authorizer(Arc::new(Permit));
    let result = executor
        .execute_command(
            "unicode",
            &command("__exec_unicode__"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(result.stdout, "执行完成\n");
    assert_eq!(result.stderr, "test stderr\n");
    assert_eq!(result.exit_code, 0);
    assert!(!result.truncated);
    assert_eq!(result.terminal_session_id, b.target.terminal_session_id);
    assert_eq!(result.task_id, b.session.task.as_ref().unwrap().id);
    let failed = executor
        .execute_command(
            "nonzero",
            &command("__exec_fail__"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(failed.exit_code, 7);
    assert!(!failed.stderr.is_empty());
    let large = executor
        .execute_command(
            "large",
            &command("__exec_large__"),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(large.truncated);
    assert!(large.stdout.len() + large.stderr.len() <= 1024);
    assert_eq!(b.fixture.stats.connections.load(Ordering::SeqCst), 1);
    assert_eq!(b.fixture.stats.shells.load(Ordering::SeqCst), 1);
    assert!(
        b.terminals
            .connected(&b.target.terminal_session_id)
            .connected
    );
    output_contains(&b.terminals, &b.target.terminal_session_id, "ready").await;
    b.terminals
        .write(&b.target.terminal_session_id, "still interactive".into())
        .await
        .unwrap();
    output_contains(
        &b.terminals,
        &b.target.terminal_session_id,
        "still interactive",
    )
    .await;
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn default_authorization_and_invalid_binding_never_dispatch_ssh() {
    let b = Bound::new(Spec::default(), TerminalSettings::default()).await;
    let executor = b.executor().await;
    assert!(matches!(
        executor
            .execute_command(
                "denied",
                &command("__exec_unicode__"),
                &CancellationToken::new()
            )
            .await,
        Err(ssh_tools::Error::Denied)
    ));
    let task_id = &b.session.task.as_ref().unwrap().id;
    assert!(SshExecutor::bind(
        b.store.clone(),
        b.terminals.clone(),
        &b.session.id,
        "other",
        task_id
    )
    .await
    .is_err());
    assert!(matches!(
        SshExecutor::bind(
            b.store.clone(),
            b.terminals.clone(),
            &b.session.id,
            "default",
            "other-task"
        )
        .await,
        Err(ssh_tools::Error::TaskChanged)
    ));
    let foreign_store = Arc::new(Store::in_memory().unwrap());
    assert!(matches!(
        SshExecutor::bind(
            foreign_store,
            b.terminals.clone(),
            &b.session.id,
            "default",
            task_id
        )
        .await,
        Err(ssh_tools::Error::TargetMismatch)
    ));
    let other = b
        .terminals
        .connect(open(&b.target.connection_id))
        .await
        .unwrap();
    // Another tab opening cannot change the immutable target of this executor.
    assert_eq!(executor.binding().tool.target, b.target);
    b.terminals.close(&other.session_id);
    let allowed = executor.with_authorizer(Arc::new(Permit));
    for cmd in [
        Command {
            command: "x\0y".into(),
            timeout_seconds: 1,
        },
        Command {
            command: "x".repeat(4097),
            timeout_seconds: 1,
        },
        Command {
            command: "x".into(),
            timeout_seconds: 0,
        },
    ] {
        assert!(matches!(
            allowed
                .execute_command("invalid", &cmd, &CancellationToken::new())
                .await,
            Err(ssh_tools::Error::InvalidArguments)
        ));
    }
    assert!(b.fixture.stats.execs.lock().unwrap().is_empty());
    eventually(|| b.fixture.stats.channels.load(Ordering::SeqCst) == 1).await;
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn waiting_authorization_rechecks_task_profile_and_disconnected_terminal() {
    for mode in 0..4 {
        let b = Bound::new(Spec::default(), TerminalSettings::default()).await;
        let gate = Arc::new(Gate::new());
        let executor = b.executor().await.with_authorizer(gate.clone());
        let running = tokio::spawn(async move {
            executor
                .execute_command(
                    "waiting",
                    &command("__exec_unicode__"),
                    &CancellationToken::new(),
                )
                .await
        });
        tokio::time::timeout(Duration::from_secs(3), gate.entered.cancelled())
            .await
            .unwrap();
        match mode {
            0 => cancel_task(&b),
            1 => {
                let mut request = b.fixture.request(false);
                request.connection_id = Some(b.target.connection_id.clone());
                request.port += 1;
                b.terminals.update_connection(request).await.unwrap();
            }
            2 => {
                let mut request = b.fixture.request(false);
                request.connection_id = Some(b.target.connection_id.clone());
                request.user_id = Some("different-owner".into());
                b.terminals.update_connection(request).await.unwrap();
            }
            _ => {
                b.terminals.close(&b.target.terminal_session_id);
                let reopened = b
                    .terminals
                    .connect(open(&b.target.connection_id))
                    .await
                    .unwrap();
                assert_ne!(reopened.session_id, b.target.terminal_session_id);
            }
        }
        gate.release.cancel();
        let error = running.await.unwrap().err().unwrap();
        assert!(matches!(
            error,
            ssh_tools::Error::TaskChanged
                | ssh_tools::Error::TargetMismatch
                | ssh_tools::Error::TargetUnavailable
        ));
        assert!(b.fixture.stats.execs.lock().unwrap().is_empty());
        b.terminals.shutdown().await;
    }
}

#[tokio::test]
async fn task_changed_while_exec_channel_opens_is_rejected_before_exec() {
    let b = Bound::new(
        Spec {
            open_delay: Duration::from_millis(100),
            ..Default::default()
        },
        TerminalSettings::default(),
    )
    .await;
    let executor = b.executor().await.with_authorizer(Arc::new(Permit));
    let running = tokio::spawn(async move {
        executor
            .execute_command(
                "opening",
                &command("__exec_unicode__"),
                &CancellationToken::new(),
            )
            .await
    });
    eventually(|| b.fixture.stats.channels.load(Ordering::SeqCst) == 2).await;
    start(&b.repo, &b.session.id, b.target.clone());
    assert!(matches!(
        running.await.unwrap(),
        Err(ssh_tools::Error::TaskChanged)
    ));
    assert!(b.fixture.stats.execs.lock().unwrap().is_empty());
    eventually(|| b.fixture.stats.channels.load(Ordering::SeqCst) == 1).await;
    assert!(
        b.terminals
            .connected(&b.target.terminal_session_id)
            .connected
    );
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn missing_exit_status_timeout_cancel_and_disconnect_are_unknown_without_retry() {
    for mode in 0..4 {
        let b = Bound::new(Spec::default(), TerminalSettings::default()).await;
        let other = b
            .terminals
            .connect(open(&b.target.connection_id))
            .await
            .unwrap();
        let executor = b.executor().await.with_authorizer(Arc::new(Permit));
        let token = CancellationToken::new();
        let stop = token.clone();
        let running = tokio::spawn(async move {
            executor
                .execute_command(
                    "uncertain",
                    &Command {
                        command: if mode == 0 {
                            "__exec_no_status__"
                        } else {
                            "__exec_quiet__"
                        }
                        .into(),
                        timeout_seconds: 1,
                    },
                    &token,
                )
                .await
        });
        eventually(|| b.fixture.stats.execs.lock().unwrap().len() == 1).await;
        if mode == 2 {
            stop.cancel();
        }
        if mode == 3 {
            b.terminals.close(&b.target.terminal_session_id);
        }
        assert!(matches!(
            running.await.unwrap(),
            Err(ssh_tools::Error::OutcomeUnknown)
        ));
        eventually(|| {
            b.fixture.stats.channels.load(Ordering::SeqCst) == if mode == 3 { 1 } else { 2 }
        })
        .await;
        assert_eq!(b.fixture.stats.execs.lock().unwrap().len(), 1);
        assert!(b.terminals.connected(&other.session_id).connected);
        if mode != 3 {
            assert!(
                b.terminals
                    .connected(&b.target.terminal_session_id)
                    .connected
            );
        }
        b.terminals.shutdown().await;
    }
}

#[tokio::test]
async fn capacity_and_offline_or_wrong_targets_never_reconnect_or_retarget() {
    let b = Bound::new(
        Spec::default(),
        TerminalSettings {
            channels_per_transport: 1,
            ..Default::default()
        },
    )
    .await;
    let executor = b.executor().await.with_authorizer(Arc::new(Permit));
    assert!(matches!(
        executor
            .execute_command(
                "busy",
                &command("__exec_unicode__"),
                &CancellationToken::new()
            )
            .await,
        Err(ssh_tools::Error::Unavailable)
    ));
    assert_eq!(b.fixture.stats.connections.load(Ordering::SeqCst), 1);
    let nonexistent = Target {
        connection_id: b.target.connection_id.clone(),
        terminal_session_id: "not-a-live-terminal".into(),
    };
    start(&b.repo, &b.session.id, nonexistent);
    let current = b.repo.session(&b.session.id, "default").unwrap();
    assert!(matches!(
        SshExecutor::bind(
            b.store.clone(),
            b.terminals.clone(),
            &current.id,
            "default",
            &current.task.unwrap().id
        )
        .await,
        Err(ssh_tools::Error::TargetUnavailable)
    ));
    // Cross-connection target cannot borrow an unrelated terminal's live transport.
    let mut profile = b.fixture.request(false);
    profile.connection_name = "other profile".into();
    let other_connection = b.store.save(profile, false).unwrap();
    start(
        &b.repo,
        &b.session.id,
        Target {
            connection_id: other_connection.connection_id,
            terminal_session_id: b.target.terminal_session_id.clone(),
        },
    );
    let current = b.repo.session(&b.session.id, "default").unwrap();
    assert!(matches!(
        SshExecutor::bind(
            b.store.clone(),
            b.terminals.clone(),
            &current.id,
            "default",
            &current.task.unwrap().id
        )
        .await,
        Err(ssh_tools::Error::TargetUnavailable)
    ));
    assert!(b.fixture.stats.execs.lock().unwrap().is_empty());
    assert!(
        b.terminals
            .connected(&b.target.terminal_session_id)
            .connected
    );
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn registered_ssh_tool_runs_through_model_loop_and_rejects_model_target_override() {
    use axum::{extract::State, routing::post, Json, Router};
    async fn model(
        State(requests): State<Arc<Mutex<Vec<Value>>>>,
        Json(body): Json<Value>,
    ) -> Json<Value> {
        let mut requests = requests.lock().unwrap();
        requests.push(body);
        let n = requests.len();
        if n == 3 {
            return Json(
                json!({"choices":[{"index":0,"message":{"role":"assistant","content":"完成"},"finish_reason":"stop"}]}),
            );
        }
        assert!(n <= 3, "no automatic model retry");
        let mut args = json!({"command":"__exec_unicode__","timeoutSeconds":2});
        if n == 1 {
            args["connectionId"] = "model-selected-host".into();
        }
        Json(
            json!({"choices":[{"index":0,"message":{"role":"assistant","content":null,"tool_calls":[{
            "id":format!("call-{n}"),"type":"function","function":{"name":ssh_tools::SSH_EXECUTE,"arguments":args.to_string()}
        }]},"finish_reason":"tool_calls"}]}),
        )
    }
    let b = Bound::new(Spec::default(), TerminalSettings::default()).await;
    let executor = b.executor().await.with_authorizer(Arc::new(Permit));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let requests = Arc::new(Mutex::new(vec![]));
    let router = Router::new()
        .route("/chat/completions", post(model))
        .with_state(requests.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let config:ModelConfig=serde_json::from_value(json!({"provider":"openAiCompatible","remoteCompaction":false,
        "baseUrl":format!("http://{address}/"),"completionsPath":"chat/completions","model":"test-model",
        "contextWindow":16384,"outputReserve":1024,"safetyReserve":256,"compactPercent":75,"outputTokenParameter":"maxCompletionTokens",
        "capabilities":{"toolCalls":true,"developerRole":false,"temperature":false,"reasoningEffort":false,"vision":false},
        "temperature":null,"reasoningEffort":null,"timeoutSeconds":3,"maxResponseBytes":65536,"requireApiKey":false})).unwrap();
    let models = ModelRepository::new(b.store.clone());
    models
        .save(
            "model",
            "default",
            0,
            ModelConfigUpdate {
                config,
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
    let client = ModelClient::new(models.runtime("model", "default").unwrap()).unwrap();
    let mut registry = ToolRegistry::default();
    executor.register(&mut registry).unwrap();
    let mut loop_ = ToolLoop::new(
        &registry,
        vec![ChatMessage::User {
            content: "检查测试目标".into(),
        }],
        Limits::default(),
    )
    .unwrap();
    loop_
        .run(&client, &executor, &CancellationToken::new())
        .await
        .unwrap();
    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 3);
    assert!(requests[1]["messages"][2]["content"]
        .as_str()
        .unwrap()
        .contains("invalid_arguments"));
    let result: Value =
        serde_json::from_str(requests[2]["messages"][4]["content"].as_str().unwrap()).unwrap();
    assert_eq!(
        result["data"]["terminalSessionId"],
        b.target.terminal_session_id
    );
    assert_eq!(
        result["data"]["taskId"],
        b.session.task.as_ref().unwrap().id
    );
    assert_eq!(result["data"]["exitCode"], 0);
    assert_eq!(*b.fixture.stats.execs.lock().unwrap(), ["__exec_unicode__"]);
    drop(requests);
    task.abort();
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn server_scope_identity_and_opening_trust_cannot_be_replaced_by_saved_configuration() {
    let b = Bound::new(Spec::default(), TerminalSettings::default()).await;
    let identity = b.executor().await.binding().identity.clone();
    let root = b.repo.create_general_session("model", "default").unwrap();
    start(&b.repo, &root.id, b.target.clone());
    let task = b.repo.session(&root.id, "default").unwrap().task.unwrap();
    assert!(matches!(
        SshExecutor::bind(
            b.store.clone(),
            b.terminals.clone(),
            &root.id,
            "default",
            &task.id
        )
        .await,
        Err(ssh_tools::Error::TargetMismatch)
    ));
    for valid in [false, true] {
        let mut expected = identity.clone();
        if !valid {
            expected.host_key = "different-host-key".into();
        }
        let scope = b
            .repo
            .server_scope("model", "default", &root.id, &b.target, expected, true)
            .unwrap();
        start(&b.repo, &scope.id, b.target.clone());
        let task = b.repo.session(&scope.id, "default").unwrap().task.unwrap();
        let bound = SshExecutor::bind(
            b.store.clone(),
            b.terminals.clone(),
            &scope.id,
            "default",
            &task.id,
        )
        .await;
        if valid {
            assert!(bound.is_ok());
        } else {
            assert!(matches!(bound, Err(ssh_tools::Error::TargetMismatch)));
        }
    }
    // Simulate a native profile write that bypassed TerminalManager's normal cache
    // invalidation. The old terminal must retain the transport's *actual* identity.
    let mut profile = b.fixture.request(false);
    profile.connection_id = Some(b.target.connection_id.clone());
    profile
        .known_hosts
        .as_mut()
        .unwrap()
        .push_str("\n# policy revalidated");
    b.store.save(profile, true).unwrap();
    let task_id = &b.session.task.as_ref().unwrap().id;
    assert!(matches!(
        SshExecutor::bind(
            b.store.clone(),
            b.terminals.clone(),
            &b.session.id,
            "default",
            task_id
        )
        .await,
        Err(ssh_tools::Error::TargetMismatch)
    ));
    let new = b
        .terminals
        .connect(open(&b.target.connection_id))
        .await
        .unwrap();
    assert_ne!(new.session_id, b.target.terminal_session_id);
    assert_eq!(b.fixture.stats.connections.load(Ordering::SeqCst), 2);
    assert!(
        b.terminals
            .connected(&b.target.terminal_session_id)
            .connected
    );
    assert!(b.fixture.stats.execs.lock().unwrap().is_empty());
    b.terminals.shutdown().await;
}
