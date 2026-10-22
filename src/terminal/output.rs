use serde::Serialize;
use std::{sync::Mutex, time::Instant};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum DisconnectReason {
    IdleTimeout,
    ClientClosed,
    ChannelDisconnected,
    ReaderError,
    SessionNotFound,
}

impl DisconnectReason {
    pub fn reconnect_allowed(self) -> bool {
        matches!(self, Self::ChannelDisconnected | Self::ReaderError)
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionState {
    pub session_id: String,
    pub connection_id: Option<String>,
    pub connected: bool,
    pub disconnect_reason: Option<DisconnectReason>,
    pub reconnect_allowed: bool,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReadResult {
    pub status: &'static str,
    pub output: String,
    pub has_data: bool,
    pub connected: bool,
    pub eof: bool,
    pub timeout: bool,
    pub buffer_overflow: bool,
    pub disconnect_reason: Option<DisconnectReason>,
    pub reconnect_allowed: bool,
}

impl ReadResult {
    pub fn new(
        status: &'static str,
        output: String,
        connected: bool,
        overflow: bool,
        reason: Option<DisconnectReason>,
    ) -> Self {
        Self {
            status,
            has_data: !output.is_empty(),
            output,
            connected,
            eof: matches!(status, "DISCONNECTED" | "READER_ERROR"),
            timeout: status == "TIMEOUT",
            buffer_overflow: overflow,
            disconnect_reason: reason,
            reconnect_allowed: reason.is_some_and(DisconnectReason::reconnect_allowed),
        }
    }
}

pub(super) struct Output {
    pub text: String,
    pending: Vec<u8>,
    pub overflow: bool,
    pub reason: Option<DisconnectReason>,
    pub generation: u64,
    pub last_active: Instant,
    pub ended_at: Option<Instant>,
}

impl Output {
    pub fn new() -> Self {
        Self {
            text: String::new(),
            pending: Vec::new(),
            overflow: false,
            reason: None,
            generation: 0,
            last_active: Instant::now(),
            ended_at: None,
        }
    }
    pub fn append(&mut self, data: &[u8], limit: usize) {
        self.pending.extend_from_slice(data);
        loop {
            match std::str::from_utf8(&self.pending) {
                Ok(s) => {
                    self.text.push_str(s);
                    self.pending.clear();
                    break;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    if valid > 0 {
                        self.text.push_str(
                            std::str::from_utf8(&self.pending[..valid]).expect("valid prefix"),
                        );
                        self.pending.drain(..valid);
                    }
                    if let Some(length) = e.error_len() {
                        self.text.push('\u{fffd}');
                        self.pending.drain(..length);
                    } else {
                        break;
                    }
                }
            }
        }
        self.trim(limit);
        self.last_active = Instant::now();
    }
    /// Display-only text is already decoded; never mix it into the PTY's pending UTF-8 bytes.
    pub fn append_display(&mut self, text: &str, limit: usize) {
        self.text.push_str(text);
        self.trim(limit);
        self.last_active = Instant::now();
    }
    fn trim(&mut self, limit: usize) {
        if self.text.len() > limit {
            let mut cut = self.text.len() - limit;
            while !self.text.is_char_boundary(cut) {
                cut += 1;
            }
            self.text.drain(..cut);
            self.overflow = true;
        }
    }
}

pub(super) struct Session {
    pub id: String,
    pub connection_id: String,
    pub identity: super::AgentIdentity,
    pub transport: std::sync::Weak<super::PooledTransport>,
    pub trust_profile: (bool, Option<String>),
    pub writer: Mutex<Option<std::sync::Arc<russh::ChannelWriteHalf<russh::client::Msg>>>>,
    pub input_gate: std::sync::Arc<tokio::sync::Mutex<()>>,
    pub input_revision: std::sync::atomic::AtomicU64,
    pub input_dirty: std::sync::atomic::AtomicBool,
    pub shell_uncertain: std::sync::atomic::AtomicBool,
    pub shell_probe: Mutex<Option<tokio::sync::mpsc::Sender<Vec<u8>>>>,
    pub shell_display: Mutex<Option<super::shared_shell::ShellDisplay>>,
    pub directory_access: Mutex<super::directory_access::Reports>,
    pub terminal_size: Mutex<(u32, u32)>,
    pub size_changed: Notify,
    pub output: Mutex<Output>,
    pub resources: tokio::sync::Mutex<super::resources::MonitorState>,
    pub changed: Notify,
    pub stop: CancellationToken,
}

impl Session {
    pub fn finish(&self, reason: DisconnectReason) {
        let mut output = self.output.lock().unwrap();
        if output.reason.is_none() {
            // Flush an incomplete UTF-8 suffix without losing previously queued output.
            if !output.pending.is_empty() {
                output.text.push('\u{fffd}');
                output.pending.clear();
            }
            output.reason = Some(reason);
            output.ended_at = Some(Instant::now());
        }
        drop(output);
        self.writer.lock().unwrap().take();
        self.stop.cancel();
        self.changed.notify_waiters();
    }
    pub fn state(&self) -> ConnectionState {
        let output = self.output.lock().unwrap();
        ConnectionState {
            session_id: self.id.clone(),
            connection_id: Some(self.connection_id.clone()),
            connected: output.reason.is_none(),
            disconnect_reason: output.reason,
            reconnect_allowed: output
                .reason
                .is_some_and(DisconnectReason::reconnect_allowed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fragmented_utf8_and_overflow_preserve_character_boundaries() {
        let mut output = Output::new();
        let bytes = "你好".as_bytes();
        output.append(&bytes[..2], 8);
        assert!(output.text.is_empty());
        output.append(&bytes[2..], 8);
        assert_eq!(output.text, "你好");
        output.append("世界".as_bytes(), 8);
        assert_eq!(output.text, "世界");
        assert!(output.overflow);
        output.append(&[0xff], 8);
        assert!(output.text.ends_with('\u{fffd}'));
    }
    #[test]
    fn reconnection_policy_matches_client_contract() {
        assert!(DisconnectReason::ChannelDisconnected.reconnect_allowed());
        assert!(DisconnectReason::ReaderError.reconnect_allowed());
        assert!(!DisconnectReason::ClientClosed.reconnect_allowed());
        assert!(!DisconnectReason::IdleTimeout.reconnect_allowed());
        assert!(!DisconnectReason::SessionNotFound.reconnect_allowed());
    }
    #[test]
    fn display_mirror_does_not_corrupt_pending_pty_utf8() {
        let mut output = Output::new();
        let bytes = "你好".as_bytes();
        output.append(&bytes[..2], 1024);
        output.append_display("AI 结果\r\n", 1024);
        output.append(&bytes[2..], 1024);
        assert_eq!(output.text, "AI 结果\r\n你好");
    }
}
