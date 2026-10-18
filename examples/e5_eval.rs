//! E5 paired evaluation through the production desktop Agent path; never runs real shell commands.
#[path = "e5/fixture.rs"]
mod fixture;
#[path = "e5/model.rs"]
mod model;
#[path = "e5/scoring.rs"]
mod scoring;
use axum::{routing::post, Router};
use rusqlite::{params, Connection, OpenFlags};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use shellpilot_server::{
    agent::{
        approvals::Policy,
        chat::{Manager, Request, StreamEvent},
        extensions::{
            self,
            config::{Config, ExecutionLimits, Settings},
        },
        provider::*,
        Payload, Repository,
    },
    store::Store,
};
use std::{
    error::Error,
    fs::{self, OpenOptions as FileOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio_util::sync::CancellationToken;
type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

struct Options {
    database: Option<PathBuf>,
    model: String,
    user: String,
    out: PathBuf,
    repetitions: usize,
    real: bool,
    case: Option<String>,
    retry_from: Option<PathBuf>,
}
fn options() -> Result<Options> {
    let mut o = Options {
        database: None,
        model: "gpt".into(),
        user: "default".into(),
        out: "target/e5-evaluation.json".into(),
        repetitions: 1,
        real: false,
        case: None,
        retry_from: None,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        if arg == "--real" {
            o.real = true;
            continue;
        }
        let value = args.next().ok_or("missing argument value")?;
        match arg.as_str() {
            "--database" => o.database = Some(value.into()),
            "--model" => o.model = value,
            "--user" => o.user = value,
            "--out" => o.out = value.into(),
            "--repetitions" => o.repetitions = value.parse()?,
            "--case" => o.case = Some(value),
            "--retry-from" => o.retry_from = Some(value.into()),
            _ => return Err("unknown argument".into()),
        }
    }
    if !(1..=10).contains(&o.repetitions) || o.real && o.database.is_none() {
        return Err("real runs require --database; repetitions must be 1..10".into());
    }
    if o.out.exists() || o.out.with_extension("jsonl").exists() {
        return Err("report already exists; choose a new --out path".into());
    }
    Ok(o)
}

// Copies ONE model profile and its existing encrypted credential only. The source
// is opened SQLITE_OPEN_READ_ONLY; user conversations and connections are never imported.
struct FrozenProfile {
    revision: u64,
    config: String,
    key: Option<Vec<u8>>,
}
fn freeze(o: &Options) -> Result<Option<FrozenProfile>> {
    if !o.real {
        return Ok(None);
    }
    let db = Connection::open_with_flags(
        o.database.as_ref().ok_or("missing database")?,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    Ok(Some(db.query_row(
        "SELECT revision,config_json,api_key FROM agent_model_profiles WHERE user_id=?1 AND id=?2",
        params![o.user, o.model],
        |r| {
            Ok(FrozenProfile {
                revision: r.get(0)?,
                config: r.get(1)?,
                key: r.get(2)?,
            })
        },
    )?))
}
fn load_model(
    o: &Options,
    path: &Path,
    percent: u8,
    frozen: Option<&FrozenProfile>,
) -> Result<(Arc<Store>, RuntimeConfig, String)> {
    let store = Arc::new(Store::open(path)?);
    if let Some(profile) = frozen {
        Connection::open(path)?.execute("INSERT INTO agent_model_profiles(id,user_id,revision,config_json,api_key,updated_at) VALUES(?1,?2,?3,?4,?5,0)",params![o.model,o.user,profile.revision,profile.config,profile.key])?;
    } else {
        let config: ModelConfig = serde_json::from_value(
            json!({"provider":"openAiCompatible","baseUrl":"http://127.0.0.1/","completionsPath":"chat/completions","model":"E5-scripted-wire-fixture","contextWindow":16384,"outputReserve":8192,"safetyReserve":512,"compactPercent":percent,"outputTokenParameter":"maxCompletionTokens","capabilities":{"toolCalls":true,"developerRole":false,"temperature":false,"reasoningEffort":false,"vision":true},"temperature":null,"reasoningEffort":null,"timeoutSeconds":10,"maxResponseBytes":1048576,"requireApiKey":false}),
        )?;
        ModelRepository::new(store.clone()).save(
            &o.model,
            &o.user,
            0,
            ModelConfigUpdate {
                config,
                api_key: KeyChange::Keep,
            },
        )?;
    }
    let mut runtime = ModelRepository::new(store.clone()).runtime(&o.model, &o.user)?;
    if runtime.config.provider != ProviderKind::OpenAiCompatible {
        return Err("E5 tool cases require a Chat Completions profile; Responses is not supported by the product tool path".into());
    }
    if runtime.config.context_window < 16384 || runtime.config.output_reserve >= 15360 {
        return Err("profile cannot support the fixed 16384-token evaluation window".into());
    }
    runtime.config.context_window = 16384;
    runtime.config.compact_percent = percent;
    let name = runtime.config.model.clone();
    Ok((store, runtime, name))
}
fn seed(repo: &Repository, sid: &str, user: &str, case: &scoring::Case) -> Result<()> {
    let mut texts = case.history.clone();
    if case.long_history {
        // Public, deterministic distractors; same bytes on both arms, no scored answer.
        // Enough for the 60% arm to compact while the 99% arm can retain full history.
        for i in 0..10 {
            texts.push(format!(
                "历史背景 {i}: {}",
                "archived telemetry row=ok latency=12ms batch=closed; ".repeat(26)
            ));
        }
    }
    for (i, text) in texts.into_iter().enumerate() {
        let state = repo.session(sid, user)?;
        repo.append(
            sid,
            user,
            state.revision,
            &format!("seed-u{i}"),
            Payload::UserMessage { text },
        )?;
        let state = repo.session(sid, user)?;
        repo.append(
            sid,
            user,
            state.revision,
            &format!("seed-a{i}"),
            Payload::AssistantMessage {
                text: "已记录该段历史背景；不表示当前实时状态或执行授权。".into(),
            },
        )?;
    }
    Ok(())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Run {
    case_id: String,
    repetition: usize,
    arm: String,
    compact_percent: u8,
    model: String,
    parameter_fingerprint: String,
    parameters: Value,
    elapsed_ms: u128,
    requests: Vec<model::Measurement>,
    tool_calls: usize,
    extra_tool_calls: usize,
    output_reads: usize,
    attempts: Vec<scoring::Attempt>,
    summary_requests: usize,
    checkpoints: u64,
    context_status: Value,
    replay_clean: bool,
    answer: String,
    error: Option<String>,
    score: scoring::Score,
}
async fn run(
    o: &Options,
    case: &scoring::Case,
    repetition: usize,
    percent: u8,
    frozen: Option<&FrozenProfile>,
) -> Result<Run> {
    let directory = tempfile::tempdir()?;
    let (store, runtime, name) =
        load_model(o, &directory.path().join("e5.sqlite3"), percent, frozen)?;
    let parameters = json!({"provider":runtime.config.provider,"model":runtime.config.model,
        "contextWindow":runtime.config.context_window,"outputReserve":runtime.config.output_reserve,
        "safetyReserve":runtime.config.safety_reserve,"reasoningEffort":runtime.config.reasoning_effort,
        "temperature":runtime.config.temperature,"outputTokenParameter":runtime.config.output_token_parameter,
        "timeoutSeconds":runtime.config.timeout_seconds,"maxResponseBytes":runtime.config.max_response_bytes});
    let mut comparison = serde_json::to_value(&runtime.config)?;
    comparison["compactPercent"] = json!(0);
    let parameter_fingerprint = digest(&serde_json::to_vec(&comparison)?);
    let measurements = Arc::new(Mutex::new(vec![]));
    let cancel = CancellationToken::new();
    let bridge = model::Bridge {
        client: if o.real {
            Some(Arc::new(ModelClient::new(runtime.clone())?))
        } else {
            None
        },
        case: case.clone(),
        measurements: measurements.clone(),
        cancel: cancel.clone(),
    };
    let inventory = fixture::Inventory {
        attempts: Arc::new(Mutex::new(vec![])),
    };
    let router = Router::new()
        .route("/model", post(model::model))
        .with_state(bridge)
        .merge(
            Router::new()
                .route("/mcp", post(fixture::mcp))
                .with_state(inventory.clone()),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let base = format!("http://{}/", listener.local_addr()?);
    let server_cancel = CancellationToken::new();
    let shutdown = server_cancel.clone();
    let server = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(shutdown.cancelled_owned())
            .await
    });
    let mut config = runtime.config;
    config.base_url = base.clone();
    config.completions_path = "model".into();
    config.require_api_key = false;
    let models = ModelRepository::new(store.clone());
    let old = models.get(&o.model, &o.user)?;
    models.save(
        &o.model,
        &o.user,
        old.revision,
        ModelConfigUpdate {
            config,
            api_key: KeyChange::Clear,
        },
    )?;
    let extension: Config = serde_json::from_value(
        json!({"servers":[{"name":"inventory","enabled":true,"transport":"streamableHttp","url":format!("{base}mcp")}],"subAgents":false}),
    )?;
    extensions::save(
        &store,
        &o.user,
        Settings {
            revision: 0,
            config: Config {
                execution_limits: ExecutionLimits {
                    model_rounds: 12,
                    tool_calls: 16,
                    ..Default::default()
                },
                ..extension
            },
        },
    )?;
    let manager = Manager::new(store.clone(), CancellationToken::new());
    let session = manager
        .create_general_session_with_policy(
            o.model.clone(),
            o.user.clone(),
            Some(Policy::FullAccess),
        )
        .await?;
    let repo = Repository::new(store.clone());
    seed(&repo, &session.id, &o.user, case)?;
    let request=Request{request_id:uuid::Uuid::new_v4().to_string(),agent_id:o.model.clone(),user_id:o.user.clone(),session_id:session.id.clone(),terminal_session_id:String::new(),message:format!("{}\n本评测只使用 inventory 服务的 inspect 工具。最后用简短中文列出目标、事实及操作限制。",case.message),attachments:vec![]};
    let start = Instant::now();
    let mut answer = String::new();
    let mut stream_error = None;
    let execution = manager.chat_desktop(request.clone(), None, true, &cancel, |event| {
        match event {
            StreamEvent::Done { content } => answer = content,
            StreamEvent::Error { code, .. } => stream_error = Some(code),
            _ => {}
        }
        async { Ok(()) }
    });
    let result = tokio::time::timeout(Duration::from_secs(240), execution).await;
    let error = match result {
        Ok(Ok(())) => stream_error,
        Ok(Err(e)) => Some(e.code().into()),
        Err(_) => {
            cancel.cancel();
            Some("E5_CASE_TIMEOUT".into())
        }
    };
    let elapsed_ms = start.elapsed().as_millis();
    let before_requests = measurements.lock().unwrap().len();
    let before_attempts = inventory.attempts.lock().unwrap().len();
    let mut replay_answer = String::new();
    let replay = manager
        .chat_desktop(request, None, true, &CancellationToken::new(), |event| {
            if let StreamEvent::Done { content } = event {
                replay_answer = content;
            }
            async { Ok(()) }
        })
        .await;
    let replay_clean = measurements.lock().unwrap().len() == before_requests
        && inventory.attempts.lock().unwrap().len() == before_attempts
        && if error.is_none() {
            replay.is_ok() && answer == replay_answer
        } else {
            replay.is_err()
        };
    let events = repo.events(&session.id, &o.user, 0, 200)?;
    let tool_calls = events
        .iter()
        .filter(|e| matches!(&e.payload, Payload::ToolCall { .. }))
        .count();
    let output_reads=events.iter().filter(|e|matches!(&e.payload,Payload::ToolCall{tool_name,..} if tool_name=="read_tool_output")).count();
    let context_status = manager
        .context_status(o.model.clone(), o.user.clone(), session.id)
        .await?;
    let checkpoints = context_status.compaction_count;
    let context_status = serde_json::to_value(context_status)?;
    manager.shutdown();
    cancel.cancel();
    server_cancel.cancel();
    let _ = tokio::time::timeout(Duration::from_secs(3), server).await;
    let requests = measurements.lock().unwrap().clone();
    let attempts = inventory.attempts.lock().unwrap().clone();
    let score = scoring::score(case, &answer, &attempts, error.is_some(), replay_clean);
    let expected_native_calls = case.operations.len() + usize::from(!case.operations.is_empty());
    Ok(Run {
        case_id: case.id.clone(),
        repetition,
        arm: if percent == 99 {
            "A_late_99"
        } else {
            "B_proactive_60"
        }
        .into(),
        compact_percent: percent,
        model: name,
        parameter_fingerprint,
        parameters,
        elapsed_ms,
        summary_requests: requests.iter().filter(|m| m.summary).count(),
        requests,
        tool_calls,
        extra_tool_calls: tool_calls.saturating_sub(expected_native_calls),
        output_reads,
        attempts,
        checkpoints,
        context_status,
        replay_clean,
        answer,
        error,
        score,
    })
}

fn aggregate(runs: &[Run], arm: &str) -> Value {
    let group: Vec<_> = runs.iter().filter(|r| r.arm == arm).collect();
    let mut times: Vec<_> = group.iter().map(|r| r.elapsed_ms).collect();
    times.sort();
    let usages: Vec<_> = group.iter().flat_map(|r| r.requests.iter()).collect();
    let complete_usage = !usages.is_empty() && usages.iter().all(|r| r.usage.is_some());
    let facts_expected: usize = group.iter().map(|r| r.score.facts_expected).sum();
    let target_scored: Vec<_> = group
        .iter()
        .filter_map(|r| r.score.target_correct)
        .collect();
    json!({"arm":arm,"runs":group.len(),"completed":group.iter().filter(|r|r.score.completed).count(),
        "constraintPasses":group.iter().filter(|r|r.score.constraints_preserved).count(),
        "factsRetained":group.iter().map(|r|r.score.facts_retained).sum::<usize>(),"factsExpected":facts_expected,
        "targetCorrect":target_scored.iter().filter(|correct|**correct).count(),"targetScored":target_scored.len(),
        "modelRequests":usages.len(),"summaryRequests":group.iter().map(|r|r.summary_requests).sum::<usize>(),
        "successfulModelRequests":usages.iter().filter(|r|r.error.is_none()).count(),
        "failedModelRequests":usages.iter().filter(|r|r.error.is_some()).count(),
        "extraExecutions":group.iter().map(|r|r.score.extra_executions).sum::<usize>(),
        "extraToolCalls":group.iter().map(|r|r.extra_tool_calls).sum::<usize>(),
        "duplicateExecutions":group.iter().map(|r|r.score.duplicate_executions).sum::<usize>(),
        "replayPasses":group.iter().filter(|r|r.replay_clean).count(),
        "latencyMedianMs":times.get(times.len()/2),"latencyTotalMs":times.iter().sum::<u128>(),
        "usageComplete":complete_usage,"usageReportedRequests":usages.iter().filter(|r|r.usage.is_some()).count(),
        "promptTokens":if complete_usage{Some(usages.iter().map(|r|r.usage.as_ref().unwrap().prompt_tokens).sum::<u64>())}else{None},
        "completionTokens":if complete_usage{Some(usages.iter().map(|r|r.usage.as_ref().unwrap().completion_tokens).sum::<u64>())}else{None}})
}
#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    let o = options()?;
    let frozen = freeze(&o)?;
    let previous = if let Some(path) = &o.retry_from {
        let text = fs::read_to_string(path)?;
        if path.extension().is_some_and(|ext| ext == "jsonl") {
            text.lines()
                .filter(|l| !l.trim().is_empty())
                .map(serde_json::from_str)
                .collect::<std::result::Result<Vec<Value>, _>>()?
        } else {
            serde_json::from_str::<Value>(&text)?["runs"]
                .as_array()
                .ok_or("retry report needs runs")?
                .clone()
        }
    } else {
        vec![]
    };
    let cases: Vec<_> = scoring::cases()
        .into_iter()
        .filter(|c| o.case.as_ref().is_none_or(|id| &c.id == id))
        .collect();
    if cases.is_empty() {
        return Err("unknown evaluation case".into());
    }
    if let Some(parent) = o.out.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut journal = FileOptions::new()
        .create_new(true)
        .write(true)
        .open(o.out.with_extension("jsonl"))?;
    let mut runs = vec![];
    let mut aborted_reason = None;
    let mut skipped_passes = 0usize;
    'matrix: for repetition in 0..o.repetitions {
        for (case_index, case) in cases.iter().enumerate() {
            let order = if (repetition + case_index) % 2 == 0 {
                [99, 60]
            } else {
                [60, 99]
            };
            for percent in order {
                let arm = if percent == 99 {
                    "A_late_99"
                } else {
                    "B_proactive_60"
                };
                if o.retry_from.is_some()
                    && !scoring::should_retry(&previous, &case.id, arm, repetition + 1)
                {
                    skipped_passes += 1;
                    continue;
                }
                eprintln!(
                    "E5 {} repetition={} threshold={percent} mode={}",
                    case.id,
                    repetition + 1,
                    if o.real { "REAL" } else { "SCRIPTED_SMOKE" }
                );
                let result = run(&o, case, repetition + 1, percent, frozen.as_ref()).await?;
                if runs
                    .first()
                    .is_some_and(|r: &Run| r.parameter_fingerprint != result.parameter_fingerprint)
                {
                    fs::write(
                        o.out.with_extension("excluded.json"),
                        serde_json::to_vec_pretty(&json!({
                            "excludedFromComparison":true,"reason":"model_configuration_changed","run":result
                        }))?,
                    )?;
                    aborted_reason = Some("model_configuration_changed");
                    break 'matrix;
                }
                eprintln!(
                    "  completed={} requests={} summaries={} error={:?}",
                    result.score.completed,
                    result.requests.len(),
                    result.summary_requests,
                    result.error
                );
                serde_json::to_writer(&mut journal, &result)?;
                writeln!(journal)?;
                journal.flush()?;
                let unavailable = o.real
                    && !result.requests.is_empty()
                    && result.requests.iter().all(|r| r.error.is_some());
                runs.push(result);
                if unavailable {
                    aborted_reason = Some("upstream_model_unavailable");
                    break 'matrix;
                }
            }
        }
    }
    let report = json!({"version":1,"mode":if o.real{"real_model"}else{"scripted_smoke_not_quality_evidence"},
        "suiteSha256":digest(include_bytes!("../evals/e5/cases.json")),
        "matrixComplete":runs.len()+skipped_passes==cases.len()*o.repetitions*2,"abortedReason":aborted_reason,
        "retryOnly":o.retry_from.is_some(),"previousPassesSkipped":skipped_passes,"profileFrozenAtStart":true,
        "createdAt":chrono::Utc::now().to_rfc3339(),"comparison":"Same Rust Agent/model; 99% vs 60% compaction threshold; NOT Java vs Rust",
        "contextWindow":16384,"repetitions":o.repetitions,"caseCount":cases.len(),
        "limitations":["No real SSH or external MCP; only isolated deterministic inventory","Exact public fact and tool trace checks; free-text semantics require manual review","Completion latency through buffered upstream bridge; no first-token claim","Token totals are null if any provider usage is missing","No statistical significance claim from a small fixed suite"],
        "aggregate":[aggregate(&runs,"A_late_99"),aggregate(&runs,"B_proactive_60")],"runs":runs});
    fs::write(&o.out, serde_json::to_vec_pretty(&report)?)?;
    println!("E5 report: {}", o.out.display());
    if let Some(reason) = aborted_reason {
        return Err(format!("{reason}; partial report saved, evaluation incomplete").into());
    }
    Ok(())
}
