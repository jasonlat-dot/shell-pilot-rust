//! D2 task-bound SSH handler. Native callers must provide trusted authorization;
//! the default is deny. D3 persists approval/admission; desktop exposure remains in D5.
use super::{
    approvals::{self, Approvals, Context, Input, Start, State},
    provider::FunctionDefinition,
    tool_loop::{ToolExecutor, ToolOutcome, ToolRegistry, ValidatedCall},
    Repository, Scope, ScopeKind, ServerIdentity, Task, ToolBinding,
};
use crate::{
    model::Connection,
    store::Store,
    terminal::{
        agent_exec::{ExecError, Lease},
        TerminalManager,
    },
};
use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub const SSH_EXECUTE: &str = "ssh_execute";
pub fn definition() -> FunctionDefinition {
    FunctionDefinition {
        name: SSH_EXECUTE.into(),
        description: "Run one command on the native task's bound SSH target in a separate exec channel. No PTY; cwd/env are not inherited from the visible terminal. Never choose another host. Explicit native authorization is required. Output is a bounded preview; do not repeat commands to retrieve truncated output.".into(),
        parameters: json!({"type":"object","properties":{
            "command":{"type":"string","minLength":1,"maxLength":4096,"description":"Command, at most 4096 UTF-8 bytes. No NUL. Execution is subject to native approval."},
            "timeoutSeconds":{"type":"integer","minimum":1,"maximum":120}
        },"required":["command","timeoutSeconds"],"additionalProperties":false}),
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SshBinding {
    pub session_id: String,
    pub user_id: String,
    pub tool: ToolBinding,
    pub identity: ServerIdentity,
}
// Deliberately not Debug: command text may contain sensitive data.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Command {
    pub command: String,
    pub timeout_seconds: u64,
}
pub struct AuthorizationRequest<'a> {
    pub binding: &'a SshBinding,
    pub call_id: &'a str,
    pub command: &'a Command,
}
pub trait Authorizer: Send + Sync {
    /// Trusted host hook, never deserialized from model arguments or summaries.
    /// Legacy native hooks supply a one-call decision, still journaled by D3. Returning true
    /// must apply to this exact task, live identity, call and command.
    fn authorize<'a>(
        &'a self,
        request: AuthorizationRequest<'a>,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, bool>;
}
struct Deny;
impl Authorizer for Deny {
    fn authorize<'a>(
        &'a self,
        _: AuthorizationRequest<'a>,
        _: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        Box::pin(async { false })
    }
}
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("SSH tool task/target does not match its native binding")]
    TargetMismatch,
    #[error("SSH tool target is unavailable")]
    TargetUnavailable,
    #[error("SSH tool task changed or stopped")]
    TaskChanged,
    #[error("SSH command arguments are invalid")]
    InvalidArguments,
    #[error("SSH execution is not authorized")]
    Denied,
    #[error("SSH tool call was cancelled before execution")]
    Cancelled,
    #[error("SSH exec channel is busy or rejected")]
    Unavailable,
    #[error("SSH execution outcome is unknown; do not automatically repeat it")]
    OutcomeUnknown,
    #[error("Execution is already preparing or awaiting approval")]
    Busy,
    #[error("Call ID conflicts with persisted execution input")]
    Conflict,
    #[error("Execution approval/storage unavailable")]
    Storage,
}
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandResult {
    pub task_id: String,
    pub connection_id: String,
    pub terminal_session_id: String,
    pub exit_code: u32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<super::OutputReference>,
    #[serde(default)]
    pub output_truncated: bool,
    #[serde(default)]
    pub output_error: bool,
}
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Profile {
    host: String,
    port: u16,
    username: String,
    user_id: String,
    strict: bool,
    known_hosts: Option<String>,
}
impl From<Connection> for Profile {
    fn from(c: Connection) -> Self {
        Self {
            host: normalize(&c.host),
            port: c.port,
            username: c.username,
            user_id: c.user_id,
            strict: c.strict_host_key_check,
            known_hosts: c.known_hosts,
        }
    }
}
fn normalize(host: &str) -> String {
    host.trim_matches(['[', ']']).to_ascii_lowercase()
}

pub struct SshExecutor {
    store: Arc<Store>,
    terminals: Arc<TerminalManager>,
    lease: Lease,
    binding: SshBinding,
    task: Task,
    scope: Option<Scope>,
    profile: Profile,
    authorizer: Arc<dyn Authorizer>,
    approvals: Approvals,
    durable_approval: bool,
}
impl SshExecutor {
    /// Reads a persisted *active targeted task*. General/offline text sessions do not
    /// acquire execution rights or adopt a newly selected tab. No SSH reconnect occurs.
    pub async fn bind(
        store: Arc<Store>,
        terminals: Arc<TerminalManager>,
        session_id: &str,
        user_id: &str,
        task_id: &str,
    ) -> Result<Self> {
        if !terminals.shares_store(&store) {
            return Err(Error::TargetMismatch);
        }
        let db_store = store.clone();
        let sid = session_id.to_owned();
        let user = user_id.to_owned();
        let (snapshot, profile) = tokio::task::spawn_blocking(move || {
            let snapshot = Repository::new(db_store.clone())
                .snapshot(&sid, &user)
                .map_err(|_| Error::TargetUnavailable)?;
            let binding =
                ToolBinding::from_snapshot(&snapshot).map_err(|_| Error::TargetMismatch)?;
            let profile = db_store
                .get(&binding.target.connection_id)
                .map(Profile::from)
                .map_err(|_| Error::TargetUnavailable)?;
            Ok::<_, Error>((snapshot, profile))
        })
        .await
        .map_err(|_| Error::TargetUnavailable)??;
        let tool = ToolBinding::from_snapshot(&snapshot).map_err(|_| Error::TargetMismatch)?;
        if tool.task_id != task_id {
            return Err(Error::TaskChanged);
        }
        let lease = terminals
            .agent_lease(
                &tool.target.terminal_session_id,
                &tool.target.connection_id,
                user_id,
            )
            .map_err(|_| Error::TargetUnavailable)?;
        let actual = lease.identity();
        let identity = ServerIdentity {
            host: normalize(&actual.host),
            port: actual.port,
            username: actual.username.clone(),
            host_key: actual.host_key.clone(),
        };
        if !lease.matches_trust(profile.strict, profile.known_hosts.as_deref())
            || profile.host != identity.host
            || profile.port != identity.port
            || profile.username != identity.username
            || profile.user_id != user_id
        {
            return Err(Error::TargetMismatch);
        }
        if let Some(scope) = &snapshot.session.scope {
            if scope.kind != ScopeKind::Server
                || scope.connection_id.as_ref() != Some(&tool.target.connection_id)
                || scope.identity.as_ref() != Some(&identity)
            {
                return Err(Error::TargetMismatch);
            }
        }
        Ok(Self {
            approvals: Approvals::new(store.clone()),
            durable_approval: false,
            store,
            terminals,
            lease,
            binding: SshBinding {
                session_id: session_id.into(),
                user_id: user_id.into(),
                tool,
                identity,
            },
            task: snapshot.session.task.ok_or(Error::TaskChanged)?,
            scope: snapshot.session.scope,
            profile,
            authorizer: Arc::new(Deny),
        })
    }
    pub fn binding(&self) -> &SshBinding {
        &self.binding
    }
    pub fn with_authorizer(mut self, authorizer: Arc<dyn Authorizer>) -> Self {
        self.authorizer = authorizer;
        self.durable_approval = false;
        self
    }
    /// Native only: enable the pending/decision workflow. No model or IPC parameter can install it.
    pub fn with_approvals(mut self, approvals: Approvals) -> Result<Self> {
        if !approvals.shares_store(&self.store) {
            return Err(Error::TargetMismatch);
        }
        self.approvals = approvals;
        self.durable_approval = true;
        Ok(self)
    }
    pub fn register(&self, registry: &mut ToolRegistry) -> super::tool_loop::Result<()> {
        registry.register(definition())
    }

    pub(crate) async fn verify(&self) -> Result<()> {
        self.terminals
            .check_agent_lease(&self.lease)
            .map_err(|_| Error::TargetUnavailable)?;
        let store = self.store.clone();
        let binding = self.binding.clone();
        let (session, profile) = tokio::task::spawn_blocking(move || {
            let session = Repository::new(store.clone())
                .session(&binding.session_id, &binding.user_id)
                .map_err(|_| Error::TargetUnavailable)?;
            let profile = store
                .get(&binding.tool.target.connection_id)
                .map(Profile::from)
                .map_err(|_| Error::TargetUnavailable)?;
            Ok::<_, Error>((session, profile))
        })
        .await
        .map_err(|_| Error::TargetUnavailable)??;
        if session.task.as_ref() != Some(&self.task) {
            return Err(Error::TaskChanged);
        }
        if session.scope != self.scope || profile != self.profile {
            return Err(Error::TargetMismatch);
        }
        self.terminals
            .check_agent_lease(&self.lease)
            .map_err(|_| Error::TargetUnavailable)
    }
    /// Native dispatch surface; not an IPC command. Authorization is host-owned; replays are read-only.
    /// Every call uses durable at-most-once admission. Replays return receipts, never execute.
    pub async fn execute_command(
        &self,
        call_id: &str,
        command: &Command,
        cancel: &CancellationToken,
    ) -> Result<CommandResult> {
        self.execute_inner(call_id, command, cancel, None, None)
            .await
    }
    pub(crate) fn store(&self) -> Arc<Store> {
        self.store.clone()
    }
    pub(crate) async fn execute_inner(
        &self,
        call_id: &str,
        command: &Command,
        cancel: &CancellationToken,
        capture: Option<Arc<super::tool_output::Capture>>,
        revision: Option<u64>,
    ) -> Result<CommandResult> {
        if call_id.trim().is_empty()
            || call_id.len() > 128
            || call_id.chars().any(char::is_control)
            || command.command.trim().is_empty()
            || command.command.len() > 4096
            || command.command.contains('\0')
            || !(1..=120).contains(&command.timeout_seconds)
        {
            return Err(Error::InvalidArguments);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let input = Input {
            context: Context {
                binding: self.binding.clone(),
                task: self.task.clone(),
                scope: self.scope.clone(),
                profile: self.profile.clone(),
            },
            command: command.clone(),
        };
        let mut ticket = match self
            .approvals
            .begin(input, call_id.into(), self.durable_approval)
            .await
            .map_err(map_approval)?
        {
            Start::Replay(result) => return Ok(result),
            Start::New(ticket) => ticket,
        };
        self.verify().await?;
        if !self
            .approvals
            .is_approved(&ticket)
            .await
            .map_err(map_approval)?
        {
            if self.durable_approval {
                let waiting = self.approvals.wait(&ticket, cancel);
                let monitor = async {
                    loop {
                        self.verify().await?;
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                };
                tokio::select! {biased;
                    _=cancel.cancelled()=>return Err(Error::Cancelled),
                    error=monitor=>{return error;},
                    approved=waiting=>approved.map_err(map_approval)?,
                }
            } else {
                let authorized = tokio::select! {
                    biased;
                    _=cancel.cancelled()=>return Err(Error::Cancelled),
                    result=self.authorizer.authorize(AuthorizationRequest{binding:&self.binding,call_id,command},cancel)=>result,
                };
                if !authorized {
                    self.approvals
                        .decide(
                            ticket.id.clone(),
                            ticket.user.clone(),
                            approvals::Decision::Deny,
                        )
                        .await
                        .map_err(map_approval)?;
                    return Err(Error::Denied);
                }
                self.approvals
                    .approve_hook(&ticket)
                    .await
                    .map_err(map_approval)?;
            }
        }
        self.verify().await?;
        let channel = self
            .terminals
            .prepare_agent_exec(&self.lease, cancel)
            .await
            .map_err(map_exec)?;
        self.verify().await?;
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if let Some(capture) = &capture {
            capture.start().await.map_err(|_| Error::Storage)?;
        }
        let terminals = self.terminals.clone();
        // The lease is not reconstructed from a tab or connection profile.
        let lease = self.lease.clone();
        let stop = cancel.clone();
        let live = move || !stop.is_cancelled() && terminals.check_agent_lease(&lease).is_ok();
        match revision {
            Some(revision) => self.approvals.admit_revision(&ticket, revision, live).await,
            None => self.approvals.admit(&ticket, live).await,
        }
        .map_err(map_approval)?;
        let result = channel
            .run(
                &command.command,
                Duration::from_secs(command.timeout_seconds),
                cancel,
                capture.clone(),
            )
            .await;
        let mut previews = None;
        let (outputs, output_truncated, output_error) = if let Some(capture) = capture {
            let sink = capture.clone();
            let outputs = tokio::task::spawn_blocking(move || sink.finish())
                .await
                .map_err(|_| Error::OutcomeUnknown)?
                .map_err(|_| Error::OutcomeUnknown)?;
            previews = Some((
                outputs
                    .iter()
                    .find(|o| o.reference.channel == super::OutputChannel::Stdout)
                    .map(|o| model_preview(&o.preview)),
                outputs
                    .iter()
                    .find(|o| o.reference.channel == super::OutputChannel::Stderr)
                    .map(|o| model_preview(&o.preview)),
            ));
            let (truncated, failed) = capture.status();
            (
                outputs.into_iter().map(|o| o.reference).collect(),
                truncated,
                failed,
            )
        } else {
            (vec![], false, false)
        };
        match result {
            Ok(result) => {
                let mut result = CommandResult {
                    task_id: self.binding.tool.task_id.clone(),
                    connection_id: self.binding.tool.target.connection_id.clone(),
                    terminal_session_id: self.binding.tool.target.terminal_session_id.clone(),
                    exit_code: result.exit_code,
                    stdout: result.stdout,
                    stderr: result.stderr,
                    truncated: result.truncated,
                    outputs,
                    output_truncated,
                    output_error,
                };
                if let Some((stdout, stderr)) = previews {
                    if let Some((text, truncated)) = stdout {
                        result.stdout = text;
                        result.truncated |= truncated;
                    }
                    if let Some((text, truncated)) = stderr {
                        result.stderr = text;
                        result.truncated |= truncated;
                    }
                    result.truncated |= output_truncated;
                }
                self.approvals
                    .finish(&mut ticket, State::Completed, Some(result.clone()))
                    .await
                    .map_err(|_| Error::OutcomeUnknown)?;
                Ok(result)
            }
            Err(error) => {
                let state = if matches!(error, ExecError::Unknown) {
                    State::Unknown
                } else {
                    State::NotSent
                };
                self.approvals
                    .finish(&mut ticket, state, None)
                    .await
                    .map_err(|_| Error::OutcomeUnknown)?;
                Err(map_exec(error))
            }
        }
    }
}
/// A5 already keeps raw head/tail previews; further bound each stream for escaped JSON.
fn model_preview(text: &str) -> (String, bool) {
    const BYTES: usize = 384;
    if text.len() <= BYTES {
        return (text.into(), false);
    }
    let mut head = BYTES / 2;
    while !text.is_char_boundary(head) {
        head -= 1;
    }
    let mut tail = text.len() - BYTES / 2;
    while !text.is_char_boundary(tail) {
        tail += 1;
    }
    (
        format!(
            "{}\n[... omitted; use read_tool_output ...]\n{}",
            &text[..head],
            &text[tail..]
        ),
        true,
    )
}
fn map_approval(error: approvals::Error) -> Error {
    match error {
        approvals::Error::Unknown => Error::OutcomeUnknown,
        approvals::Error::Busy => Error::Busy,
        approvals::Error::Conflict => Error::Conflict,
        approvals::Error::Storage => Error::Storage,
        approvals::Error::Stale => Error::TaskChanged,
        _ => Error::Denied,
    }
}
fn map_exec(error: ExecError) -> Error {
    match error {
        ExecError::Unavailable => Error::TargetUnavailable,
        ExecError::Busy | ExecError::Rejected => Error::Unavailable,
        ExecError::Unknown => Error::OutcomeUnknown,
    }
}
impl ToolExecutor for SshExecutor {
    fn execute<'a>(
        &'a self,
        call: &'a ValidatedCall,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, ToolOutcome> {
        Box::pin(async move {
            if call.name() != SSH_EXECUTE {
                return ToolOutcome::Denied;
            }
            let Ok(command) = serde_json::from_value::<Command>(call.arguments().clone()) else {
                return ToolOutcome::Denied;
            };
            match self.execute_command(call.id(), &command, cancel).await {
                Ok(result) => ToolOutcome::Success(json!(result)),
                Err(Error::OutcomeUnknown) => ToolOutcome::OutcomeUnknown,
                Err(Error::Unavailable) => ToolOutcome::Failed,
                Err(_) => ToolOutcome::Denied,
            }
        })
    }
}
