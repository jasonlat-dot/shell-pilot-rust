//! Exact MCP call admission, using the conversation's live policy and native task.
use super::*;
use rusqlite::{params, OptionalExtension};
use serde::Serialize;
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub id: String,
    pub session_id: String,
    pub call_id: String,
    pub tool_name: String,
    pub arguments: Value,
    pub destination: String,
    pub task: super::super::Task,
    pub state: String,
    pub review: Option<Value>,
    pub expires_at: i64,
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
struct PendingApproval<'a> {
    store: &'a Store,
    id: &'a str,
}
impl Drop for PendingApproval<'_> {
    fn drop(&mut self) {
        if let Ok(db) = self.store.database.lock() {
            let _=db.execute("UPDATE agent_extension_approvals SET state='cancelled' WHERE id=?1 AND state IN ('pending','approved')",[self.id]);
        }
    }
}
pub fn list(store: &Store, user: &str, session: &str) -> Result<Vec<Receipt>> {
    let db = store.database.lock().map_err(|_| Error::Storage)?;
    super::super::repository::read_session(&db, session, user).map_err(|_| Error::Unavailable)?;
    db.execute("UPDATE agent_extension_approvals SET state='expired' WHERE session_id=?1 AND state IN ('pending','approved') AND expires_at<=?2",params![session,now()])?;
    let mut q=db.prepare("SELECT id,call_id,tool_name,arguments_json,destination,task_json,state,review_json,expires_at FROM agent_extension_approvals WHERE session_id=?1 AND user_id=?2 ORDER BY created_at DESC,rowid DESC LIMIT 100")?;
    let rows = q.query_map(params![session, user], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, String>(3)?,
            r.get::<_, String>(4)?,
            r.get::<_, String>(5)?,
            r.get::<_, String>(6)?,
            r.get::<_, Option<String>>(7)?,
            r.get::<_, i64>(8)?,
        ))
    })?;
    rows.map(|r| {
        let (id, call_id, tool_name, args, destination, task, state, review, expires_at) = r?;
        Ok(Receipt {
            id,
            session_id: session.into(),
            call_id,
            tool_name,
            arguments: serde_json::from_str(&args).map_err(|_| Error::Storage)?,
            destination,
            task: serde_json::from_str(&task).map_err(|_| Error::Storage)?,
            state,
            review: review
                .map(|v| serde_json::from_str(&v))
                .transpose()
                .map_err(|_| Error::Storage)?,
            expires_at,
        })
    })
    .collect()
}
pub fn decide(store: &Store, user: &str, session: &str, id: &str, allow: bool) -> Result<()> {
    let mut db = store.database.lock().map_err(|_| Error::Storage)?;
    let tx = db.transaction()?;
    let current =
        super::super::repository::read_session(&tx, session, user).map_err(|_| Error::Changed)?;
    let(task,revision,expires,state,call):(String,u64,i64,String,String)=tx.query_row("SELECT task_json,policy_revision,expires_at,state,call_id FROM agent_extension_approvals WHERE id=?1 AND session_id=?2 AND user_id=?3",params![id,session,user],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?.ok_or(Error::Unavailable)?;
    let policy: u64 = tx
        .query_row(
            "SELECT revision FROM agent_execution_policies WHERE session_id=?1",
            [session],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if current
        .task
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(|_| Error::Storage)?
        .as_deref()
        != Some(task.as_str())
        || revision != policy
        || expires <= now()
        || state != "pending"
    {
        return Err(Error::Changed);
    }
    tx.execute(
        "UPDATE agent_extension_approvals SET state=?2 WHERE id=?1",
        params![id, if allow { "approved" } else { "denied" }],
    )?;
    tx.commit()?;
    let metadata = db.query_row("SELECT t.request_id,s.agent_id FROM agent_events e JOIN agent_sessions s ON s.id=e.session_id JOIN agent_chat_turns t ON t.session_id=e.session_id AND t.user_event_id<=e.id WHERE e.session_id=?1 AND s.user_id=?2 AND e.call_id=?3 AND e.kind='tool_call' ORDER BY t.user_event_id DESC LIMIT 1",params![session,user,call],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).optional().ok().flatten();
    drop(db);
    if let Some((request,agent))=metadata {store.diagnostics.trace(&request,session,&agent).event(if allow{crate::diagnostics::Level::Info}else{crate::diagnostics::Level::Warn},"approval.mcp_decision",json!({"approvalId":id,"callId":call,"allowed":allow}));}
    Ok(())
}
impl Runtime {
    pub(super) async fn admit(
        &self,
        call: &super::super::tool_loop::ValidatedCall,
        destination: &str,
        cancel: &CancellationToken,
    ) -> Result<String> {
        let approvals = super::super::approvals::Approvals::new(self.store.clone());
        let (mode, revision) = approvals
            .conversation_policy(self.session.clone(), self.user.clone())
            .await
            .map_err(|_| Error::Changed)?;
        if mode == super::super::approvals::Policy::Deny || cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let id = uuid::Uuid::new_v4().to_string();
        let mut arguments = call.arguments().clone();
        if let Some(raw) = arguments["argumentsJson"].as_str() {
            arguments["argumentsJson"] = serde_json::from_str(raw).map_err(|_| Error::Invalid)?;
        }
        {
            let db = self.store.database.lock().map_err(|_| Error::Storage)?;
            let current = super::super::repository::read_session(&db, &self.session, &self.user)
                .map_err(|_| Error::Changed)?;
            if current.task.as_ref() != Some(&self.task)
                || config::settings_revision(&db, &self.user)? != self.settings.revision
            {
                return Err(Error::Changed);
            }
            // A repeated call ID is never a new permission, including after crash/restart.
            db.execute("INSERT INTO agent_extension_approvals(id,session_id,user_id,call_id,task_json,tool_name,arguments_json,destination,config_revision,policy_revision,state,created_at,expires_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending',?11,?12)",params![id,self.session,self.user,call.id(),serde_json::to_string(&self.task).map_err(|_|Error::Storage)?,call.name(),arguments.to_string(),destination,self.settings.revision,revision,now(),now()+300000])?;
        }
        crate::diagnostics::event(crate::diagnostics::Level::Info,"approval.mcp_created",json!({"approvalId":id,"callId":call.id(),"toolName":call.name(),"policy":mode}));
        let _pending = PendingApproval {
            store: &self.store,
            id: &id,
        };
        let verdict = if mode == super::super::approvals::Policy::FullAccess {
            Some(json!({"decision":"allow","reason":"当前对话已由用户选择完全访问权限"}))
        } else if mode == super::super::approvals::Policy::Auto {
            let messages = {
                let db = self.store.database.lock().map_err(|_| Error::Storage)?;
                let mut q=db.prepare("SELECT json_extract(payload,'$.text') FROM agent_events WHERE session_id=?1 AND kind='user_message' ORDER BY id DESC LIMIT 4")?;
                let values = q
                    .query_map([&self.session], |r| r.get::<_, String>(0))?
                    .collect::<std::result::Result<Vec<_>, _>>()?;
                values
            };
            let review = super::super::approvals::reviewer::review_tool_action(
                self.store.clone(),
                self.agent.clone(),
                self.user.clone(),
                super::super::approvals::reviewer::ToolReviewAction {
                    tool_name: call.name().into(),
                    arguments: call.arguments().clone(),
                    task: json!(self.task),
                    target: json!({"scope":self.scope,"destination":destination}),
                    recent_user_messages: messages,
                },
                cancel,
            )
            .await;
            Some(
                review
                    .and_then(|r| serde_json::to_value(r).ok())
                    .unwrap_or(json!({"decision":"ask","reason":"审查未能完成，请人工确认"})),
            )
        } else {
            None
        };
        if let Some(review) = verdict {
            crate::diagnostics::event(crate::diagnostics::Level::Info,"approval.mcp_review_result",json!({"approvalId":id,"callId":call.id(),"decision":review["decision"],"risk":review["risk"]}));
            let db = self.store.database.lock().map_err(|_| Error::Storage)?;
            db.execute("UPDATE agent_extension_approvals SET review_json=?2,state=?3 WHERE id=?1 AND state='pending'",params![id,review.to_string(),if review["decision"]=="allow"{"approved"}else{"pending"}])?;
        }
        let mut previous = String::new();
        loop {
            let state = self.try_admit(&id, revision, cancel)?;
            if state!=previous {crate::diagnostics::event(crate::diagnostics::Level::Info,"approval.mcp_state",json!({"approvalId":id,"callId":call.id(),"state":state}));previous=state.clone();}
            match state.as_str() {
                "admitted" => return Ok(id.clone()),
                "pending" => {}
                _ => return Err(Error::Changed),
            }
            tokio::select! {biased;_=cancel.cancelled()=>{self.finish_approval(&id,"cancelled");return Err(Error::Cancelled)},_=tokio::time::sleep(std::time::Duration::from_millis(100))=>{}}
        }
    }
    fn try_admit(&self, id: &str, revision: u64, cancel: &CancellationToken) -> Result<String> {
        let mut db = self.store.database.lock().map_err(|_| Error::Storage)?;
        let tx = db.transaction()?;
        let current = super::super::repository::read_session(&tx, &self.session, &self.user)
            .map_err(|_| Error::Changed)?;
        let policy: Option<(String, u64)> = tx
            .query_row(
                "SELECT mode,revision FROM agent_execution_policies WHERE session_id=?1",
                [&self.session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        let (mode, live_revision) = policy.unwrap_or(("confirm".into(), 0));
        let (state, expires): (String, i64) = tx.query_row(
            "SELECT state,expires_at FROM agent_extension_approvals WHERE id=?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let lease:Option<(String,bool,i64)>=tx.query_row("SELECT state,cancel_requested,lease_until FROM agent_chat_turns WHERE session_id=?1 ORDER BY user_event_id DESC LIMIT 1",[&self.session],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
        let valid = !cancel.is_cancelled()
            && mode != "deny"
            && revision == live_revision
            && expires > now()
            && current.task.as_ref() == Some(&self.task)
            && config::settings_revision(&tx, &self.user)? == self.settings.revision
            && lease.is_none_or(|(s, c, l)| s == "running" && !c && l >= now());
        let uncertain:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_extension_approvals old JOIN agent_extension_approvals candidate ON candidate.id=?1 WHERE old.session_id=candidate.session_id AND old.id<>candidate.id AND old.task_json=candidate.task_json AND old.destination=candidate.destination AND old.arguments_json=candidate.arguments_json AND old.state IN ('admitted','unknown'))",[id],|r|r.get(0))?;
        let next = if !valid || uncertain {
            "cancelled"
        } else if state == "approved" {
            "admitted"
        } else {
            state.as_str()
        };
        if next != state {
            tx.execute(
                "UPDATE agent_extension_approvals SET state=?2 WHERE id=?1",
                params![id, next],
            )?;
        }
        let out = next.into();
        tx.commit()?;
        Ok(out)
    }
    pub(super) fn finish_approval(&self, id: &str, state: &str) {
        if let Ok(db) = self.store.database.lock() {
            let _=db.execute("UPDATE agent_extension_approvals SET state=?2 WHERE id=?1 AND state IN ('admitted','pending','approved')",params![id,state]);
        }
    }
}
