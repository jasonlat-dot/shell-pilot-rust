//! Durable conversation scopes; no histories/windows are copied between scopes.
use super::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScopeKind {
    General,
    Server,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ServerIdentity {
    pub host: String,
    pub port: u16,
    pub username: String,
    pub host_key: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Scope {
    pub root_id: String,
    pub kind: ScopeKind,
    pub connection_id: Option<String>,
    pub identity: Option<ServerIdentity>,
    pub origin_terminal_id: Option<String>,
}
pub(super) fn read(db: &Connection, id: &str) -> Result<Option<Scope>> {
    let row = db.query_row(
        "SELECT root_id,kind,connection_id,identity_json,origin_terminal_id FROM agent_session_scopes WHERE session_id=?1",
        [id], |r| Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?,
            r.get::<_,Option<String>>(3)?,r.get::<_,Option<String>>(4)?))).optional()?;
    row.map(|(root, kind, connection, identity, terminal)| {
        Ok(Scope {
            root_id: root,
            kind: match kind.as_str() {
                "general" => ScopeKind::General,
                "server" => ScopeKind::Server,
                _ => return Err(Error::Invalid("scope kind".into())),
            },
            connection_id: connection,
            identity: identity.map(|s| serde_json::from_str(&s)).transpose()?,
            origin_terminal_id: terminal,
        })
    })
    .transpose()
}
impl Repository {
    pub fn create_general_session(&self, agent: &str, user: &str) -> Result<Session> {
        model::identifier(agent)?;
        model::identifier(user)?;
        let id = uuid::Uuid::new_v4().to_string();
        self.db(|db| {
            let tx = db.transaction()?;
            tx.execute(
                "INSERT INTO agent_sessions(id,user_id,agent_id,created_at) VALUES(?1,?2,?3,?4)",
                params![id, user, agent, chrono::Utc::now().timestamp()],
            )?;
            tx.execute(
                "INSERT INTO agent_session_scopes(session_id,root_id,kind) VALUES(?1,?1,'general')",
                [&id],
            )?;
            let result = repository::read_session(&tx, &id, user)?;
            tx.commit()?;
            Ok(result)
        })
    }
    pub fn server_scope(
        &self,
        agent: &str,
        user: &str,
        root: &str,
        target: &Target,
        identity: ServerIdentity,
        new_task: bool,
    ) -> Result<Session> {
        let connection = &target.connection_id;
        let terminal = &target.terminal_session_id;
        model::identifier(connection)?;
        model::identifier(terminal)?;
        self.db(|db|{
            let tx=db.transaction()?;
            let parent=repository::read_session(&tx,root,user)?;
            if parent.agent_id!=agent || parent.scope.as_ref().is_none_or(|s|s.kind!=ScopeKind::General || s.root_id!=root) {
                return Err(Error::NotFound);
            }
            let identity=serde_json::to_string(&identity)?;
            if !new_task {
                let found=tx.query_row("SELECT session_id FROM agent_session_scopes WHERE root_id=?1 AND kind='server' AND connection_id=?2 AND identity_json=?3 ORDER BY rowid DESC LIMIT 1",
                    params![root,connection,identity],|r|r.get::<_,String>(0)).optional()?;
                if let Some(id)=found {return repository::read_session(&tx,&id,user);}
            }
            let count:u64=tx.query_row("SELECT COUNT(*) FROM agent_session_scopes WHERE root_id=?1",[root],|r|r.get(0))?;
            if count>=64{return Err(Error::Invalid("too many conversation scopes".into()));}
            let id=uuid::Uuid::new_v4().to_string();
            tx.execute("INSERT INTO agent_sessions(id,user_id,agent_id,created_at) VALUES(?1,?2,?3,?4)",
                params![id,user,agent,chrono::Utc::now().timestamp()])?;
            tx.execute("INSERT INTO agent_session_scopes(session_id,root_id,kind,connection_id,identity_json,origin_terminal_id) VALUES(?1,?2,'server',?3,?4,?5)",
                params![id,root,connection,identity,terminal])?;
            let result=repository::read_session(&tx,&id,user)?;
            tx.commit()?;Ok(result)
        })
    }
    pub fn scopes(&self, agent: &str, user: &str, root: &str) -> Result<Vec<Session>> {
        self.db(|db|{
            let parent=repository::read_session(db,root,user)?;
            if parent.agent_id!=agent || parent.scope.as_ref().is_none_or(|s|s.root_id!=root || s.kind!=ScopeKind::General){return Err(Error::NotFound);}
            let mut q=db.prepare("SELECT a.id FROM agent_sessions a JOIN agent_session_scopes s ON s.session_id=a.id WHERE s.root_id=?1 AND a.user_id=?2 AND a.agent_id=?3 ORDER BY s.rowid LIMIT 64")?;
            let ids=q.query_map(params![root,user,agent],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
            ids.into_iter().map(|id|repository::read_session(db,&id,user)).collect()
        })
    }
}
