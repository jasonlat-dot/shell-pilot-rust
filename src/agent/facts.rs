//! Provenance checks for bounded user facts. Quotes remain untrusted historical data.
use super::{model::text_value, Error, Payload, Result, RetainedUserFact};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashSet;

pub(crate) const MAX_FACTS: usize = 32;
pub(crate) const MAX_QUOTE_BYTES: usize = 512;

pub(crate) fn validate(
    db: &Connection,
    session: &str,
    through: u64,
    facts: &[RetainedUserFact],
) -> Result<()> {
    if facts.len() > MAX_FACTS {
        return Err(Error::Invalid("too many retained user facts".into()));
    }
    let mut seen = HashSet::new();
    for fact in facts {
        text_value(&fact.quote, MAX_QUOTE_BYTES)?;
        if fact.event_id == 0
            || fact.event_id > through
            || !seen.insert((fact.event_id, fact.quote.as_str()))
        {
            return Err(Error::Invalid("invalid retained fact reference".into()));
        }
        let payload: Option<String> = db.query_row(
            "SELECT payload FROM agent_events WHERE session_id=?1 AND id=?2 AND kind='user_message'",
            params![session, fact.event_id], |row| row.get(0),
        ).optional()?;
        let Some(payload) = payload else {
            return Err(Error::Invalid(
                "retained fact must cite this session's user evidence".into(),
            ));
        };
        match serde_json::from_str::<Payload>(&payload)? {
            Payload::UserMessage { text } if text.contains(&fact.quote) => {}
            _ => {
                return Err(Error::Invalid(
                    "retained fact is not a verbatim user quote".into(),
                ))
            }
        }
    }
    Ok(())
}
