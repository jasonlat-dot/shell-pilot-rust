//! Exec on the originating terminal's authenticated transport, never a profile reconnect.
use super::*;

#[derive(Clone)]
pub(crate) struct Lease {
    pub(super) session: Arc<Session>,
}
pub(crate) struct PreparedExec {
    pub(super) opening: OpeningChannel,
    pub(super) session: Arc<Session>,
    _transport: Arc<PooledTransport>,
    _slot: tokio::sync::OwnedSemaphorePermit,
    pub(super) shutdown: CancellationToken,
    pub(super) display_limit: usize,
    shared: Option<super::shared_shell::PreparedShell>,
}
#[derive(Debug, thiserror::Error)]
pub(crate) enum ExecError {
    #[error("SSH target is unavailable")]
    Unavailable,
    #[error("SSH channel capacity exhausted")]
    Busy,
    #[error("SSH server rejected the exec request")]
    Rejected,
    #[error("SSH execution outcome is unknown")]
    Unknown,
    #[error(
        "SSH command produced no output before the inactivity deadline; remote outcome is unknown"
    )]
    IdleTimeout,
    #[error("共享终端尚未就绪，请先回到 Shell 提示符；上次结果未知时需核查并重连")]
    ShellNotReady,
}
pub(crate) struct ExecResult {
    pub exit_code: u32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}
impl Lease {
    pub fn input_revision(&self) -> u64 {
        self.session.input_revision.load(Ordering::SeqCst)
    }
    pub fn matches_trust(&self, strict: bool, known_hosts: Option<&str>) -> bool {
        self.session.trust_profile.0 == strict
            && self.session.trust_profile.1.as_deref() == known_hosts
    }
    pub fn identity(&self) -> &AgentIdentity {
        &self.session.identity
    }
}
impl TerminalManager {
    pub(crate) fn shares_store(&self, store: &Arc<Store>) -> bool {
        Arc::ptr_eq(&self.store, store)
    }
    pub(crate) fn agent_lease(
        &self,
        id: &str,
        connection: &str,
        user: &str,
    ) -> Result<Lease, ExecError> {
        let session = self.session(id).map_err(|_| ExecError::Unavailable)?;
        if session.identity.connection_id != connection || session.identity.user_id != user {
            return Err(ExecError::Unavailable);
        }
        let lease = Lease { session };
        self.check_agent_lease(&lease)?;
        Ok(lease)
    }
    pub(crate) fn check_agent_lease(&self, lease: &Lease) -> Result<(), ExecError> {
        let session = &lease.session;
        if self.shutdown.is_cancelled()
            || session.stop.is_cancelled()
            || !session.state().connected
            || !self
                .session(&session.id)
                .is_ok_and(|actual| Arc::ptr_eq(&actual, session))
            || session
                .transport
                .upgrade()
                .is_none_or(|entry| entry.transport.is_closed())
        {
            return Err(ExecError::Unavailable);
        }
        Ok(())
    }
    pub(crate) async fn prepare_agent_exec(
        &self,
        lease: &Lease,
        cancel: &CancellationToken,
        shared: bool,
    ) -> Result<PreparedExec, ExecError> {
        self.check_agent_lease(lease)?;
        let shared = if shared {
            Some(super::shared_shell::PreparedShell::prepare(lease)?)
        } else {
            None
        };
        let entry = lease
            .session
            .transport
            .upgrade()
            .ok_or(ExecError::Unavailable)?;
        // Do not move to a new transport when full or rejected: the authenticated
        // target pinned by this lease is the only allowed endpoint.
        let slot = self
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExecError::Busy)?;
        let channel_slot = entry
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExecError::Busy)?;
        let opening_slot = self
            .opening_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| ExecError::Busy)?;
        let opening = tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(ExecError::Unavailable),
            _ = lease.session.stop.cancelled() => return Err(ExecError::Unavailable),
            _ = self.shutdown.cancelled() => return Err(ExecError::Unavailable),
            result = open_channel(entry.transport.clone(), Duration::from_secs(5), opening_slot, Some(channel_slot)) =>
                result.map_err(|_| ExecError::Unavailable)?,
        };
        self.check_agent_lease(lease)?;
        Ok(PreparedExec {
            opening,
            session: lease.session.clone(),
            _transport: entry,
            _slot: slot,
            shutdown: self.shutdown.clone(),
            display_limit: self.settings.buffer_bytes,
            shared,
        })
    }
}
impl PreparedExec {
    pub async fn run(
        mut self,
        command: &str,
        timeout: Duration,
        cancel: &CancellationToken,
        sink: Option<Arc<crate::agent::tool_output::Capture>>,
    ) -> Result<ExecResult, ExecError> {
        if let Some(shared) = self.shared.take() {
            return shared.run(self, command, timeout, cancel, sink).await;
        }
        if cancel.is_cancelled() || self.session.stop.is_cancelled() || self.shutdown.is_cancelled()
        {
            return Err(ExecError::Unavailable);
        }
        let mirror = TerminalMirror::new(self.session.clone(), self.display_limit);
        let operation = async {
            let channel = self.opening.0.as_mut().expect("owned exec channel");
            // Bound dispatch separately; do not wrap the entire command in a total deadline.
            tokio::time::timeout(timeout, async {
                channel
                    .exec(true, command.as_bytes())
                    .await
                    .map_err(|_| ExecError::Unknown)?;
                // No stdin/PTY: this shell is independent of the interactive terminal's cwd/env.
                channel.eof().await.map_err(|_| ExecError::Unknown)
            })
            .await
            .map_err(|_| ExecError::Unknown)??;
            mirror.start(command);
            receive_output(
                futures_util::stream::unfold(channel, |channel| async move {
                    channel.wait().await.map(|message| (message, channel))
                }),
                sink.as_ref(),
                Some(&mirror),
                timeout,
            )
            .await
        };
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ExecError::Unknown),
            _ = self.session.stop.cancelled() => Err(ExecError::Unknown),
            _ = self.shutdown.cancelled() => Err(ExecError::Unknown),
            result = operation => result,
        };
        mirror.finish(&result);
        result
        // OpeningChannel drops on every path and closes only this exec channel.
    }
}
pub(super) async fn receive_output(
    messages: impl futures_util::Stream<Item = ChannelMsg>,
    sink: Option<&Arc<crate::agent::tool_output::Capture>>,
    mirror: Option<&TerminalMirror>,
    idle_timeout: Duration,
) -> Result<ExecResult, ExecError> {
    use futures_util::StreamExt;
    tokio::pin!(messages);
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut truncated = false;
    let mut accepted = false;
    let mut exit_code = None;
    let mut deadline = tokio::time::Instant::now() + idle_timeout;
    loop {
        // Only this command's nonempty stdout/stderr refresh the deadline.
        // Flow-control, empty packets and terminal resize events do not count.
        if tokio::time::Instant::now() >= deadline {
            return Err(ExecError::IdleTimeout);
        }
        let message = tokio::time::timeout_at(deadline, messages.next())
            .await
            .map_err(|_| ExecError::IdleTimeout)?;
        match message {
            Some(ChannelMsg::Success) => accepted = true,
            Some(ChannelMsg::Failure) if !accepted && exit_code.is_none() => {
                return Err(ExecError::Rejected)
            }
            Some(ChannelMsg::Data { data }) => {
                if !data.is_empty() {
                    deadline = tokio::time::Instant::now() + idle_timeout;
                }
                if let Some(mirror) = mirror {
                    mirror.output(&data);
                }
                if let Some(sink) = &sink {
                    sink.write(false, data.to_vec()).await;
                }
                capture(&mut stdout, &stderr, &data, &mut truncated)
            }
            Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                if !data.is_empty() {
                    deadline = tokio::time::Instant::now() + idle_timeout;
                }
                if let Some(mirror) = mirror {
                    mirror.output(&data);
                }
                if let Some(sink) = &sink {
                    sink.write(true, data.to_vec()).await;
                }
                capture(&mut stderr, &stdout, &data, &mut truncated)
            }
            Some(ChannelMsg::ExitStatus { exit_status }) if exit_code.is_none() => {
                exit_code = Some(exit_status)
            }
            // russh already updates its send window before forwarding this informational
            // event. OpenSSH may send it immediately after exec, before any output.
            Some(ChannelMsg::WindowAdjusted { .. } | ChannelMsg::XonXoff { .. }) => {}
            Some(ChannelMsg::Eof) => {}
            Some(ChannelMsg::Close) | None => {
                let exit_code = exit_code.ok_or(ExecError::Unknown)?;
                return Ok(ExecResult {
                    exit_code,
                    stdout: String::from_utf8_lossy(&stdout).into_owned(),
                    stderr: String::from_utf8_lossy(&stderr).into_owned(),
                    truncated,
                });
            }
            _ => return Err(ExecError::Unknown),
        }
    }
}
/// UI output only: never call the PTY writer, send stdin, or feed these bytes back to a tool.
/// The original session Arc keeps late output pinned even if the user reconnects another tab.
pub(super) struct TerminalMirror {
    session: Arc<Session>,
    limit: usize,
    state: Mutex<MirrorState>,
    plain: bool,
}
struct MirrorState {
    started: bool,
    finished: bool,
    previous_cr: bool,
    decoder: Output,
}
impl TerminalMirror {
    pub(super) fn new(session: Arc<Session>, limit: usize) -> Self {
        Self {
            session,
            limit,
            state: Mutex::new(MirrorState {
                started: false,
                finished: false,
                previous_cr: false,
                decoder: Output::new(),
            }),
            plain: false,
        }
    }
    pub(super) fn plain(session: Arc<Session>, limit: usize) -> Self {
        let mut display = Self::new(session, limit);
        display.plain = true;
        display
    }
    fn start(&self, command: &str) {
        self.start_with_label(
            command,
            &format!("SSH 登录账号 {}", self.session.identity.username),
        );
    }
    pub(super) fn start_with_label(&self, command: &str, label: &str) {
        self.state.lock().unwrap().started = true;
        let command = command
            .chars()
            .map(|c| {
                if c.is_control() && c != '\n' && c != '\t' {
                    c.escape_default().to_string()
                } else {
                    c.to_string()
                }
            })
            .collect::<String>();
        if self.plain {
            self.output(format!("{command}\n").as_bytes());
            return;
        }
        self.output(format!("\n[AI 工具 · {label}]\n$ {command}\n").as_bytes());
    }
    fn output(&self, bytes: &[u8]) {
        let mut state = self.state.lock().unwrap();
        let mut display = Vec::with_capacity(bytes.len());
        for &byte in bytes {
            if byte == b'\n' && !state.previous_cr {
                display.push(b'\r');
            }
            display.push(byte);
            state.previous_cr = byte == b'\r';
        }
        // Decode independently of the PTY, so fragmented Unicode from either stream stays intact.
        state.decoder.append(&display, self.limit);
        let text = std::mem::take(&mut state.decoder.text);
        drop(state);
        let mut output = self.session.output.lock().unwrap();
        if output.reason.is_some() {
            return;
        }
        output.append_display(&text, self.limit);
        drop(output);
        self.session.changed.notify_waiters();
    }
    pub(super) fn finish(&self, result: &Result<ExecResult, ExecError>) {
        let started = {
            let mut state = self.state.lock().unwrap();
            state.finished = true;
            state.started
        };
        if !started {
            return;
        }
        if self.plain {
            return;
        }
        let status = match result {
            Ok(result) => format!("退出码 {}", result.exit_code),
            Err(ExecError::Rejected) => "请求被服务器拒绝".into(),
            Err(_) => "结果未知，请先核查；未自动重试".into(),
        };
        self.output(format!("\n[AI 工具 · {status}]\n").as_bytes());
    }
}
impl Drop for TerminalMirror {
    fn drop(&mut self) {
        let state = self.state.lock().unwrap();
        let (started, finished) = (state.started, state.finished);
        drop(state);
        if started && !finished && !self.plain {
            self.output("\n[AI 工具 · 结果未知，请先核查；未自动重试]\n".as_bytes());
        }
    }
}
fn capture(output: &mut Vec<u8>, other: &[u8], data: &[u8], truncated: &mut bool) {
    // The D4 sink stores raw bytes separately. Keep a bounded combined model preview,
    // continues draining, and never claims truncated data is the complete output.
    let take = data
        .len()
        .min(1024usize.saturating_sub(output.len() + other.len()));
    output.extend_from_slice(&data[..take]);
    *truncated |= take < data.len();
}

#[cfg(test)]
mod protocol_tests {
    use super::*;
    #[tokio::test]
    async fn window_updates_before_between_and_after_output_preserve_exit_code_and_streams() {
        let frames = vec![
            ChannelMsg::WindowAdjusted { new_size: 2097152 },
            ChannelMsg::XonXoff {
                client_can_do: false,
            },
            ChannelMsg::Success,
            ChannelMsg::Data {
                data: b"docker_".to_vec().into(),
            },
            ChannelMsg::WindowAdjusted { new_size: 4194304 },
            ChannelMsg::Data {
                data: b"installed=yes\n".to_vec().into(),
            },
            ChannelMsg::ExtendedData {
                data: b"service inactive\n".to_vec().into(),
                ext: 1,
            },
            ChannelMsg::ExitStatus { exit_status: 7 },
            ChannelMsg::WindowAdjusted { new_size: 4194320 },
            ChannelMsg::Eof,
            ChannelMsg::Close,
        ];
        let result = receive_output(
            futures_util::stream::iter(frames),
            None,
            None,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert_eq!(result.stdout, "docker_installed=yes\n");
        assert_eq!(result.stderr, "service inactive\n");
        assert_eq!(result.exit_code, 7);
        assert!(!result.truncated);
    }
    #[tokio::test]
    async fn flow_control_is_neither_execution_acceptance_nor_a_substitute_for_exit_status() {
        let rejected = receive_output(
            futures_util::stream::iter(vec![
                ChannelMsg::WindowAdjusted { new_size: 4096 },
                ChannelMsg::Failure,
            ]),
            None,
            None,
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(rejected, Err(ExecError::Rejected)));
        let missing = receive_output(
            futures_util::stream::iter(vec![
                ChannelMsg::Success,
                ChannelMsg::Data {
                    data: b"partial".to_vec().into(),
                },
                ChannelMsg::WindowAdjusted { new_size: 4096 },
                ChannelMsg::Eof,
                ChannelMsg::Close,
            ]),
            None,
            None,
            Duration::from_secs(1),
        )
        .await;
        assert!(matches!(missing, Err(ExecError::Unknown)));
    }
}
