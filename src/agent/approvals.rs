//! Native-only approval, exact task-command grants and at-most-once dispatch admission.
//! There is no remote exactly-once guarantee: an admitted request without a durable
//! result is uncertain and can never be reclaimed for automatic execution.
pub(crate) mod reviewer;
use super::{
    repository::read_session,
    ssh_tools::{Command, CommandResult, Profile, SshBinding},
    Scope, Task, TaskStatus,
};
use crate::{model::Connection, store::Store};
use rusqlite::{params, Connection as Database, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::{sync::Arc, time::Duration};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Approval request not found")]
    NotFound,
    #[error("Approval request no longer matches its task or target")]
    Stale,
    #[error("Execution call ID already belongs to different input")]
    Conflict,
    #[error("Approval has expired")]
    Expired,
    #[error("Execution has been denied or cancelled")]
    Denied,
    #[error("Execution is already being prepared")]
    Busy,
    #[error("Execution outcome is unknown; do not repeat automatically")]
    Unknown,
    #[error("Approval storage unavailable")]
    Storage,
    #[error("Invalid approval input")]
    Invalid,
}
pub type Result<T> = std::result::Result<T, Error>;
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::Storage
    }
}
impl From<serde_json::Error> for Error {
    fn from(_: serde_json::Error) -> Self {
        Self::Storage
    }
}
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Policy {
    Deny,
    Confirm,
    Auto,
    FullAccess,
}
impl Policy {
    fn text(self) -> &'static str {
        match self {
            Self::Deny => "deny",
            Self::Confirm => "confirm",
            Self::Auto => "auto",
            Self::FullAccess => "fullAccess",
        }
    }
    fn from_text(mode: &str) -> Self {
        match mode {
            "deny" => Self::Deny,
            "auto" => Self::Auto,
            "fullAccess" => Self::FullAccess,
            _ => Self::Confirm,
        }
    }
}
#[derive(Clone, Copy)]
pub enum Decision {
    Deny,
    ApproveOnce,
    ApproveTaskCommand { seconds: u32 },
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum State {
    Pending,
    Approved,
    Denied,
    Cancelled,
    Expired,
    Admitted,
    Completed,
    NotSent,
    Unknown,
}
impl State {
    fn parse(s: &str) -> Result<Self> {
        serde_json::from_value(serde_json::Value::String(s.into())).map_err(Into::into)
    }
    fn text(&self) -> Result<String> {
        Ok(serde_json::to_value(self)?
            .as_str()
            .ok_or(Error::Storage)?
            .into())
    }
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Context {
    pub binding: SshBinding,
    pub task: Task,
    pub scope: Option<Scope>,
    pub profile: Profile,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_input_revision: Option<u64>,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Input {
    pub context: Context,
    pub command: Command,
}
/// No Debug: the displayed command/output can contain user secrets. Never log this.
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Grant {
    pub expires_at: i64,
    pub revoked: bool,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Receipt {
    pub id: String,
    pub call_id: String,
    pub binding: SshBinding,
    pub task: Task,
    pub command: Command,
    pub state: State,
    pub result: Option<CommandResult>,
    pub created_at: i64,
    pub expires_at: i64,
    pub grant: Option<Grant>,
    pub review: Option<Review>,
}
#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Review {
    pub mode: Policy,
    pub decision: String,
    pub risk: String,
    pub reason: String,
}
struct Row {
    id: String,
    call: String,
    input: Input,
    state: State,
    result: Option<CommandResult>,
    created: i64,
    expires: i64,
    grant: Option<Grant>,
    review: Option<Review>,
}
impl Row {
    fn receipt(self) -> Receipt {
        Receipt {
            id: self.id,
            call_id: self.call,
            binding: self.input.context.binding,
            task: self.input.context.task,
            command: self.input.command,
            state: self.state,
            result: self.result,
            created_at: self.created,
            expires_at: self.expires,
            grant: self.grant,
            review: self.review,
        }
    }
}
fn row(db: &Database, id: &str, user: &str) -> Result<Row> {
    let raw=db.query_row("SELECT id,call_id,request_json,state,result_json,created_at,expires_at FROM agent_execution_requests WHERE id=?1 AND user_id=?2",params![id,user],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,i64>(5)?,r.get::<_,i64>(6)?))).optional()?.ok_or(Error::NotFound)?;
    let grant = db
        .query_row(
            "SELECT expires_at,revoked FROM agent_execution_grants WHERE request_id=?1",
            [id],
            |r| {
                Ok(Grant {
                    expires_at: r.get(0)?,
                    revoked: r.get(1)?,
                })
            },
        )
        .optional()?;
    let review = db.query_row(
        "SELECT mode,decision,risk,reason FROM agent_execution_policy_reviews WHERE request_id=?1",
        [id],
        |r| Ok(Review { mode: Policy::from_text(&r.get::<_, String>(0)?), decision:r.get(1)?, risk:r.get(2)?, reason:r.get(3)? }),
    ).optional()?;
    Ok(Row {
        review,
        grant,
        id: raw.0,
        call: raw.1,
        input: serde_json::from_str(&raw.2)?,
        state: State::parse(&raw.3)?,
        result: raw.4.map(|s| serde_json::from_str(&s)).transpose()?,
        created: raw.5,
        expires: raw.6,
    })
}
fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}
fn current(db: &Database, c: &Context) -> Result<()> {
    let binding = &c.binding;
    let s = read_session(db, &binding.session_id, &binding.user_id).map_err(|_| Error::NotFound)?;
    if s.task.as_ref() != Some(&c.task)
        || s.scope != c.scope
        || c.task.status != TaskStatus::Active
        || c.task.id != binding.tool.task_id
        || c.task.target.as_ref() != Some(&binding.tool.target)
    {
        return Err(Error::Stale);
    }
    let json: String = db
        .query_row(
            "SELECT metadata FROM ssh_connections WHERE id=?1",
            [&binding.tool.target.connection_id],
            |r| r.get(0),
        )
        .optional()?
        .ok_or(Error::Stale)?;
    let p = Profile::from(serde_json::from_str::<Connection>(&json)?);
    if p != c.profile {
        return Err(Error::Stale);
    }
    Ok(())
}
fn policy(db: &Database, c: &Context) -> Result<Policy> {
    let mode = db
        .query_row(
            "SELECT mode FROM agent_execution_policies WHERE session_id=?1 AND task_id=?2",
            params![c.binding.session_id, c.task.id],
            |r| r.get::<_, String>(0),
        )
        .optional()?;
    Ok(mode
        .as_deref()
        .map(Policy::from_text)
        .unwrap_or(Policy::Confirm))
}
#[derive(Clone)]
pub struct Approvals {
    store: Arc<Store>,
    changed: Arc<Notify>,
    lifetime: Duration,
}
impl Approvals {
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            changed: Arc::new(Notify::new()),
            lifetime: Duration::from_secs(300),
        }
    }
    pub fn with_lifetime(store: Arc<Store>, lifetime: Duration) -> Result<Self> {
        if lifetime.as_millis() == 0 || lifetime > Duration::from_secs(300) {
            return Err(Error::Invalid);
        }
        Ok(Self {
            lifetime,
            ..Self::new(store)
        })
    }
    pub(crate) fn shares_store(&self, store: &Arc<Store>) -> bool {
        Arc::ptr_eq(&self.store, store)
    }
    async fn db<T: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Database) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = store.database.lock().map_err(|_| Error::Storage)?;
            f(&mut db)
        })
        .await
        .map_err(|_| Error::Storage)?
    }
    pub async fn policy(
        &self,
        session: String,
        user: String,
        task: String,
    ) -> Result<(Policy, u64)> {
        self.read_policy(session, user, task, true).await
    }
    /// Configuration remains readable after a failed/stopped turn. This does
    /// not authorize execution; policy() and admission still require an active task.
    pub(crate) async fn policy_view(
        &self,
        session: String,
        user: String,
        task: String,
    ) -> Result<(Policy, u64)> {
        self.read_policy(session, user, task, false).await
    }
    async fn read_policy(
        &self,
        session: String,
        user: String,
        task: String,
        require_active: bool,
    ) -> Result<(Policy, u64)> {
        self.db(move|db|{
            let s=read_session(db,&session,&user).map_err(|_|Error::NotFound)?;
            if !s.task.as_ref().is_some_and(|t|t.id==task&&(!require_active||t.status==TaskStatus::Active)){return Err(Error::Stale);}
            let row=db.query_row("SELECT task_id,mode,revision FROM agent_execution_policies WHERE session_id=?1",[session],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,u64>(2)?))).optional()?;
            Ok(match row{Some((id,mode,rev))=>(if id==task{Policy::from_text(&mode)}else{Policy::Confirm},rev),None=>(Policy::Confirm,0)})
        }).await
    }
    /// Task-wide tool policy, shared by native tool authorization adapters.
    /// Human-facing native host only; it never changes a tool's pinned target.
    pub async fn set_policy(
        &self,
        session: String,
        user: String,
        task: String,
        expected: u64,
        mode: Policy,
    ) -> Result<u64> {
        let result=self.db(move|db|{
   let tx=db.transaction()?;
   let s=read_session(&tx,&session,&user).map_err(|_|Error::NotFound)?;
   let active=s.task.as_ref().filter(|t|t.id==task).ok_or(Error::Stale)?.status==TaskStatus::Active;
   let rev:u64=tx.query_row("SELECT revision FROM agent_execution_policies WHERE session_id=?1",[&session],|r|r.get(0)).optional()?.unwrap_or(0);
   if rev!=expected{return Err(Error::Conflict);}let next=rev.checked_add(1).ok_or(Error::Invalid)?;
   tx.execute("INSERT INTO agent_execution_policies(session_id,task_id,mode,revision) VALUES(?1,?2,?3,?4) ON CONFLICT(session_id) DO UPDATE SET task_id=excluded.task_id,mode=excluded.mode,revision=excluded.revision",
    params![session,task,mode.text(),next])?;
   // Pending work adopts the new policy. Admitted commands are never interrupted.
   // Return unconsumed automatic/grant approvals to pending before retiring their
   // provenance; an explicit one-call human approval remains valid unless denied.
   tx.execute("UPDATE agent_execution_requests SET state='pending',grant_id=NULL,updated_at=?2 WHERE session_id=?1 AND state='approved' AND (grant_id IS NOT NULL OR id IN (SELECT request_id FROM agent_execution_policy_reviews WHERE decision='allow'))",params![session,now()])?;
   tx.execute("UPDATE agent_execution_grants SET revoked=1 WHERE session_id=?1",[&session])?;
   tx.execute("DELETE FROM agent_execution_policy_reviews WHERE request_id IN (SELECT id FROM agent_execution_requests WHERE session_id=?1 AND state='pending')",[&session])?;
   if mode==Policy::FullAccess&&active {
    tx.execute("INSERT INTO agent_execution_policy_reviews(request_id,mode,revision,decision,risk,reason) SELECT id,'fullAccess',?3,'allow','notReviewed','当前任务已由用户选择完全访问权限' FROM agent_execution_requests WHERE session_id=?1 AND task_id=?2 AND state='pending' AND expires_at>?4",params![session,task,next,now()])?;
    tx.execute("UPDATE agent_execution_requests SET state='approved',updated_at=?3 WHERE session_id=?1 AND task_id=?2 AND state='pending' AND expires_at>?3",params![session,task,now()])?;
   }
   if mode==Policy::Deny{tx.execute("UPDATE agent_execution_requests SET state='denied',updated_at=?2 WHERE session_id=?1 AND state IN ('pending','approved')",params![session,now()])?;}
   tx.commit()?;Ok(next)
  }).await;
        self.changed.notify_waiters();
        result
    }
    /// Only a trusted human-facing host may call this; the model registry never exposes it.
    pub async fn decide(&self, id: String, user: String, decision: Decision) -> Result<()> {
        let result=self.db(move|db|{
   let tx=db.transaction()?;let old=row(&tx,&id,&user)?;
   if old.state!=State::Pending{return Err(Error::Conflict);}
   if old.expires<=now(){return Err(Error::Expired);}
   current(&tx,&old.input.context)?;
   if policy(&tx,&old.input.context)?==Policy::Deny{return Err(Error::Denied);}
   let state=match decision{Decision::Deny=>"denied",_=>"approved"};
   if let Decision::ApproveTaskCommand{seconds}=decision{
    if !(1..=3600).contains(&seconds){return Err(Error::Invalid);}
    tx.execute("INSERT INTO agent_execution_grants(request_id,session_id,task_id,context_json,command_json,expires_at) VALUES(?1,?2,?3,?4,?5,?6)",
     params![id,old.input.context.binding.session_id,old.input.context.task.id,serde_json::to_string(&old.input.context)?,serde_json::to_string(&old.input.command.approval_key())?,now()+i64::from(seconds)*1000])?;
   }
   tx.execute("UPDATE agent_execution_requests SET state=?2,updated_at=?3 WHERE id=?1 AND state='pending'",params![id,state,now()])?;
   tx.commit()?;Ok(())
  }).await;
        self.changed.notify_waiters();
        result
    }
    pub async fn revoke_grant(&self, id: String, user: String) -> Result<()> {
        let result = self
            .db(move |db| {
                let tx = db.transaction()?;
                let _ = row(&tx, &id, &user)?;
                tx.execute(
                    "UPDATE agent_execution_grants SET revoked=1 WHERE request_id=?1",
                    [&id],
                )?;
                tx.commit()?;
                Ok(())
            })
            .await;
        self.changed.notify_waiters();
        result
    }
    /// Withdraw an outstanding request, including after restart. Does not terminate remote processes.
    pub async fn cancel_pending(&self, id: String, user: String) -> Result<()> {
        let result=self.db(move|db|{
            let tx=db.transaction()?;let r=row(&tx,&id,&user)?;
            if !matches!(r.state,State::Pending|State::Approved){return Err(Error::Conflict);}
            tx.execute("UPDATE agent_execution_requests SET state='cancelled',updated_at=?2 WHERE id=?1",params![id,now()])?;
            tx.commit()?;Ok(())
        }).await;
        self.changed.notify_waiters();
        result
    }
    /// Bounded native history; admitted without a committed result means possible execution.
    pub async fn receipt(&self, id: String, user: String) -> Result<Receipt> {
        self.db(move|db|{let mut r=row(db,&id,&user)?;
   if matches!(r.state,State::Pending|State::Approved)&&r.expires<=now(){
    db.execute("UPDATE agent_execution_requests SET state='expired',updated_at=?2 WHERE id=?1 AND state IN ('pending','approved')",params![id,now()])?;r.state=State::Expired;
   }Ok(r.receipt())}).await
    }
    pub async fn list(&self, session: String, user: String, after: String) -> Result<Vec<Receipt>> {
        self.db(move|db|{
   read_session(db,&session,&user).map_err(|_|Error::NotFound)?;
   let cursor=if after.is_empty(){None}else{Some(db.query_row("SELECT rowid FROM agent_execution_requests WHERE id=?1 AND session_id=?2 AND user_id=?3",params![after,session,user],|r|r.get::<_,i64>(0)).optional()?.ok_or(Error::NotFound)?)};
   db.execute("UPDATE agent_execution_requests SET state='expired',updated_at=?3 WHERE session_id=?1 AND user_id=?2 AND state IN ('pending','approved') AND expires_at<=?3",params![session,user,now()])?;
   let mut q=db.prepare("SELECT id FROM agent_execution_requests WHERE session_id=?1 AND user_id=?2 AND (?3 IS NULL OR rowid<?3) ORDER BY rowid DESC LIMIT 50")?;
   let ids=q.query_map(params![session,user,cursor],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
   ids.into_iter().map(|id|Ok(row(db,&id,&user)?.receipt())).collect()
  }).await
    }
    pub(crate) async fn begin(
        &self,
        input: Input,
        call: String,
        allow_grants: bool,
    ) -> Result<Start> {
        let lifetime = self.lifetime.as_millis() as i64;
        let store = self.store.clone();
        self.db(move|db|{
   let tx=db.transaction()?;
   let old=tx.query_row("SELECT id FROM agent_execution_requests WHERE session_id=?1 AND call_id=?2",params![input.context.binding.session_id,call],|r|r.get::<_,String>(0)).optional()?;
   if let Some(id)=old{
    let old=row(&tx,&id,&input.context.binding.user_id)?;
    if old.input!=input{return Err(Error::Conflict);}
    return match old.state{State::Completed=>old.result.map(Start::Replay).ok_or(Error::Storage),State::Admitted|State::Unknown=>Err(Error::Unknown),State::Pending|State::Approved=>Err(Error::Busy),_=>Err(Error::Denied)};
   }
   current(&tx,&input.context)?;
   // Changing call IDs or timeouts is not a recovery strategy for an uncertain side effect.
   let uncertain:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_execution_requests WHERE session_id=?1 AND task_id=?2 AND json_extract(request_json,'$.command.command')=?3 AND state IN ('admitted','unknown'))",
    params![input.context.binding.session_id,input.context.task.id,input.command.command],|r|r.get(0))?;
   if uncertain{return Err(Error::Unknown);}
   let mode=policy(&tx,&input.context)?;
   let denied=mode==Policy::Deny;
   let full_access=allow_grants&&mode==Policy::FullAccess;
   let grant:Option<String>=if denied||!allow_grants{None}else{tx.query_row("SELECT request_id FROM agent_execution_grants WHERE session_id=?1 AND task_id=?2 AND context_json=?3 AND command_json=?4 AND revoked=0 AND expires_at>?5 ORDER BY rowid DESC LIMIT 1",
    params![input.context.binding.session_id,input.context.task.id,serde_json::to_string(&input.context)?,serde_json::to_string(&input.command.approval_key())?,now()],|r|r.get(0)).optional()?};
   let id=uuid::Uuid::new_v4().to_string();let state=if denied{"denied"}else if full_access||grant.is_some(){"approved"}else{"pending"};
   tx.execute("INSERT INTO agent_execution_requests(id,session_id,user_id,task_id,call_id,request_json,state,created_at,expires_at,updated_at,grant_id) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?8,?10)",
    params![id,input.context.binding.session_id,input.context.binding.user_id,input.context.task.id,call,serde_json::to_string(&input)?,state,now(),now()+lifetime,grant])?;
   if full_access {
    let revision:u64=tx.query_row("SELECT revision FROM agent_execution_policies WHERE session_id=?1 AND task_id=?2",params![input.context.binding.session_id,input.context.task.id],|r|r.get(0))?;
    tx.execute("INSERT INTO agent_execution_policy_reviews(request_id,mode,revision,decision,risk,reason) VALUES(?1,'fullAccess',?2,'allow','notReviewed','当前任务已由用户选择完全访问权限')",params![id,revision])?;
   }
   tx.commit()?;
   if denied{return Err(Error::Denied);}
   Ok(Start::New(Ticket{store,id,user:input.context.binding.user_id,armed:true}))
  }).await
    }
    pub(crate) async fn wait(&self, ticket: &Ticket, cancel: &CancellationToken) -> Result<()> {
        loop {
            let notified = self.changed.notified();
            let id = ticket.id.clone();
            let user = ticket.user.clone();
            self.db(move |db| {
                let r = row(db, &id, &user)?;
                current(db, &r.input.context)
            })
            .await?;
            // A policy change can move an outstanding request into auto review.
            // The reviewer itself retires stale decisions when the mode changes.
            self.review_pending(ticket, cancel).await?;
            let receipt = self.receipt(ticket.id.clone(), ticket.user.clone()).await?;
            match receipt.state {
                State::Approved => return Ok(()),
                State::Pending => {}
                State::Expired => return Err(Error::Expired),
                _ => return Err(Error::Denied),
            }
            tokio::select! {biased; _=cancel.cancelled()=>return Err(Error::Denied),_=notified=>{},_=tokio::time::sleep(Duration::from_millis(100))=>{}}
        }
    }
    pub(crate) async fn approve_hook(&self, ticket: &Ticket) -> Result<()> {
        self.decide(
            ticket.id.clone(),
            ticket.user.clone(),
            Decision::ApproveOnce,
        )
        .await
    }
    pub(crate) async fn is_approved(&self, ticket: &Ticket) -> Result<bool> {
        Ok(self
            .receipt(ticket.id.clone(), ticket.user.clone())
            .await?
            .state
            == State::Approved)
    }
    /// Commit before sending the SSH exec request. No path can claim this call a second time.
    pub(crate) async fn admit(
        &self,
        ticket: &Ticket,
        live: impl FnOnce() -> bool + Send + 'static,
    ) -> Result<()> {
        self.admit_checked(ticket, None, live).await
    }
    /// D4's raw event frontier must still describe the approved plan at dispatch admission.
    pub(crate) async fn admit_revision(
        &self,
        ticket: &Ticket,
        revision: u64,
        live: impl FnOnce() -> bool + Send + 'static,
    ) -> Result<()> {
        self.admit_checked(ticket, Some(revision), live).await
    }
    async fn admit_checked(
        &self,
        ticket: &Ticket,
        revision: Option<u64>,
        live: impl FnOnce() -> bool + Send + 'static,
    ) -> Result<()> {
        let id = ticket.id.clone();
        let user = ticket.user.clone();
        self.db(move|db|{
   let tx=db.transaction()?;let old=row(&tx,&id,&user)?;
   if let Some(revision)=revision {
    let binding=&old.input.context.binding;
    if read_session(&tx,&binding.session_id,&binding.user_id).map_err(|_|Error::Stale)?.revision!=revision {
     return Err(Error::Stale);
    }
   }
   let binding=&old.input.context.binding;
   let desktop:Option<(String,bool,i64)>=tx.query_row("SELECT t.state,t.cancel_requested,t.lease_until FROM agent_chat_turns t JOIN agent_tool_turns k USING(session_id,request_id) WHERE t.session_id=?1 AND t.user_event_id<(SELECT id FROM agent_events WHERE session_id=?1 AND call_id=?2 AND kind='tool_call') ORDER BY t.user_event_id DESC LIMIT 1",params![binding.session_id,old.call],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
   if desktop.is_some_and(|(state,cancel,lease)|state!="running"||cancel||lease<now()){return Err(Error::Denied);}
   let steering_pending:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_turn_inputs i JOIN agent_chat_turns t USING(session_id,request_id) WHERE i.session_id=?1 AND i.event_id IS NULL AND t.state='running')",[&binding.session_id],|r|r.get(0))?;
   if steering_pending{return Err(Error::Denied);}
   if old.state!=State::Approved{return Err(Error::Denied);}
   if old.expires<=now(){return Err(Error::Expired);}
   current(&tx,&old.input.context)?;
   if policy(&tx,&old.input.context)?==Policy::Deny{return Err(Error::Denied);}
   let source:Option<String>=tx.query_row("SELECT grant_id FROM agent_execution_requests WHERE id=?1",[&id],|r|r.get(0))?;
   let automatic:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_execution_policy_reviews WHERE request_id=?1 AND decision='allow')",[&id],|r|r.get(0))?;
   if automatic {
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_execution_policy_reviews r JOIN agent_execution_policies p ON p.session_id=?2 AND p.task_id=?3 AND p.mode=r.mode AND p.revision=r.revision WHERE r.request_id=?1 AND r.decision='allow')",
     params![id,binding.session_id,old.input.context.task.id],|r|r.get(0))?;
    if !valid{return Err(Error::Denied);}
   }
   if let Some(source)=source{
    let valid:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_execution_grants WHERE request_id=?1 AND revoked=0 AND expires_at>?2)",params![source,now()],|r|r.get(0))?;
    if !valid{return Err(Error::Denied);}
   }
   let another_uncertain:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM agent_execution_requests WHERE session_id=?1 AND task_id=?2 AND id<>?3 AND json_extract(request_json,'$.command.command')=?4 AND state IN ('admitted','unknown'))",
    params![old.input.context.binding.session_id,old.input.context.task.id,id,old.input.command.command],|r|r.get(0))?;
   if another_uncertain{return Err(Error::Unknown);}
   if !live(){return Err(Error::Stale);}
   tx.execute("UPDATE agent_execution_requests SET state='admitted',updated_at=?2 WHERE id=?1 AND state='approved'",params![id,now()])?;
   tx.commit()?;Ok(())
  }).await
    }
    pub(crate) async fn finish(
        &self,
        ticket: &mut Ticket,
        state: State,
        result: Option<CommandResult>,
    ) -> Result<()> {
        let id = ticket.id.clone();
        let user = ticket.user.clone();
        let state = state.text()?;
        self.db(move|db|{
   let changed=db.execute("UPDATE agent_execution_requests SET state=?3,result_json=?4,updated_at=?5 WHERE id=?1 AND user_id=?2 AND state='admitted'",params![id,user,state,result.map(|r|serde_json::to_string(&r)).transpose()?,now()])?;
   if changed!=1{return Err(Error::Unknown);}Ok(())
  }).await?;
        ticket.armed = false;
        Ok(())
    }
}
pub(crate) enum Start {
    New(Ticket),
    Replay(CommandResult),
}
/// Dropping a future cannot leave a reusable permit. After admission, be conservative.
pub(crate) struct Ticket {
    store: Arc<Store>,
    pub id: String,
    pub user: String,
    armed: bool,
}
impl Drop for Ticket {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        if let Ok(db) = self.store.database.lock() {
            let _=db.execute("UPDATE agent_execution_requests SET state=CASE WHEN state='admitted' THEN 'unknown' ELSE 'cancelled' END,updated_at=?3 WHERE id=?1 AND user_id=?2 AND state IN ('pending','approved','admitted')",params![self.id,self.user,now()]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        agent::{Repository, Target, TaskRequest, ToolBinding},
        model::ConnectionRequest,
    };
    fn setup(path: &std::path::Path) -> (Arc<Store>, Input) {
        let store = Arc::new(Store::open(path).unwrap());
        let connection = store
            .save(
                ConnectionRequest {
                    connection_name: "approval fixture".into(),
                    host: "127.0.0.1".into(),
                    port: 22,
                    username: "tester".into(),
                    user_id: Some("default".into()),
                    strict_host_key_check: Some(false),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        let repo = Repository::new(store.clone());
        let session = repo.create_session("model", "default").unwrap();
        let target = Target {
            connection_id: connection.connection_id.clone(),
            terminal_session_id: "original-pty".into(),
        };
        repo.start_task(
            &session.id,
            "default",
            0,
            "task",
            TaskRequest {
                goal: "original".into(),
                constraints: vec!["keep".into()],
                target: Some(target.clone()),
            },
        )
        .unwrap();
        let task = repo.session(&session.id, "default").unwrap().task.unwrap();
        let input = Input {
            context: Context {
                binding: SshBinding {
                    session_id: session.id,
                    user_id: "default".into(),
                    tool: ToolBinding {
                        task_id: task.id.clone(),
                        target,
                    },
                    identity: super::super::ServerIdentity {
                        host: connection.host.clone(),
                        port: 22,
                        username: connection.username.clone(),
                        host_key: "SHA256:fixture".into(),
                    },
                },
                task,
                scope: None,
                profile: Profile::from(connection),
                terminal_input_revision: None,
            },
            command: Command {
                command: "unchanged command".into(),
                timeout_seconds: 2,
                explanation: None,
            },
        };
        (store, input)
    }
    async fn ticket(service: &Approvals, input: Input, call: &str) -> Ticket {
        match service.begin(input, call.into(), true).await.unwrap() {
            Start::New(ticket) => ticket,
            _ => panic!("new approval expected"),
        }
    }
    async fn mode(service: &Approvals, input: &Input, revision: u64, policy: Policy) -> u64 {
        service
            .set_policy(
                input.context.binding.session_id.clone(),
                input.context.binding.user_id.clone(),
                input.context.task.id.clone(),
                revision,
                policy,
            )
            .await
            .unwrap()
    }
    fn verdict(decision: &str, risk: &str) -> Review {
        Review {
            mode: Policy::Auto,
            decision: decision.into(),
            risk: risk.into(),
            reason: "test review".into(),
        }
    }
    #[tokio::test]
    async fn full_access_is_durable_scoped_and_revoked_before_admission() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("modes.sqlite");
        let (store, input) = setup(&path);
        let service = Approvals::new(store.clone());
        let revision = mode(&service, &input, 0, Policy::FullAccess).await;
        let t = ticket(&service, input.clone(), "full").await;
        assert!(service.is_approved(&t).await.unwrap());
        assert_eq!(
            service
                .receipt(t.id.clone(), t.user.clone())
                .await
                .unwrap()
                .review
                .unwrap()
                .mode
                .text(),
            "fullAccess"
        );
        let reopened = Approvals::new(Arc::new(Store::open(&path).unwrap()));
        assert_eq!(
            reopened
                .policy(
                    input.context.binding.session_id.clone(),
                    t.user.clone(),
                    input.context.task.id.clone()
                )
                .await
                .unwrap()
                .0
                .text(),
            "fullAccess"
        );
        assert!(service
            .set_policy(
                input.context.binding.session_id.clone(),
                t.user.clone(),
                input.context.task.id.clone(),
                0,
                Policy::Confirm
            )
            .await
            .is_err());
        mode(&service, &input, revision, Policy::Confirm).await;
        assert!(service.admit(&t, || true).await.is_err());
        let next = ticket(&service, input, "manual").await;
        assert!(!service.is_approved(&next).await.unwrap());
    }
    #[tokio::test]
    async fn auto_review_only_low_risk_passes_and_policy_changes_retire_approval() {
        let dir = tempfile::tempdir().unwrap();
        let (store, input) = setup(&dir.path().join("review.sqlite"));
        let service = Approvals::new(store.clone());
        let revision = mode(&service, &input, 0, Policy::Auto).await;
        let session_revision = Repository::new(store)
            .session(
                &input.context.binding.session_id,
                &input.context.binding.user_id,
            )
            .unwrap()
            .revision;
        let low = ticket(&service, input.clone(), "low").await;
        assert!(!service.is_approved(&low).await.unwrap());
        service
            .commit_review(&low, revision, session_revision, verdict("allow", "low"))
            .await
            .unwrap();
        assert!(service.is_approved(&low).await.unwrap());
        let risky = ticket(&service, input.clone(), "risk").await;
        service
            .commit_review(&risky, revision, session_revision, verdict("ask", "high"))
            .await
            .unwrap();
        assert!(!service.is_approved(&risky).await.unwrap());
        let late = ticket(&service, input.clone(), "late").await;
        mode(&service, &input, revision, Policy::Confirm).await;
        assert!(service
            .commit_review(&late, revision, session_revision, verdict("allow", "low"))
            .await
            .is_err());
        assert!(service.admit(&low, || true).await.is_err());
        // A human can still approve the previously flagged exact command.
        service
            .decide(risky.id.clone(), risky.user.clone(), Decision::ApproveOnce)
            .await
            .unwrap();
        service.admit(&risky, || true).await.unwrap();
    }
    #[tokio::test]
    async fn missing_review_model_falls_back_to_human_and_never_autoapproves() {
        let dir = tempfile::tempdir().unwrap();
        let (store, input) = setup(&dir.path().join("fallback.sqlite"));
        let service = Approvals::new(store);
        mode(&service, &input, 0, Policy::Auto).await;
        let t = ticket(&service, input, "fallback").await;
        service
            .review_pending(&t, &CancellationToken::new())
            .await
            .unwrap();
        let receipt = service.receipt(t.id.clone(), t.user.clone()).await.unwrap();
        assert_eq!(receipt.state, State::Pending);
        assert_eq!(receipt.review.unwrap().decision, "ask");
        assert!(service.admit(&t, || true).await.is_err());
    }
    #[tokio::test]
    async fn isolated_reviewer_roundtrip_has_no_tools_and_persists_low_and_high_risk_results() {
        use axum::{extract::State as HttpState, routing::post, Json, Router};
        use crate::agent::provider::{ModelConfig, ModelConfigUpdate, ModelRepository, KeyChange};
        use serde_json::{json, Value};
        let requests=Arc::new(std::sync::Mutex::new(Vec::<Value>::new()));
        async fn review(HttpState(requests):HttpState<Arc<std::sync::Mutex<Vec<Value>>>>,Json(request):Json<Value>)->Json<Value>{
            let evidence:Value=serde_json::from_str(request["messages"][1]["content"].as_str().unwrap()).unwrap();
            let low=evidence["command"]["command"]=="whoami";
            requests.lock().unwrap().push(request);
            Json(json!({"id":"review","model":"reviewer-test","choices":[{"index":0,"message":{"role":"assistant","content":json!({"decision":if low{"allow"}else{"ask"},"risk":if low{"low"}else{"high"},"reason":if low{"只读查询"}else{"删除数据需要确认"}}).to_string()},"finish_reason":"stop"}]}))
        }
        let listener=tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address=listener.local_addr().unwrap();
        let router=Router::new().route("/v1/chat/completions",post(review)).with_state(requests.clone());
        let server=tokio::spawn(async move { axum::serve(listener,router).await.unwrap(); });
        let dir=tempfile::tempdir().unwrap();
        let (store,mut input)=setup(&dir.path().join("roundtrip.sqlite"));
        let config:ModelConfig=serde_json::from_value(json!({
            "provider":"openAiCompatible","baseUrl":format!("http://{address}/v1/"),"completionsPath":"chat/completions","model":"reviewer-test",
            "contextWindow":8192,"outputReserve":512,"safetyReserve":256,"compactPercent":75,
            "outputTokenParameter":"maxTokens","capabilities":{"toolCalls":false,"developerRole":false,"temperature":false,"reasoningEffort":false,"vision":false},
            "temperature":null,"reasoningEffort":null,"timeoutSeconds":3,"maxResponseBytes":16384,"requireApiKey":false
        })).unwrap();
        ModelRepository::new(store.clone()).save("model","default",0,ModelConfigUpdate{config,api_key:KeyChange::Keep}).unwrap();
        let service=Approvals::new(store);
        mode(&service,&input,0,Policy::Auto).await;
        input.command.command="whoami".into();
        let low=ticket(&service,input.clone(),"review-low").await;
        service.review_pending(&low,&CancellationToken::new()).await.unwrap();
        assert!(service.is_approved(&low).await.unwrap());
        input.command.command="rm -rf /server-data".into();
        let high=ticket(&service,input,"review-high").await;
        service.review_pending(&high,&CancellationToken::new()).await.unwrap();
        assert!(!service.is_approved(&high).await.unwrap());
        service.review_pending(&high,&CancellationToken::new()).await.unwrap();
        let recorded=requests.lock().unwrap();
        assert_eq!(recorded.len(),2);
        for request in recorded.iter() {
            assert!(request.get("tools").is_none() || request["tools"].as_array().is_some_and(|tools|tools.is_empty()));
            assert_eq!(request["messages"].as_array().unwrap().len(),2);
            assert!(request["messages"][0]["content"].as_str().unwrap().contains("independent SSH command approval reviewer"));
        }
        drop(recorded);
        server.abort();
    }
    #[tokio::test]
    async fn durable_admission_without_result_is_never_reclaimed_after_restart_even_with_new_call_id(
    ) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sqlite");
        let (store, input) = setup(&path);
        let service = Approvals::new(store.clone());
        let mut ticket = ticket(&service, input.clone(), "call").await;
        service
            .decide(
                ticket.id.clone(),
                ticket.user.clone(),
                Decision::ApproveOnce,
            )
            .await
            .unwrap();
        assert!(service.admit(&ticket, || false).await.is_err());
        assert_eq!(
            service
                .receipt(ticket.id.clone(), ticket.user.clone())
                .await
                .unwrap()
                .state,
            State::Approved
        );
        service.admit(&ticket, || true).await.unwrap();
        let id = ticket.id.clone();
        ticket.armed = false;
        drop(ticket);
        drop(service);
        drop(store);
        let reopened = Approvals::new(Arc::new(Store::open(&path).unwrap()));
        assert_eq!(
            reopened.receipt(id, "default".into()).await.unwrap().state,
            State::Admitted
        );
        assert!(matches!(
            reopened.begin(input.clone(), "call".into(), true).await,
            Err(Error::Unknown)
        ));
        assert!(matches!(
            reopened.begin(input, "new-id".into(), true).await,
            Err(Error::Unknown)
        ));
    }
    #[tokio::test]
    async fn expired_or_revoked_grant_and_policy_changes_cannot_admit_an_autoapproved_call() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sqlite");
        let (store, input) = setup(&path);
        let service = Approvals::new(store);
        let first = ticket(&service, input.clone(), "first").await;
        service
            .decide(
                first.id.clone(),
                first.user.clone(),
                Decision::ApproveTaskCommand { seconds: 60 },
            )
            .await
            .unwrap();
        let next = ticket(&service, input.clone(), "next").await;
        let id = first.id.clone();
        service
            .db(move |db| {
                db.execute(
                    "UPDATE agent_execution_grants SET expires_at=0 WHERE request_id=?1",
                    [id],
                )?;
                Ok(())
            })
            .await
            .unwrap();
        assert!(matches!(
            service.admit(&next, || true).await,
            Err(Error::Denied)
        ));
        let third = ticket(&service, input.clone(), "third").await;
        assert_eq!(
            service
                .receipt(third.id.clone(), third.user.clone())
                .await
                .unwrap()
                .state,
            State::Pending
        );
        service
            .decide(
                third.id.clone(),
                third.user.clone(),
                Decision::ApproveTaskCommand { seconds: 60 },
            )
            .await
            .unwrap();
        let fourth = ticket(&service, input.clone(), "fourth").await;
        service
            .set_policy(
                input.context.binding.session_id.clone(),
                "default".into(),
                input.context.task.id.clone(),
                0,
                Policy::Confirm,
            )
            .await
            .unwrap();
        assert!(matches!(
            service.admit(&fourth, || true).await,
            Err(Error::Denied)
        ));
    }
    #[tokio::test]
    async fn denied_policy_is_persisted_and_scoped_to_the_current_task() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sqlite");
        let (store, input) = setup(&path);
        let service = Approvals::new(store.clone());
        service
            .set_policy(
                input.context.binding.session_id.clone(),
                "default".into(),
                input.context.task.id.clone(),
                0,
                Policy::Deny,
            )
            .await
            .unwrap();
        let reopened = Approvals::new(Arc::new(Store::open(&path).unwrap()));
        assert!(matches!(
            reopened.begin(input.clone(), "denied".into(), true).await,
            Err(Error::Denied)
        ));
        let mut changed = input.clone();
        let repo = Repository::new(store);
        let s = repo
            .session(&input.context.binding.session_id, "default")
            .unwrap();
        repo.start_task(
            &s.id,
            "default",
            s.revision,
            "new-task",
            TaskRequest {
                goal: "other".into(),
                constraints: vec![],
                target: s.task.as_ref().unwrap().target.clone(),
            },
        )
        .unwrap();
        changed.context.task = repo.session(&s.id, "default").unwrap().task.unwrap();
        changed.context.binding.tool.task_id = changed.context.task.id.clone();
        let t = ticket(&reopened, changed, "different-task").await;
        assert_eq!(
            reopened
                .receipt(t.id.clone(), t.user.clone())
                .await
                .unwrap()
                .state,
            State::Pending
        );
    }
    #[tokio::test]
    async fn separate_database_handles_cannot_claim_the_same_request_twice() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sqlite");
        let (store, input) = setup(&path);
        let first = Approvals::new(store);
        let second = Approvals::new(Arc::new(Store::open(&path).unwrap()));
        let (a, b) = tokio::join!(
            first.begin(input.clone(), "same".into(), true),
            second.begin(input, "same".into(), true)
        );
        assert_eq!(
            usize::from(matches!(a, Ok(Start::New(_))))
                + usize::from(matches!(b, Ok(Start::New(_)))),
            1
        );
    }
    #[tokio::test]
    async fn schema_eleven_upgrade_preserves_original_events_and_context_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sqlite");
        let (store, input) = setup(&path);
        let repo = Repository::new(store.clone());
        let before = repo
            .snapshot(&input.context.binding.session_id, "default")
            .unwrap();
        let raw = repo.events(&before.session.id, "default", 0, 20).unwrap();
        drop(repo);
        drop(store);
        let db = Database::open(&path).unwrap();
        db.execute_batch("DROP TABLE agent_execution_grants; DROP TABLE agent_execution_policies; DROP TABLE agent_execution_requests; PRAGMA user_version=11;").unwrap();
        drop(db);
        let store = Arc::new(Store::open(&path).unwrap());
        let repo = Repository::new(store.clone());
        assert_eq!(
            repo.snapshot(&before.session.id, "default").unwrap(),
            before
        );
        assert_eq!(
            repo.events(&before.session.id, "default", 0, 20).unwrap(),
            raw
        );
        let service = Approvals::new(store);
        let t = ticket(&service, input, "upgraded").await;
        assert_eq!(
            service
                .receipt(t.id.clone(), t.user.clone())
                .await
                .unwrap()
                .state,
            State::Pending
        );
        assert_eq!(
            service
                .db(|db| Ok(db.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))?))
                .await
                .unwrap(),
            16
        );
    }
}
