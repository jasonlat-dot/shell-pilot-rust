//! Native provider windows are committed with their checkpoint or completed assistant event.
use super::{
    provider::responses::{Binding, ResponseWindow, MAX_WINDOW_BYTES},
    Error, Repository, Result,
};
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

fn encode(window: &ResponseWindow) -> Result<(String, String)> {
    window
        .validate_binding(&window.binding)
        .map_err(|_| Error::Invalid("invalid Responses window".into()))?;
    let binding = serde_json::to_string(&window.binding)?;
    let output = serde_json::to_string(&window.items)?;
    if output.len() > MAX_WINDOW_BYTES {
        return Err(Error::Invalid("Responses window exceeds 2 MiB".into()));
    }
    Ok((binding, output))
}
fn decode(binding: String, output: String) -> Result<ResponseWindow> {
    if binding.len() > 8192 || output.len() > MAX_WINDOW_BYTES {
        return Err(Error::Invalid(
            "stored Responses window exceeds limits".into(),
        ));
    }
    let window = ResponseWindow {
        binding: serde_json::from_str(&binding)?,
        items: serde_json::from_str(&output)?,
    };
    window
        .validate_binding(&window.binding)
        .map_err(|_| Error::Invalid("invalid stored Responses window".into()))?;
    Ok(window)
}
pub(crate) fn save_checkpoint(db: &Connection, id: &str, window: &ResponseWindow) -> Result<()> {
    let (binding, output) = encode(window)?;
    db.execute(
        "INSERT INTO agent_responses_checkpoints(checkpoint_id,binding,output) VALUES(?1,?2,?3)",
        params![id, binding, output],
    )?;
    Ok(())
}
pub(crate) fn save_turn(db: &Connection, event: u64, window: &ResponseWindow) -> Result<()> {
    let (binding, output) = encode(window)?;
    db.execute(
        "INSERT INTO agent_responses_outputs(event_id,binding,output) VALUES(?1,?2,?3)",
        params![event, binding, output],
    )?;
    Ok(())
}
pub(crate) type Windows = (Option<ResponseWindow>, HashMap<u64, ResponseWindow>);
pub(crate) fn load(
    repo: &Repository,
    session: &str,
    user: &str,
    after: u64,
    through: u64,
) -> Result<Windows> {
    repo.db(|db| {
        let checkpoint: Option<(String, String)> = db
            .query_row(
                "SELECT c.binding,c.output FROM agent_responses_checkpoints c JOIN agent_sessions s
             ON s.checkpoint_id=c.checkpoint_id WHERE s.id=?1 AND s.user_id=?2",
                params![session, user],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let mut total = checkpoint.as_ref().map_or(0, |(_, s)| s.len());
        let checkpoint = checkpoint.map(|(b, o)| decode(b, o)).transpose()?;
        let mut statement = db.prepare(
            "SELECT o.event_id,o.binding,o.output FROM agent_responses_outputs o
             JOIN agent_events e ON e.id=o.event_id JOIN agent_sessions s ON s.id=e.session_id
             WHERE s.id=?1 AND s.user_id=?2 AND e.id>?3 AND e.id<=?4 ORDER BY e.id LIMIT 257",
        )?;
        let rows = statement.query_map(params![session, user, after, through], |r| {
            Ok((
                r.get::<_, u64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut outputs = HashMap::new();
        for row in rows {
            let (id, binding, output) = row?;
            total = total.saturating_add(output.len());
            if total > 8 * 1024 * 1024 || outputs.len() >= 256 {
                return Err(Error::Invalid(
                    "Responses history exceeds bounded native read".into(),
                ));
            }
            outputs.insert(id, decode(binding, output)?);
        }
        Ok((checkpoint, outputs))
    })
}
pub(crate) fn validate(windows: &Windows, binding: &Binding) -> super::provider::Result<()> {
    for window in windows.0.iter().chain(windows.1.values()) {
        window.validate_binding(binding)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::{
            provider::{ModelConfig, RuntimeConfig},
            CheckpointContent, Payload,
        },
        store::Store,
    };
    use serde_json::json;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    #[test]
    fn remote_window_and_checkpoint_roll_back_together_and_remain_owner_scoped() {
        let store = Arc::new(Store::in_memory().unwrap());
        let repo = Repository::new(store.clone());
        let session = repo.create_session("model", "owner").unwrap();
        repo.append(
            &session.id,
            "owner",
            0,
            "user",
            Payload::UserMessage {
                text: "original".into(),
            },
        )
        .unwrap();
        let event = repo
            .append(
                &session.id,
                "owner",
                1,
                "assistant",
                Payload::AssistantMessage {
                    text: "answer".into(),
                },
            )
            .unwrap()
            .event;
        let snap = repo.snapshot(&session.id, "owner").unwrap();
        let config:ModelConfig=serde_json::from_value(json!({"provider":"openAiResponses","remoteCompaction":true,
            "baseUrl":"https://example.test/v1/","completionsPath":"responses","model":"test","contextWindow":8192,
            "outputReserve":1024,"safetyReserve":512,"compactPercent":75,"outputTokenParameter":"maxCompletionTokens",
            "capabilities":{"toolCalls":false,"developerRole":false,"temperature":false,"reasoningEffort":false,"vision":false},
            "temperature":null,"reasoningEffort":null,"timeoutSeconds":3,"maxResponseBytes":65536,"requireApiKey":false})).unwrap();
        let window = ResponseWindow {
            binding: Binding::from_runtime(&RuntimeConfig {
                config,
                api_key: None,
            })
            .unwrap(),
            items: vec![
                json!({"type":"message","role":"user","content":"retained"}),
                json!({"type":"compaction","encrypted_content":"opaque","metadata":{"keep":true}}),
            ],
        };
        let content = CheckpointContent {
            task: None,
            summary: "native reference".into(),
            next_steps: vec![],
            evidence_event_ids: vec![event.id],
            pending_tool_call_ids: vec![],
            uncertain_tool_call_ids: vec![],
        };
        let checks = AtomicUsize::new(0);
        assert!(repo
            .save_checkpoint_guarded_remote(&snap, event.id, content.clone(), Some(&window), || {
                checks.fetch_add(1, Ordering::SeqCst) > 0
            })
            .is_err());
        assert_eq!(repo.snapshot(&session.id, "owner").unwrap(), snap);
        assert_eq!(
            store
                .database
                .lock()
                .unwrap()
                .query_row(
                    "SELECT COUNT(*) FROM agent_responses_checkpoints",
                    [],
                    |r| r.get::<_, u64>(0)
                )
                .unwrap(),
            0
        );
        repo.save_checkpoint_guarded_remote(&snap, event.id, content, Some(&window), || false)
            .unwrap();
        let owned = load(&repo, &session.id, "owner", event.id, event.id)
            .unwrap()
            .0
            .unwrap();
        assert_eq!(owned.items, window.items);
        assert!(load(&repo, &session.id, "another", event.id, event.id)
            .unwrap()
            .0
            .is_none());
    }
}
