//! Exact-turn input inbox. Acceptance never cancels a model request or authorizes a tool.
use super::{
    chat::{Error, Result},
    repository::{insert_event, next_revision, read_session},
    Payload, Repository, Snapshot,
};
use rusqlite::{params, OptionalExtension, TransactionBehavior};
use serde::Serialize;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    pub id: String,
    pub message: String,
    pub event_id: Option<u64>,
    pub created_at: i64,
}
fn storage(_: rusqlite::Error) -> Error {
    Error::Storage
}
pub(crate) fn list(db: &rusqlite::Connection, session: &str, request: &str) -> Result<Vec<Input>> {
    let mut q=db.prepare("SELECT id,message,event_id,created_at FROM agent_turn_inputs WHERE session_id=?1 AND request_id=?2 ORDER BY rowid LIMIT 32").map_err(storage)?;
    let values = q
        .query_map(params![session, request], |r| {
            Ok(Input {
                id: r.get(0)?,
                message: r.get(1)?,
                event_id: r.get(2)?,
                created_at: r.get(3)?,
            })
        })
        .map_err(storage)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage)?;
    Ok(values)
}
pub(crate) fn accept(
    repo: &Repository,
    agent: &str,
    user: &str,
    session: &str,
    request: &str,
    id: &str,
    message: &str,
) -> Result<Input> {
    super::model::identifier(id).map_err(|_| Error::Invalid)?;
    super::model::text_value(message, 16000).map_err(|_| Error::Invalid)?;
    if message.trim().is_empty() {
        return Err(Error::Invalid);
    }
    let mut db = repo.store.database.lock().map_err(|_| Error::Storage)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let s = read_session(&tx, session, user)?;
    if s.agent_id != agent {
        return Err(Error::NotFound);
    }
    let old: Option<(String, String)> = tx
        .query_row(
            "SELECT request_id,message FROM agent_turn_inputs WHERE session_id=?1 AND id=?2",
            params![session, id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(storage)?;
    if let Some((old_request, old_message)) = old {
        if old_request != request || old_message != message {
            return Err(Error::Conflict);
        }
        return list(&tx, session, request)?
            .into_iter()
            .find(|v| v.id == id)
            .ok_or(Error::Storage);
    }
    let now = chrono::Utc::now().timestamp_millis();
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_chat_turns t WHERE session_id=?1 AND request_id=?2 AND state='running' AND cancel_requested=0 AND lease_until>=?3 AND NOT EXISTS(SELECT 1 FROM agent_input_gates g WHERE g.session_id=t.session_id AND g.request_id=t.request_id))",params![session,request,now],|r|r.get(0)).map_err(storage)?;
    if !valid {
        return Err(Error::Busy);
    }
    let count: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM agent_turn_inputs WHERE session_id=?1 AND request_id=?2",
            params![session, request],
            |r| r.get(0),
        )
        .map_err(storage)?;
    if count >= 32 {
        return Err(Error::Invalid);
    }
    tx.execute("INSERT INTO agent_turn_inputs(session_id,request_id,id,message,created_at) VALUES(?1,?2,?3,?4,?5)",params![session,request,id,message,now]).map_err(storage)?;
    // Pending plans must be reconsidered. Already admitted remote processes keep their real outcome.
    tx.execute("UPDATE agent_execution_requests SET state='cancelled',updated_at=?3 WHERE session_id=?1 AND state IN ('pending','approved') AND call_id IN (SELECT call_id FROM agent_events WHERE session_id=?1 AND id>(SELECT user_event_id FROM agent_chat_turns WHERE session_id=?1 AND request_id=?2) AND kind='tool_call')",params![session,request,now]).map_err(storage)?;
    tx.execute("UPDATE agent_execution_grants SET revoked=1 WHERE session_id=?1 AND task_id=(SELECT task_id FROM agent_chat_turns WHERE session_id=?1 AND request_id=?2)",params![session,request]).map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(Input {
        id: id.into(),
        message: message.into(),
        event_id: None,
        created_at: now,
    })
}
/// Drain only at a complete model/tool boundary. Closing and accepting serialize in SQLite.
pub(crate) fn drain(
    repo: &Repository,
    request: &super::chat::Request,
    expected: u64,
    assistant: Option<&super::provider::Completion>,
    close: bool,
) -> Result<Vec<Input>> {
    let mut db = repo.store.database.lock().map_err(|_| Error::Storage)?;
    let tx = db
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage)?;
    let s = read_session(&tx, &request.session_id, &request.user_id)?;
    if s.agent_id != request.agent_id {
        return Err(Error::NotFound);
    }
    if s.revision != expected {
        return Err(Error::ContextChanged);
    }
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_chat_turns WHERE session_id=?1 AND request_id=?2 AND state='running' AND cancel_requested=0 AND lease_until>=?3)",params![s.id,request.request_id,chrono::Utc::now().timestamp_millis()],|r|r.get(0)).map_err(storage)?;
    if !valid {
        return Err(Error::Busy);
    }
    let mut inputs = list(&tx, &s.id, &request.request_id)?
        .into_iter()
        .filter(|v| v.event_id.is_none())
        .collect::<Vec<_>>();
    if inputs.is_empty() {
        if close {
            tx.execute(
                "INSERT OR IGNORE INTO agent_input_gates(session_id,request_id) VALUES(?1,?2)",
                params![s.id, request.request_id],
            )
            .map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        return Ok(inputs);
    }
    let task = s.task.as_ref().map(|t| t.id.as_str());
    let mut revision = s.revision;
    if let Some(completion) = assistant {
        if let Some(text) = completion
            .message
            .content
            .as_ref()
            .or(completion.message.refusal.as_ref())
            .filter(|t| !t.trim().is_empty())
        {
            let event = insert_event(
                &tx,
                &s.id,
                task,
                &format!("{}:step:{}", request.request_id, inputs[0].id),
                &Payload::AssistantMessage { text: text.clone() },
            )?;
            if let Some(window) = &completion.response_output {
                super::responses_store::save_turn(&tx, event.id, window)?;
            }
            revision = next_revision(revision)?;
        }
    }
    for input in &mut inputs {
        let event = insert_event(
            &tx,
            &s.id,
            task,
            &format!("{}:steer:{}", request.request_id, input.id),
            &Payload::UserMessage {
                text: input.message.clone(),
            },
        )?;
        input.event_id = Some(event.id);
        revision = next_revision(revision)?;
        tx.execute(
            "UPDATE agent_turn_inputs SET event_id=?3 WHERE session_id=?1 AND id=?2",
            params![s.id, input.id, event.id],
        )
        .map_err(storage)?;
    }
    tx.execute(
        "UPDATE agent_sessions SET revision=?2 WHERE id=?1",
        params![s.id, revision],
    )
    .map_err(storage)?;
    tx.execute(
        "UPDATE agent_chat_turns SET revision=?3 WHERE session_id=?1 AND request_id=?2",
        params![s.id, request.request_id, revision],
    )
    .map_err(storage)?;
    tx.commit().map_err(storage)?;
    Ok(inputs)
}
pub(crate) fn snapshot(repo: &Repository, request: &super::chat::Request) -> Result<Snapshot> {
    Ok(repo.snapshot(&request.session_id, &request.user_id)?)
}

/// The UI keeps the complete reply; each model step appears only once in prompt history.
pub(crate) fn final_text(
    db: &rusqlite::Connection,
    session: &str,
    request: &str,
    text: &str,
) -> Result<String> {
    let prefix = format!("{request}:step:");
    let mut q=db.prepare("SELECT payload FROM agent_events WHERE session_id=?1 AND substr(request_id,1,length(?2))=?2 ORDER BY id LIMIT 32").map_err(storage)?;
    let values = q
        .query_map(params![session, prefix], |r| r.get::<_, String>(0))
        .map_err(storage)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(storage)?;
    let mut before = String::new();
    for value in values {
        if let Payload::AssistantMessage { text } =
            serde_json::from_str(&value).map_err(|_| Error::Storage)?
        {
            before.push_str(&text);
            before.push_str("\n\n");
        }
    }
    Ok(text.strip_prefix(&before).unwrap_or(text).to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::{
            chat::Request,
            turns::{TurnState, Turns},
        },
        store::Store,
    };
    use std::sync::Arc;
    fn fixture() -> (Repository, Request, Turns) {
        let store = Arc::new(Store::in_memory().unwrap());
        let repo = Repository::new(store.clone());
        let session = repo.create_session("model", "user").unwrap();
        let request = Request {
            request_id: "turn".into(),
            agent_id: "model".into(),
            user_id: "user".into(),
            session_id: session.id,
            terminal_session_id: String::new(),
            message: "first".into(),
            attachments: vec![],
        };
        let turns = Turns::new(store);
        turns.begin(&request, None, "owner", 0).unwrap();
        (repo, request, turns)
    }
    #[test]
    fn steering_exact_turn_idempotency_and_owner_checks() {
        let (repo, r, _) = fixture();
        assert!(matches!(
            accept(&repo, "model", "wrong", &r.session_id, "turn", "a", "text"),
            Err(Error::NotFound)
        ));
        assert!(matches!(
            accept(&repo, "model", "user", &r.session_id, "other", "a", "text"),
            Err(Error::Busy)
        ));
        let a = accept(&repo, "model", "user", &r.session_id, "turn", "a", "text").unwrap();
        assert!(a.event_id.is_none());
        assert_eq!(repo.session(&r.session_id, "user").unwrap().revision, 2);
        accept(&repo, "model", "user", &r.session_id, "turn", "a", "text").unwrap();
        assert!(matches!(
            accept(
                &repo,
                "model",
                "user",
                &r.session_id,
                "turn",
                "a",
                "changed"
            ),
            Err(Error::Conflict)
        ));
    }
    #[test]
    fn steering_drains_once_before_closing_and_preserves_history() {
        let (repo, r, turns) = fixture();
        for id in ["a", "b"] {
            accept(&repo, "model", "user", &r.session_id, "turn", id, id).unwrap();
        }
        let inputs = drain(&repo, &r, 2, None, true).unwrap();
        assert_eq!(inputs.len(), 2);
        assert!(inputs.iter().all(|v| v.event_id.is_some()));
        let revision = repo.session(&r.session_id, "user").unwrap().revision;
        assert!(drain(&repo, &r, revision, None, true).unwrap().is_empty());
        assert!(matches!(
            accept(
                &repo,
                "model",
                "user",
                &r.session_id,
                "turn",
                "late",
                "late"
            ),
            Err(Error::Busy)
        ));
        assert!(
            accept(&repo, "model", "user", &r.session_id, "turn", "a", "a")
                .unwrap()
                .event_id
                .is_some()
        );
        let turn = turns
            .finish(&r, "owner", TurnState::Completed, "answer", None, None)
            .unwrap();
        assert_eq!(turn.state, TurnState::Completed);
        assert_eq!(
            turns
                .history("model", "user", &r.session_id, 0, 20)
                .unwrap()
                .turns[0]
                .inputs
                .len(),
            2
        );
    }
    #[test]
    fn steering_stop_and_changed_revision_never_apply_pending_input() {
        let (repo, r, turns) = fixture();
        accept(&repo, "model", "user", &r.session_id, "turn", "a", "text").unwrap();
        assert!(matches!(
            drain(&repo, &r, 0, None, false),
            Err(Error::ContextChanged)
        ));
        turns
            .stop("model", "user", &r.session_id, Some("turn"))
            .unwrap();
        assert!(matches!(drain(&repo, &r, 2, None, false), Err(Error::Busy)));
        assert!(matches!(
            accept(&repo, "model", "user", &r.session_id, "turn", "b", "text"),
            Err(Error::Busy)
        ));
        assert_eq!(repo.session(&r.session_id, "user").unwrap().revision, 2);
    }
    #[test]
    fn steering_limits_empty_input_and_inbox_growth() {
        let (repo, r, _) = fixture();
        assert!(accept(&repo, "model", "user", &r.session_id, "turn", "empty", " ").is_err());
        for n in 0..32 {
            accept(
                &repo,
                "model",
                "user",
                &r.session_id,
                "turn",
                &format!("i{n}"),
                "text",
            )
            .unwrap();
        }
        assert!(matches!(
            accept(
                &repo,
                "model",
                "user",
                &r.session_id,
                "turn",
                "overflow",
                "text"
            ),
            Err(Error::Invalid)
        ));
    }
}
