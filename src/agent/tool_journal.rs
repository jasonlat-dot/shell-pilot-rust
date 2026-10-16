//! Atomic raw-call batches shared by the native D4 runtime and A5 output storage.
use super::repository::{insert_event, next_revision, read_session};
use super::*;
use rusqlite::params;

impl Repository {
    pub(crate) fn append_runtime_batch(&self, snapshot: &Snapshot, binding: Option<&ToolBinding>, calls: Vec<Payload>, text: Option<String>, parent: Option<&str>) -> Result<()> {
        let task = snapshot.session.task.as_ref().filter(|t| t.status == TaskStatus::Active).ok_or(Error::TargetMismatch)?;
        if calls.is_empty() || calls.len() > 64 {
            return Err(Error::ToolProtocol);
        }
        if let Some(text) = &text {
            Payload::AssistantMessage { text: text.clone() }.validate()?;
        }
        let mut ids = std::collections::HashSet::new();
        for payload in &calls {
            payload.validate()?;
            if let Some(binding) = binding { binding.validate_call(payload)?; }
            if !matches!(payload, Payload::ToolCall { .. }) { return Err(Error::ToolProtocol); }
            if !ids.insert(payload.call_id().ok_or(Error::ToolProtocol)?) {
                return Err(Error::ToolProtocol);
            }
        }
        self.db(|db| {
            let tx = db.transaction()?;
            let id = &snapshot.session.id;
            let session = read_session(&tx, id, &snapshot.session.user_id)?;
            if session.revision != snapshot.session.revision { return Err(Error::Stale); }
            if session.task != snapshot.session.task
                || binding.is_some_and(|b| ToolBinding::from_snapshot(snapshot).ok().as_ref() != Some(b))
            {
                return Err(Error::TargetMismatch);
            }
            let pending: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_events c WHERE c.session_id=?1 AND c.kind='tool_call'
                 AND NOT EXISTS(SELECT 1 FROM agent_events r WHERE r.session_id=c.session_id
                 AND r.call_id=c.call_id AND r.kind='tool_result'))",
                [id], |r| r.get(0))?;
            if pending && parent.is_none() { return Err(Error::ToolProtocol); }
            if let Some(parent) = parent {
                let valid: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_events c WHERE session_id=?1 AND call_id=?2 AND kind='tool_call' AND NOT EXISTS(SELECT 1 FROM agent_events r WHERE r.session_id=c.session_id AND r.call_id=c.call_id AND r.kind='tool_result'))", params![id,parent], |r|r.get(0))?;
                if !valid { return Err(Error::ToolProtocol); }
            }
            let mut revision = session.revision;
            if let Some(text) = &text {
                insert_event(&tx, id, Some(&task.id), &uuid::Uuid::new_v4().to_string(),
                    &Payload::AssistantMessage { text: text.clone() })?;
                revision = next_revision(revision)?;
            }
            for payload in &calls {
                if tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_events WHERE session_id=?1 AND call_id=?2 AND kind='tool_call')",
                    params![id,payload.call_id()], |r| r.get::<_,bool>(0))? {
                    return Err(Error::Conflict);
                }
                insert_event(&tx, id, Some(&task.id), &uuid::Uuid::new_v4().to_string(), payload)?;
                revision = next_revision(revision)?;
            }
            tx.execute("UPDATE agent_sessions SET revision=?2 WHERE id=?1", params![id,revision])?;
            tx.commit()?;
            Ok(())
        })
    }
}
