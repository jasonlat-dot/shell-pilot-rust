//! Persistent project membership does not alter conversation logs or permissions.
use super::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub id: String,
    pub name: String,
    pub connection_id: String,
    pub identity: Option<ServerIdentity>,
    pub profile: Option<serde_json::Value>,
}

#[cfg(test)]
mod history_status_tests {
    use super::*;
    use crate::store::Store;
    use std::sync::Arc;

    #[test]
    fn history_waits_only_for_live_human_approvals_in_the_exact_session() {
        let repo = Repository::new(Arc::new(Store::in_memory().unwrap()));
        let chat = repo.create_general_session("model", "user").unwrap();
        let other = repo.create_general_session("model", "user").unwrap();
        for session in [&chat, &other] {
            repo.append(
                &session.id,
                "user",
                0,
                "message",
                Payload::UserMessage {
                    text: "有历史的对话".into(),
                },
            )
            .unwrap();
        }
        repo.db(|db| {
            db.execute("INSERT INTO agent_execution_requests(id,session_id,user_id,task_id,call_id,request_json,state,created_at,expires_at,updated_at) VALUES('pending',?1,'user','task','call','{}','pending',0,?2,0)",params![chat.id,chrono::Utc::now().timestamp_millis()+300_000])?;
            db.execute("INSERT INTO agent_execution_policies VALUES(?1,'task','confirm',1)",[&chat.id])?;
            Ok(())
        }).unwrap();
        let status = || {
            let chats = repo.project_chats("user", None, "").unwrap();
            assert!(
                !chats
                    .items
                    .iter()
                    .find(|c| c.session_id == other.id)
                    .unwrap()
                    .awaiting_approval
            );
            chats
                .items
                .iter()
                .find(|c| c.session_id == chat.id)
                .unwrap()
                .awaiting_approval
        };
        assert!(status());
        for state in [
            "approved",
            "admitted",
            "completed",
            "denied",
            "cancelled",
            "expired",
            "unknown",
        ] {
            repo.db(|db| {
                db.execute("UPDATE agent_execution_requests SET state=?1", [state])?;
                Ok(())
            })
            .unwrap();
            assert!(!status(), "{state} is no longer awaiting approval");
        }
        repo.db(|db| {
            db.execute_batch("UPDATE agent_execution_requests SET state='pending',expires_at=0")?;
            Ok(())
        })
        .unwrap();
        assert!(!status());
        repo.db(|db| {
            db.execute(
                "UPDATE agent_execution_requests SET expires_at=?1",
                [chrono::Utc::now().timestamp_millis() + 300_000],
            )?;
            db.execute_batch("UPDATE agent_execution_policies SET mode='auto'")?;
            Ok(())
        })
        .unwrap();
        assert!(!status(), "automatic review alone does not require a human");
        repo.db(|db| {db.execute_batch("INSERT INTO agent_execution_policy_reviews VALUES('pending','auto',1,'ask','high','需要确认')")?;Ok(())}).unwrap();
        assert!(status());
        repo.db(|db| {
            db.execute_batch("UPDATE agent_execution_policy_reviews SET decision='allow'")?;
            Ok(())
        })
        .unwrap();
        assert!(!status());
        repo.db(|db| {db.execute_batch("UPDATE agent_execution_policy_reviews SET decision='ask'; UPDATE agent_execution_policies SET mode='fullAccess'")?;Ok(())}).unwrap();
        assert!(!status());
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChat {
    pub session_id: String,
    pub agent_id: String,
    pub title: String,
    pub created_at: i64,
    pub running: bool,
    pub awaiting_approval: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectChats {
    pub items: Vec<ProjectChat>,
    pub next_after: Option<String>,
}
fn project(db: &Connection, id: &str, user: &str) -> Result<Project> {
    let (id,name,connection,identity,profile)=db.query_row(
  "SELECT id,name,connection_id,identity_json,profile_json FROM agent_projects WHERE id=?1 AND user_id=?2 AND NOT EXISTS(SELECT 1 FROM agent_removed_projects WHERE project_id=?1)",
  params![id,user],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?)))
  .optional()?.ok_or(Error::NotFound)?;
    Ok(Project {
        id,
        name,
        connection_id: connection,
        identity: identity.map(|s| serde_json::from_str(&s)).transpose()?,
        profile: profile.map(|s| serde_json::from_str(&s)).transpose()?,
    })
}
fn bind(
    db: &Connection,
    id: &str,
    user: &str,
    connection: &str,
    identity: &ServerIdentity,
) -> Result<Project> {
    let p = project(db, id, user)?;
    if p.connection_id != connection || p.identity.as_ref().is_some_and(|old| old != identity) {
        return Err(Error::TargetMismatch);
    }
    let expected: Option<String> = db.query_row(
        "SELECT profile_json FROM agent_projects WHERE id=?1",
        [id],
        |r| r.get(0),
    )?;
    if let Some(expected) = expected {
        let expected: serde_json::Value = serde_json::from_str(&expected)?;
        if expected["host"] != identity.host
            || expected["port"] != identity.port
            || expected["username"] != identity.username
        {
            return Err(Error::TargetMismatch);
        }
    }
    db.execute(
        "UPDATE agent_projects SET identity_json=?1 WHERE id=?2 AND identity_json IS NULL",
        params![serde_json::to_string(identity)?, id],
    )?;
    project(db, id, user)
}
impl Repository {
    pub fn create_project(&self, user: &str, name: &str, connection: &str) -> Result<Project> {
        model::identifier(user)?;
        model::identifier(connection)?;
        let name = name.trim();
        if name.is_empty() {
            return Err(Error::Invalid("empty project name".into()));
        }
        model::text_value(name, 100)?;
        let configured = self.store.get(connection).map_err(|_| Error::NotFound)?;
        if configured.user_id != user {
            return Err(Error::NotFound);
        }
        let profile = serde_json::json!({"host":configured.host.trim().trim_matches(['[',']']).to_lowercase(),"port":configured.port,"username":configured.username});
        let id = uuid::Uuid::new_v4().to_string();
        self.db(|db|{
   db.execute("INSERT INTO agent_projects(id,user_id,name,connection_id,profile_json,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
    params![id,user,name,connection,profile.to_string(),chrono::Utc::now().timestamp()])?;
   project(db,&id,user)
  })
    }
    pub fn projects(&self, user: &str) -> Result<Vec<Project>> {
        self.db(|db| {
            let mut q = db.prepare(
                "SELECT id FROM agent_projects WHERE user_id=?1 AND NOT EXISTS(SELECT 1 FROM agent_removed_projects WHERE project_id=agent_projects.id) ORDER BY created_at DESC,id DESC",
            )?;
            let ids = q
                .query_map([user], |r| r.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            ids.into_iter().map(|id| project(db, &id, user)).collect()
        })
    }
    pub fn bind_project(
        &self,
        id: &str,
        user: &str,
        connection: &str,
        identity: &ServerIdentity,
    ) -> Result<Project> {
        self.db(|db| bind(db, id, user, connection, identity))
    }
    pub fn create_project_chat(
        &self,
        id: &str,
        user: &str,
        agent: &str,
        target: &Target,
        identity: ServerIdentity,
    ) -> Result<Session> {
        model::identifier(agent)?;
        target.validate()?;
        self.db(|db|{
   let tx=db.transaction()?;
   bind(&tx,id,user,&target.connection_id,&identity)?;
   let session=uuid::Uuid::new_v4().to_string();
   tx.execute("INSERT INTO agent_sessions(id,user_id,agent_id,created_at) VALUES(?1,?2,?3,?4)",
    params![session,user,agent,chrono::Utc::now().timestamp()])?;
   tx.execute("INSERT INTO agent_session_scopes(session_id,root_id,kind,connection_id,identity_json,origin_terminal_id) VALUES(?1,?1,'server',?2,?3,?4)",
    params![session,target.connection_id,serde_json::to_string(&identity)?,target.terminal_session_id])?;
   tx.execute("INSERT INTO agent_project_chats(session_id,project_id) VALUES(?1,?2)",params![session,id])?;
   let result=repository::read_session(&tx,&session,user)?;tx.commit()?;Ok(result)
  })
    }
    pub fn project_chats(&self, user: &str, id: Option<&str>, after: &str) -> Result<ProjectChats> {
        self.db(|db|{
   if let Some(id)=id {project(db,id,user)?;}
   // Stable owner/project cursor. Empty preparations are not conversation history.
   let cursor=if after.is_empty(){None}else{
    Some(db.query_row("SELECT a.rowid FROM agent_sessions a LEFT JOIN agent_project_chats c ON c.session_id=a.id WHERE a.id=?1 AND a.user_id=?2 AND ((?3 IS NULL AND (c.project_id IS NULL OR EXISTS(SELECT 1 FROM agent_removed_projects WHERE project_id=c.project_id))) OR c.project_id=?3)",
     params![after,user,id],|r|r.get::<_,i64>(0)).optional()?.ok_or(Error::NotFound)?)
   };
   let mut q=db.prepare("SELECT a.id,a.agent_id,COALESCE((SELECT json_extract(input_json,'$.message') FROM agent_chat_turns t WHERE t.session_id=a.id ORDER BY user_event_id LIMIT 1),(SELECT json_extract(payload,'$.text') FROM agent_events e WHERE e.session_id=a.id AND kind='user_message' ORDER BY id LIMIT 1),json_extract(a.task_json,'$.goal'),'新对话'),a.created_at,EXISTS(SELECT 1 FROM agent_chat_turns t WHERE t.session_id=a.id AND t.state='running'),
    EXISTS(SELECT 1 FROM agent_execution_requests r LEFT JOIN agent_execution_policies p ON p.session_id=r.session_id LEFT JOIN agent_execution_policy_reviews v ON v.request_id=r.id WHERE r.session_id=a.id AND r.state='pending' AND r.expires_at>?4 AND (COALESCE(p.mode,'confirm')='confirm' OR (p.mode='auto' AND v.decision='ask')))
    FROM agent_sessions a LEFT JOIN agent_project_chats c ON c.session_id=a.id WHERE a.user_id=?1 AND ((?2 IS NULL AND (c.project_id IS NULL OR EXISTS(SELECT 1 FROM agent_removed_projects WHERE project_id=c.project_id))) OR c.project_id=?2) AND (EXISTS(SELECT 1 FROM agent_chat_turns t WHERE t.session_id=a.id) OR EXISTS(SELECT 1 FROM agent_events e WHERE e.session_id=a.id AND kind IN ('user_message','assistant_message')) OR (a.task_json IS NOT NULL AND NOT EXISTS(SELECT 1 FROM agent_session_scopes s WHERE s.session_id=a.id))) AND (?3 IS NULL OR a.rowid<?3) ORDER BY a.rowid DESC LIMIT 51")?;
   let mut items=q.query_map(params![user,id,cursor,chrono::Utc::now().timestamp_millis()],|r|Ok(ProjectChat{session_id:r.get(0)?,agent_id:r.get(1)?,title:r.get(2)?,created_at:r.get(3)?,running:r.get(4)?,awaiting_approval:r.get(5)?}))?.collect::<std::result::Result<Vec<_>,_>>()?;
   let more=items.len()>50;items.truncate(50);
   for item in &mut items {item.title=item.title.chars().take(80).collect();}
   let next_after=if more{items.last().map(|i|i.session_id.clone())}else{None};
   Ok(ProjectChats{items,next_after})
  })
    }
}
