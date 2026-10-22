//! Session-bound reports from the already authenticated interactive root shell.
use super::*;

#[derive(Clone, Default, PartialEq, Eq)]
pub(super) struct Access {
    pub elevated: bool,
    pub bridge: Option<String>,
    pub revision: u64,
}
pub(super) struct Reports {
    nonce: Option<String>,
    pending: Vec<u8>,
    pub access: Access,
}
impl Reports {
    pub fn new(nonce: Option<String>) -> Self {
        Self {
            nonce,
            pending: vec![],
            access: Access::default(),
        }
    }
    pub fn adopt(&mut self, nonce: String) {
        if self.nonce.as_ref() != Some(&nonce) {
            self.nonce = Some(nonce);
            self.pending.clear();
            self.access.bridge = None;
            self.access.revision = self.access.revision.wrapping_add(1);
        }
    }
    pub fn feed(&mut self, bytes: &[u8]) {
        let Some(nonce) = self.nonce.as_ref() else {
            return;
        };
        self.pending.extend_from_slice(bytes);
        while let Some(start) = self.pending.windows(6).position(|s| s == b"\x1b]777;") {
            self.pending.drain(..start);
            let Some(end) = self.pending.iter().position(|b| *b == 7) else {
                break;
            };
            let frame = String::from_utf8_lossy(&self.pending[6..end]).into_owned();
            self.pending.drain(..=end);
            let Some(data) = frame.strip_prefix(&format!("spc;{nonce};F;")) else {
                continue;
            };
            let fields: Vec<_> = data.split(';').collect();
            if fields.len() != 3
                || fields[..2]
                    .iter()
                    .any(|s| s.is_empty() || s.len() > 10 || !s.bytes().all(|b| b.is_ascii_digit()))
            {
                continue;
            }
            let elevated = fields[0] == "0" && fields[1] != "0";
            let bridge = if elevated && valid_bridge(fields[2], nonce) {
                Some(fields[2].to_owned())
            } else {
                None
            };
            if elevated != self.access.elevated || bridge != self.access.bridge {
                self.access = Access {
                    elevated,
                    bridge,
                    revision: self.access.revision.wrapping_add(1),
                };
            }
        }
        if self.pending.len() > 1024 {
            self.pending = self.pending[self.pending.len() - 6..].to_vec();
        }
    }
}
impl TerminalManager {
    pub fn directory_integration(&self, id: &str, user: &str, nonce: &str) -> Result<(), Error> {
        if nonce.len() != 32
            || !nonce
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
        {
            return Err(Error::Invalid("目录集成标识无效".into()));
        }
        let session = self.session(id)?;
        if session.identity.user_id != user || !session.state().connected {
            return Err(Error::TerminalMissing);
        }
        session
            .directory_access
            .lock()
            .unwrap()
            .adopt(nonce.to_owned());
        session.changed.notify_waiters();
        Ok(())
    }
}
fn valid_bridge(path: &str, nonce: &str) -> bool {
    path.strip_prefix(&format!("/tmp/shellpilot-files.{nonce}."))
        .is_some_and(|tail| tail.len() == 10 && tail.bytes().all(|b| b.is_ascii_alphanumeric()))
}

impl Transport {
    pub(super) async fn elevated_directory(
        self: Arc<Self>,
        bridge: &str,
        path: &str,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<Vec<u8>, Error> {
        let mut opening = open_channel(self, Duration::from_secs(2), slot, None).await?;
        let channel = opening.0.as_mut().unwrap();
        let token = uuid::Uuid::new_v4().simple().to_string();
        let encoded: String = path.as_bytes().iter().map(|b| format!("{b:02x}")).collect();
        // bridge is validated against the session nonce, token is random hex and
        // path is data encoded as hex. This collector never executes as root.
        let command = include_str!("directory_collect.sh")
            .replace("\r\n", "\n")
            .replace("__BRIDGE__", bridge)
            .replace("__REQUEST__", &token)
            .replace("__PATH__", &encoded);
        channel
            .exec(true, command.as_bytes())
            .await
            .map_err(|_| Error::Ssh)?;
        let mut output = Vec::new();
        let mut succeeded = false;
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) => {
                    if output.len() + data.len() > 1_057_000 {
                        return Err(Error::Invalid("目录响应过大".into()));
                    }
                    output.extend_from_slice(&data);
                }
                Some(ChannelMsg::ExitStatus { exit_status }) if exit_status != 0 => {
                    return Err(access_error())
                }
                Some(ChannelMsg::ExitStatus { .. }) => succeeded = true,
                Some(ChannelMsg::Close) | None => break,
                Some(ChannelMsg::Failure) => return Err(access_error()),
                _ => {}
            }
        }
        if succeeded {
            Ok(output)
        } else {
            Err(access_error())
        }
    }
}
pub(super) fn access_error() -> Error {
    Error::Invalid("当前终端的 root 目录通道不可用，请退出 root 后重新 sudo -i 或重新连接终端。服务器需要 GNU find、timeout 和命名管道支持。".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reports_are_fragmented_nonce_bound_and_revoke_on_exit() {
        let nonce = "a".repeat(32);
        let bridge = format!("/tmp/shellpilot-files.{nonce}.abcdefgh12");
        let mut reports = Reports::new(Some(nonce.clone()));
        let frame = format!("\x1b]777;spc;{nonce};F;0;1000;{bridge}\x07");
        for byte in frame.bytes() {
            reports.feed(&[byte]);
        }
        assert_eq!(reports.access.bridge.as_deref(), Some(bridge.as_str()));
        reports.feed(b"\x1b]777;spc;wrong;F;1000;1000;\x07");
        assert!(reports.access.elevated);
        reports.feed(
            format!(
                "\x1b]777;spc;{nonce};F;0;1000;/tmp/shellpilot-files.{}.abcdefgh12\x07",
                "b".repeat(32)
            )
            .as_bytes(),
        );
        assert!(reports.access.bridge.is_none());
        reports.feed(frame.as_bytes());
        reports.feed(format!("\x1b]777;spc;{nonce};F;1000;1000;\x07").as_bytes());
        assert!(!reports.access.elevated);
        assert!(reports.access.bridge.is_none());
        reports.feed(frame.as_bytes());
        let revision = reports.access.revision;
        reports.adopt("b".repeat(32));
        assert!(reports.access.bridge.is_none());
        assert!(reports.access.revision > revision);
        reports.feed(frame.as_bytes());
        assert!(
            reports.access.bridge.is_none(),
            "重新接入后不接受旧 nonce 的权限报告"
        );
    }
}
