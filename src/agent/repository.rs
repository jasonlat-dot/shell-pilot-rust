use super::*;
use crate::store::Store;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Arc;

/// Synchronous SQLite kernel; async hosts must call it from a blocking worker.
#[derive(Clone)]
pub struct Repository {
    pub(super) store: Arc<Store>,
    pub(super) output_limits: OutputLimits,
}
impl Repository {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            output_limits: OutputLimits::default(),
        }
    }
    pub(super) fn db<T>(&self, action: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut db = self.store.database.lock().map_err(|_| Error::Worker)?;
        action(&mut db)
    }
    pub fn create_session(&self, agent_id: &str, user_id: &str) -> Result<Session> {
        model::identifier(agent_id)?;
        model::identifier(user_id)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.db(|db| {
            db.execute(
                "INSERT INTO agent_sessions(id,user_id,agent_id,created_at) VALUES(?1,?2,?3,?4)",
                params![id, user_id, agent_id, chrono::Utc::now().timestamp()],
            )?;
            read_session(db, &id, user_id)
        })
    }
    pub fn session(&self, id: &str, user_id: &str) -> Result<Session> {
        self.db(|db| read_session(db, id, user_id))
    }
    pub fn snapshot(&self, id: &str, user_id: &str) -> Result<Snapshot> {
        self.db(|db| {
            let tx = db.transaction()?;
            let snapshot = read_snapshot(&tx, id, user_id)?;
            tx.commit()?;
            Ok(snapshot)
        })
    }
    pub fn events(&self, id: &str, user_id: &str, after: u64, limit: usize) -> Result<Vec<Event>> {
        if !(1..=200).contains(&limit) || after > i64::MAX as u64 {
            return Err(Error::Invalid("event pagination out of bounds".into()));
        }
        self.db(|db| {
            let tx = db.transaction()?;
            read_session(&tx, id, user_id)?;
            let records = {
                let mut stmt = tx.prepare("SELECT id,session_id,task_id,request_id,payload,created_at FROM agent_events WHERE session_id=?1 AND id>?2 ORDER BY id LIMIT ?3")?;
                let rows = stmt.query_map(params![id,after,limit], read_event_row)?;
                rows.map(|row| decode_event(row?)).collect::<Result<Vec<_>>>()?
            };
            tx.commit()?; Ok(records)
        })
    }
    pub fn start_task(
        &self,
        id: &str,
        user_id: &str,
        expected_revision: u64,
        request_id: &str,
        request: TaskRequest,
    ) -> Result<Event> {
        let TaskRequest {
            goal,
            constraints,
            target,
        } = request;
        model::identifier(request_id)?;
        model::text_value(&goal, 16000)?;
        model::text_list(&constraints)?;
        if let Some(target) = &target {
            target.validate()?;
        }
        self.db(|db| {
            let tx = db.transaction()?;
            let session = read_session(&tx, id, user_id)?;
            if let Some(event) = event_by_request(&tx, id, request_id)? {
                if let Payload::TaskStarted { task } = &event.payload {
                    if task.goal == goal && task.constraints == constraints && task.target == target {
                        return Ok(event);
                    }
                }
                return Err(Error::Conflict);
            }
            ensure_revision(&session, expected_revision)?;
            let task = Task { id: uuid::Uuid::new_v4().to_string(), goal, constraints,
                target, status: TaskStatus::Active };
            let payload = Payload::TaskStarted { task: task.clone() };
            if serde_json::to_vec(&payload)?.len() > 65536 { return Err(Error::Invalid("task exceeds 64 KiB".into())); }
            let event = insert_event(&tx, id, Some(&task.id), request_id, &payload)?;
            let revision = next_revision(session.revision)?;
            if tx.execute("UPDATE agent_sessions SET task_json=?1,checkpoint_id=NULL,revision=?2 WHERE id=?3 AND revision=?4",
                params![serde_json::to_string(&task)?, revision,id,expected_revision])? != 1 {
                return Err(Error::Stale);
            }
            tx.commit()?; Ok(event)
        })
    }
    pub fn append(
        &self,
        id: &str,
        user_id: &str,
        expected_revision: u64,
        request_id: &str,
        payload: Payload,
    ) -> Result<AppendReceipt> {
        self.append_inner(id, user_id, expected_revision, request_id, payload, None)
    }
    /// Atomic bound-call admission. A replay is a receipt, never an execution permit.
    /// The executor must additionally verify live SSH identity and trusted authorization.
    pub fn append_tool_call(
        &self,
        id: &str,
        user_id: &str,
        expected_revision: u64,
        request_id: &str,
        binding: &ToolBinding,
        payload: Payload,
    ) -> Result<AppendReceipt> {
        binding.validate_call(&payload)?;
        self.append_inner(
            id,
            user_id,
            expected_revision,
            request_id,
            payload,
            Some(binding),
        )
    }
    /// Recover the immutable original target, not the currently selected task or UI tab.
    /// This is historical identity, not proof that a stopped task may execute again.
    pub fn tool_call_binding(&self, id: &str, user_id: &str, call_id: &str) -> Result<ToolBinding> {
        model::identifier(call_id)?;
        self.db(|db| {
            let tx = db.transaction()?;
            read_session(&tx, id, user_id)?;
            let task_id: String = tx.query_row(
                "SELECT task_id FROM agent_events WHERE session_id=?1 AND call_id=?2 AND kind='tool_call'",
                params![id, call_id], |row| row.get(0),
            ).optional()?.ok_or(Error::NotFound)?;
            let binding = recorded_binding(&tx, id, &task_id)?;
            tx.commit()?;
            Ok(binding)
        })
    }
    fn append_inner(
        &self,
        id: &str,
        user_id: &str,
        expected_revision: u64,
        request_id: &str,
        payload: Payload,
        binding: Option<&ToolBinding>,
    ) -> Result<AppendReceipt> {
        model::identifier(request_id)?;
        payload.validate()?;
        self.db(|db| {
            let tx = db.transaction()?;
            let session = read_session(&tx, id, user_id)?;
            if let Some(event) = event_by_request(&tx, id, request_id)? {
                if let Some(binding) = binding {
                    let task_id = event.task_id.as_deref().ok_or(Error::TargetMismatch)?;
                    if recorded_binding(&tx, id, task_id)? != *binding {
                        return Err(Error::TargetMismatch);
                    }
                }
                return if event.payload == payload { Ok(AppendReceipt { event, inserted: false }) } else { Err(Error::Conflict) };
            }
            ensure_revision(&session, expected_revision)?;
            if let Some(binding) = binding {
                let task = session.task.as_ref().ok_or(Error::TargetMismatch)?;
                if task.status != TaskStatus::Active || task.id != binding.task_id
                    || task.target.as_ref() != Some(&binding.target) {
                    return Err(Error::TargetMismatch);
                }
            }
            let mut task_id = session.task.as_ref().map(|task| task.id.clone());
            match &payload {
                Payload::ToolCall { call_id, .. } => {
                    let pending: usize = tx.query_row(
                        "SELECT COUNT(*) FROM agent_events AS c WHERE session_id=?1 AND kind='tool_call' AND NOT EXISTS(SELECT 1 FROM agent_events AS r WHERE r.session_id=c.session_id AND r.call_id=c.call_id AND r.kind='tool_result')",
                        [id], |row| row.get(0))?;
                    if pending >= 64 { return Err(Error::Invalid("too many unresolved tool calls".into())); }
                    if !session.task.as_ref().is_some_and(|task| task.status == TaskStatus::Active) {
                        return Err(Error::Invalid("an active task is required for tool calls".into()));
                    }
                    if tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_events WHERE session_id=?1 AND call_id=?2 AND kind='tool_call')",
                        params![id,call_id], |row| row.get::<_,bool>(0))? { return Err(Error::Conflict); }
                }
                Payload::ToolResult { call_id, outputs, .. } => {
                    super::output::validate_references(&tx, id, call_id, outputs)?;
                    let call_task: Option<Option<String>> = tx.query_row(
                        "SELECT task_id FROM agent_events WHERE session_id=?1 AND call_id=?2 AND kind='tool_call'",
                        params![id,call_id], |row| row.get(0)).optional()?;
                    task_id = call_task.ok_or(Error::ToolProtocol)?;
                    if tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_events WHERE session_id=?1 AND call_id=?2 AND kind='tool_result')",
                        params![id,call_id], |row| row.get::<_,bool>(0))? { return Err(Error::Conflict); }
                }
                _ => {}
            }
            let event = insert_event(&tx, id, task_id.as_deref(), request_id, &payload)?;
            super::output::link_references(&tx, &event)?;
            let mut task = session.task;
            if matches!(payload, Payload::Cancelled { .. }) {
                if let Some(task) = &mut task { task.status = TaskStatus::Cancelled; }
            }
            let task_json = task.as_ref().map(serde_json::to_string).transpose()?;
            if tx.execute("UPDATE agent_sessions SET task_json=?1,revision=?2 WHERE id=?3 AND revision=?4",
                params![task_json,next_revision(expected_revision)?,id,expected_revision])? != 1 {
                return Err(Error::Stale);
            }
            tx.commit()?; Ok(AppendReceipt { event, inserted: true })
        })
    }
    pub fn save_checkpoint(
        &self,
        snapshot: &Snapshot,
        content: CheckpointContent,
    ) -> Result<Checkpoint> {
        self.save_checkpoint_guarded(snapshot, snapshot.through_event_id, content, || false)
    }
    /// Prefix checkpoints cover only the stated frontier; the full snapshot still guards every change.
    pub(crate) fn save_checkpoint_guarded(
        &self,
        snapshot: &Snapshot,
        through: u64,
        content: CheckpointContent,
        cancelled: impl Fn() -> bool,
    ) -> Result<Checkpoint> {
        model::text_value(&content.summary, 16000)?;
        model::text_list(&content.next_steps)?;
        if content.evidence_event_ids.len() > 200
            || content.pending_tool_call_ids.len() > 200
            || content.uncertain_tool_call_ids.len() > 200
            || serde_json::to_vec(&content)?.len() > 65536
        {
            return Err(Error::Invalid("checkpoint exceeds size limits".into()));
        }
        self.db(|db| {
            let tx = db.transaction()?;
            let current = read_snapshot(&tx, &snapshot.session.id, &snapshot.session.user_id)?;
            if current != *snapshot { return Err(Error::Stale); }
            if cancelled() { return Err(Error::Conflict); }
            if (through==0 && current.through_event_id>0) || through>current.through_event_id
                || through>0 && !tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_events WHERE id=?1 AND session_id=?2)",
                    params![through,current.session.id],|r|r.get::<_,bool>(0))? {
                return Err(Error::Invalid("invalid checkpoint frontier".into()));
            }
            let old_through: u64 = if let Some(id)=&current.session.checkpoint_id {
                tx.query_row("SELECT through_event_id FROM agent_checkpoints WHERE id=?1 AND session_id=?2",
                    params![id,current.session.id],|r|r.get(0))?
            } else { 0 };
            if through<old_through { return Err(Error::Invalid("checkpoint frontier cannot move backwards".into())); }
            if through<current.through_event_id && tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_events c WHERE c.session_id=?1 AND c.kind='tool_call'
                 AND c.id<=?2 AND NOT EXISTS(SELECT 1 FROM agent_events r WHERE r.session_id=c.session_id
                 AND r.call_id=c.call_id AND r.kind='tool_result' AND r.id<=?2))",
                params![current.session.id,through],|r|r.get::<_,bool>(0))? {
                return Err(Error::ToolProtocol);
            }
            if content.task != current.session.task || content.pending_tool_call_ids != current.pending_tool_call_ids || content.uncertain_tool_call_ids != current.uncertain_tool_call_ids {
                return Err(Error::Invalid("checkpoint must preserve the task, constraints, target and uncertain calls".into()));
            }
            let mut seen = std::collections::HashSet::new();
            for &event_id in &content.evidence_event_ids {
                if event_id == 0 || event_id > through || !seen.insert(event_id) {
                    return Err(Error::Invalid("invalid evidence reference".into()));
                }
                if !tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_events WHERE id=?1 AND session_id=?2)",
                    params![event_id,current.session.id], |row| row.get::<_,bool>(0))? {
                    return Err(Error::Invalid("evidence belongs to another session".into()));
                }
            }
            let checkpoint = Checkpoint { id: uuid::Uuid::new_v4().to_string(),
                session_id: current.session.id.clone(), through_event_id: through,
                source_revision: current.session.revision,
                committed_revision: next_revision(current.session.revision)?, content,
                created_at: chrono::Utc::now().timestamp() };
            tx.execute("INSERT INTO agent_checkpoints(id,session_id,through_event_id,source_revision,committed_revision,content,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
                params![checkpoint.id,checkpoint.session_id,checkpoint.through_event_id,checkpoint.source_revision,
                    checkpoint.committed_revision,serde_json::to_string(&checkpoint.content)?,checkpoint.created_at])?;
            if tx.execute("UPDATE agent_sessions SET checkpoint_id=?1,revision=?2 WHERE id=?3 AND revision=?4",
                params![checkpoint.id,checkpoint.committed_revision,checkpoint.session_id,checkpoint.source_revision])? != 1 {
                return Err(Error::Stale);
            }
            if cancelled() { return Err(Error::Conflict); }
            tx.commit()?; Ok(checkpoint)
        })
    }
    pub fn checkpoint(&self, id: &str, user_id: &str) -> Result<Option<Checkpoint>> {
        self.db(|db| {
            let tx = db.transaction()?;
            let session = read_session(&tx,id,user_id)?;
            let result = if let Some(checkpoint_id) = session.checkpoint_id {
                let (through,source,committed,content,created): (u64,u64,u64,String,i64) = tx.query_row(
                    "SELECT through_event_id,source_revision,committed_revision,content,created_at FROM agent_checkpoints WHERE id=?1 AND session_id=?2",
                    params![checkpoint_id,id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)))?;
                Some(Checkpoint { id: checkpoint_id, session_id: id.into(), through_event_id: through,
                    source_revision: source, committed_revision: committed, content: serde_json::from_str(&content)?,
                    created_at: created })
            } else { None };
            tx.commit()?; Ok(result)
        })
    }
}
fn recorded_binding(db: &Connection, session_id: &str, task_id: &str) -> Result<ToolBinding> {
    let payload: String = db.query_row(
        "SELECT payload FROM agent_events WHERE session_id=?1 AND task_id=?2 AND kind='task_started'",
        params![session_id, task_id], |row| row.get(0),
    ).optional()?.ok_or(Error::TargetMismatch)?;
    let Payload::TaskStarted { task } = serde_json::from_str(&payload)? else {
        return Err(Error::ToolProtocol);
    };
    if task.id != task_id {
        return Err(Error::TargetMismatch);
    }
    Ok(ToolBinding {
        task_id: task.id,
        target: task.target.ok_or(Error::TargetMismatch)?,
    })
}
pub(super) fn read_session(db: &Connection, id: &str, user_id: &str) -> Result<Session> {
    let row: Option<(String,u64,Option<String>,Option<String>)> = db.query_row(
        "SELECT agent_id,revision,task_json,checkpoint_id FROM agent_sessions WHERE id=?1 AND user_id=?2",
        params![id,user_id], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional()?;
    let (agent_id, revision, task_json, checkpoint_id) = row.ok_or(Error::NotFound)?;
    Ok(Session {
        id: id.into(),
        user_id: user_id.into(),
        agent_id,
        revision,
        task: task_json.as_deref().map(serde_json::from_str).transpose()?,
        checkpoint_id,
    })
}
fn read_snapshot(db: &Connection, id: &str, user_id: &str) -> Result<Snapshot> {
    let session = read_session(db, id, user_id)?;
    let through_event_id = db.query_row(
        "SELECT COALESCE(MAX(id),0) FROM agent_events WHERE session_id=?1",
        [id],
        |row| row.get(0),
    )?;
    let mut statement = db.prepare("SELECT call_id FROM agent_events AS c WHERE session_id=?1 AND kind='tool_call' AND NOT EXISTS(SELECT 1 FROM agent_events AS r WHERE r.session_id=c.session_id AND r.call_id=c.call_id AND r.kind='tool_result') ORDER BY id")?;
    let pending_tool_call_ids = statement
        .query_map([id], |row| row.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    let mut statement = db.prepare("SELECT call_id FROM agent_events AS c WHERE session_id=?1 AND kind='tool_call' AND NOT EXISTS(SELECT 1 FROM agent_events AS r WHERE r.session_id=c.session_id AND r.call_id=c.call_id AND r.kind='tool_result' AND json_extract(r.payload,'$.outcome') NOT IN ('unknown','cancelled')) ORDER BY id")?;
    let uncertain_tool_call_ids = statement
        .query_map([id], |row| row.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(Snapshot {
        session,
        through_event_id,
        pending_tool_call_ids,
        uncertain_tool_call_ids,
    })
}
type EventRow = (u64, String, Option<String>, String, String, i64);
fn read_event_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<EventRow> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
    ))
}
fn decode_event(row: EventRow) -> Result<Event> {
    let (id, session_id, task_id, request_id, payload, created_at) = row;
    Ok(Event {
        id,
        session_id,
        task_id,
        request_id,
        payload: serde_json::from_str(&payload)?,
        created_at,
    })
}
fn event_by_request(db: &Connection, id: &str, request: &str) -> Result<Option<Event>> {
    db.query_row("SELECT id,session_id,task_id,request_id,payload,created_at FROM agent_events WHERE session_id=?1 AND request_id=?2",
        params![id,request],read_event_row).optional()?.map(decode_event).transpose()
}
pub(super) fn insert_event(
    db: &Connection,
    id: &str,
    task_id: Option<&str>,
    request_id: &str,
    payload: &Payload,
) -> Result<Event> {
    let created_at = chrono::Utc::now().timestamp();
    db.execute("INSERT INTO agent_events(session_id,task_id,request_id,kind,call_id,payload,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",
        params![id,task_id,request_id,payload.kind(),payload.call_id(),serde_json::to_string(payload)?,created_at])?;
    Ok(Event {
        id: db.last_insert_rowid() as u64,
        session_id: id.into(),
        task_id: task_id.map(str::to_string),
        request_id: request_id.into(),
        payload: payload.clone(),
        created_at,
    })
}
fn ensure_revision(session: &Session, expected: u64) -> Result<()> {
    if session.revision != expected {
        Err(Error::Stale)
    } else {
        Ok(())
    }
}
pub(super) fn next_revision(revision: u64) -> Result<u64> {
    revision
        .checked_add(1)
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or(Error::Conflict)
}
