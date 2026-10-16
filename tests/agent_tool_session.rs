#![cfg(windows)]
#[allow(dead_code)]
mod support;
use axum::{extract::State, routing::post, Json, Router};
use futures_util::future::BoxFuture;
use serde_json::{json, Value};
use shellpilot_server::{
    agent::{
        approvals::{Approvals, State as ApprovalState},
        provider::*,
        ssh_tools::{AuthorizationRequest, Authorizer, SshExecutor},
        tool_loop::{self, Limits},
        tool_session::{ToolSession, READ_OUTPUT},
        Outcome, OutputChannel, OutputLimits, Payload, Repository, Target, TaskRequest,
    },
    store::Store,
    terminal::{TerminalManager, TerminalSettings},
};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};
use support::*;
use tokio_util::sync::CancellationToken;
// The expanded host/shell instructions put a minimal checkpoint above the old
// 35% low watermark (2078 tokens). 45% allows the mandatory checkpoint to fit
// below 2672 tokens while the four-result batch (~3759 tokens) still triggers.
const BATCH_COMPACTION_PERCENT: u8 = 45;
struct Permit;
impl Authorizer for Permit {
    fn authorize<'a>(
        &'a self,
        r: AuthorizationRequest<'a>,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        assert!(r.command.command.starts_with("__exec_"));
        Box::pin(async { true })
    }
}
fn tool(id: &str, name: &str, args: Value) -> Value {
    json!({"id":id,"type":"function","function":{"name":name,"arguments":args.to_string()}})
}
fn exec(id: &str, command: &str) -> Value {
    tool(
        id,
        "ssh_execute",
        json!({"command":command,"timeoutSeconds":2}),
    )
}
fn reply(message: Value, reason: &str) -> Value {
    json!({"choices":[{"index":0,"message":message,"finish_reason":reason}]})
}
enum Step {
    Calls(Vec<Value>),
    Read {
        id: String,
        source: String,
        offset: u64,
        limit: u64,
    },
    Final,
}
struct Script {
    steps: VecDeque<Step>,
    requests: Vec<Value>,
    outputs: HashMap<String, String>,
    summaries: usize,
    valid_summary: bool,
}
#[derive(Clone)]
struct Wire {
    script: Arc<Mutex<Script>>,
    summary_entered: CancellationToken,
    summary_release: Option<CancellationToken>,
}
fn remember(script: &mut Script, call: &str, outputs: &Value) {
    if let Some(items) = outputs.as_array() {
        if let Some(output) = items.iter().find(|o| o["channel"] == "stdout") {
            script
                .outputs
                .insert(call.into(), output["id"].as_str().unwrap().into());
        }
    }
}
async fn respond(State(wire): State<Wire>, Json(body): Json<Value>) -> Json<Value> {
    let summary = body["tools"].as_array().is_none_or(|t| t.is_empty());
    let value = {
        let mut s = wire.script.lock().unwrap();
        s.requests.push(body.clone());
        if summary {
            s.summaries += 1;
            let source: Value =
                serde_json::from_str(body["messages"][1]["content"].as_str().unwrap()).unwrap();
            for event in source["events"].as_array().unwrap() {
                if event["payload"]["type"] == "toolResult" {
                    remember(
                        &mut s,
                        event["payload"]["callId"].as_str().unwrap(),
                        &event["payload"]["outputs"],
                    );
                }
            }
            let content = if s.valid_summary {
                json!({"summary":"已执行当前任务的工具批次；结果和输出引用见原始事件，禁止重执行。",
                    "nextSteps":["按输出引用继续检查"],"evidenceEventIds":source["availableEvidenceEventIds"],
                    "retainedUserFacts":[]}).to_string()
            } else {
                "invalid summary".into()
            };
            reply(json!({"role":"assistant","content":content}), "stop")
        } else {
            for message in body["messages"].as_array().unwrap() {
                if message["role"] == "tool" {
                    let value: Value =
                        serde_json::from_str(message["content"].as_str().unwrap()).unwrap();
                    remember(
                        &mut s,
                        message["tool_call_id"].as_str().unwrap(),
                        &value["data"]["outputs"],
                    );
                }
            }
            match s
                .steps
                .pop_front()
                .expect("unexpected model request or retry")
            {
                Step::Calls(calls) => reply(
                    json!({"role":"assistant","content":"检查当前批次","tool_calls":calls}),
                    "tool_calls",
                ),
                Step::Read {
                    id,
                    source,
                    offset,
                    limit,
                } => {
                    let output = s
                        .outputs
                        .get(&source)
                        .expect("model must have seen an output reference");
                    reply(
                        json!({"role":"assistant","content":null,"tool_calls":[tool(&id,READ_OUTPUT,
                        json!({"outputId":output,"offset":offset,"limit":limit}))]}),
                        "tool_calls",
                    )
                }
                Step::Final => reply(
                    json!({"role":"assistant","content":"已根据工具证据完成分析"}),
                    "stop",
                ),
            }
        }
    };
    if summary {
        wire.summary_entered.cancel();
        if let Some(release) = &wire.summary_release {
            release.cancelled().await;
        }
    }
    Json(value)
}
struct Bound {
    _directory: tempfile::TempDir,
    path: std::path::PathBuf,
    fixture: Fixture,
    store: Arc<Store>,
    repo: Repository,
    terminals: Arc<TerminalManager>,
    sid: String,
    target: Target,
    wire: Wire,
    model_task: tokio::task::JoinHandle<()>,
}
impl Drop for Bound {
    fn drop(&mut self) {
        self.model_task.abort();
    }
}
impl Bound {
    async fn new(steps: Vec<Step>, percent: u8, valid_summary: bool, gate: bool) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("agent.sqlite");
        let fixture = Fixture::new(Spec::default()).await;
        let store = Arc::new(Store::open(&path).unwrap());
        let connection = store.save(fixture.request(false), false).unwrap();
        let terminals = Arc::new(TerminalManager::with_settings(
            store.clone(),
            TerminalSettings::default(),
        ));
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
        repo.start_task(
            &session.id,
            "default",
            0,
            "task",
            TaskRequest {
                goal: "检查任务日志，保持约束".into(),
                constraints: vec!["不要重启服务".into()],
                target: Some(target.clone()),
            },
        )
        .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config:ModelConfig=serde_json::from_value(json!({"provider":"openAiCompatible","remoteCompaction":false,
            "baseUrl":format!("http://{}/",listener.local_addr().unwrap()),"completionsPath":"chat/completions",
            "model":"loopback-test","contextWindow":8192,"outputReserve":512,"safetyReserve":256,
            "compactPercent":percent,"outputTokenParameter":"maxCompletionTokens","capabilities":{"toolCalls":true,"developerRole":false,"temperature":false,"reasoningEffort":false,"vision":false},
            "temperature":null,"reasoningEffort":null,"timeoutSeconds":3,"maxResponseBytes":65536,"requireApiKey":false})).unwrap();
        ModelRepository::new(store.clone())
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
        let wire = Wire {
            script: Arc::new(Mutex::new(Script {
                steps: steps.into(),
                requests: vec![],
                outputs: HashMap::new(),
                summaries: 0,
                valid_summary,
            })),
            summary_entered: CancellationToken::new(),
            summary_release: gate.then(CancellationToken::new),
        };
        let router = Router::new()
            .route("/chat/completions", post(respond))
            .with_state(wire.clone());
        let model_task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            _directory: directory,
            path,
            fixture,
            store,
            repo,
            terminals,
            sid: session.id,
            target,
            wire,
            model_task,
        }
    }
    async fn session(&self) -> ToolSession {
        let task = self
            .repo
            .session(&self.sid, "default")
            .unwrap()
            .task
            .unwrap();
        ToolSession::new(
            SshExecutor::bind(
                self.store.clone(),
                self.terminals.clone(),
                &self.sid,
                "default",
                &task.id,
            )
            .await
            .unwrap()
            .with_authorizer(Arc::new(Permit)),
        )
    }
    fn events(&self) -> Vec<shellpilot_server::agent::Event> {
        self.repo.events(&self.sid, "default", 0, 200).unwrap()
    }
    fn execs(&self) -> usize {
        self.fixture.stats.execs.lock().unwrap().len()
    }
    fn normal(&self) -> Vec<Value> {
        self.wire
            .script
            .lock()
            .unwrap()
            .requests
            .iter()
            .filter(|r| r["tools"].as_array().is_some_and(|t| !t.is_empty()))
            .cloned()
            .collect()
    }
}
fn result(events: &[shellpilot_server::agent::Event], id: &str) -> Value {
    let event = events
        .iter()
        .find(|e| matches!(&e.payload,Payload::ToolResult{call_id,..} if call_id==id))
        .unwrap();
    let Payload::ToolResult { text, .. } = &event.payload else {
        unreachable!()
    };
    serde_json::from_str(text).unwrap()
}
fn raw_references(
    events: &[shellpilot_server::agent::Event],
    id: &str,
) -> Vec<shellpilot_server::agent::OutputReference> {
    let event = events
        .iter()
        .find(|e| matches!(&e.payload,Payload::ToolResult{call_id,..} if call_id==id))
        .unwrap();
    let Payload::ToolResult { outputs, .. } = &event.payload else {
        unreachable!()
    };
    outputs.clone()
}
#[tokio::test]
async fn complete_batch_captures_cross_chunk_bytes_reads_without_exec_and_preserves_nonzero_exit() {
    let b = Bound::new(
        vec![
            Step::Calls(vec![
                exec("large", "__exec_chunked__"),
                exec("failed", "__exec_fail__"),
            ]),
            Step::Read {
                id: "read".into(),
                source: "large".into(),
                offset: 65534,
                limit: 512,
            },
            Step::Final,
        ],
        90,
        true,
        false,
    )
    .await;
    let session = b.session().await;
    session
        .run(
            "run",
            "检查公开日志",
            Limits::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(b.execs(), 2);
    let events = b.events();
    assert!(shellpilot_server::agent::budget::group_history(&events)
        .unwrap()
        .iter()
        .all(|g| g.complete));
    let value = result(&events, "failed");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"], "execution_failed");
    assert_eq!(value["data"]["exitCode"], 7);
    let refs = raw_references(&events, "large");
    assert_eq!(refs.len(), 2);
    assert!(result(&events, "large")["data"]["stdout"]
        .as_str()
        .unwrap()
        .contains("[... omitted; use read_tool_output ...]"));
    assert_eq!(result(&events, "large")["data"]["stderr"], "test stderr\n");
    let stdout = refs
        .iter()
        .find(|r| r.channel == OutputChannel::Stdout)
        .unwrap();
    assert_eq!(stdout.size, 131073);
    assert_eq!(result(&events, "large")["data"]["truncated"], true);
    assert_eq!(result(&events, "large")["data"]["outputTruncated"], false);
    let expected = (65534..66046).map(|i| (i % 251) as u8).collect::<Vec<_>>();
    let hex = expected
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let value = result(&events, "read");
    assert_eq!(value["data"]["bytesHex"], hex);
    assert_eq!(value["data"]["nextOffset"], 66046);
    assert_eq!(b.normal().len(), 3);
    let persisted = Approvals::new(b.store.clone())
        .list(b.sid.clone(), "default".into(), "".into())
        .await
        .unwrap();
    assert_eq!(persisted.len(), 2);
    assert!(persisted
        .iter()
        .all(|r| r.state == ApprovalState::Completed
            && r.result.as_ref().unwrap().outputs.len() == 2));
    assert!(matches!(
        session
            .run(
                "again",
                "另一个请求",
                Limits::default(),
                &CancellationToken::new()
            )
            .await,
        Err(tool_loop::Error::AlreadyStarted)
    ));
    assert!(matches!(
        b.session()
            .await
            .run(
                "run",
                "检查公开日志",
                Limits::default(),
                &CancellationToken::new()
            )
            .await,
        Err(tool_loop::Error::AlreadyStarted)
    ));
    b.terminals.shutdown().await;
    let reopened = Repository::new(Arc::new(Store::open(&b.path).unwrap()));
    let range = reopened
        .read_output_range(&b.sid, "default", &stdout.id, 65534, 512)
        .unwrap();
    assert_eq!(range.bytes, expected);
    assert!(reopened.output(&b.sid, "other", &stdout.id).is_err());
    assert!(reopened
        .discard_output(&b.sid, "default", &stdout.id)
        .is_err());
    assert_eq!(
        reopened
            .session(&b.sid, "default")
            .unwrap()
            .task
            .unwrap()
            .constraints,
        ["不要重启服务"]
    );
}
#[tokio::test]
async fn task_output_is_not_available_to_another_task_and_read_errors_are_model_visible() {
    let b = Bound::new(
        vec![
            Step::Calls(vec![exec("old", "__exec_unicode__")]),
            Step::Final,
            Step::Read {
                id: "foreign".into(),
                source: "old".into(),
                offset: 0,
                limit: 32,
            },
            Step::Final,
        ],
        90,
        true,
        false,
    )
    .await;
    b.session()
        .await
        .run(
            "first",
            "检查",
            Limits::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    let s = b.repo.session(&b.sid, "default").unwrap();
    b.repo
        .start_task(
            &b.sid,
            "default",
            s.revision,
            "different-task",
            TaskRequest {
                goal: "新任务".into(),
                constraints: vec![],
                target: Some(b.target.clone()),
            },
        )
        .unwrap();
    b.session()
        .await
        .run(
            "second",
            "检查旧输出",
            Limits::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(
        result(&b.events(), "foreign")["error"],
        "output_unavailable"
    );
    assert_eq!(b.execs(), 1);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn invalid_model_arguments_are_persisted_without_dispatch_and_can_be_corrected() {
    let b = Bound::new(
        vec![
            Step::Calls(vec![tool(
                "bad",
                "ssh_execute",
                json!({"command":"__exec_unicode__","timeoutSeconds":2,"connectionId":"forged"}),
            )]),
            Step::Calls(vec![exec("good", "__exec_unicode__")]),
            Step::Final,
        ],
        90,
        true,
        false,
    )
    .await;
    b.session()
        .await
        .run("run", "检查", Limits::default(), &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(result(&b.events(), "bad")["error"], "invalid_arguments");
    assert_eq!(result(&b.events(), "good")["ok"], true);
    assert_eq!(b.execs(), 1);
    let stats = b.repo.output_stats(&b.sid, "default").unwrap();
    assert_eq!(stats.active_writers, 0);
    assert_eq!(stats.outputs, 2);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn cancel_or_drop_preserves_unknown_and_closes_unstarted_batch_without_reexecution() {
    for abort in [false, true] {
        let b = Bound::new(
            vec![Step::Calls(vec![
                exec("quiet", "__exec_quiet__"),
                exec("later", "__exec_unicode__"),
            ])],
            90,
            true,
            false,
        )
        .await;
        let session = b.session().await;
        let token = CancellationToken::new();
        let stop = token.clone();
        let job =
            tokio::spawn(
                async move { session.run("run", "检查", Limits::default(), &token).await },
            );
        eventually(|| b.execs() == 1).await;
        if abort {
            job.abort();
        } else {
            stop.cancel();
        }
        let finished = job.await;
        if !abort {
            assert!(matches!(
                finished.unwrap(),
                Err(tool_loop::Error::OutcomeUnknown)
            ));
        }
        let events = b.events();
        assert_eq!(result(&events, "quiet")["error"], "result_unknown");
        assert_eq!(result(&events, "later")["error"], "not_executed");
        assert!(events.iter().any(|e|matches!(&e.payload,Payload::ToolResult{call_id,outcome:Outcome::Unknown,..}if call_id=="quiet")));
        assert!(b
            .repo
            .snapshot(&b.sid, "default")
            .unwrap()
            .pending_tool_call_ids
            .is_empty());
        assert_eq!(
            b.repo
                .output_stats(&b.sid, "default")
                .unwrap()
                .active_writers,
            0
        );
        assert_eq!(b.execs(), 1);
        let approvals = Approvals::new(Arc::new(Store::open(&b.path).unwrap()));
        assert_eq!(
            approvals
                .list(b.sid.clone(), "default".into(), "".into())
                .await
                .unwrap()[0]
                .state,
            ApprovalState::Unknown
        );
        assert_eq!(b.normal().len(), 1);
        b.terminals.shutdown().await;
    }
}
#[tokio::test]
async fn compaction_uses_whole_batch_and_keeps_real_output_references_and_latest_request() {
    let b = Bound::new(
        vec![
            Step::Calls(vec![
                exec("large", "__exec_large__"),
                exec("more", "__exec_large__"),
                exec("extra", "__exec_large__"),
                exec("other", "__exec_large__"),
            ]),
            Step::Read {
                id: "read".into(),
                source: "large".into(),
                offset: 2048,
                limit: 32,
            },
            Step::Final,
        ],
        BATCH_COMPACTION_PERCENT,
        true,
        false,
    )
    .await;
    b.session()
        .await
        .run(
            "run",
            "只检查日志，不要重启服务",
            Limits::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(b.execs(), 4);
    assert_eq!(b.wire.script.lock().unwrap().summaries, 1);
    let checkpoint = b.repo.checkpoint(&b.sid, "default").unwrap().unwrap();
    let events = b.events();
    let batch_end=events.iter().rfind(|e|matches!(&e.payload,Payload::ToolResult{call_id,..}if call_id=="large"||call_id=="other")).unwrap();
    assert_eq!(checkpoint.through_event_id, batch_end.id);
    assert_eq!(
        checkpoint.content.task.unwrap().constraints,
        ["不要重启服务"]
    );
    let after = &b.normal()[1];
    assert!(after["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "user" && m["content"] == "只检查日志，不要重启服务"));
    assert!(after["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains(&raw_references(&events, "large")[0].id));
    assert_eq!(result(&events, "read")["data"]["text"], "x".repeat(32));
    assert_eq!(b.repo.output_stats(&b.sid, "default").unwrap().outputs, 8);
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn cancelled_or_stale_summary_never_commits_or_replays_tool_batch() {
    for change in [false, true] {
        let b = Bound::new(
            vec![Step::Calls(vec![
                exec("a", "__exec_large__"),
                exec("c", "__exec_large__"),
                exec("d", "__exec_large__"),
                exec("b", "__exec_large__"),
            ])],
            BATCH_COMPACTION_PERCENT,
            true,
            true,
        )
        .await;
        let session = b.session().await;
        let token = CancellationToken::new();
        let stop = token.clone();
        let job = tokio::spawn(async move {
            session
                .run("run", "检查日志", Limits::default(), &token)
                .await
        });
        tokio::time::timeout(Duration::from_secs(5), b.wire.summary_entered.cancelled())
            .await
            .unwrap();
        if change {
            let s = b.repo.session(&b.sid, "default").unwrap();
            b.repo
                .append(
                    &b.sid,
                    "default",
                    s.revision,
                    "new-constraint",
                    Payload::UserMessage {
                        text: "补充：不要修改任何文件".into(),
                    },
                )
                .unwrap();
            b.wire.summary_release.as_ref().unwrap().cancel();
        } else {
            stop.cancel();
        }
        assert!(job.await.unwrap().is_err());
        assert!(b.repo.checkpoint(&b.sid, "default").unwrap().is_none());
        assert_eq!(b.execs(), 4);
        assert_eq!(b.normal().len(), 1);
        assert_eq!(b.repo.output_stats(&b.sid, "default").unwrap().outputs, 8);
        assert!(b
            .repo
            .snapshot(&b.sid, "default")
            .unwrap()
            .pending_tool_call_ids
            .is_empty());
        b.terminals.shutdown().await;
    }
}
#[tokio::test]
async fn invalid_summary_falls_back_to_unchanged_hard_fitting_batch_without_losing_outputs() {
    let b = Bound::new(
        vec![
            Step::Calls(vec![
                exec("a", "__exec_large__"),
                exec("c", "__exec_large__"),
                exec("d", "__exec_large__"),
                exec("b", "__exec_large__"),
            ]),
            Step::Final,
        ],
        BATCH_COMPACTION_PERCENT,
        false,
        false,
    )
    .await;
    b.session()
        .await
        .run(
            "run",
            "检查日志",
            Limits::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert!(b.repo.checkpoint(&b.sid, "default").unwrap().is_none());
    assert_eq!(b.execs(), 4);
    assert_eq!(b.wire.script.lock().unwrap().summaries, 1);
    assert_eq!(
        b.normal()[1]["messages"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|m| m["role"] == "tool")
            .count(),
        4
    );
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn output_capture_caps_are_explicit_and_reservation_failure_never_sends_exec() {
    for quota in [false, true] {
        let b = Bound::new(
            vec![
                Step::Calls(vec![exec("call", "__exec_large__")]),
                Step::Final,
            ],
            90,
            true,
            false,
        )
        .await;
        let limits = if quota {
            OutputLimits {
                max_writers: 1,
                ..Default::default()
            }
        } else {
            OutputLimits {
                max_output_bytes: 32,
                ..Default::default()
            }
        };
        b.session()
            .await
            .with_output_limits(limits)
            .unwrap()
            .run(
                "run",
                "检查输出边界",
                Limits::default(),
                &CancellationToken::new(),
            )
            .await
            .unwrap();
        let value = result(&b.events(), "call");
        let stats = b.repo.output_stats(&b.sid, "default").unwrap();
        assert_eq!(stats.active_writers, 0);
        if quota {
            assert_eq!(b.execs(), 0);
            assert_eq!(value["error"], "output_storage_failed");
            assert_eq!(stats.outputs, 0);
        } else {
            assert_eq!(b.execs(), 1);
            assert_eq!(value["data"]["outputTruncated"], true);
            assert_eq!(value["data"]["outputError"], false);
            assert_eq!(stats.used_bytes, 44);
            assert_eq!(
                raw_references(&b.events(), "call")
                    .iter()
                    .find(|r| r.channel == OutputChannel::Stdout)
                    .unwrap()
                    .size,
                32
            );
        }
        b.terminals.shutdown().await;
    }
}
#[tokio::test]
async fn reused_call_in_a_new_batch_rolls_back_the_entire_batch_before_any_dispatch() {
    let b = Bound::new(
        vec![
            Step::Calls(vec![exec("same", "__exec_unicode__")]),
            Step::Final,
            Step::Calls(vec![
                exec("new", "__exec_unicode__"),
                exec("same", "__exec_unicode__"),
            ]),
        ],
        90,
        true,
        false,
    )
    .await;
    b.session()
        .await
        .run(
            "first",
            "检查",
            Limits::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    // Drop the old protocol calls from the model window, so only the durable journal
    // can reject reuse. A fresh call followed by the reused ID must roll back together.
    let snapshot = b.repo.snapshot(&b.sid, "default").unwrap();
    b.repo
        .save_checkpoint(
            &snapshot,
            shellpilot_server::agent::CheckpointContent {
                task: snapshot.session.task.clone(),
                summary: "已保存旧回执".into(),
                next_steps: vec![],
                evidence_event_ids: vec![snapshot.through_event_id],
                retained_user_facts: vec![],
                pending_tool_call_ids: vec![],
                uncertain_tool_call_ids: vec![],
            },
        )
        .unwrap();
    let before = b.events().len();
    assert!(b
        .session()
        .await
        .run(
            "second",
            "继续检查",
            Limits::default(),
            &CancellationToken::new()
        )
        .await
        .is_err());
    // Only the explicit second user request is new: no partial assistant/call batch is published.
    assert_eq!(b.events().len(), before + 1);
    assert_eq!(b.execs(), 1);
    assert!(b
        .repo
        .snapshot(&b.sid, "default")
        .unwrap()
        .pending_tool_call_ids
        .is_empty());
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn oversized_mandatory_input_stops_before_model_or_ssh_without_pruning_raw_evidence() {
    let b = Bound::new(vec![], 90, true, false).await;
    let models = ModelRepository::new(b.store.clone());
    // Fit a valid tiny model profile, while the requested mandatory user input cannot fit.
    let mut config = models.runtime("model", "default").unwrap().config;
    config.context_window = 4096;
    config.output_reserve = 2048;
    models
        .save(
            "model",
            "default",
            1,
            ModelConfigUpdate {
                config,
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
    let input = "日志".repeat(2000);
    assert!(b
        .session()
        .await
        .run("run", &input, Limits::default(), &CancellationToken::new())
        .await
        .is_err());
    assert_eq!(b.execs(), 0);
    assert!(b.wire.script.lock().unwrap().requests.is_empty());
    assert!(!b
        .events()
        .iter()
        .any(|e| matches!(&e.payload,Payload::UserMessage{text}if text==&input)));
    // Rejection must not poison a later fitting turn.
    b.wire.script.lock().unwrap().steps.push_back(Step::Final);
    b.session()
        .await
        .run(
            "fitting",
            "继续",
            Limits::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(b.normal().len(), 1);
    assert!(b.repo.checkpoint(&b.sid, "default").unwrap().is_none());
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn failed_summary_after_disconnect_refreshes_target_status_without_reexecuting() {
    let b = Bound::new(
        vec![
            Step::Calls(vec![
                exec("a", "__exec_large__"),
                exec("b", "__exec_large__"),
                exec("c", "__exec_large__"),
                exec("d", "__exec_large__"),
            ]),
            Step::Final,
        ],
        BATCH_COMPACTION_PERCENT,
        false,
        true,
    )
    .await;
    let session = b.session().await;
    let job = tokio::spawn(async move {
        session
            .run(
                "run",
                "检查日志",
                Limits::default(),
                &CancellationToken::new(),
            )
            .await
    });
    tokio::time::timeout(Duration::from_secs(5), b.wire.summary_entered.cancelled())
        .await
        .unwrap();
    b.terminals.close(&b.target.terminal_session_id);
    b.wire.summary_release.as_ref().unwrap().cancel();
    job.await.unwrap().unwrap();
    assert!(b.repo.checkpoint(&b.sid, "default").unwrap().is_none());
    assert_eq!(b.execs(), 4);
    assert_eq!(b.normal().len(), 2);
    assert!(b.normal()[1]["messages"][0]["content"]
        .as_str()
        .unwrap()
        .contains("\"targetAvailable\":false"));
    assert_eq!(b.repo.output_stats(&b.sid, "default").unwrap().outputs, 8);
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn user_supplement_during_approval_invalidates_the_dispatch_plan_inside_admission() {
    let b = Bound::new(
        vec![Step::Calls(vec![exec("planned", "__exec_unicode__")])],
        90,
        true,
        false,
    )
    .await;
    let task = b.repo.session(&b.sid, "default").unwrap().task.unwrap();
    let approvals = Approvals::new(b.store.clone());
    let executor = SshExecutor::bind(
        b.store.clone(),
        b.terminals.clone(),
        &b.sid,
        "default",
        &task.id,
    )
    .await
    .unwrap()
    .with_approvals(approvals.clone())
    .unwrap();
    let session = ToolSession::new(executor);
    let job = tokio::spawn(async move {
        session
            .run(
                "run",
                "检查日志",
                Limits::default(),
                &CancellationToken::new(),
            )
            .await
    });
    let pending = tokio::time::timeout(Duration::from_secs(4), async {
        loop {
            let requests = approvals
                .list(b.sid.clone(), "default".into(), "".into())
                .await
                .unwrap();
            if let Some(request) = requests.into_iter().find(|r| r.call_id == "planned") {
                break request;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(pending.state, ApprovalState::Pending);
    assert_eq!(b.execs(), 0);
    let before = b.repo.session(&b.sid, "default").unwrap();
    b.repo
        .append(
            &b.sid,
            "default",
            before.revision,
            "supplement",
            Payload::UserMessage {
                text: "补充：先不要执行任何命令".into(),
            },
        )
        .unwrap();
    // The task itself is unchanged: D4 must also check the plan's event version,
    // rather than treating the older human approval as permission for a stale plan.
    assert_eq!(b.repo.session(&b.sid, "default").unwrap().task, Some(task));
    approvals
        .decide(
            pending.id.clone(),
            "default".into(),
            shellpilot_server::agent::approvals::Decision::ApproveOnce,
        )
        .await
        .unwrap();
    assert!(job.await.unwrap().is_err());
    assert_eq!(b.execs(), 0);
    assert_eq!(b.normal().len(), 1);
    assert_eq!(
        approvals
            .receipt(pending.id, "default".into())
            .await
            .unwrap()
            .state,
        ApprovalState::Cancelled
    );
    assert_eq!(
        b.repo
            .output_stats(&b.sid, "default")
            .unwrap()
            .active_writers,
        0
    );
    assert!(b
        .repo
        .snapshot(&b.sid, "default")
        .unwrap()
        .pending_tool_call_ids
        .is_empty());
    b.terminals.shutdown().await;
}

// D5 uses the actual desktop Manager: persistent turn receipt + bound SSH + human decisions.
async fn desktop(
    b: &Bound,
) -> (
    Arc<shellpilot_server::agent::chat::Manager>,
    shellpilot_server::agent::chat::Request,
) {
    use shellpilot_server::agent::chat::*;
    let manager = Arc::new(Manager::with_terminals(
        b.store.clone(),
        CancellationToken::new(),
        b.terminals.clone(),
    ));
    let root = manager
        .create_general_session("model".into(), "default".into())
        .await
        .unwrap();
    let session = manager
        .server_scope(
            "model".into(),
            "default".into(),
            root.id,
            b.target.terminal_session_id.clone(),
            false,
        )
        .await
        .unwrap();
    manager
        .bind_execution(
            "model".into(),
            "default".into(),
            session.id.clone(),
            b.target.terminal_session_id.clone(),
        )
        .await
        .unwrap();
    let request = Request {
        request_id: "desktop-turn".into(),
        agent_id: "model".into(),
        user_id: "default".into(),
        session_id: session.id,
        terminal_session_id: b.target.terminal_session_id.clone(),
        message: "只检查日志，不要重启服务".into(),
        attachments: vec![],
    };
    (manager, request)
}
async fn pending_approval(
    manager: &shellpilot_server::agent::chat::Manager,
    request: &shellpilot_server::agent::chat::Request,
) -> shellpilot_server::agent::approvals::Receipt {
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            let list = manager
                .execution_requests(
                    "model".into(),
                    "default".into(),
                    request.session_id.clone(),
                    "".into(),
                )
                .await
                .unwrap();
            if let Some(receipt) = list.into_iter().find(|r| r.state == ApprovalState::Pending) {
                return receipt;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("desktop must present a real pending approval")
}
#[tokio::test]
async fn desktop_steering_replans_pending_command_without_interrupting_or_executing_it() {
    use shellpilot_server::agent::chat::{StreamEvent, TurnState};
    let b = Bound::new(
        vec![
            Step::Calls(vec![exec("old-plan", "__exec_unicode__")]),
            Step::Final,
        ],
        90,
        true,
        false,
    )
    .await;
    let (manager, request) = desktop(&b).await;
    let m = manager.clone();
    let r = request.clone();
    let frames = Arc::new(Mutex::new(Vec::new()));
    let events = frames.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), move |e| {
            events.lock().unwrap().push(e);
            async { Ok(()) }
        })
        .await
    });
    let approval = pending_approval(&manager, &request).await;
    manager
        .steer(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            request.request_id.clone(),
            "steer-tool".into(),
            "补充：不要执行命令，只讨论方案".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(b.execs(), 0);
    let receipts = manager
        .execution_requests(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            "".into(),
        )
        .await
        .unwrap();
    assert!(receipts
        .iter()
        .any(|r| r.id == approval.id && r.state == ApprovalState::Cancelled));
    let history = manager
        .history(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            0,
            20,
        )
        .await
        .unwrap();
    assert_eq!(history.turns.len(), 1);
    assert_eq!(history.turns[0].state, TurnState::Completed);
    assert!(history.turns[0].inputs[0].event_id.is_some());
    let bodies = &b.wire.script.lock().unwrap().requests;
    assert!(bodies.last().unwrap()["messages"]
        .as_array()
        .unwrap()
        .iter()
        .any(|m| m["role"] == "user" && m["content"] == "补充：不要执行命令，只讨论方案"));
    assert!(frames
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, StreamEvent::Input { .. })));
}
#[tokio::test]
async fn desktop_d5_approval_output_history_reopen_and_replay_never_repeat_ssh() {
    use shellpilot_server::agent::chat::StreamEvent;
    let b = Bound::new(
        vec![
            Step::Calls(vec![exec("desktop-exec", "__exec_chunked__")]),
            Step::Final,
        ],
        90,
        true,
        false,
    )
    .await;
    let (manager, request) = desktop(&b).await;
    let frames = Arc::new(Mutex::new(vec![]));
    let events = frames.clone();
    let m = manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), move |e| {
            events.lock().unwrap().push(e);
            async { Ok(()) }
        })
        .await
    });
    let approval = pending_approval(&manager, &request).await;
    assert_eq!(b.execs(), 0);
    assert_eq!(approval.command.command, "__exec_chunked__");
    assert_eq!(approval.binding.tool.target, b.target);
    assert!(manager
        .execution_decision(
            "other-model".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id.clone(),
            "approveOnce".into()
        )
        .await
        .is_err());
    assert!(manager
        .execution_decision(
            "model".into(),
            "intruder".into(),
            request.session_id.clone(),
            approval.id.clone(),
            "approveOnce".into()
        )
        .await
        .is_err());
    manager
        .execution_decision(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            approval.id,
            "approveOnce".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(b.execs(), 1);
    {
        let frames = frames.lock().unwrap();
        assert!(frames
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolCall { .. })));
        assert!(frames
            .iter()
            .any(|e| matches!(e, StreamEvent::ToolResult { .. })));
        assert!(matches!(frames.last(), Some(StreamEvent::Done { .. })));
    }
    let history = manager
        .history(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            0,
            20,
        )
        .await
        .unwrap();
    assert_eq!(history.turns.len(), 1);
    assert_eq!(
        history.turns[0].state,
        shellpilot_server::agent::chat::TurnState::Completed
    );
    assert_eq!(history.turns[0].tool_events.len(), 2);
    let output = raw_references(&history.turns[0].tool_events, "desktop-exec")
        .into_iter()
        .find(|o| o.channel == OutputChannel::Stdout)
        .unwrap();
    let range = manager
        .execution_output(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            output.id.clone(),
            65534,
            512,
        )
        .await
        .unwrap();
    let actual: Vec<u8> = serde_json::from_value(range["bytes"].clone()).unwrap();
    assert_eq!(
        actual,
        (65534..66046).map(|i| (i % 251) as u8).collect::<Vec<_>>()
    );
    assert!(manager
        .execution_output(
            "model".into(),
            "intruder".into(),
            request.session_id.clone(),
            output.id.clone(),
            0,
            1
        )
        .await
        .is_err());
    b.terminals.shutdown().await;
    b.model_task.abort();
    let reopened = Arc::new(Store::open(&b.path).unwrap());
    let restored = shellpilot_server::agent::chat::Manager::new(reopened, CancellationToken::new());
    restored
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) },
        )
        .await
        .unwrap();
    assert_eq!(
        restored
            .history(
                "model".into(),
                "default".into(),
                request.session_id.clone(),
                0,
                20
            )
            .await
            .unwrap()
            .turns[0]
            .tool_events
            .len(),
        2
    );
    restored
        .execution_output(
            "model".into(),
            "default".into(),
            request.session_id,
            output.id,
            0,
            32,
        )
        .await
        .unwrap();
    assert_eq!(b.execs(), 1);
}
#[tokio::test]
async fn desktop_d5_deny_stop_and_policy_do_not_send_unapproved_commands() {
    for action in ["deny", "stop", "policy"] {
        let b = Bound::new(
            vec![
                Step::Calls(vec![exec("decline", "__exec_chunked__")]),
                Step::Final,
            ],
            90,
            true,
            false,
        )
        .await;
        let (manager, request) = desktop(&b).await;
        let m = manager.clone();
        let r = request.clone();
        let run = tokio::spawn(async move {
            m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await
        });
        let approval = pending_approval(&manager, &request).await;
        match action {
            "stop" => {
                assert!(manager
                    .stop_request(
                        "model".into(),
                        "default".into(),
                        request.session_id.clone(),
                        Some(request.request_id.clone())
                    )
                    .await
                    .unwrap());
            }
            "policy" => {
                manager
                    .execution_policy(
                        "model".into(),
                        "default".into(),
                        request.session_id.clone(),
                        Some((0, shellpilot_server::agent::approvals::Policy::Deny)),
                    )
                    .await
                    .unwrap();
            }
            _ => {
                manager
                    .execution_decision(
                        "model".into(),
                        "default".into(),
                        request.session_id.clone(),
                        approval.id,
                        "deny".into(),
                    )
                    .await
                    .unwrap();
            }
        }
        let result = tokio::time::timeout(Duration::from_secs(8), run)
            .await
            .unwrap()
            .unwrap();
        if action == "stop" {
            assert!(result.is_err());
        } else {
            result.unwrap();
        }
        assert_eq!(b.execs(), 0);
        assert!(
            manager
                .history(
                    "model".into(),
                    "default".into(),
                    request.session_id.clone(),
                    0,
                    20
                )
                .await
                .unwrap()
                .turns[0]
                .state
                != shellpilot_server::agent::chat::TurnState::Running
        );
        assert!(manager
            .execution_requests(
                "model".into(),
                "default".into(),
                request.session_id,
                "".into()
            )
            .await
            .unwrap()
            .iter()
            .all(|r| r.state != ApprovalState::Pending));
        b.terminals.shutdown().await;
    }
}
#[tokio::test]
async fn desktop_d5_fixed_target_busy_guard_and_dropped_ssh_are_not_replayed() {
    let b = Bound::new(
        vec![Step::Calls(vec![exec("quiet", "__exec_quiet__")])],
        90,
        true,
        false,
    )
    .await;
    let (manager, request) = desktop(&b).await;
    let m = manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
    });
    let receipt = pending_approval(&manager, &request).await;
    let mut other = request.clone();
    other.request_id = "another-turn".into();
    assert!(matches!(
        manager
            .chat_desktop(other, None, true, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await,
        Err(shellpilot_server::agent::chat::Error::Busy)
    ));
    let second = b
        .terminals
        .connect(open(&b.target.connection_id))
        .await
        .unwrap();
    assert!(matches!(
        manager
            .bind_execution(
                "model".into(),
                "default".into(),
                request.session_id.clone(),
                second.session_id.clone()
            )
            .await,
        Err(shellpilot_server::agent::chat::Error::Busy)
    ));
    manager
        .execution_decision(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            receipt.id,
            "approveOnce".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while b.execs() == 0 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    run.abort();
    assert!(run.await.unwrap_err().is_cancelled());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if manager
                .history(
                    "model".into(),
                    "default".into(),
                    request.session_id.clone(),
                    0,
                    20,
                )
                .await
                .unwrap()
                .turns[0]
                .state
                != shellpilot_server::agent::chat::TurnState::Running
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) }
        )
        .await
        .is_err());
    assert_eq!(b.execs(), 1);
    let mut wrong = request.clone();
    wrong.request_id = "wrong-target".into();
    wrong.terminal_session_id = second.session_id;
    assert!(matches!(
        manager
            .chat_desktop(wrong, None, true, &CancellationToken::new(), |_| async {
                Ok(())
            })
            .await,
        Err(shellpilot_server::agent::chat::Error::TargetMismatch)
    ));
    let events = b
        .repo
        .events(&request.session_id, "default", 0, 200)
        .unwrap();
    assert_eq!(result(&events, "quiet")["error"], "result_unknown");
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn desktop_d5_batch_compaction_reports_real_checkpoint_and_grants_can_be_revoked() {
    let b = Bound::new(
        vec![
            Step::Calls(vec![
                exec("one", "__exec_large__"),
                exec("two", "__exec_large__"),
                exec("three", "__exec_large__"),
                exec("four", "__exec_large__"),
            ]),
            Step::Final,
        ],
        BATCH_COMPACTION_PERCENT,
        true,
        false,
    )
    .await;
    let (manager, request) = desktop(&b).await;
    let m = manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
    });
    let receipt = pending_approval(&manager, &request).await;
    let id = receipt.id.clone();
    manager
        .execution_decision(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            id.clone(),
            "approveTaskCommand".into(),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(b.execs(), 4);
    assert_eq!(b.wire.script.lock().unwrap().summaries, 1);
    let status = manager
        .context_status("model".into(), "default".into(), request.session_id.clone())
        .await
        .unwrap();
    assert_eq!(status.compaction_count, 1);
    assert!(status.usage.is_some());
    assert_eq!(
        status.phase,
        shellpilot_server::agent::context_status::Phase::Succeeded
    );
    assert_eq!(
        manager
            .history(
                "model".into(),
                "default".into(),
                request.session_id.clone(),
                0,
                20
            )
            .await
            .unwrap()
            .turns[0]
            .tool_events
            .len(),
        8
    );
    manager
        .execution_decision(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            id.clone(),
            "revoke".into(),
        )
        .await
        .unwrap();
    let approvals = manager
        .execution_requests(
            "model".into(),
            "default".into(),
            request.session_id,
            "".into(),
        )
        .await
        .unwrap();
    assert!(
        approvals
            .iter()
            .find(|r| r.id == id)
            .unwrap()
            .grant
            .as_ref()
            .unwrap()
            .revoked
    );
    b.terminals.shutdown().await;
}
#[tokio::test]
async fn desktop_d5_overbudget_input_never_creates_a_turn_or_contacts_model() {
    let b = Bound::new(vec![], 90, true, false).await;
    let (manager, mut request) = desktop(&b).await;
    let models = ModelRepository::new(b.store.clone());
    let mut model = models.get("model", "default").unwrap();
    model.config.context_window = 4096;
    models
        .save(
            "model",
            "default",
            model.revision,
            ModelConfigUpdate {
                config: model.config,
                api_key: KeyChange::Keep,
            },
        )
        .unwrap();
    request.message = "汉".repeat(4000);
    assert!(manager
        .chat_desktop(
            request.clone(),
            None,
            true,
            &CancellationToken::new(),
            |_| async { Ok(()) }
        )
        .await
        .is_err());
    assert!(manager
        .history("model".into(), "default".into(), request.session_id, 0, 20)
        .await
        .unwrap()
        .turns
        .is_empty());
    assert_eq!(b.execs(), 0);
    assert!(b.normal().is_empty());
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn desktop_d5_persisted_stop_blocks_admission_before_token_delivery() {
    let b = Bound::new(
        vec![
            Step::Calls(vec![exec("stopped", "__exec_chunked__")]),
            Step::Final,
        ],
        90,
        true,
        false,
    )
    .await;
    let (manager, request) = desktop(&b).await;
    let m = manager.clone();
    let r = request.clone();
    let run = tokio::spawn(async move {
        m.chat_desktop(r, None, true, &CancellationToken::new(), |_| async {
            Ok(())
        })
        .await
    });
    let receipt = pending_approval(&manager, &request).await;
    // Simulate the exact stop-transaction / token-delivery gap. No token is cancelled here.
    let db = rusqlite::Connection::open(&b.path).unwrap();
    db.execute(
        "UPDATE agent_chat_turns SET cancel_requested=1 WHERE session_id=?1",
        [&request.session_id],
    )
    .unwrap();
    manager
        .execution_decision(
            "model".into(),
            "default".into(),
            request.session_id.clone(),
            receipt.id,
            "approveOnce".into(),
        )
        .await
        .unwrap();
    let _ = tokio::time::timeout(Duration::from_secs(8), run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(b.execs(), 0);
    b.terminals.shutdown().await;
}

#[tokio::test]
async fn tool_turn_receives_confirmed_memory_as_separate_budgeted_context() {
    use shellpilot_server::agent::memory::{Kind, Scope, Write};
    let b = Bound::new(vec![Step::Final], 90, true, false).await;
    b.repo
        .save_memory(
            "model",
            "default",
            &b.sid,
            Write {
                id: uuid::Uuid::new_v4().to_string(),
                revision: 0,
                title: "工具回合偏好".into(),
                content: "MEMORY_TOOL_MARKER：优先解释原始证据。".into(),
                kind: Kind::Preference,
                scope: Scope::Global,
                enabled: true,
            },
        )
        .unwrap();
    b.session()
        .await
        .run(
            "run",
            "分析已有结果",
            Limits::default(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(b.execs(), 0);
    let requests = b.normal();
    assert!(requests[0]["messages"][1]["content"]
        .as_str()
        .unwrap()
        .contains("MEMORY_TOOL_MARKER"));
    assert_eq!(requests[0]["messages"][1]["role"], "system");
    assert!(!serde_json::to_string(&b.events())
        .unwrap()
        .contains("MEMORY_TOOL_MARKER"));
}
