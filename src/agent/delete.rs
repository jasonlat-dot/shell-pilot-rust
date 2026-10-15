//! Explicit conversation deletion, serialized with turn admission and scoped by owner/model.
use super::*;
use rusqlite::{params, Connection, TransactionBehavior};

fn ensure_chat_idle(db: &Connection, user: &str, id: &str) -> Result<()> {
    repository::read_session(db, id, user)?;
    let running:bool=db.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_chat_turns WHERE state='running' AND
         (session_id=?1 OR session_id IN (SELECT session_id FROM agent_session_scopes WHERE root_id=?1)))
         OR EXISTS(SELECT 1 FROM agent_execution_requests WHERE session_id=?1 AND state='admitted')
         OR EXISTS(SELECT 1 FROM agent_outputs WHERE session_id=?1 AND state='writing')",
        [id],|row|row.get(0))?;
    if running {
        return Err(Error::Stale);
    }
    Ok(())
}

fn delete_chat_data(db: &Connection, user: &str, id: &str) -> Result<()> {
    ensure_chat_idle(db, user, id)?;
    db.execute(
        "UPDATE agent_session_scopes SET root_id=session_id WHERE root_id=?1 AND session_id<>?1",
        [id],
    )?;
    for sql in [
        "DELETE FROM agent_execution_grants WHERE session_id=?1",
        "DELETE FROM agent_execution_policy_reviews WHERE request_id IN (SELECT id FROM agent_execution_requests WHERE session_id=?1)",
        "DELETE FROM agent_execution_requests WHERE session_id=?1",
        "DELETE FROM agent_execution_policies WHERE session_id=?1",
        "DELETE FROM agent_turn_inputs WHERE session_id=?1",
        "DELETE FROM agent_input_gates WHERE session_id=?1",
        "DELETE FROM agent_tool_turns WHERE session_id=?1",
        "DELETE FROM agent_chat_turns WHERE session_id=?1",
        "DELETE FROM agent_context_status WHERE session_id=?1",
        "DELETE FROM agent_responses_outputs WHERE event_id IN (SELECT id FROM agent_events WHERE session_id=?1)",
        "DELETE FROM agent_responses_checkpoints WHERE checkpoint_id IN (SELECT id FROM agent_checkpoints WHERE session_id=?1)",
        "DELETE FROM agent_checkpoints WHERE session_id=?1",
        "DELETE FROM agent_output_chunks WHERE output_id IN (SELECT id FROM agent_outputs WHERE session_id=?1)",
        "DELETE FROM agent_outputs WHERE session_id=?1",
        "DELETE FROM agent_events WHERE session_id=?1",
        "DELETE FROM agent_project_chats WHERE session_id=?1",
        "DELETE FROM agent_session_scopes WHERE session_id=?1",
    ] { db.execute(sql,[id])?; }
    db.execute(
        "DELETE FROM agent_sessions WHERE id=?1 AND user_id=?2",
        params![id, user],
    )?;
    Ok(())
}

fn project_chat_ids(db: &Connection, user: &str, project: &str) -> Result<Vec<String>> {
    let owned: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM agent_projects WHERE id=?1 AND user_id=?2 AND NOT EXISTS(SELECT 1 FROM agent_removed_projects WHERE project_id=?1))",
        params![project, user],
        |row| row.get(0),
    )?;
    if !owned {
        return Err(Error::NotFound);
    }
    let mut q=db.prepare("SELECT a.id FROM agent_sessions a JOIN agent_project_chats p ON p.session_id=a.id WHERE p.project_id=?1 AND a.user_id=?2 ORDER BY a.rowid")?;
    let ids = q
        .query_map(params![project, user], |row| row.get(0))?
        .collect::<std::result::Result<Vec<String>, _>>()?;
    Ok(ids)
}

impl Repository {
    pub(crate) fn project_chat_ids(&self, user: &str, project: &str) -> Result<Vec<String>> {
        self.db(|db| project_chat_ids(db, user, project))
    }
    pub fn delete_project(&self, user: &str, project: &str) -> Result<()> {
        model::identifier(user)?;
        model::identifier(project)?;
        self.db(|db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let ids = project_chat_ids(&tx, user, project)?;
            // Removing a sidebar group preserves every conversation and its execution scope.
            for id in ids {
                ensure_chat_idle(&tx, user, &id)?;
            }
            tx.execute(
                "INSERT INTO agent_removed_projects(project_id,removed_at) VALUES(?1,?2)",
                params![project, chrono::Utc::now().timestamp()],
            )?;
            tx.commit()?;
            Ok(())
        })
    }
    pub fn delete_chat(&self, agent: &str, user: &str, id: &str) -> Result<()> {
        model::identifier(agent)?;
        model::identifier(user)?;
        model::identifier(id)?;
        self.db(|db| {
            let tx = db.transaction_with_behavior(TransactionBehavior::Immediate)?;
            let session = repository::read_session(&tx, id, user)?;
            if session.agent_id != agent {
                return Err(Error::NotFound);
            }
            delete_chat_data(&tx, user, id)?;
            tx.commit()?;
            Ok(())
        })
    }
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
    use rusqlite::OptionalExtension;
    use std::sync::Arc;

    #[test]
    fn project_removal_preserves_every_page_and_rejects_busy_chats() {
        let store = Arc::new(Store::in_memory().unwrap());
        let connection = store
            .save(
                crate::model::ConnectionRequest {
                    connection_id: Some("server".into()),
                    connection_name: "服务器".into(),
                    host: "localhost".into(),
                    port: 22,
                    username: "user".into(),
                    user_id: Some("user".into()),
                    strict_host_key_check: Some(false),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        let repo = Repository::new(store.clone());
        let project = repo
            .create_project("user", "待移除项目", &connection.connection_id)
            .unwrap();
        let other = repo.create_general_session("model", "user").unwrap();
        let mut ids = Vec::new();
        for i in 0..55 {
            let chat = repo
                .create_project_chat(
                    &project.id,
                    "user",
                    if i % 2 == 0 { "model-a" } else { "model-b" },
                    &Target {
                        connection_id: "server".into(),
                        terminal_session_id: "terminal".into(),
                    },
                    ServerIdentity {
                        host: "localhost".into(),
                        port: 22,
                        username: "user".into(),
                        host_key: "key".into(),
                    },
                )
                .unwrap();
            if i < 54 {
                repo.append(
                    &chat.id,
                    "user",
                    0,
                    "message",
                    Payload::UserMessage {
                        text: "项目对话".into(),
                    },
                )
                .unwrap();
            }
            ids.push(chat.id);
        }
        assert_eq!(
            repo.project_chats("user", Some(&project.id), "")
                .unwrap()
                .items
                .len(),
            50
        );
        let r = Request {
            request_id: "busy-turn".into(),
            agent_id: "model-a".into(),
            user_id: "user".into(),
            session_id: ids[54].clone(),
            terminal_session_id: String::new(),
            message: "处理中".into(),
            attachments: vec![],
        };
        let turns = Turns::new(store.clone());
        turns.begin(&r, None, "owner", 0).unwrap();
        repo.db(|db| {
            db.execute_batch("PRAGMA foreign_keys=ON")?;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            repo.delete_project("other-user", &project.id),
            Err(Error::NotFound)
        ));
        assert!(matches!(
            repo.delete_project("user", &project.id),
            Err(Error::Stale)
        ));
        for id in &ids {
            assert!(repo.session(id, "user").is_ok());
        }
        repo.db(|db| {
            db.execute("INSERT INTO agent_execution_requests(id,session_id,user_id,task_id,call_id,request_json,state,result_json,created_at,expires_at,updated_at) VALUES('kept-result',?1,'user','task','call','{}','completed','{\"exitCode\":0}',0,0,0)",[&ids[0]])?;
            db.execute("INSERT INTO agent_outputs(id,session_id,task_id,call_id,channel,request_id,writer_token,state,size,max_bytes,expires_at,created_at) VALUES('kept-output',?1,'task','call','stdout','output-request','writer','complete',3,1024,100,0)",[&ids[0]])?;
            db.execute("INSERT INTO agent_output_chunks VALUES('kept-output',0,x'616263',?1)",["a".repeat(64)])?;
            Ok(())
        }).unwrap();
        turns
            .finish(&r, "owner", TurnState::Completed, "结束", None, None)
            .unwrap();
        repo.delete_project("user", &project.id).unwrap();
        assert!(repo.projects("user").unwrap().is_empty());
        for id in &ids {
            let session = repo.session(id, "user").unwrap();
            assert_eq!(
                session.scope.unwrap().origin_terminal_id.as_deref(),
                Some("terminal")
            );
        }
        let first = repo.project_chats("user", None, "").unwrap();
        assert_eq!(first.items.len(), 50);
        let last = repo
            .project_chats("user", None, first.next_after.as_deref().unwrap())
            .unwrap();
        assert_eq!(last.items.len(), 5);
        assert_eq!(repo.events(&ids[0], "user", 0, 200).unwrap().len(), 1);
        repo.db(|db| {
            let result: String = db.query_row(
                "SELECT result_json FROM agent_execution_requests WHERE id='kept-result'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(result, "{\"exitCode\":0}");
            let bytes: Vec<u8> = db.query_row(
                "SELECT data FROM agent_output_chunks WHERE output_id='kept-output'",
                [],
                |r| r.get(0),
            )?;
            assert_eq!(bytes, b"abc");
            Ok(())
        })
        .unwrap();
        assert!(repo.session(&other.id, "user").is_ok());
        assert!(store.get("server").is_ok());
        assert!(repo.project_chats("user", Some(&project.id), "").is_err());
    }

    #[test]
    fn deletion_checks_owner_running_state_and_removes_all_conversation_data() {
        let store = Arc::new(Store::in_memory().unwrap());
        let repo = Repository::new(store.clone());
        let session = repo.create_general_session("model", "user").unwrap();
        let other = repo.create_general_session("model", "user").unwrap();
        repo.db(|db| {
            db.execute_batch("PRAGMA foreign_keys=ON")?;
            Ok(())
        })
        .unwrap();
        let r = Request {
            request_id: "turn".into(),
            agent_id: "model".into(),
            user_id: "user".into(),
            session_id: session.id.clone(),
            terminal_session_id: String::new(),
            message: "需要删除的内容".into(),
            attachments: vec![],
        };
        let turns = Turns::new(store);
        turns.begin(&r, None, "owner", 0).unwrap();
        assert!(repo.project_chats("user", None, "").unwrap().items[0].running);
        assert!(matches!(
            repo.delete_chat("model", "other", &session.id),
            Err(Error::NotFound)
        ));
        assert!(matches!(
            repo.delete_chat("wrong-model", "user", &session.id),
            Err(Error::NotFound)
        ));
        assert!(matches!(
            repo.delete_chat("model", "user", &session.id),
            Err(Error::Stale)
        ));
        assert!(!repo.events(&session.id, "user", 0, 200).unwrap().is_empty());
        turns
            .finish(&r, "owner", TurnState::Completed, "完成", None, None)
            .unwrap();
        repo.db(|db|{
            let id=&session.id;
            db.execute("INSERT INTO agent_turn_inputs VALUES(?1,'turn','input','补充',NULL,1)",[id])?;
            db.execute("INSERT INTO agent_input_gates VALUES(?1,'turn')",[id])?;
            db.execute("INSERT INTO agent_tool_turns VALUES(?1,'turn')",[id])?;
            db.execute("INSERT INTO agent_context_status VALUES(?1,'owner','{}',0)",[id])?;
            db.execute("INSERT INTO agent_execution_requests(id,session_id,user_id,task_id,call_id,request_json,state,created_at,expires_at,updated_at) VALUES('approval',?1,'user','task','call','{}','completed',0,0,0)",[id])?;
            db.execute("INSERT INTO agent_execution_grants VALUES('approval',?1,'task','{}','{}',0,0)",[id])?;
            db.execute("INSERT INTO agent_execution_policy_reviews VALUES('approval','auto',1,'allow','low','test')",[])?;
            db.execute("INSERT INTO agent_execution_policies VALUES(?1,'task','confirm',1)",[id])?;
            db.execute("INSERT INTO agent_checkpoints VALUES('checkpoint',?1,0,0,1,'{}',0)",[id])?;
            db.execute("INSERT INTO agent_responses_checkpoints VALUES('checkpoint','{}','{}')",[])?;
            db.execute("INSERT INTO agent_responses_outputs SELECT id,'{}','{}' FROM agent_events WHERE session_id=?1",[id])?;
            db.execute("INSERT INTO agent_outputs(id,session_id,task_id,call_id,channel,request_id,writer_token,state,size,max_bytes,expires_at,created_at) VALUES('output',?1,'task','call','stdout','output-request','writer','complete',3,1024,100,0)",[id])?;
            db.execute("INSERT INTO agent_output_chunks VALUES('output',0,x'616263',?1)",["a".repeat(64)])?;
            Ok(())
        }).unwrap();
        repo.delete_chat("model", "user", &session.id).unwrap();
        assert!(matches!(
            repo.session(&session.id, "user"),
            Err(Error::NotFound)
        ));
        assert!(repo.session(&other.id, "user").is_ok());
        repo.db(|db| {
            for table in [
                "agent_events",
                "agent_chat_turns",
                "agent_turn_inputs",
                "agent_input_gates",
                "agent_tool_turns",
                "agent_context_status",
                "agent_execution_requests",
                "agent_execution_grants",
                "agent_execution_policy_reviews",
                "agent_execution_policies",
                "agent_checkpoints",
                "agent_responses_checkpoints",
                "agent_responses_outputs",
                "agent_outputs",
                "agent_output_chunks",
            ] {
                let count: u64 =
                    db.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?;
                assert_eq!(count, 0, "{table} retained conversation data");
            }
            assert!(db
                .query_row("PRAGMA foreign_key_check", [], |_| Ok(()))
                .optional()?
                .is_none());
            Ok(())
        })
        .unwrap();
    }

    #[test]
    fn deleting_legacy_root_retains_independent_server_history() {
        let repo = Repository::new(Arc::new(Store::in_memory().unwrap()));
        let root = repo.create_general_session("model", "user").unwrap();
        let child = repo
            .server_scope(
                "model",
                "user",
                &root.id,
                &Target {
                    connection_id: "server".into(),
                    terminal_session_id: "terminal".into(),
                },
                ServerIdentity {
                    host: "host".into(),
                    port: 22,
                    username: "user".into(),
                    host_key: "key".into(),
                },
                true,
            )
            .unwrap();
        repo.append(
            &child.id,
            "user",
            0,
            "message",
            Payload::UserMessage {
                text: "保留这条独立对话".into(),
            },
        )
        .unwrap();
        repo.delete_chat("model", "user", &root.id).unwrap();
        assert_eq!(
            repo.session(&child.id, "user")
                .unwrap()
                .scope
                .unwrap()
                .root_id,
            child.id
        );
        assert_eq!(repo.events(&child.id, "user", 0, 200).unwrap().len(), 1);
    }
}
