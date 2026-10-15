//! Current-shell execution with a dedicated output PTY. The bound shell keeps its user
//! and environment; a separate SSH PTY streams only this command's terminal output.
//! Manual and background PTY output is NEVER a source of tool result bytes.
use super::{
    agent_exec::{receive_output, ExecError, ExecResult, Lease, PreparedExec, TerminalMirror},
    *,
};
use tokio::sync::{mpsc, OwnedMutexGuard};

pub(super) struct PreparedShell {
    session: Arc<Session>,
    _input: OwnedMutexGuard<()>,
    probe: mpsc::Receiver<Vec<u8>>,
    dispatched: bool,
    completed: bool,
    token: String,
}
impl PreparedShell {
    pub fn prepare(lease: &Lease) -> Result<Self, ExecError> {
        let session = lease.session.clone();
        let input = session
            .input_gate
            .clone()
            .try_lock_owned()
            .map_err(|_| ExecError::Busy)?;
        if session.input_dirty.load(Ordering::SeqCst)
            || session.shell_uncertain.load(Ordering::SeqCst)
        {
            return Err(ExecError::ShellNotReady);
        }
        let (sender, probe) = mpsc::channel(16);
        *session.shell_probe.lock().unwrap() = Some(sender);
        let token = uuid::Uuid::new_v4().simple().to_string();
        *session.shell_display.lock().unwrap() = Some(ShellDisplay::new(&token));
        Ok(Self {
            session,
            _input: input,
            probe,
            dispatched: false,
            completed: false,
            token,
        })
    }
    pub async fn run(
        mut self,
        mut exec: PreparedExec,
        command: &str,
        timeout: Duration,
        cancel: &CancellationToken,
        sink: Option<Arc<crate::agent::tool_output::Capture>>,
    ) -> Result<ExecResult, ExecError> {
        let stop = self.session.stop.clone();
        let shutdown = exec.shutdown.clone();
        let mirror = TerminalMirror::plain(self.session.clone(), exec.display_limit);
        let result = tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ExecError::Unknown),
            _ = stop.cancelled() => Err(ExecError::Unknown),
            _ = shutdown.cancelled() => Err(ExecError::Unknown),
            // `timeout` is an inactivity window, not a wall-clock deadline. A
            // long download remains alive while the SSH server keeps producing
            // output; the collector refreshes this window for every data frame.
            result = self.execute(&mut exec,command,sink.as_ref(),&mirror,timeout) => result,
        };
        let result = if self.dispatched {
            result
        } else {
            Err(ExecError::ShellNotReady)
        };
        self.completed = result.is_ok();
        mirror.finish(&result);
        result
    }
    async fn send(&self, input: String, timeout: Duration) -> Result<(), ExecError> {
        let writer = self
            .session
            .writer
            .lock()
            .unwrap()
            .clone()
            .ok_or(ExecError::Unavailable)?;
        self.session.output.lock().unwrap().last_active = Instant::now();
        tokio::time::timeout(timeout, writer.data_bytes(input.into_bytes()))
            .await
            .map_err(|_| ExecError::Unknown)?
            .map_err(|_| ExecError::Unknown)
    }
    async fn execute(
        &mut self,
        exec: &mut PreparedExec,
        command: &str,
        sink: Option<&Arc<crate::agent::tool_output::Capture>>,
        mirror: &TerminalMirror,
        timeout: Duration,
    ) -> Result<ExecResult, ExecError> {
        let token = self.token.clone();
        let marker = format!("\x1b]777;ShellPilot={token};ready\x07");
        // A password prompt/fullscreen app may echo literal probe text, but cannot
        // acknowledge it by merely echoing the backslash escapes. No business payload yet.
        self.send(
            format!("command printf '\\033]777;ShellPilot={token};ready\\007'\n"),
            timeout,
        )
        .await?;
        tokio::time::timeout(timeout, wait_probe(&mut self.probe, marker.as_bytes()))
            .await
            .map_err(|_| ExecError::ShellNotReady)??;
        self.session.shell_probe.lock().unwrap().take();
        let channel = exec.opening.0.as_mut().expect("owned collector channel");
        let size = *self.session.terminal_size.lock().unwrap();
        // Setup is bounded independently. Disabling the tool-loop total timeout
        // must not leave PTY confirmation or collector startup waiting forever.
        let target = tokio::time::timeout(timeout, async {
            channel
                .request_pty(true, "xterm-256color", size.0, size.1, 0, 0, &[])
                .await
                .map_err(|_| ExecError::Unavailable)?;
            super::confirmation(channel, &mut Vec::new(), 4096)
                .await
                .map_err(|_| ExecError::Rejected)?;
            channel
                .exec(true, terminal_collector_script(&token).as_bytes())
                .await
                .map_err(|_| ExecError::Unavailable)?;
            // Keep stdin open without forwarding keyboard input to this PTY.
            collector_ready(channel, &token).await
        })
        .await
        .map_err(|_| ExecError::Unavailable)??;
        mirror.start_with_label(command, "当前终端 Shell · 输出独立捕获");
        self.dispatched = true;
        self.send(terminal_shell_command(&token, &target, command), timeout)
            .await?;
        let session = self.session.clone();
        let result = receive_output(
            futures_util::stream::unfold((channel, size), move |(channel, mut size)| {
                let session = session.clone();
                async move {
                    loop {
                        let changed = session.size_changed.notified();
                        tokio::pin!(changed);
                        changed.as_mut().enable();
                        let latest = *session.terminal_size.lock().unwrap();
                        if latest != size {
                            channel.window_change(latest.0, latest.1, 0, 0).await.ok()?;
                            size = latest;
                        }
                        let message = tokio::select! {
                            message = channel.wait() => Some(message),
                            _ = &mut changed => None,
                        };
                        if let Some(message) = message {
                            return message.map(|message| (message, (channel, size)));
                        }
                    }
                }
            }),
            sink,
            Some(mirror),
            timeout,
        )
        .await?;
        // All result bytes have now reached the display. Release the real shell prompt
        // only after its hidden completion marker; no guessed or synthetic prompt.
        tokio::time::timeout(timeout, async {
            loop {
                let changed = self.session.changed.notified();
                tokio::pin!(changed);
                changed.as_mut().enable();
                if self
                    .session
                    .shell_display
                    .lock()
                    .unwrap()
                    .as_ref()
                    .is_some_and(|display| display.phase == DisplayPhase::Done)
                {
                    break;
                }
                changed.await;
            }
        })
        .await
        .map_err(|_| ExecError::Unknown)?;
        Ok(result)
    }
}
impl Drop for PreparedShell {
    fn drop(&mut self) {
        self.session.shell_probe.lock().unwrap().take();
        if let Some(display) = self.session.shell_display.lock().unwrap().take() {
            let mut output = self.session.output.lock().unwrap();
            if output.reason.is_none() {
                output.append(&display.deferred, display.limit);
                output.overflow |= display.overflow;
            }
            drop(output);
            self.session.changed.notify_waiters();
        }
        // A timeout/stop must not let the next tool type into an unconfirmed remote app.
        if self.dispatched && !self.completed {
            self.session.shell_uncertain.store(true, Ordering::SeqCst);
        }
    }
}
async fn wait_probe(probe: &mut mpsc::Receiver<Vec<u8>>, marker: &[u8]) -> Result<(), ExecError> {
    let mut pending = Vec::new();
    while let Some(bytes) = probe.recv().await {
        pending.extend(bytes);
        if pending.windows(marker.len()).any(|part| part == marker) {
            return Ok(());
        }
        if pending.len() > marker.len() {
            pending.drain(..pending.len() - marker.len());
        }
    }
    Err(ExecError::ShellNotReady)
}
async fn collector_ready(
    channel: &mut Channel<client::Msg>,
    token: &str,
) -> Result<CollectorTarget, ExecError> {
    let expected = format!("SHELLPILOT_READY_{token} ");
    let mut line = Vec::new();
    loop {
        match channel.wait().await {
            Some(
                ChannelMsg::Success
                | ChannelMsg::WindowAdjusted { .. }
                | ChannelMsg::XonXoff { .. },
            ) => {}
            Some(ChannelMsg::Data { data }) => {
                line.extend_from_slice(&data);
                if line.len() > 256 {
                    return Err(ExecError::Rejected);
                }
                if line.last() == Some(&b'\n') {
                    let line = std::str::from_utf8(&line).map_err(|_| ExecError::Rejected)?;
                    let path = line
                        .trim_end_matches(['\r', '\n'])
                        .strip_prefix(&expected)
                        .ok_or(ExecError::Rejected)?;
                    let (path, tty) = path.split_once(' ').ok_or(ExecError::Rejected)?;
                    if !path.starts_with("/tmp/shellpilot-tool.")
                        || path.len() > 128
                        || !path
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b"/._-".contains(&b))
                    {
                        return Err(ExecError::Rejected);
                    }
                    let number = tty.strip_prefix("/dev/pts/").ok_or(ExecError::Rejected)?;
                    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
                        return Err(ExecError::Rejected);
                    }
                    return Ok(CollectorTarget {
                        directory: path.into(),
                        tty: tty.into(),
                    });
                }
            }
            _ => return Err(ExecError::Rejected),
        }
    }
}
struct CollectorTarget {
    directory: String,
    tty: String,
}
fn terminal_collector_script(token: &str) -> String {
    format!(
        r#"# ShellPilot collector {token}
umask 077
d=$(mktemp -d /tmp/shellpilot-tool.XXXXXXXXXX) || exit 125
# Only the collector removes status, after reading it. On interruption, a late
# command-side status write is nonblocking and its failed redirection is hidden.
cleanup() {{ rm -f "$d/status.tmp" "$d/status" 2>/dev/null; rmdir "$d" 2>/dev/null || :; }}
trap cleanup EXIT
trap 'exit 125' HUP INT TERM
t=$(tty) || exit 125
case "$t" in /dev/pts/*) ;; *) exit 125;; esac
stty -echo -onlcr -tostop || exit 125
printf 'SHELLPILOT_READY_{token} %s %s\n' "$d" "$t"
while [ ! -s "$d/status" ]; do sleep 0.05; done
IFS= read -r rc < "$d/status" || exit 125
case "$rc" in ''|*[!0-9]*) exit 125;; esac
exit "$rc"
"#
    )
}
fn terminal_shell_command(token: &str, target: &CollectorTarget, command: &str) -> String {
    let encoded = encode_command(command);
    let path = &target.directory;
    let tty = &target.tty;
    // Both output descriptors are real TTYs, so Docker/curl and similar programs emit
    // their normal live progress. Stdin and eval remain in the already-bound shell.
    // The private directory remains mode 0700. Its status file contains only an
    // exit code and must be readable by the collector's SSH login user even when
    // the current shell has become root with umask 077.
    // Do not delete status here: the collector may not have read it yet.
    format!(
        r#"# ShellPilot command {token}
command printf '\033]777;ShellPilot={token};begin\007'; {{ command eval "$(command printf '%b' '{encoded}')"; }} >'{tty}' 2>&1; {{ command printf '%s\n' "$?" >'{path}/status.tmp' && command chmod 644 '{path}/status.tmp' && command mv -f '{path}/status.tmp' '{path}/status'; }} 2>/dev/null || {{ command rm -f '{path}/status.tmp' '{path}/status'; command rmdir '{path}'; }} 2>/dev/null; command printf '\033]777;ShellPilot={token};done\007'
"#
    )
}
#[cfg(test)]
fn collector_script(token: &str) -> String {
    format!(
        r#"# ShellPilot collector {token}
umask 077
d=$(mktemp -d /tmp/shellpilot-tool.XXXXXXXXXX) || exit 125
a= b=
cleanup() {{ kill "$a" "$b" 2>/dev/null || :; rm -f "$d/out" "$d/err" "$d/status"; rmdir "$d" 2>/dev/null || :; }}
trap cleanup EXIT
trap 'exit 125' HUP INT TERM
mkfifo "$d/out" "$d/err" "$d/status" || exit 125
printf 'SHELLPILOT_READY_{token} %s\n' "$d"
cat "$d/out" &
a=$!
cat "$d/err" >&2 &
b=$!
IFS= read -r rc < "$d/status" || exit 125
wait "$a"; wait "$b"
case "$rc" in ''|*[!0-9]*) exit 125;; esac
exit "$rc"
"#
    )
}
fn encode_command(command: &str) -> String {
    let mut encoded = String::new();
    for (i, byte) in command.bytes().enumerate() {
        // Readline cannot interpret user tabs/control keys; physical lines stay below PTY limits.
        if i > 0 && i % 256 == 0 {
            encoded.push_str("'\\\n'");
        }
        encoded.push_str(&format!("\\0{byte:03o}"));
    }
    encoded
}
#[cfg(test)]
fn shell_command(token: &str, path: &str, command: &str) -> String {
    let encoded = encode_command(command);
    // Braces/eval run in the EXISTING shell, preserving completed sudo -i and cd/export.
    // Redirections belong only to this command, never to earlier background/manual jobs.
    format!("# ShellPilot command {token}\ncommand printf '\\033]777;ShellPilot={token};begin\\007'; {{ command eval \"$(command printf '%b' '{encoded}')\"; }} >'{path}/out' 2>'{path}/err'; command printf '%s\\n' \"$?\" >'{path}/status'; command printf '\\033]777;ShellPilot={token};done\\007'\n")
}

#[derive(PartialEq, Eq)]
enum DisplayPhase {
    Hidden,
    Body,
    Done,
}
pub(super) struct ShellDisplay {
    phase: DisplayPhase,
    begin: Vec<u8>,
    done: Vec<u8>,
    pending: Vec<u8>,
    deferred: Vec<u8>,
    limit: usize,
    overflow: bool,
}
impl ShellDisplay {
    fn new(token: &str) -> Self {
        Self {
            phase: DisplayPhase::Hidden,
            begin: format!("\x1b]777;ShellPilot={token};begin\x07").into_bytes(),
            done: format!("\x1b]777;ShellPilot={token};done\x07").into_bytes(),
            pending: vec![],
            deferred: vec![],
            limit: 1024 * 1024,
            overflow: false,
        }
    }
    fn defer(&mut self, bytes: &[u8]) {
        self.deferred.extend_from_slice(bytes);
        if self.deferred.len() > self.limit {
            self.deferred.drain(..self.deferred.len() - self.limit);
            self.overflow = true;
        }
    }
    fn consume(&mut self, data: &[u8], limit: usize) {
        self.limit = limit;
        if self.phase == DisplayPhase::Done {
            self.defer(data);
            return;
        }
        self.pending.extend_from_slice(data);
        loop {
            let marker = if self.phase == DisplayPhase::Hidden {
                &self.begin
            } else {
                &self.done
            };
            if let Some(index) = self
                .pending
                .windows(marker.len())
                .position(|part| part == marker)
            {
                let length = marker.len();
                let before = self.pending[..index].to_vec();
                if self.phase == DisplayPhase::Body {
                    self.defer(&before);
                }
                self.pending.drain(..index + length);
                if self.phase == DisplayPhase::Hidden {
                    self.phase = DisplayPhase::Body;
                    continue;
                }
                self.phase = DisplayPhase::Done;
                let tail = std::mem::take(&mut self.pending);
                self.defer(&tail);
                break;
            }
            let keep = (1..marker.len().min(self.pending.len() + 1))
                .rev()
                .find(|n| self.pending.ends_with(&marker[..*n]))
                .unwrap_or(0);
            let safe = self.pending.len() - keep;
            let bytes = self.pending[..safe].to_vec();
            if self.phase == DisplayPhase::Body {
                self.defer(&bytes);
            }
            self.pending.drain(..safe);
            break;
        }
    }
}
pub(super) fn display_pty(session: &Session, data: &[u8], limit: usize) {
    let mut guard = session.shell_display.lock().unwrap();
    if let Some(display) = guard.as_mut() {
        display.consume(data, limit);
    } else {
        session.output.lock().unwrap().append(data, limit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_hides_protocol_echo_and_preserves_background_bytes_and_real_prompt() {
        let source="command printf '\\033]777;ShellPilot=nonce;ready\\007'\r\n\x1b]777;ShellPilot=nonce;ready\x07root@host:~# # ShellPilot command nonce\r\ncommand eval encoded > /tmp/private\r\n\x1b]777;ShellPilot=nonce;begin\x07manual background\r\n\x1b]777;ShellPilot=nonce;done\x07root@host:~# ";
        for width in [1, 2, 7, source.len()] {
            let mut display = ShellDisplay::new("nonce");
            for chunk in source.as_bytes().chunks(width) {
                display.consume(chunk, 4096);
            }
            assert!(display.phase == DisplayPhase::Done);
            assert_eq!(
                String::from_utf8(display.deferred).unwrap(),
                "manual background\r\nroot@host:~# "
            );
        }
    }
    #[tokio::test]
    async fn echoed_probe_and_manual_output_are_not_shell_acknowledgement() {
        let (sender, mut receiver) = mpsc::channel(2);
        sender
            .send(
                b"command printf '\\033]777;ShellPilot=nonce;ready\\007'\r\nmanual output".to_vec(),
            )
            .await
            .unwrap();
        drop(sender);
        assert!(matches!(
            wait_probe(&mut receiver, b"\x1b]777;ShellPilot=nonce;ready\x07").await,
            Err(ExecError::ShellNotReady)
        ));
    }
    #[test]
    fn payload_is_ascii_bounded_and_redirects_only_the_current_command() {
        let source = "汉🙂\t\n'\u{1b}".repeat(200);
        let payload = shell_command("nonce", "/tmp/shellpilot-tool.test", &source);
        assert!(payload.is_ascii());
        assert!(payload.lines().all(|line| line.len() < 4096));
        assert!(!payload.contains('\u{1b}'));
        assert!(payload
            .contains("} >'/tmp/shellpilot-tool.test/out' 2>'/tmp/shellpilot-tool.test/err'"));
    }
    #[test]
    #[ignore = "Requires a local POSIX shell and coreutils; runs temporary local FIFO fixtures only"]
    fn posix_current_shell_and_descriptor_isolation() {
        use std::io::{BufRead, Read, Write};
        use std::process::{Command, Stdio};
        let shell = if cfg!(windows) { "bash.exe" } else { "bash" };
        // Send scripts via stdin: the legacy Windows WSL launcher does not preserve
        // nested -c argument quoting the same way as a native POSIX process.
        let spawn = |script: &str| {
            let mut child = Command::new(shell)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap();
            let input=format!("exec timeout 10s sh <<'SHELLPILOT_TEST_SCRIPT_EOF'\n{script}\nSHELLPILOT_TEST_SCRIPT_EOF\n");
            child
                .stdin
                .take()
                .unwrap()
                .write_all(input.as_bytes())
                .unwrap();
            child
        };
        let token = uuid::Uuid::new_v4().simple().to_string();
        let script = collector_script(&token);
        let mut collector = spawn(&script);
        let mut output = std::io::BufReader::new(collector.stdout.take().unwrap());
        let mut ready = String::new();
        output.read_line(&mut ready).unwrap();
        let prefix = format!("SHELLPILOT_READY_{token} ");
        if !ready.starts_with(&prefix) {
            let diagnostic = collector.wait_with_output().unwrap();
            panic!(
                "collector setup failed: {:?}; {}",
                ready,
                String::from_utf8_lossy(&diagnostic.stderr)
            );
        }
        let path = ready
            .trim_end()
            .strip_prefix(&prefix)
            .expect("collector ready");
        let call="cd /tmp; export SHELLPILOT_ISOLATION_TEST=retained; printf 'AI_USER=%s;ENV=%s;PWD=%s\\n' \"$(id -un)\" \"$SHELLPILOT_ISOLATION_TEST\" \"$PWD\"; printf 'AI_STDERR\\n' >&2";
        let manual=format!("printf 'MANUAL_BEFORE\\n'; printf 'MANUAL_BACKGROUND\\n' &\n{}printf 'MANUAL_AFTER:%s:%s\\n' \"$SHELLPILOT_ISOLATION_TEST\" \"$PWD\"",shell_command(&token,path,call));
        let visible = spawn(&manual).wait_with_output().unwrap();
        assert!(visible.status.success());
        let visible = String::from_utf8(visible.stdout).unwrap();
        assert!(visible.contains("MANUAL_BEFORE"));
        assert!(visible.contains("MANUAL_AFTER:retained:/tmp"));
        let mut tool = String::new();
        output.read_to_string(&mut tool).unwrap();
        let complete = collector.wait_with_output().unwrap();
        assert!(complete.status.success());
        assert!(!tool.contains("MANUAL"));
        assert!(tool.contains("ENV=retained;PWD=/tmp"));
        let user = spawn("id -un").wait_with_output().unwrap();
        assert!(tool.contains(&format!(
            "AI_USER={};",
            String::from_utf8(user.stdout).unwrap().trim()
        )));
        assert_eq!(String::from_utf8(complete.stderr).unwrap(), "AI_STDERR\n");
    }
}
