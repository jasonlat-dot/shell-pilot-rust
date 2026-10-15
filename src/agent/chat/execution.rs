//! D5 desktop bridge. Model tools never expose any of these human authorization APIs.
use super::*;
use crate::agent::{
    approvals::{self, Decision},
    provider::ProviderKind,
    ssh_tools::SshExecutor,
    tool_loop,
    tool_session::ToolSession,
    ScopeKind, TaskRequest, ToolBinding,
};
use serde_json::{json, Value};

fn tool_error(e: tool_loop::Error) -> Error {
    match e {
        tool_loop::Error::Model(e) => e.into(),
        tool_loop::Error::Cancelled => ModelError::Cancelled.into(),
        tool_loop::Error::NeedsCompaction => Error::CompactionLimit,
        tool_loop::Error::Limit => Error::ToolLimit,
        tool_loop::Error::OutcomeUnknown => Error::ToolUnknown,
        tool_loop::Error::AlreadyStarted => Error::Conflict,
        tool_loop::Error::Context => Error::ContextChanged,
        tool_loop::Error::Invalid => Error::Invalid,
    }
}
fn approval_error(e: approvals::Error) -> Error {
    match e {
        approvals::Error::NotFound => Error::NotFound,
        approvals::Error::Storage => Error::Storage,
        approvals::Error::Invalid => Error::Invalid,
        _ => Error::ApprovalChanged,
    }
}
struct Slot<'a> {
    manager: &'a Manager,
    key: ActiveKey,
}
impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.manager.active.lock().unwrap().remove(&self.key);
    }
}
impl Manager {
    fn slot(&self, user: &str, session: &str) -> Result<Slot<'_>> {
        let key = (user.into(), session.into());
        let mut active = self.active.lock().unwrap();
        if active.len() >= 32 || active.contains_key(&key) {
            return Err(Error::Busy);
        }
        active.insert(
            key.clone(),
            Active {
                request: uuid::Uuid::new_v4().to_string(),
                token: self.stop.child_token(),
                started: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            },
        );
        Ok(Slot { manager: self, key })
    }
    async fn owned(
        &self,
        agent: String,
        user: String,
        session: String,
    ) -> Result<super::super::Session> {
        let repo = Repository::new(self.store.clone());
        worker(move || {
            let s = repo.session(&session, &user)?;
            if s.agent_id != agent {
                return Err(Error::NotFound);
            }
            Ok(s)
        })
        .await
    }
    /// Explicit user action. Rebinding is never inferred from the active terminal tab.
    pub async fn bind_execution(
        &self,
        agent: String,
        user: String,
        session: String,
        terminal: String,
    ) -> Result<super::super::Session> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        let _slot = self.slot(&user, &session)?;
        let r = Request {
            request_id: uuid::Uuid::new_v4().to_string(),
            agent_id: agent.clone(),
            user_id: user.clone(),
            session_id: session.clone(),
            terminal_session_id: terminal.clone(),
            message: "绑定 SSH 工具目标".into(),
            attachments: vec![],
        };
        let source = self.live.clone();
        let store = self.store.clone();
        worker(move||{
            source.capture_for(&r,&None)?;
            let (connection,identity)=source.server_identity(&terminal,&user)?;
            let repo=Repository::new(store.clone());
            let s=repo.session(&session,&user)?;
            if s.agent_id!=agent || s.scope.as_ref().is_none_or(|scope|scope.kind!=ScopeKind::Server || scope.connection_id.as_ref()!=Some(&connection) || scope.identity.as_ref()!=Some(&identity)){return Err(Error::TargetMismatch);}
            let running=repo.db(|db|Ok(db.query_row("SELECT EXISTS(SELECT 1 FROM agent_chat_turns WHERE session_id=?1 AND state='running')",[&session],|r|r.get::<_,bool>(0))?))?;
            if running{return Err(Error::Busy);}
            let target=Some(Target{connection_id:connection,terminal_session_id:terminal});
            if s.task.as_ref().is_some_and(|t|t.target==target && t.status==super::super::TaskStatus::Active){return Ok(s);}
            repo.start_task(&session,&user,s.revision,&uuid::Uuid::new_v4().to_string(),TaskRequest{goal:s.task.as_ref().map_or_else(||"在已绑定服务器协助用户；所有命令须明确审批".into(),|t|t.goal.clone()),constraints:s.task.map_or_else(Vec::new,|t|t.constraints),target})?;
            Ok(repo.session(&session,&user)?)
        }).await
    }
    pub async fn execution_requests(
        &self,
        agent: String,
        user: String,
        session: String,
        after: String,
    ) -> Result<Vec<approvals::Receipt>> {
        self.owned(agent, user.clone(), session.clone()).await?;
        self.approvals
            .list(session, user, after)
            .await
            .map_err(approval_error)
    }
    pub async fn execution_decision(
        &self,
        agent: String,
        user: String,
        session: String,
        id: String,
        action: String,
    ) -> Result<()> {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        self.owned(agent, user.clone(), session.clone()).await?;
        let receipt = self
            .approvals
            .receipt(id.clone(), user.clone())
            .await
            .map_err(approval_error)?;
        if receipt.binding.session_id != session {
            return Err(Error::NotFound);
        }
        match action.as_str() {
            "approveOnce" => self.approvals.decide(id, user, Decision::ApproveOnce).await,
            "approveTaskCommand" => {
                self.approvals
                    .decide(id, user, Decision::ApproveTaskCommand { seconds: 300 })
                    .await
            }
            "deny" => self.approvals.decide(id, user, Decision::Deny).await,
            "cancel" => self.approvals.cancel_pending(id, user).await,
            "revoke" => self.approvals.revoke_grant(id, user).await,
            _ => return Err(Error::Invalid),
        }
        .map_err(approval_error)
    }
    pub async fn execution_policy(
        &self,
        agent: String,
        user: String,
        session: String,
        mode: Option<(u64, approvals::Policy)>,
    ) -> Result<Value> {
        let s = self.owned(agent, user.clone(), session.clone()).await?;
        let task = s.task.ok_or(Error::Invalid)?.id;
        if let Some((revision, mode)) = mode {
            self.approvals
                .set_policy(session.clone(), user.clone(), task.clone(), revision, mode)
                .await
                .map_err(approval_error)?;
        }
        let (mode, revision) = self
            .approvals
            .policy(session, user, task)
            .await
            .map_err(approval_error)?;
        Ok(json!({"mode":mode,"revision":revision}))
    }
    pub async fn execution_output(
        &self,
        agent: String,
        user: String,
        session: String,
        id: String,
        offset: u64,
        limit: usize,
    ) -> Result<Value> {
        self.owned(agent, user.clone(), session.clone()).await?;
        if !(1..=65536).contains(&limit) {
            return Err(Error::Invalid);
        }
        let repo = Repository::new(self.store.clone());
        worker(move||{
            let output=repo.output(&session,&user,&id)?;
            if output.event_id.is_none(){return Err(Error::NotFound);}
            let range=repo.read_output_range(&session,&user,&id,offset,limit)?;
            Ok(json!({"bytes":range.bytes,"offset":range.offset,"nextOffset":range.next_offset,"eof":range.eof,"size":range.size,"channel":output.reference.channel}))
        }).await
    }
    pub async fn chat_desktop<F, Fut>(
        &self,
        request: Request,
        target: Option<Target>,
        execution_enabled: bool,
        cancel: &CancellationToken,
        send: F,
    ) -> Result<()>
    where
        F: FnMut(StreamEvent) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if execution_enabled {
            self.chat_tools(request, cancel, send).await
        } else {
            self.chat(request, target, cancel, send).await
        }
    }
    async fn chat_tools<F, Fut>(
        &self,
        request: Request,
        cancel: &CancellationToken,
        mut send: F,
    ) -> Result<()>
    where
        F: FnMut(StreamEvent) -> Fut,
        Fut: Future<Output = Result<()>>,
    {
        if self.stop.is_cancelled() {
            return Err(Error::Stopped);
        }
        validate(&request)?;
        if cancel.is_cancelled() {
            return Err(ModelError::Cancelled.into());
        }
        if let Some(turn) = self.receipt(request.clone()).await? {
            if turn.state == TurnState::Completed {
                return send(StreamEvent::Done {
                    content: turn.result_text.ok_or(Error::Storage)?,
                })
                .await;
            }
            return Err(recorded(&turn));
        }
        let token = self.stop.child_token();
        let owner = uuid::Uuid::new_v4().to_string();
        let key = (request.user_id.clone(), request.session_id.clone());
        let started = Arc::new(std::sync::atomic::AtomicBool::new(false));
        {
            let mut active = self.active.lock().unwrap();
            if active.len() >= 32 || active.contains_key(&key) {
                return Err(Error::Busy);
            }
            active.insert(
                key.clone(),
                Active {
                    request: request.request_id.clone(),
                    token: token.clone(),
                    started: started.clone(),
                },
            );
        }
        let heartbeat = CancellationToken::new();
        let mut running = Running {
            manager: self,
            key,
            token: token.clone(),
            heartbeat: heartbeat.clone(),
            request: request.clone(),
            owner: owner.clone(),
            partial: Arc::new(Mutex::new(String::new())),
            finished: true,
            observation: None,
        };
        let terminals = self.terminals.clone().ok_or(Error::TargetUnavailable)?;
        let repo = Repository::new(self.store.clone());
        let r = request.clone();
        let snapshot = worker(move || {
            let s = repo.snapshot(&r.session_id, &r.user_id)?;
            if s.session.agent_id != r.agent_id {
                return Err(Error::NotFound);
            }
            if s.session
                .scope
                .as_ref()
                .is_none_or(|s| s.kind != ScopeKind::Server)
            {
                return Err(Error::TargetMismatch);
            }
            let binding = ToolBinding::from_snapshot(&s).map_err(|_| Error::TargetMismatch)?;
            if binding.target.terminal_session_id != r.terminal_session_id {
                return Err(Error::TargetMismatch);
            }
            Ok(s)
        })
        .await?;
        let binding = ToolBinding::from_snapshot(&snapshot).map_err(|_| Error::TargetMismatch)?;
        let executor = SshExecutor::bind(
            self.store.clone(),
            terminals,
            &request.session_id,
            &request.user_id,
            &binding.task_id,
        )
        .await
        .map_err(|e| match e {
            crate::agent::ssh_tools::Error::TargetUnavailable => Error::TargetUnavailable,
            _ => Error::TargetMismatch,
        })?
        .with_approvals(self.approvals.clone())
        .map_err(|_| Error::Storage)?;
        let models = ModelRepository::new(self.store.clone());
        let r = request.clone();
        let runtime = worker(move || Ok(models.runtime(&r.agent_id, &r.user_id)?)).await?;
        if runtime.config.provider != ProviderKind::OpenAiCompatible
            || !runtime.config.capabilities.tool_calls
        {
            return Err(Error::ToolsUnsupported);
        }
        ToolSession::preflight(&snapshot, executor.binding(), &request.message, &runtime)
            .map_err(tool_error)?;
        if token.is_cancelled() || cancel.is_cancelled() {
            return Err(ModelError::Cancelled.into());
        }
        let turns = Turns::new(self.store.clone());
        let r = request.clone();
        let who = owner.clone();
        let target = binding.target;
        let initial_revision = snapshot.session.revision;
        let admission =
            worker(move || turns.begin_tools(&r, target, &who, initial_revision)).await?;
        if !admission.1 {
            return Err(recorded(&admission.0));
        }
        running.finished = false;
        started.store(true, std::sync::atomic::Ordering::Release);
        use crate::agent::context_status::{Observation, Reporter, Update};
        let store = self.store.clone();
        let r = request.clone();
        let observation = worker(move || Observation::begin(store, &r)).await?;
        running.observation = Some(observation.clone());
        let (reporter, mut updates) = Reporter::channel(observation.clone());
        let (tx, mut events) = tokio::sync::mpsc::channel(64);
        let session =
            ToolSession::new(executor).managed(request.clone(), owner.clone(), tx, reporter);
        // Native tool execution is nonstreaming at the provider; Channel emits real tool/result events.
        let operation = session.run(
            &request.request_id,
            &request.message,
            tool_loop::Limits {
                tool_timeout: std::time::Duration::from_secs(300),
                ..Default::default()
            },
            &token,
        );
        tokio::pin!(operation);
        let mut tick = tokio::time::interval(std::time::Duration::from_secs(1));
        let result = loop {
            tokio::select! {biased;
                _=cancel.cancelled(),if !token.is_cancelled()=>{token.cancel();},
                Some(update)=updates.recv()=>forward_context(&observation,update,&mut send).await?,
                Some(event)=events.recv()=>send(event).await?,
                result=&mut operation=>break result.map_err(tool_error),
                _=tick.tick()=>{
                    let turns=Turns::new(self.store.clone());let r=request.clone();let who=owner.clone();let observed=observation.clone();
                    if !worker(move||{observed.heartbeat()?;turns.checkpoint_partial(&r,&who,None)}).await?{token.cancel();}
                }
            }
        };
        while let Ok(event) = events.try_recv() {
            send(event).await?;
        }
        while let Ok(update) = updates.try_recv() {
            forward_context(&observation, update, &mut send).await?;
        }
        let code = if token.is_cancelled() || cancel.is_cancelled() {
            Some("AGENT_CANCELLED".into())
        } else {
            result.as_ref().err().map(|e| e.code().to_owned())
        };
        forward_context(&observation, Update::Finished(code.clone()), &mut send).await?;
        let text = result
            .as_ref()
            .ok()
            .and_then(|r| r.content.clone().or_else(|| r.refusal.clone()))
            .unwrap_or_default();
        let state = if token.is_cancelled() || cancel.is_cancelled() {
            TurnState::Cancelled
        } else if result.is_ok() {
            TurnState::Completed
        } else {
            TurnState::Failed
        };
        let expected = session.revision().unwrap_or(initial_revision + 1);
        let turns = Turns::new(self.store.clone());
        let r = request.clone();
        let who = owner.clone();
        let internal_cancel = token.clone();
        let external_cancel = cancel.clone();
        let turn = worker(move || {
            let (state, code) = if internal_cancel.is_cancelled() || external_cancel.is_cancelled()
            {
                (TurnState::Cancelled, Some("AGENT_CANCELLED".into()))
            } else {
                (state, code)
            };
            turns.finish_tools(&r, &who, state, &text, code.as_deref(), expected)
        })
        .await?;
        running.finished = true;
        if turn.state == TurnState::Completed {
            send(StreamEvent::Done {
                content: turn.result_text.ok_or(Error::Storage)?,
            })
            .await
        } else {
            Err(recorded(&turn))
        }
    }
}
