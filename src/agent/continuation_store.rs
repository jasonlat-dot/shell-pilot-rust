//! Journal-aligned native protocol state. User/model payloads cannot install it.
use super::*;
use rusqlite::{params, Connection};

pub(crate) struct Stored {
    pub after: u64,
    pub through: u64,
    pub state: provider::Continuation,
}
/// A checkpoint cannot divide one provider-owned assistant response.
pub(crate) fn require_frontier(db: &Connection, session: &str, through: u64) -> Result<()> {
    let splits: bool = db.query_row("SELECT EXISTS(SELECT 1 FROM agent_provider_continuations WHERE session_id=?1 AND after_event_id<?2 AND through_event_id>?2)", params![session,through], |row| row.get(0))?;
    if splits { return Err(Error::ToolProtocol); }
    Ok(())
}
pub(crate) fn save(
    db: &Connection,
    session: &str,
    after: u64,
    through: u64,
    state: &provider::Continuation,
) -> Result<()> {
    let value = serde_json::to_string(state)?;
    if value.len() > 8 * 1024 * 1024 || after >= through {
        return Err(Error::ToolProtocol);
    }
    db.execute("INSERT INTO agent_provider_continuations(session_id,after_event_id,through_event_id,state) VALUES(?1,?2,?3,?4)",params![session,after,through,value])?;
    Ok(())
}
pub(crate) fn load(
    store: &crate::store::Store,
    session: &Session,
    after: u64,
    binding: &provider::responses::Binding,
) -> Result<Vec<Stored>> {
    let db = store.database.lock().map_err(|_| Error::Worker)?;
    repository::read_session(&db, &session.id, &session.user_id)?;
    require_frontier(&db, &session.id, after)?;
    let mut query = db.prepare("SELECT after_event_id,through_event_id,state FROM agent_provider_continuations WHERE session_id=?1 AND after_event_id>=?2 ORDER BY through_event_id LIMIT 256")?;
    let rows = query.query_map(params![session.id, after], |row| {
        Ok((
            row.get::<_, u64>(0)?,
            row.get::<_, u64>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut result = vec![];
    for row in rows {
        let (after, through, json) = row?;
        let state: provider::Continuation = serde_json::from_str(&json)?;
        if state.validate_binding(binding).is_ok() {
            result.push(Stored {
                after,
                through,
                state,
            });
        }
    }
    Ok(result)
}
