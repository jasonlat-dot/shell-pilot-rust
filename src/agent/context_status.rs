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
            Update::Measured(usage) => status.usage = Some(usage),
            Update::Started(method) => {
                if status.phase != Phase::Succeeded {
                    status.phase = Phase::Running;
                }
                status.method = Some(method);
                status.error_code = None;
                status.fallback = false;
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
        observation
            .apply(Update::Finished(Some("AGENT_CANCELLED".into())))
            .unwrap();
        let status = read(store, "model", "owner", &r.session_id).unwrap();
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
