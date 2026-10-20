//! Request-input measurements and real compaction lifecycle; never stores prompts or credentials.
use super::{
    chat::{Error, Request, Result},
    provider::ModelConfig,
    Repository,
};
use crate::store::Store;
use rusqlite::{params, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio::sync::mpsc;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InputUsage {
    pub input_tokens: u64,
    pub available_input_tokens: u64,
    pub context_window: u64,
    pub output_reserve: u64,
    pub safety_reserve: u64,
    pub compact_percent: u8,
    pub source: Source,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Source {
    Estimated,
    ProviderCount,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum Phase {
    Idle,
    Running,
    Succeeded,
    Failed,
    Cancelled,
    Interrupted,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Method {
    Summary,
    Remote,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub request_id: String,
    pub usage: Option<InputUsage>,
    #[serde(default)]
    pub usage_stale: bool,
    pub phase: Phase,
    pub method: Option<Method>,
    pub error_code: Option<String>,
    pub fallback: bool,
    pub checkpoint_id: Option<String>,
    pub compaction_count: u64,
    pub updated_at: i64,
}
#[derive(Clone)]
pub(crate) enum Update {
    Measured(InputUsage),
    Started(Method),
    ToolStarted(Option<String>),
    Committed(String),
    Failed { code: String, fallback: bool },
    Finished(Option<String>),
}
#[derive(Clone, Default)]
pub(crate) struct Reporter(Option<mpsc::UnboundedSender<Update>>, Option<Observation>);
impl Reporter {
    pub fn channel(observation: Observation) -> (Self, mpsc::UnboundedReceiver<Update>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self(Some(tx), Some(observation)), rx)
    }
    pub fn emit(&self, update: Update) {
        use crate::diagnostics::{event,Level};
        match &update {
            Update::Measured(usage)=>event(Level::Debug,"context.measured",serde_json::json!({"inputTokens":usage.input_tokens})),
            Update::Started(method)=>event(Level::Info,"context.compaction_started",serde_json::json!({"method":method})),
            Update::ToolStarted(checkpoint)=>event(Level::Info,"context.tool_compaction_started",serde_json::json!({"checkpointId":checkpoint})),
            Update::Committed(checkpoint)=>event(Level::Info,"context.compaction_committed",serde_json::json!({"checkpointId":checkpoint})),
            Update::Failed{code,fallback}=>event(Level::Warn,"context.compaction_failed",serde_json::json!({"errorCode":code,"fallback":fallback})),
            Update::Finished(code)=>event(Level::Debug,"context.finished",serde_json::json!({"errorCode":code})),
        }
        // Called immediately after the guarded checkpoint transaction, in its blocking worker.
        // Persist success before waiting for a UI ACK; cancellation or a closed subscriber must
        // not turn an actually committed checkpoint into a cancelled compaction record.
        if matches!(update, Update::Committed(_)) {
            if let Some(observation) = &self.1 {
                let _ = observation.apply(update.clone());
            }
        }
        if let Some(tx) = &self.0 {
            let _ = tx.send(update);
        }
    }
    pub fn measured(&self, config: &ModelConfig, tokens: u64, source: Source) -> Result<()> {
        self.emit(Update::Measured(InputUsage {
            input_tokens: tokens,
            available_input_tokens: config
                .budget()
                .available_input()
                .map_err(|_| Error::CompactionLimit)?,
            context_window: config.context_window,
            output_reserve: config.output_reserve,
            safety_reserve: config.safety_reserve,
            compact_percent: config.compact_percent,
            source,
        }));
        Ok(())
    }
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn db_error(_: rusqlite::Error) -> Error {
    Error::Storage
}
fn counts(db: &rusqlite::Connection, session: &str) -> Result<u64> {
    db.query_row(
        "SELECT COUNT(*) FROM agent_checkpoints WHERE session_id=?1",
        [session],
        |r| r.get(0),
    )
    .map_err(db_error)
}
#[derive(Clone)]
pub(crate) struct Observation {
    store: Arc<Store>,
    session: String,
    owner: String,
}
impl Observation {
    pub fn begin(store: Arc<Store>, request: &Request) -> Result<Self> {
        if Repository::new(store.clone())
            .session(&request.session_id, &request.user_id)?
            .agent_id
            != request.agent_id
        {
            return Err(Error::NotFound);
        }
        let owner = uuid::Uuid::new_v4().to_string();
        let status = Status {
            request_id: request.request_id.clone(),
            usage: None,
            usage_stale: false,
            phase: Phase::Idle,
            method: None,
            error_code: None,
            fallback: false,
            checkpoint_id: None,
            compaction_count: 0,
            updated_at: now(),
        };
        {
            let db = store.database.lock().map_err(|_| Error::Storage)?;
            let previous: Option<String> = db.query_row("SELECT status_json FROM agent_context_status WHERE session_id=?1", [&request.session_id], |r|r.get(0)).optional().map_err(db_error)?;
            let mut status = status;
            status.usage = previous.and_then(|json|serde_json::from_str::<Status>(&json).ok()).and_then(|previous|previous.usage);
            status.usage_stale = status.usage.is_some();
            let json = serde_json::to_string(&status).map_err(|_| Error::Storage)?;
            db.execute("INSERT INTO agent_context_status(session_id,owner,status_json,lease_until) VALUES(?1,?2,?3,?4) ON CONFLICT(session_id) DO UPDATE SET owner=excluded.owner,status_json=excluded.status_json,lease_until=excluded.lease_until",params![request.session_id,owner,json,now()+30_000]).map_err(db_error)?;
        }
        Ok(Self {
            store,
            session: request.session_id.clone(),
            owner,
        })
    }
    pub fn heartbeat(&self) -> Result<()> {
        self.store
            .database
            .lock()
            .map_err(|_| Error::Storage)?
            .execute(
                "UPDATE agent_context_status SET lease_until=?1 WHERE session_id=?2 AND owner=?3",
                params![now() + 30_000, self.session, self.owner],
            )
            .map_err(db_error)?;
        Ok(())
    }
    pub fn apply(&self, update: Update) -> Result<Option<Status>> {
        let db = self.store.database.lock().map_err(|_| Error::Storage)?;
        let json: Option<String> = db
            .query_row(
                "SELECT status_json FROM agent_context_status WHERE session_id=?1 AND owner=?2",
                params![self.session, self.owner],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        let Some(json) = json else { return Ok(None) };
        let mut status: Status = serde_json::from_str(&json).map_err(|_| Error::Storage)?;
        match update {
            Update::Measured(usage) => { status.usage = Some(usage); status.usage_stale = false; }
            Update::Started(method) => {
                if status.phase != Phase::Succeeded {
                    status.phase = Phase::Running;
                }
                status.method = Some(method);
                status.error_code = None;
                status.fallback = false;
            }
            Update::ToolStarted(base) => {
                // A fresh tool boundary can compact again within the same turn. An old queued
                // start must not undo a checkpoint already committed before the UI ACK.
                if status.checkpoint_id == base || status.checkpoint_id.is_none() {
                    status.phase = Phase::Running;
                    status.method = Some(Method::Summary);
                    status.error_code = None;
                    status.fallback = false;
                }
            }
            Update::Committed(id) => {
                let exists:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM agent_checkpoints WHERE id=?1 AND session_id=?2)",params![id,self.session],|r|r.get(0)).map_err(db_error)?;
                if !exists {
                    return Err(Error::Storage);
                }
                status.phase = Phase::Succeeded;
                status.checkpoint_id = Some(id);
                status.error_code = None;
                status.fallback = false;
            }
            Update::Failed { code, fallback } => {
                if status.phase != Phase::Succeeded {
                    status.phase = Phase::Failed;
                    status.error_code = Some(code);
                    status.fallback = fallback;
                }
            }
            Update::Finished(code) => {
                // A completed/failed attempt is immutable. In particular, a late run-guard
                // cleanup must not rewrite updatedAt after the caller has observed Done.
                if status.phase != Phase::Running {
                    return Ok(None);
                }
                if status.phase == Phase::Running {
                    status.phase = match code.as_deref() {
                        Some("AGENT_CANCELLED") => Phase::Cancelled,
                        Some("AGENT_INTERRUPTED") | None => Phase::Interrupted,
                        Some(_) => Phase::Failed,
                    };
                    status.error_code = code;
                }
            }
        }
        status.compaction_count = counts(&db, &self.session)?;
        status.updated_at = now();
        db.execute("UPDATE agent_context_status SET status_json=?1,lease_until=?2 WHERE session_id=?3 AND owner=?4",params![serde_json::to_string(&status).map_err(|_|Error::Storage)?,now()+30_000,self.session,self.owner]).map_err(db_error)?;
        Ok(Some(status))
    }
}
pub(crate) fn read(store: Arc<Store>, agent: &str, user: &str, session: &str) -> Result<Status> {
    let existing = Repository::new(store.clone()).session(session, user)?;
    if existing.agent_id != agent {
        return Err(Error::NotFound);
    }
    let db = store.database.lock().map_err(|_| Error::Storage)?;
    let row: Option<(String, i64)> = db
        .query_row(
            "SELECT status_json,lease_until FROM agent_context_status WHERE session_id=?1",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(db_error)?;
    let mut status = if let Some((json, lease)) = row {
        let mut status: Status = serde_json::from_str(&json).map_err(|_| Error::Storage)?;
        if status.phase == Phase::Running && lease < now() {
            status.phase = Phase::Interrupted;
            status.error_code = Some("AGENT_INTERRUPTED".into());
        }
        status
    } else {
        Status {
            request_id: String::new(),
            usage: None,
            usage_stale: false,
            phase: Phase::Idle,
            method: None,
            error_code: None,
            fallback: false,
            checkpoint_id: None,
            compaction_count: 0,
            updated_at: 0,
        }
    };
    status.compaction_count = counts(&db, session)?;
    Ok(status)
}

/// Retain display-only measurements across a binding change. These values never
/// participate in request budgeting. Revoke old reporters only for an explicit
/// idle model switch; a binding refresh can already have a new active reporter.
pub(crate) fn invalidate_usage(db: &rusqlite::Connection, session: &str, revoke_reporter: bool) -> super::Result<()> {
    let json: Option<String> = db.query_row("SELECT status_json FROM agent_context_status WHERE session_id=?1", [session], |r|r.get(0)).optional()?;
    let Some(mut status) = json.and_then(|json|serde_json::from_str::<Status>(&json).ok()) else {
        db.execute("DELETE FROM agent_context_status WHERE session_id=?1", [session])?;
        return Ok(());
    };
    status.usage_stale = status.usage.is_some();
    status.phase = Phase::Idle; status.method = None; status.error_code = None; status.fallback = false; status.checkpoint_id = None;
    db.execute("UPDATE agent_context_status SET status_json=?2,owner=CASE WHEN ?3 THEN ?4 ELSE owner END,lease_until=CASE WHEN ?3 THEN 0 ELSE lease_until END WHERE session_id=?1", params![session,serde_json::to_string(&status)?,revoke_reporter,uuid::Uuid::new_v4().to_string()])?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn committed_checkpoint_survives_a_closed_subscriber_and_late_cancellation() {
        let store = Arc::new(Store::in_memory().unwrap());
        let r = request(&store, "commit-before-ack");
        let repo = Repository::new(store.clone());
        let event = repo
            .append(
                &r.session_id,
                "owner",
                0,
                "evidence",
                crate::agent::Payload::UserMessage {
                    text: "keep evidence".into(),
                },
            )
            .unwrap()
            .event;
        let snapshot = repo.snapshot(&r.session_id, "owner").unwrap();
        let observation = Observation::begin(store.clone(), &r).unwrap();
        let (reporter, mut updates) = Reporter::channel(observation.clone());
        reporter.emit(Update::Started(Method::Summary));
        let checkpoint = repo
            .save_checkpoint(
                &snapshot,
                crate::agent::CheckpointContent {
                    task: None,
                    summary: "summary".into(),
                    next_steps: vec![],
                    evidence_event_ids: vec![event.id],
                    retained_user_facts: vec![],
                    pending_tool_call_ids: vec![],
                    uncertain_tool_call_ids: vec![],
                },
            )
            .unwrap();
        reporter.emit(Update::Committed(checkpoint.id));
        observation.apply(updates.try_recv().unwrap()).unwrap(); // delayed start must not undo commit
        drop(updates);
        let before = read(store.clone(), "model", "owner", &r.session_id)
            .unwrap()
            .updated_at;
        assert!(observation
            .apply(Update::Finished(Some("AGENT_CANCELLED".into())))
            .unwrap()
            .is_none());
        let status = read(store, "model", "owner", &r.session_id).unwrap();
        assert_eq!(status.updated_at, before);
        assert_eq!(status.phase, Phase::Succeeded);
        assert_eq!(status.compaction_count, 1);
    }
    fn request(store: &Arc<Store>, id: &str) -> Request {
        let session = Repository::new(store.clone())
            .create_session("model", "owner")
            .unwrap();
        Request {
            request_id: id.into(),
            agent_id: "model".into(),
            user_id: "owner".into(),
            session_id: session.id,
            terminal_session_id: String::new(),
            message: "hello".into(),
            attachments: vec![],
        }
    }
    fn measured(tokens: u64) -> Update {
        Update::Measured(InputUsage { input_tokens: tokens, available_input_tokens: 6000, context_window: 8192, output_reserve: 1680, safety_reserve: 512, compact_percent: 75, source: Source::Estimated })
    }
    #[test]
    fn next_request_retains_last_usage_until_measurement_even_if_preparation_fails() {
        let store=Arc::new(Store::in_memory().unwrap());let mut r=request(&store,"first");
        let first=Observation::begin(store.clone(),&r).unwrap();first.apply(measured(1500)).unwrap();
        r.request_id="next".into();let next=Observation::begin(store.clone(),&r).unwrap();
        let status=read(store.clone(),"model","owner",&r.session_id).unwrap();assert_eq!(status.usage.unwrap().input_tokens,1500);assert!(status.usage_stale);assert_eq!(status.request_id,"next");
        assert!(first.apply(measured(999)).unwrap().is_none());
        next.apply(Update::Started(Method::Summary)).unwrap();next.apply(Update::Finished(Some("MODEL_HTTP_ERROR".into()))).unwrap();
        let failed=read(store.clone(),"model","owner",&r.session_id).unwrap();assert_eq!(failed.usage.unwrap().input_tokens,1500);assert!(failed.usage_stale);
        let updated=next.apply(measured(2400)).unwrap().unwrap();assert_eq!(updated.usage.unwrap().input_tokens,2400);assert!(!updated.usage_stale);
    }
    #[test]
    fn model_switch_retains_stale_usage_across_reopen_and_revokes_old_reporters() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("retained.db");let store=Arc::new(Store::open(&path).unwrap());
        let r=request(&store,"old");let old=Observation::begin(store.clone(),&r).unwrap();old.apply(measured(1500)).unwrap();
        let repo=Repository::new(store.clone());repo.change_model("owner",&r.session_id,"model","model",0).unwrap();assert!(!read(store.clone(),"model","owner",&r.session_id).unwrap().usage_stale);
        // Legacy JSON without the new optional marker remains readable.
        { let db=store.database.lock().unwrap();let json:String=db.query_row("SELECT status_json FROM agent_context_status WHERE session_id=?1",[&r.session_id],|row|row.get(0)).unwrap();let mut raw:serde_json::Value=serde_json::from_str(&json).unwrap();raw.as_object_mut().unwrap().remove("usageStale");db.execute("UPDATE agent_context_status SET status_json=?1 WHERE session_id=?2",params![raw.to_string(),r.session_id]).unwrap(); }
        repo.change_model("owner",&r.session_id,"model","next-model",0).unwrap();assert!(old.apply(measured(999)).unwrap().is_none());
        assert!(read(store.clone(),"model","owner",&r.session_id).is_err());assert!(read(store.clone(),"next-model","other-user",&r.session_id).is_err());
        drop(old);drop(repo);drop(store);let store=Arc::new(Store::open(&path).unwrap());
        let status=read(store,"next-model","owner",&r.session_id).unwrap();assert_eq!(status.usage.unwrap().input_tokens,1500);assert!(status.usage_stale);assert_eq!(status.phase,Phase::Idle);
    }
    #[test]
    fn binding_refresh_keeps_the_new_request_reporter_and_unknown_sessions_do_not_invent_usage() {
        let store=Arc::new(Store::in_memory().unwrap());let r=request(&store,"fresh");let repo=Repository::new(store.clone());
        repo.change_model("owner",&r.session_id,"model","model-two",0).unwrap();let status=read(store.clone(),"model-two","owner",&r.session_id).unwrap();assert!(status.usage.is_none());assert!(!status.usage_stale);
        let mut r=r;r.agent_id="model-two".into();let observation=Observation::begin(store.clone(),&r).unwrap();observation.apply(measured(1500)).unwrap();
        repo.refresh_model_binding("owner",&r.session_id,"model-two",1).unwrap();assert!(read(store.clone(),"model-two","owner",&r.session_id).unwrap().usage_stale);
        let updated=observation.apply(measured(2400)).unwrap().unwrap();assert_eq!(updated.usage.unwrap().input_tokens,2400);assert!(!updated.usage_stale);
    }
    #[test]
    fn measurements_survive_schema_ten_upgrade_and_reopen_without_keys_or_model_calls() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("context.db");
        let store = Arc::new(Store::open(&path).unwrap());
        let r = request(&store, "measured");
        store
            .database
            .lock()
            .unwrap()
            .execute_batch("DROP TABLE agent_context_status; PRAGMA user_version=10;")
            .unwrap();
        drop(store);
        let store = Arc::new(Store::open(&path).unwrap());
        let observation = Observation::begin(store.clone(), &r).unwrap();
        let (reporter, mut updates) = Reporter::channel(observation.clone());
        let config:ModelConfig=serde_json::from_value(serde_json::json!({"provider":"openAiCompatible","baseUrl":"http://127.0.0.1/v1/","completionsPath":"chat/completions","model":"test","contextWindow":8192,"outputReserve":1024,"safetyReserve":512,"compactPercent":75,"outputTokenParameter":"maxTokens","capabilities":{"toolCalls":false,"developerRole":false,"temperature":false,"reasoningEffort":false,"vision":false},"temperature":null,"reasoningEffort":null,"timeoutSeconds":30,"maxResponseBytes":65536,"requireApiKey":false})).unwrap();
        reporter.measured(&config, 1200, Source::Estimated).unwrap();
        observation.apply(updates.try_recv().unwrap()).unwrap();
        drop(observation);
        drop(store);
        let store = Arc::new(Store::open(&path).unwrap());
        let status = read(store.clone(), "model", "owner", &r.session_id).unwrap();
        assert_eq!(status.usage.unwrap().input_tokens, 1200);
        assert_eq!(status.compaction_count, 0);
        assert!(read(store.clone(), "model", "other-user", &r.session_id).is_err());
        assert!(read(store, "other-model", "owner", &r.session_id).is_err());
    }
    #[test]
    fn late_observations_do_not_replace_the_next_request() {
        let store = Arc::new(Store::in_memory().unwrap());
        let mut r = request(&store, "old");
        let old = Observation::begin(store.clone(), &r).unwrap();
        r.request_id = "next".into();
        let next = Observation::begin(store.clone(), &r).unwrap();
        assert!(old
            .apply(Update::Started(Method::Summary))
            .unwrap()
            .is_none());
        next.apply(Update::Started(Method::Remote)).unwrap();
        let status = read(store, "model", "owner", &r.session_id).unwrap();
        assert_eq!(status.request_id, "next");
        assert_eq!(status.phase, Phase::Running);
    }
    #[test]
    fn tool_boundaries_report_new_attempts_without_undoing_a_delayed_committed_checkpoint() {
        let store = Arc::new(Store::in_memory().unwrap());
        let r = request(&store, "tool-status");
        let repo = Repository::new(store.clone());
        let event = repo
            .append(
                &r.session_id,
                "owner",
                0,
                "user",
                crate::agent::Payload::UserMessage {
                    text: "logs".into(),
                },
            )
            .unwrap()
            .event;
        let observation = Observation::begin(store.clone(), &r).unwrap();
        let (reporter, mut updates) = Reporter::channel(observation.clone());
        reporter.emit(Update::ToolStarted(None));
        let snapshot = repo.snapshot(&r.session_id, "owner").unwrap();
        let checkpoint = repo
            .save_checkpoint(
                &snapshot,
                crate::agent::CheckpointContent {
                    task: None,
                    summary: "summary".into(),
                    next_steps: vec![],
                    evidence_event_ids: vec![event.id],
                    retained_user_facts: vec![],
                    pending_tool_call_ids: vec![],
                    uncertain_tool_call_ids: vec![],
                },
            )
            .unwrap();
        reporter.emit(Update::Committed(checkpoint.id.clone()));
        observation.apply(updates.try_recv().unwrap()).unwrap();
        assert_eq!(
            read(store.clone(), "model", "owner", &r.session_id)
                .unwrap()
                .phase,
            Phase::Succeeded
        );
        observation
            .apply(Update::ToolStarted(Some(checkpoint.id.clone())))
            .unwrap();
        assert_eq!(
            read(store.clone(), "model", "owner", &r.session_id)
                .unwrap()
                .phase,
            Phase::Running
        );
        observation
            .apply(Update::Failed {
                code: "summary_failed".into(),
                fallback: true,
            })
            .unwrap();
        let status = read(store.clone(), "model", "owner", &r.session_id).unwrap();
        assert_eq!(status.phase, Phase::Failed);
        assert_eq!(status.compaction_count, 1);
        assert_eq!(status.checkpoint_id, Some(checkpoint.id.clone()));
        observation
            .apply(Update::ToolStarted(Some(checkpoint.id)))
            .unwrap();
        observation
            .apply(Update::Finished(Some("AGENT_CANCELLED".into())))
            .unwrap();
        let status = read(store, "model", "owner", &r.session_id).unwrap();
        assert_eq!(status.phase, Phase::Cancelled);
        assert_eq!(status.compaction_count, 1);
    }
    #[test]
    fn unfinished_or_uncommitted_compaction_never_reports_success() {
        let store = Arc::new(Store::in_memory().unwrap());
        let r = request(&store, "interrupted");
        let observation = Observation::begin(store.clone(), &r).unwrap();
        observation.apply(Update::Started(Method::Summary)).unwrap();
        assert!(observation
            .apply(Update::Committed("missing-checkpoint".into()))
            .is_err());
        store
            .database
            .lock()
            .unwrap()
            .execute("UPDATE agent_context_status SET lease_until=0", [])
            .unwrap();
        let status = read(store.clone(), "model", "owner", &r.session_id).unwrap();
        assert_eq!(status.phase, Phase::Interrupted);
        assert_eq!(status.compaction_count, 0);
        observation
            .apply(Update::Finished(Some("AGENT_CANCELLED".into())))
            .unwrap();
        assert_eq!(
            read(store, "model", "owner", &r.session_id).unwrap().phase,
            Phase::Cancelled
        );
    }
}
