//! Durable text-turn admission, stop/finish serialization and bounded crash recovery.
use super::{
    chat::{Error, Request, Result},
    provider::{Completion, Usage},
    repository::{insert_event, next_revision, read_session},
    Payload, Session, Target, Task, TaskStatus,
};
use crate::store::Store;
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::sync::Arc;

const LEASE_MS: i64 = 30_000;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TurnState {
    Running,
    Completed,
    Cancelled,
    Failed,
    Interrupted,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct Input {
    message: String,
    terminal_session_id: String,
    target: Option<Target>,
    #[serde(default)]
    attachments: Vec<super::chat::Attachment>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Turn {
    pub request_id: String,
    pub message: String,
    pub target: Option<Target>,
    pub state: TurnState,
    pub partial_text: String,
    pub result_text: Option<String>,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub usage: Option<Usage>,
    pub cursor: u64,
    pub created_at: i64,
    pub updated_at: i64,
    pub tool_events: Vec<super::Event>,
    pub started_tool_call_ids: Vec<String>,
    pub inputs: Vec<super::steering::Input>,
    pub attachments: Vec<super::attachments::Reference>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct History {
    pub session: Session,
    pub turns: Vec<Turn>,
    pub has_more: bool,
    pub next_after: u64,
}
struct Row {
    turn: Turn,
    input: Input,
    owner: String,
    revision: u64,
    task_id: Option<String>,
    cancel: bool,
    lease: i64,
}
#[derive(Clone)]
pub(super) struct Turns {
    store: Arc<Store>,
}
fn storage<T>(value: std::result::Result<T, rusqlite::Error>) -> Result<T> {
    value.map_err(|_| Error::Storage)
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn read_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<Row> {
    let input_json: String = r.get("input_json")?;
    let input: Input =
        serde_json::from_str(&input_json).map_err(|_| rusqlite::Error::InvalidQuery)?;
    let state: String = r.get("state")?;
    let state = match state.as_str() {
        "running" => TurnState::Running,
        "completed" => TurnState::Completed,
        "cancelled" => TurnState::Cancelled,
        "failed" => TurnState::Failed,
        "interrupted" => TurnState::Interrupted,
        _ => return Err(rusqlite::Error::InvalidQuery),
    };
    let usage_json: Option<String> = r.get("usage_json")?;
    let usage = usage_json
        .map(|json| serde_json::from_str(&json).map_err(|_| rusqlite::Error::InvalidQuery))
        .transpose()?;
    Ok(Row {
        turn: Turn {
            request_id: r.get("request_id")?,
            message: input.message.clone(),
            target: input.target.clone(),
            state,
            partial_text: r.get("partial_text")?,
            result_text: r.get("result_text")?,
            error_code: r.get("error_code")?,
            error_message: None,
            usage,
            cursor: r.get("user_event_id")?,
            created_at: r.get("created_at")?,
            updated_at: r.get("updated_at")?,
            tool_events: vec![],
            started_tool_call_ids: vec![],
            inputs: vec![],
            attachments: vec![],
        },
        input,
        owner: r.get("owner")?,
        revision: r.get("revision")?,
        task_id: r.get("task_id")?,
        cancel: r.get("cancel_requested")?,
        lease: r.get("lease_until")?,
    })
}
fn failure_message(db: &Connection, session: &str, request: &str) -> Result<Option<String>> {
    storage(db.query_row("SELECT message FROM agent_turn_errors WHERE session_id=?1 AND request_id=?2",params![session,request],|row|row.get(0)).optional())
}
fn row(db: &Connection, session: &str, request: &str) -> Result<Option<Row>> {
    let mut result = storage(
        db.query_row(
            "SELECT * FROM agent_chat_turns WHERE session_id=?1 AND request_id=?2",
            params![session, request],
            read_row,
        )
        .optional(),
    )?;
    if let Some(row) = &mut result {
        let user: String = storage(db.query_row(
            "SELECT user_id FROM agent_sessions WHERE id=?1",
            [session],
            |r| r.get(0),
        ))?;
        row.turn.attachments = super::attachments::references(db, session, &user, row.turn.cursor)?;
        row.turn.error_message = failure_message(db, session, request)?;
    }
    Ok(result)
}
fn verify(s: &Session, agent: &str) -> Result<()> {
    if s.agent_id != agent {
        Err(Error::NotFound)
    } else {
        Ok(())
    }
}
fn input(request: &Request, target: Option<Target>) -> Input {
    Input {
        message: request.message.clone(),
        terminal_session_id: request.terminal_session_id.clone(),
        target,
        attachments: request.attachments.clone(),
    }
}
struct Finalization<'a> {
    error: Option<&'a str>,
    state: TurnState,
    text: &'a str,
    code: Option<&'a str>,
    completion: Option<&'a Completion>,
}
fn finish_row(
    db: &Connection,
    session: &str,
    old: Row,
    owner: Option<&str>,
    finish: Finalization<'_>,
) -> Result<Turn> {
    let Finalization {
        error,
        state,
        text,
        code,
        completion,
    } = finish;
    if old.turn.state != TurnState::Running {
        return Ok(old.turn);
    }
    if owner.is_some_and(|owner| old.owner != owner) {
        return Err(Error::Conflict);
    }
    let s = read_session(
        db,
        session,
        &storage(db.query_row(
            "SELECT user_id FROM agent_sessions WHERE id=?1",
            [session],
            |r| r.get::<_, String>(0),
        ))?,
    )?;
    let mut state = state;
    let mut code = code;
    let tool_failure = state != TurnState::Completed
        && storage(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_tool_turns WHERE session_id=?1 AND request_id=?2)",
            params![session, old.turn.request_id],
            |r| r.get::<_, bool>(0),
        ))?;
    if (s.revision != old.revision && !tool_failure)
        || s.task.as_ref().map(|t| t.id.as_str()) != old.task_id.as_deref()
    {
        state = TurnState::Interrupted;
        code = Some("AGENT_CONTEXT_CHANGED");
    } else if old.cancel && state == TurnState::Completed {
        state = TurnState::Cancelled;
        code = Some("AGENT_CANCELLED");
    } else if old.lease < now() && state == TurnState::Completed {
        state = TurnState::Interrupted;
        code = Some("AGENT_INTERRUPTED");
    }
    let completed = state == TurnState::Completed;
    let mut assistant_event_id = None;
    if s.revision == old.revision
        && s.task.as_ref().map(|t| t.id.as_str()) == old.task_id.as_deref()
    {
        let payload = if completed {
            Payload::AssistantMessage {
                text: super::steering::final_text(db, session, &old.turn.request_id, text)?,
            }
        } else {
            Payload::Cancelled {
                reason: code.unwrap_or("AGENT_INTERRUPTED").into(),
            }
        };
        payload.validate()?;
        let id = format!("{}:finish", old.turn.request_id);
        let event = insert_event(db, session, old.task_id.as_deref(), &id, &payload)?;
        if completed {
            assistant_event_id = Some(event.id);
        }
        let mut task = s.task;
        if !completed {
            if let Some(task) = &mut task {
                task.status = TaskStatus::Cancelled;
            }
        }
        let task_json = task
            .map(|task| serde_json::to_string(&task).map_err(|_| Error::Storage))
            .transpose()?;
        storage(db.execute(
            "UPDATE agent_sessions SET revision=?1,task_json=?2 WHERE id=?3 AND revision=?4",
            params![next_revision(s.revision)?, task_json, session, s.revision],
        ))?;
    }
    if completed {
        if let (Some(event), Some(state)) = (assistant_event_id, completion.and_then(|c| c.message.continuation.as_ref())) {
            super::continuation_store::save(db, session, event - 1, event, state)?;
        }
        if let (Some(event), Some(output)) = (
            assistant_event_id,
            completion.and_then(|c| c.response_output.as_ref()),
        ) {
            super::responses_store::save_turn(db, event, output).map_err(Error::from)?;
        }
    }
    if state == TurnState::Failed {
        if let Some(error) = error {
            let message: String=error.chars().take(2048).collect();
            storage(db.execute("INSERT INTO agent_turn_errors(session_id,request_id,message) VALUES(?1,?2,?3)",params![session,old.turn.request_id,message]))?;
        }
    }
    let state = match state {
        TurnState::Running => return Err(Error::Invalid),
        TurnState::Completed => "completed",
        TurnState::Cancelled => "cancelled",
        TurnState::Failed => "failed",
        TurnState::Interrupted => "interrupted",
    };
    let usage = if completed {
        completion
            .and_then(|c| c.usage.as_ref())
            .map(serde_json::to_string)
            .transpose()
            .map_err(|_| Error::Storage)?
    } else {
        None
    };
    storage(db.execute("UPDATE agent_chat_turns SET state=?1,partial_text=?2,result_text=?3,error_code=?4,usage_json=?5,assistant_event_id=?6,updated_at=?7 WHERE session_id=?8 AND request_id=?9 AND state='running'",
        params![state,text,completed.then_some(text),code,usage,assistant_event_id,now(),session,old.turn.request_id]))?;
    Ok(row(db, session, &old.turn.request_id)?
        .ok_or(Error::Storage)?
        .turn)
}
fn recover(db: &Connection, session: &str) -> Result<()> {
    let mut statement = storage(db.prepare(
        "SELECT * FROM agent_chat_turns WHERE session_id=?1 AND state='running' AND lease_until<?2",
    ))?;
    let rows = storage(statement.query_map(params![session, now()], read_row))?;
    let rows = rows
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|_| Error::Storage)?;
    drop(statement);
    for old in rows {
        let partial = old.turn.partial_text.clone();
        let tools: bool = storage(db.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_tool_turns WHERE session_id=?1 AND request_id=?2)",
            params![session, old.turn.request_id],
            |r| r.get(0),
        ))?;
        if tools {
            let mut q=storage(db.prepare("SELECT c.call_id,c.task_id FROM agent_events c WHERE c.session_id=?1 AND c.kind='tool_call' AND c.id>?2 AND NOT EXISTS(SELECT 1 FROM agent_events r WHERE r.session_id=c.session_id AND r.call_id=c.call_id AND r.kind='tool_result') ORDER BY c.id"))?;
            let pending = storage(q.query_map(params![session, old.turn.cursor], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            }))?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|_| Error::Storage)?;
            drop(q);
            for (call, task) in pending {
                insert_event(
                    db,
                    session,
                    task.as_deref(),
                    &uuid::Uuid::new_v4().to_string(),
                    &Payload::ToolResult {
                        call_id: call,
                        outcome: super::Outcome::Unknown,
                        exit_code: None,
                        text: "{\"ok\":false,\"error\":\"result_unknown\"}".into(),
                        outputs: vec![],
                    },
                )?;
                storage(db.execute(
                    "UPDATE agent_sessions SET revision=revision+1 WHERE id=?1",
                    [session],
                ))?;
            }
            storage(db.execute("UPDATE agent_execution_requests SET state=CASE WHEN state='admitted' THEN 'unknown' ELSE 'cancelled' END,updated_at=?3 WHERE session_id=?1 AND call_id IN (SELECT call_id FROM agent_events WHERE session_id=?1 AND id>?2 AND kind='tool_call') AND state IN ('pending','approved','admitted')",params![session,old.turn.cursor,now()]))?;
            storage(db.execute("UPDATE agent_extension_approvals SET state=CASE WHEN state='admitted' THEN 'unknown' ELSE 'cancelled' END WHERE session_id=?1 AND call_id IN (SELECT call_id FROM agent_events WHERE session_id=?1 AND id>?2 AND kind='tool_call') AND state IN ('pending','approved','admitted')",params![session,old.turn.cursor]))?;
        }
        finish_row(
            db,
            session,
            old,
            None,
            Finalization {
                error: None,
                state: TurnState::Interrupted,
                text: &partial,
                code: Some("AGENT_INTERRUPTED"),
                completion: None,
            },
        )?;
    }
    Ok(())
}
impl Turns {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
    fn db<T>(&self, action: impl FnOnce(&mut Connection) -> Result<T>) -> Result<T> {
        let mut db = self.store.database.lock().map_err(|_| Error::Storage)?;
        action(&mut db)
    }
    pub fn receipt(&self, request: &Request) -> Result<Option<Turn>> {
        self.db(|db| {
            let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
            verify(
                &read_session(&tx, &request.session_id, &request.user_id)?,
                &request.agent_id,
            )?;
            recover(&tx, &request.session_id)?;
            let old = row(&tx, &request.session_id, &request.request_id)?;
            let result = old
                .map(|old| {
                    if old.input != input(request, old.input.target.clone()) {
                        return Err(Error::Conflict);
                    }
                    Ok(old.turn)
                })
                .transpose()?;
            storage(tx.commit())?;
            Ok(result)
        })
    }
    pub fn begin(
        &self,
        request: &Request,
        target: Option<Target>,
        owner: &str,
        expected: u64,
    ) -> Result<(Turn, bool)> {
        self.begin_kind(request, target, owner, expected, false)
    }
    #[cfg(test)]
    pub fn begin_tools(
        &self,
        request: &Request,
        target: Target,
        owner: &str,
        expected: u64,
    ) -> Result<(Turn, bool)> {
        self.begin_kind(request, Some(target), owner, expected, true)
    }
    pub(crate) fn begin_runtime_tools(
        &self,
        request: &Request,
        target: Option<Target>,
        owner: &str,
        expected: u64,
    ) -> Result<(Turn, bool)> {
        self.begin_kind(request, target, owner, expected, true)
    }
    fn begin_kind(
        &self,
        request: &Request,
        target: Option<Target>,
        owner: &str,
        expected: u64,
        tools: bool,
    ) -> Result<(Turn, bool)> {
        self.db(|db| {
            let tx=storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
            let mut s=read_session(&tx,&request.session_id,&request.user_id)?;verify(&s,&request.agent_id)?;
            recover(&tx,&s.id)?;
            if let Some(old)=row(&tx,&s.id,&request.request_id)? {
                if old.input!=input(request,target) {return Err(Error::Conflict);}
                storage(tx.commit())?;return Ok((old.turn,false));
            }
            if s.revision!=expected {return Err(Error::ContextChanged);}
            if storage(tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_chat_turns WHERE session_id=?1 AND state='running')",[&s.id],|r|r.get::<_,bool>(0)))? {return Err(Error::Busy);}
            if s.task.as_ref().is_some_and(|t|t.target!=target) {return Err(Error::TargetMismatch);}
            if s.task.as_ref().is_none_or(|t|t.status!=TaskStatus::Active) {
                let task=Task {id:uuid::Uuid::new_v4().to_string(),goal:if request.message.trim().is_empty(){"分析所附文件".into()}else{request.message.clone()},
                    constraints:s.task.as_ref().map(|t|t.constraints.clone()).unwrap_or_default(),target:target.clone(),status:TaskStatus::Active};
                let payload=Payload::TaskStarted{task:task.clone()};
                if serde_json::to_vec(&payload).map_err(|_|Error::Storage)?.len()>65536 {return Err(Error::Invalid);}
                insert_event(&tx,&s.id,Some(&task.id),&format!("{}:task",request.request_id),&payload)?;
                s.task=Some(task);s.revision=next_revision(s.revision)?;
            }
            let event=insert_event(&tx,&s.id,s.task.as_ref().map(|t|t.id.as_str()),&format!("{}:user",request.request_id),&Payload::UserMessage{text:if request.message.trim().is_empty(){"请分析所附文件。".into()}else{request.message.clone()}})?;
            super::attachments::link(&tx,request,event.id)?;
            s.revision=next_revision(s.revision)?;
            let task_json=s.task.as_ref().map(serde_json::to_string).transpose().map_err(|_|Error::Storage)?;
            storage(tx.execute("UPDATE agent_sessions SET revision=?1,task_json=?2 WHERE id=?3",params![s.revision,task_json,s.id]))?;
            let json=serde_json::to_string(&input(request,target)).map_err(|_|Error::Storage)?;
            storage(tx.execute("INSERT INTO agent_chat_turns(session_id,request_id,input_json,owner,state,user_event_id,revision,task_id,lease_until,created_at,updated_at) VALUES(?1,?2,?3,?4,'running',?5,?6,?7,?8,?9,?9)",
                params![s.id,request.request_id,json,owner,event.id,s.revision,s.task.as_ref().map(|t|&t.id),now()+LEASE_MS,now()]))?;
            if tools {storage(tx.execute("INSERT INTO agent_tool_turns(session_id,request_id) VALUES(?1,?2)",params![s.id,request.request_id]))?;}
            let turn=row(&tx,&s.id,&request.request_id)?.ok_or(Error::Storage)?.turn;
            storage(tx.commit())?;Ok((turn,true))
        })
    }
    pub fn checkpoint_partial(
        &self,
        request: &Request,
        owner: &str,
        partial: Option<&str>,
    ) -> Result<bool> {
        self.db(|db| {
            let old=row(db,&request.session_id,&request.request_id)?.ok_or(Error::NotFound)?;
            if old.owner!=owner || old.turn.state!=TurnState::Running || old.cancel || old.lease<now() {return Ok(false);}
            let changed=storage(db.execute("UPDATE agent_chat_turns SET partial_text=COALESCE(?1,partial_text),lease_until=?2,updated_at=?3 WHERE session_id=?4 AND request_id=?5 AND state='running' AND owner=?6 AND cancel_requested=0 AND lease_until>=?3",
                params![partial,now()+LEASE_MS,now(),request.session_id,request.request_id,owner]))?;
            Ok(changed==1)
        })
    }
    pub fn stop(
        &self,
        agent: &str,
        user: &str,
        session: &str,
        request: Option<&str>,
    ) -> Result<Option<String>> {
        self.db(|db| {
            let tx=storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
            verify(&read_session(&tx,session,user)?,agent)?;recover(&tx,session)?;
            let id:Option<String>=storage(tx.query_row("SELECT request_id FROM agent_chat_turns WHERE session_id=?1 AND state='running' AND (?2 IS NULL OR request_id=?2)",params![session,request],|r|r.get(0)).optional())?;
            if let Some(id)=&id {storage(tx.execute("UPDATE agent_chat_turns SET cancel_requested=1,updated_at=?1 WHERE session_id=?2 AND request_id=?3",params![now(),session,id]))?;}
            storage(tx.commit())?;Ok(id)
        })
    }
    pub fn finish(
        &self,
        request: &Request,
        owner: &str,
        state: TurnState,
        partial: &str,
        code: Option<&str>,
        completion: Option<&Completion>,
    ) -> Result<Turn> {
        self.finish_detailed(request, owner, state, partial, code, completion, None)
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish_detailed(&self, request: &Request, owner: &str, state: TurnState, partial: &str, code: Option<&str>, completion: Option<&Completion>, error: Option<&str>) -> Result<Turn> {
        self.db(|db| {
            let tx = storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
            verify(
                &read_session(&tx, &request.session_id, &request.user_id)?,
                &request.agent_id,
            )?;
            let old = row(&tx, &request.session_id, &request.request_id)?.ok_or(Error::NotFound)?;
            let turn = finish_row(
                &tx,
                &request.session_id,
                old,
                Some(owner),
                Finalization {
                    error,
                    state,
                    text: partial,
                    code,
                    completion,
                },
            )?;
            storage(tx.commit())?;
            Ok(turn)
        })
    }
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn finish_tools_reply(&self, request: &Request, owner: &str, state: TurnState, text: &str, code: Option<&str>, expected: u64, completion: Option<&Completion>, error: Option<&str>) -> Result<Turn> {
        self.db(|db| {
            let tx=storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
            verify(&read_session(&tx,&request.session_id,&request.user_id)?,&request.agent_id)?;
            let mut old=row(&tx,&request.session_id,&request.request_id)?.ok_or(Error::NotFound)?;
            let is_tool:bool=storage(tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_tool_turns WHERE session_id=?1 AND request_id=?2)",params![request.session_id,request.request_id],|r|r.get(0)))?;
            if !is_tool {return Err(Error::Conflict);}
            old.revision=expected;
            let turn=finish_row(&tx,&request.session_id,old,Some(owner),Finalization{state,text,code,completion,error})?;
            storage(tx.commit())?;Ok(turn)
        })
    }
    pub fn history(
        &self,
        agent: &str,
        user: &str,
        session: &str,
        after: u64,
        limit: usize,
    ) -> Result<History> {
        if !(1..=20).contains(&limit) || after > i64::MAX as u64 {
            return Err(Error::Invalid);
        }
        self.db(|db| {
            let tx=storage(db.transaction_with_behavior(TransactionBehavior::Immediate))?;
            verify(&read_session(&tx,session,user)?,agent)?;recover(&tx,session)?;
            let session=read_session(&tx,session,user)?;
            let mut stmt=storage(tx.prepare("SELECT * FROM agent_chat_turns WHERE session_id=?1 AND user_event_id>?2 ORDER BY user_event_id LIMIT ?3"))?;
            let rows=storage(stmt.query_map(params![session.id,after,limit+1],read_row))?;
            let mut turns=rows.map(|r|r.map(|r|r.turn)).collect::<std::result::Result<Vec<_>,_>>().map_err(|_|Error::Storage)?;
            let has_more=turns.len()>limit;turns.truncate(limit);
            let next_after=turns.last().map(|t|t.cursor).unwrap_or(after);drop(stmt);
            for turn in &mut turns {
                turn.error_message=failure_message(&tx,&session.id,&turn.request_id)?;
                turn.attachments=super::attachments::references(&tx,&session.id,user,turn.cursor)?;
                turn.inputs=super::steering::list(&tx,&session.id,&turn.request_id)?;
                let end:Option<u64>=storage(tx.query_row("SELECT MIN(user_event_id) FROM agent_chat_turns WHERE session_id=?1 AND user_event_id>?2",params![session.id,turn.cursor],|r|r.get(0)))?;
                let mut q=storage(tx.prepare("SELECT id,session_id,task_id,request_id,payload,created_at FROM agent_events WHERE session_id=?1 AND id>?2 AND (?3 IS NULL OR id<?3) ORDER BY id LIMIT 600"))?;
                let rows=storage(q.query_map(params![session.id,turn.cursor,end],super::repository::read_event_row))?;
                turn.tool_events=rows.map(|r|super::repository::decode_event(r.map_err(|_|Error::Storage)?).map_err(Error::from)).collect::<Result<Vec<_>>>()?.into_iter().filter(|e|matches!(e.payload,Payload::ToolCall{..}|Payload::ToolResult{..})).collect();
                let mut started=storage(tx.prepare("SELECT DISTINCT w.parent_call_id FROM agent_workflow_events w JOIN agent_events c ON c.session_id=w.session_id AND c.call_id=w.parent_call_id AND c.kind='tool_call' WHERE w.session_id=?1 AND w.kind='tool_started' AND c.id>?2 AND (?3 IS NULL OR c.id<?3)"))?;
                turn.started_tool_call_ids=storage(started.query_map(params![session.id,turn.cursor,end],|r|r.get(0)))?.collect::<std::result::Result<Vec<_>,_>>().map_err(|_|Error::Storage)?;
            }
            storage(tx.commit())?;Ok(History {session,turns,has_more,next_after})
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::{provider::AssistantReply, Repository};
    fn request(store: Arc<Store>) -> Request {
        Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent_id: "model".into(),
            user_id: "default".into(),
            session_id: Repository::new(store)
                .create_session("model", "default")
                .unwrap()
                .id,
            terminal_session_id: String::new(),
            message: "hello".into(),
            attachments: vec![],
        }
    }
    fn completion() -> Completion {
        Completion {
            message: AssistantReply {
                continuation: None,
            role: "assistant".into(),
                content: Some("success".into()),
                tool_calls: vec![],
                refusal: None,
            },
            usage: Some(Usage {
                prompt_tokens: 10,
                completion_tokens: 2,
                total_tokens: 12,
            }),
            finish_reason: "stop".into(),
            response_output: None,
        }
    }
    #[test]
    fn failure_detail_is_durable_separate_from_prose_and_terminal_receipt_is_immutable() {
        let dir=tempfile::tempdir().unwrap();let path=dir.path().join("failure.sqlite");
        let request={
            let store=Arc::new(Store::open(&path).unwrap());let turns=Turns::new(store.clone());let r=request(store.clone());
            turns.begin(&r,None,"owner",0).unwrap();
            let t=turns.finish_detailed(&r,"owner",TurnState::Failed,"partial reply",Some("MODEL_HTTP_ERROR"),None,Some("Model provider returned HTTP 400")).unwrap();
            assert_eq!(t.partial_text,"partial reply");assert_eq!(t.error_message.as_deref(),Some("Model provider returned HTTP 400"));
            let replay=turns.finish_detailed(&r,"owner",TurnState::Failed,"late",Some("MODEL_TIMEOUT"),None,Some("late error")).unwrap();
            assert_eq!(replay.error_message,t.error_message);
            assert!(turns.history("model","other",&r.session_id,0,20).is_err());
            r
        };
        let store=Arc::new(Store::open(&path).unwrap());let turns=Turns::new(store.clone());
        let history=turns.history("model","default",&request.session_id,0,20).unwrap();
        assert_eq!(history.turns[0].error_message.as_deref(),Some("Model provider returned HTTP 400"));
        assert_eq!(history.turns[0].partial_text,"partial reply");
        assert_eq!(history.turns[0].request_id,request.request_id);
        Repository::new(store.clone()).delete_chat("model","default",&request.session_id).unwrap();
        assert_eq!(store.database.lock().unwrap().query_row("SELECT COUNT(*) FROM agent_turn_errors",[],|r|r.get::<_,u64>(0)).unwrap(),0);
    }
    #[test]
    fn stop_and_completion_commit_are_ordered_and_terminal_receipts_are_immutable() {
        let store = Arc::new(Store::in_memory().unwrap());
        let turns = Turns::new(store.clone());
        let r = request(store.clone());
        assert!(turns.begin(&r, None, "owner", 0).unwrap().1);
        assert_eq!(
            turns
                .stop("model", "default", &r.session_id, Some(&r.request_id))
                .unwrap(),
            Some(r.request_id.clone())
        );
        let finished = turns
            .finish(
                &r,
                "owner",
                TurnState::Completed,
                "success",
                None,
                Some(&completion()),
            )
            .unwrap();
        assert_eq!(finished.state, TurnState::Cancelled);
        assert!(finished.result_text.is_none());
        assert_eq!(
            turns
                .finish(
                    &r,
                    "owner",
                    TurnState::Completed,
                    "late",
                    None,
                    Some(&completion())
                )
                .unwrap()
                .state,
            TurnState::Cancelled
        );
        assert!(!Repository::new(store.clone())
            .events(&r.session_id, "default", 0, 200)
            .unwrap()
            .iter()
            .any(|e| matches!(e.payload, Payload::AssistantMessage { .. })));
        let next = request(store);
        turns.begin(&next, None, "owner", 0).unwrap();
        let finished = turns
            .finish(
                &next,
                "owner",
                TurnState::Completed,
                "success",
                None,
                Some(&completion()),
            )
            .unwrap();
        assert_eq!(finished.state, TurnState::Completed);
        assert_eq!(finished.usage.unwrap().total_tokens, 12);
        assert!(turns
            .stop("model", "default", &next.session_id, Some(&next.request_id))
            .unwrap()
            .is_none());
    }
    #[test]
    fn request_identity_and_one_running_turn_are_enforced_by_sqlite() {
        let store = Arc::new(Store::in_memory().unwrap());
        let turns = Turns::new(store.clone());
        let r = request(store);
        turns.begin(&r, None, "owner", 0).unwrap();
        assert!(!turns.begin(&r, None, "other", 0).unwrap().1);
        let mut changed = r.clone();
        changed.message = "different".into();
        assert!(matches!(turns.receipt(&changed), Err(Error::Conflict)));
        let mut next = r.clone();
        next.request_id = uuid::Uuid::new_v4().to_string();
        assert!(matches!(
            turns.begin(&next, None, "other", 2),
            Err(Error::Busy)
        ));
        assert!(matches!(
            turns.finish(
                &r,
                "other",
                TurnState::Completed,
                "success",
                None,
                Some(&completion())
            ),
            Err(Error::Conflict)
        ));
    }
    #[test]
    fn future_context_changes_cannot_be_overwritten_by_an_old_reply() {
        let store = Arc::new(Store::in_memory().unwrap());
        let turns = Turns::new(store.clone());
        let r = request(store.clone());
        turns.begin(&r, None, "owner", 0).unwrap();
        let repo = Repository::new(store);
        let session = repo.session(&r.session_id, "default").unwrap();
        repo.append(
            &r.session_id,
            "default",
            session.revision,
            "new-constraint",
            Payload::UserMessage {
                text: "不要重启".into(),
            },
        )
        .unwrap();
        let current = repo.session(&r.session_id, "default").unwrap();
        let old = turns
            .finish(
                &r,
                "owner",
                TurnState::Completed,
                "old reply",
                None,
                Some(&completion()),
            )
            .unwrap();
        assert_eq!(old.state, TurnState::Interrupted);
        assert_eq!(old.error_code.as_deref(), Some("AGENT_CONTEXT_CHANGED"));
        assert_eq!(repo.session(&r.session_id, "default").unwrap(), current);
    }
    #[test]
    fn restart_recovers_expired_partial_without_reexecuting_or_accepting_a_late_owner() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("restart.sqlite");
        let r = {
            let store = Arc::new(Store::open(&path).unwrap());
            let turns = Turns::new(store.clone());
            let r = request(store.clone());
            turns.begin(&r, None, "old-owner", 0).unwrap();
            assert!(turns
                .checkpoint_partial(&r, "old-owner", Some("partial🙂"))
                .unwrap());
            // A fresh lease belongs to a live owner and cannot be stolen by another Manager.
            assert_eq!(
                turns
                    .history("model", "default", &r.session_id, 0, 20)
                    .unwrap()
                    .turns[0]
                    .state,
                TurnState::Running
            );
            store
                .database
                .lock()
                .unwrap()
                .execute("UPDATE agent_chat_turns SET lease_until=0", [])
                .unwrap();
            r
        };
        let store = Arc::new(Store::open(&path).unwrap());
        let turns = Turns::new(store.clone());
        let history = turns
            .history("model", "default", &r.session_id, 0, 20)
            .unwrap();
        assert_eq!(history.turns[0].state, TurnState::Interrupted);
        assert_eq!(history.turns[0].partial_text, "partial🙂");
        assert!(history.turns[0].result_text.is_none());
        assert!(!turns
            .checkpoint_partial(&r, "old-owner", Some("late"))
            .unwrap());
        assert_eq!(
            turns
                .finish(
                    &r,
                    "old-owner",
                    TurnState::Completed,
                    "late",
                    None,
                    Some(&completion())
                )
                .unwrap()
                .state,
            TurnState::Interrupted
        );
        let events = Repository::new(store)
            .events(&r.session_id, "default", 0, 200)
            .unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(
            events.last().unwrap().payload,
            Payload::Cancelled { .. }
        ));
    }
    #[test]
    fn old_stop_request_cannot_cancel_the_next_request_and_pages_keep_owner_scope() {
        let store = Arc::new(Store::in_memory().unwrap());
        let turns = Turns::new(store);
        let r = request(turns.store.clone());
        turns.begin(&r, None, "owner", 0).unwrap();
        turns
            .finish(
                &r,
                "owner",
                TurnState::Completed,
                "one",
                None,
                Some(&completion()),
            )
            .unwrap();
        let mut next = r.clone();
        next.request_id = uuid::Uuid::new_v4().to_string();
        next.message = "two".into();
        turns.begin(&next, None, "owner", 3).unwrap();
        assert!(turns
            .stop("model", "default", &r.session_id, Some(&r.request_id))
            .unwrap()
            .is_none());
        assert!(turns
            .checkpoint_partial(&next, "owner", Some("still running"))
            .unwrap());
        let first = turns
            .history("model", "default", &r.session_id, 0, 1)
            .unwrap();
        assert!(first.has_more);
        let second = turns
            .history("model", "default", &r.session_id, first.next_after, 1)
            .unwrap();
        assert_eq!(second.turns[0].request_id, next.request_id);
        assert!(!second.has_more);
        assert!(matches!(
            turns.history("model", "other", &r.session_id, 0, 1),
            Err(Error::NotFound)
        ));
    }
}

#[cfg(test)]
mod tool_recovery_tests {
    use super::*;
    #[test]
    fn expired_tool_turn_closes_pending_calls_as_unknown_without_reexecuting_or_deleting_evidence()
    {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("recover.sqlite");
        let store = Arc::new(Store::open(&path).unwrap());
        let repo = super::super::Repository::new(store.clone());
        let s = repo.create_session("model", "default").unwrap();
        let target = Target {
            connection_id: "connection".into(),
            terminal_session_id: "terminal".into(),
        };
        let request = Request {
            request_id: "tool-turn".into(),
            agent_id: "model".into(),
            user_id: "default".into(),
            session_id: s.id.clone(),
            terminal_session_id: "terminal".into(),
            message: "check logs".into(),
            attachments: vec![],
        };
        Turns::new(store.clone())
            .begin_tools(&request, target, "old-owner", 0)
            .unwrap();
        let s = repo.session(&s.id, "default").unwrap();
        repo.append(
            &s.id,
            "default",
            s.revision,
            "call",
            Payload::ToolCall {
                call_id: "uncertain".into(),
                tool_name: "ssh_execute".into(),
                arguments: serde_json::json!({"command":"test"}),
            },
        )
        .unwrap();
        store
            .database
            .lock()
            .unwrap()
            .execute("UPDATE agent_chat_turns SET lease_until=0", [])
            .unwrap();
        drop(repo);
        drop(store);
        let reopened = Arc::new(Store::open(&path).unwrap());
        let history = Turns::new(reopened.clone())
            .history("model", "default", &s.id, 0, 20)
            .unwrap();
        assert_eq!(history.turns[0].state, TurnState::Interrupted);
        assert_eq!(
            history.turns[0].error_code.as_deref(),
            Some("AGENT_INTERRUPTED")
        );
        assert_eq!(history.turns[0].tool_events.len(), 2);
        let snapshot = super::super::Repository::new(reopened.clone())
            .snapshot(&s.id, "default")
            .unwrap();
        assert!(snapshot.pending_tool_call_ids.is_empty());
        assert_eq!(snapshot.uncertain_tool_call_ids, ["uncertain"]);
        let again = Turns::new(reopened)
            .history("model", "default", &s.id, 0, 20)
            .unwrap();
        assert_eq!(again.turns[0].tool_events.len(), 2);
    }
}
