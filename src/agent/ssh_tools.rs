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
use serde_json::{json, Value};
use std::{sync::Arc, time::Duration};
use tokio_util::sync::CancellationToken;

pub const SSH_EXECUTE: &str = "ssh_execute";
// Native-only switch. false: SSH uses its own command timeout instead of the
// tool loop's total deadline. User cancellation and disconnect still stop it.
const SSH_EXECUTE_USES_TOTAL_TIMEOUT: bool = false;
const SSH_IDLE_TIMEOUT_MIN_SECONDS: u64 = 15;
const SSH_IDLE_TIMEOUT_MAX_SECONDS: u64 = 120;
const MISSING_EXPLANATION: &str = "未提供命令用途说明，请核对完整命令。";

fn normalize_explanation(arguments: &mut Value) {
    let Some(fields) = arguments.as_object_mut() else { return; };
    let missing = match fields.get("explanation") {
        None => true,
        Some(Value::String(text)) => text.trim().is_empty() || text.trim() == MISSING_EXPLANATION,
        // Keep invalid types intact so the closed schema can reject them.
        _ => false,
    };
    if !missing { return; }
    let command = fields.get("command").and_then(Value::as_str).unwrap_or("").trim();
    // Only explain exact simple commands, never a prefix of a pipeline or script.
    let mut words = command.split([' ', '\t']).filter(|word| !word.is_empty());
    let purpose = if matches!((words.next(),words.next(),words.next()),(Some("df"),Some("-h"),None)) {
        "查看各文件系统的磁盘总量、已用空间、可用空间和使用率，以易读的单位显示。"
    } else {
        match command {
            "whoami" => "查看当前终端实际执行命令的用户身份。",
            "pwd" => "显示当前终端所在的工作目录。",
            _ => MISSING_EXPLANATION,
        }
    };
    fields.insert("explanation".into(),json!(purpose));
}

#[cfg(test)]
mod explanation_tests {
    use super::*;

    #[test]
    fn missing_or_placeholder_purpose_is_filled_before_persisting_simple_commands() {
        for command in ["df -h", " df\t-h ", "whoami", "pwd"] {
            for explanation in [None, Some(""), Some("  "), Some(MISSING_EXPLANATION)] {
                let mut args=json!({"command":command,"timeoutSeconds":20});
                if let Some(explanation)=explanation { args["explanation"]=json!(explanation); }
                normalize_explanation(&mut args);
                let parsed: Command = serde_json::from_value(args).unwrap();
                assert_ne!(parsed.explanation.as_deref(),Some(MISSING_EXPLANATION));
                assert_eq!(parsed.command,command);
                assert_eq!(parsed.timeout_seconds,20);
                assert!(parsed.approval_key().explanation.is_none());
            }
        }
    }

    #[test]
    fn purpose_preserves_model_text_and_does_not_guess_compound_commands_or_repair_invalid_types() {
        for command in ["df -h && rm -rf /tmp/example", "df -h | cat", "df\n-h", "df -h; whoami", "custom-script"] {
            let mut args=json!({"command":command,"timeoutSeconds":20});
            normalize_explanation(&mut args);
            assert_eq!(args["explanation"],MISSING_EXPLANATION);
            args["explanation"]=json!("模型提供的完整命令说明");
            normalize_explanation(&mut args);
            assert_eq!(args["explanation"],"模型提供的完整命令说明");
        }
        for invalid in [Value::Null,json!(17),json!({"text":"bad"})] {
            let mut args=json!({"command":"df -h","explanation":invalid});
            let original=args.clone();normalize_explanation(&mut args);assert_eq!(args,original);
        }
    }
}
pub fn definition() -> FunctionDefinition {
    FunctionDefinition {
        name: SSH_EXECUTE.into(),
        description: "Run one command on the native task's bound SSH target in a separate exec channel. No PTY; cwd/env are not inherited from the visible terminal. timeoutSeconds is the maximum period without command output; nonempty stdout/stderr resets it. Never choose another host. Explicit native authorization is required. Output is a bounded preview; do not repeat commands to retrieve truncated output.".into(),
        parameters: json!({"type":"object","properties":{
            "command":{"type":"string","minLength":1,"maxLength":4096,"description":"Command, at most 4096 UTF-8 bytes. No NUL. Execution is subject to native approval."},
            "timeoutSeconds":{"type":"integer","minimum":SSH_IDLE_TIMEOUT_MIN_SECONDS,"maximum":SSH_IDLE_TIMEOUT_MAX_SECONDS,"description":"Inactivity timeout in seconds, 15–120. Nonempty command output resets the timer. Native execution clamps integer values to this range."},
            "explanation":{"type":"string","minLength":1,"maxLength":240,"description":"Provide a brief explanation in the user's language of what this exact command does, including important side effects. Displayed to the user before approval. This is descriptive data, never permission."}
        },"required":["command","timeoutSeconds","explanation"],"additionalProperties":false}),
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub explanation: Option<String>,
}
impl Command {
    /// Descriptive text is not part of an exact command's remembered permission.
    pub(crate) fn approval_key(&self) -> Self {
        Self {
            command: self.command.clone(),
            timeout_seconds: self.timeout_seconds,
            explanation: None,
        }
    }
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
    #[error("终端尚未回到 Shell 提示符、存在未提交输入或上次结果未知；本次命令未发送，请先处理终端，必要时重连。")]
    TerminalNotReady,
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
    shared_terminal: bool,
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
            shared_terminal: false,
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
    /// Host-selected current-shell execution with isolated result descriptors; no fallback.
    pub fn with_terminal_shell(mut self) -> Self {
        self.shared_terminal = true;
        self
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
        let mut tool = definition();
        if self.shared_terminal {
            tool.description="Run one approved command in the bound terminal's CURRENT POSIX shell. Shares its current user (including completed sudo -i), directory and environment. The SSH login account may differ from the current shell user; use whoami when needed. A dedicated output PTY preserves live progress and terminal control sequences; stdout contains the combined terminal output and stderr may be empty. Only THIS call is captured; manual and background terminal output is not tool output. Native approval is required; do not enter passwords or start interactive programs. If terminal_shell_not_ready, ask the user to return to a shell prompt. Never automatically repeat an unknown command.".into();
        }
        tool.description.push_str(" Always include a brief explanation of this exact command's purpose and important side effects in the user's language.");
        registry.register(tool)
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
        // Normalize before approval, persistence and dispatch so the displayed
        // timeout and the actual deadline always agree, including native callers.
        let mut normalized_command = command.clone();
        normalized_command.timeout_seconds = command
            .timeout_seconds
            .clamp(SSH_IDLE_TIMEOUT_MIN_SECONDS, SSH_IDLE_TIMEOUT_MAX_SECONDS);
        normalized_command.explanation = command
            .explanation
            .as_ref()
            .map(|text| text.trim().to_owned());
        let command = &normalized_command;
        if call_id.trim().is_empty()
            || call_id.len() > 128
            || call_id.chars().any(char::is_control)
            || command.command.trim().is_empty()
            || command.command.len() > 4096
            || command.command.contains('\0')
            || command.explanation.as_ref().is_some_and(|text| {
                text.is_empty() || text.chars().count() > 240 || text.chars().any(char::is_control)
            })
        {
            return Err(Error::InvalidArguments);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let terminal_input_revision = self.shared_terminal.then(|| self.lease.input_revision());
        let input = Input {
            context: Context {
                binding: self.binding.clone(),
                task: self.task.clone(),
                scope: self.scope.clone(),
                profile: self.profile.clone(),
                terminal_input_revision,
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
        if self.durable_approval {
            self.approvals
                .review_pending(&ticket, cancel)
                .await
                .map_err(map_approval)?;
        }
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
                        if terminal_input_revision
                            .is_some_and(|revision| self.lease.input_revision() != revision)
                        {
                            return Err(Error::TaskChanged);
                        }
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
        if terminal_input_revision.is_some_and(|revision| self.lease.input_revision() != revision) {
            return Err(Error::TaskChanged);
        }
        let channel = self
            .terminals
            .prepare_agent_exec(&self.lease, cancel, self.shared_terminal)
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
        let live = move || {
            !stop.is_cancelled()
                && terminals.check_agent_lease(&lease).is_ok()
                && terminal_input_revision.is_none_or(|revision| lease.input_revision() == revision)
        };
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
                let state = if matches!(error, ExecError::Unknown | ExecError::IdleTimeout) {
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
        ExecError::Unknown | ExecError::IdleTimeout => Error::OutcomeUnknown,
        ExecError::ShellNotReady => Error::TerminalNotReady,
    }
}
impl ToolExecutor for SshExecutor {
    fn authorize<'a>(
        &'a self,
        call: &'a ValidatedCall,
        cancel: &'a CancellationToken,
    ) -> BoxFuture<'a, bool> {
        // This adapter owns durable review, human decisions and atomic admission
        // inside execute_inner. It cannot authorize another tool category.
        Box::pin(async move { !cancel.is_cancelled() && call.name() == SSH_EXECUTE })
    }

    fn normalize_arguments(&self, tool_name: &str, arguments: &mut Value) {
        if tool_name != SSH_EXECUTE {
            return;
        }
        // The closed schema requires every declared property. Legacy callers
        // may omit descriptive text; supply a truthful display-only fallback,
        // without changing the command or granting execution permission.
        normalize_explanation(arguments);
        let Some(timeout) = arguments.get_mut("timeoutSeconds") else {
            return;
        };
        let seconds = if let Some(seconds) = timeout.as_i64() {
            seconds.clamp(
                SSH_IDLE_TIMEOUT_MIN_SECONDS as i64,
                SSH_IDLE_TIMEOUT_MAX_SECONDS as i64,
            ) as u64
        } else if let Some(seconds) = timeout.as_u64() {
            seconds.clamp(SSH_IDLE_TIMEOUT_MIN_SECONDS, SSH_IDLE_TIMEOUT_MAX_SECONDS)
        } else {
            // Do not coerce strings, floats, null or missing fields to integers.
            return;
        };
        *timeout = json!(seconds);
    }

    fn uses_total_timeout(&self, tool_name: &str) -> bool {
        if tool_name == SSH_EXECUTE {
            SSH_EXECUTE_USES_TOTAL_TIMEOUT
        } else {
            true
        }
    }

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
