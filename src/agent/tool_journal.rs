//! Atomic raw-call batches shared by the native D4 runtime and A5 output storage.
use super::repository::{insert_event, next_revision, read_session};
use super::*;
use rusqlite::params;

impl Repository {
    pub(crate) fn append_tool_batch(
        &self,
        snapshot: &Snapshot,
        binding: &ToolBinding,
        calls: Vec<Payload>,
        text: Option<String>,
    ) -> Result<()> {
        if calls.is_empty() || calls.len() > 64 {
            return Err(Error::ToolProtocol);
        }
        if let Some(text) = &text {
            Payload::AssistantMessage { text: text.clone() }.validate()?;
        }
        let mut ids = std::collections::HashSet::new();
        for payload in &calls {
            payload.validate()?;
            binding.validate_call(payload)?;
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
                || ToolBinding::from_snapshot(snapshot)? != *binding
            {
                return Err(Error::TargetMismatch);
            }
            let pending: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM agent_events c WHERE c.session_id=?1 AND c.kind='tool_call'
                 AND NOT EXISTS(SELECT 1 FROM agent_events r WHERE r.session_id=c.session_id
                 AND r.call_id=c.call_id AND r.kind='tool_result'))",
                [id], |r| r.get(0))?;
            if pending { return Err(Error::ToolProtocol); }
            let mut revision = session.revision;
            if let Some(text) = &text {
                insert_event(&tx, id, Some(&binding.task_id), &uuid::Uuid::new_v4().to_string(),
                    &Payload::AssistantMessage { text: text.clone() })?;
                revision = next_revision(revision)?;
            }
            for payload in &calls {
                if tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_events WHERE session_id=?1 AND call_id=?2 AND kind='tool_call')",
                    params![id,payload.call_id()], |r| r.get::<_,bool>(0))? {
                    return Err(Error::Conflict);
                }
                insert_event(&tx, id, Some(&binding.task_id), &uuid::Uuid::new_v4().to_string(), payload)?;
                revision = next_revision(revision)?;
            }
            tx.execute("UPDATE agent_sessions SET revision=?2 WHERE id=?1", params![id,revision])?;
            tx.commit()?;
            Ok(())
        })
    }
}
