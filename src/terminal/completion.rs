//! Optional integration at SSH creation; directory names never enter model requests.
use super::*;
use russh_sftp::{client::RawSftpSession, protocol::StatusCode};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ShellIntegration {
    pub nonce: String,
    pub script: String,
}
impl ShellIntegration {
    pub(super) fn validate(&self) -> Result<(), Error> {
        if self.nonce.len() != 32
            || !self
                .nonce
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || self.script.is_empty()
            || self.script.len() > 8192
            || self.script.contains(['\r', '\n', '\0'])
        {
            return Err(Error::Invalid("补全集成参数无效".into()));
        }
        Ok(())
    }
}
pub(super) async fn install(
    channel: &mut Channel<client::Msg>,
    output: &mut Vec<u8>,
    setup: &ShellIntegration,
    cap: usize,
) -> Result<bool, Error> {
    setup.validate()?;
    let ready = format!("\x1b]777;spc;{};R\x07", setup.nonce);
    let installed = format!("\x1b]777;spc;{};I\x07", setup.nonce);
    let restored = format!("\x1b]777;spc;{};A\x07", setup.nonce);
    let restore_failed = format!("\x1b]777;spc;{};F\x07", setup.nonce);
    // Only a new shell receives this line, before user input and before any startup command.
    let loader=format!(" if [ -n \"${{BASH_VERSION-}}\" ]; then read -erst 10 -p $'\\e]777;spc;{};R\\a' __spc_setup && eval \"$__spc_setup\"; unset __spc_setup; else printf '\\033]777;spc;{};U\\007'; fi; if stty echo; then printf '\\033]777;spc;{};A\\007'; else printf '\\033]777;spc;{};F\\007'; fi\r",setup.nonce,setup.nonce,setup.nonce,setup.nonce);
    channel
        .data_bytes(loader.into_bytes())
        .await
        .map_err(|_| Error::Ssh)?;
    let mut wire = Vec::new();
    let mut sent = false;
    let operation = async {
        loop {
            match channel.wait().await {
                Some(ChannelMsg::Data { data }) | Some(ChannelMsg::ExtendedData { data, .. }) => {
                    if output.len() + data.len() > cap {
                        return Err(Error::Invalid("补全集成输出超出限制".into()));
                    }
                    output.extend_from_slice(&data);
                    wire.extend_from_slice(&data);
                    if wire
                        .windows(restore_failed.len())
                        .any(|v| v == restore_failed.as_bytes())
                    {
                        return Err(Error::Invalid(
                            "补全集成未能恢复终端输入回显，已关闭本次连接".into(),
                        ));
                    }
                    if !sent && wire.windows(ready.len()).any(|v| v == ready.as_bytes()) {
                        channel
                            .data_bytes(format!("{}\r", setup.script).into_bytes())
                            .await
                            .map_err(|_| Error::Ssh)?;
                        sent = true;
                    }
                    if wire
                        .windows(restored.len())
                        .any(|v| v == restored.as_bytes())
                    {
                        let supported = sent
                            && wire
                                .windows(installed.len())
                                .any(|v| v == installed.as_bytes());
                        // Even unsupported/read-timeout paths may be published only after echo is back.
                        return Ok(supported);
                    }
                }
                Some(ChannelMsg::Eof | ChannelMsg::Close) | None => return Err(Error::Ssh),
                _ => {}
            }
        }
    };
    // On failure do not replay payloads. Bash read restores terminal mode on its own timeout.
    tokio::time::timeout(Duration::from_secs(12), operation)
        .await
        .map_err(|_| Error::ConnectTimeout)?
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompletionEntry {
    pub name: String,
    pub directory: bool,
}
impl TerminalManager {
    pub async fn completion_entries(
        &self,
        id: &str,
        user: &str,
        path: &str,
    ) -> Result<Vec<CompletionEntry>, Error> {
        if !path.starts_with('/') || path.len() > 2048 || path.chars().any(char::is_control) {
            return Err(Error::Invalid("补全目录无效".into()));
        }
        let session = self.session(id)?;
        if session.identity.user_id != user
            || !session.state().connected
            || session.stop.is_cancelled()
        {
            return Err(Error::TerminalMissing);
        }
        let pooled = session.transport.upgrade().ok_or(Error::TerminalMissing)?;
        let _channel = pooled
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let permit = self
            .opening_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let operation = async {
            let stream = pooled
                .transport
                .clone()
                .sftp_stream(Duration::from_secs(2), permit)
                .await?;
            let raw = RawSftpSession::new_with_config(
                stream,
                russh_sftp::client::Config {
                    request_timeout_secs: 2,
                    max_packet_len: 262144,
                    ..Default::default()
                },
            );
            raw.init().await.map_err(|_| Error::Ssh)?;
            let handle = raw.opendir(path).await.map_err(|_| Error::Ssh)?.handle;
            let mut entries = Vec::new();
            loop {
                match raw.readdir(&handle).await {
                    Ok(reply) => {
                        if reply.files.is_empty() {
                            break;
                        }
                        for file in reply.files {
                            if entries.len() >= 256 {
                                break;
                            }
                            if file.filename != "."
                                && file.filename != ".."
                                && file.filename.len() <= 255
                                && !file.filename.chars().any(char::is_control)
                            {
                                entries.push(CompletionEntry {
                                    name: file.filename,
                                    directory: file.attrs.is_dir(),
                                });
                            }
                        }
                        if entries.len() >= 256 {
                            break;
                        }
                    }
                    Err(russh_sftp::client::error::Error::Status(status))
                        if status.status_code == StatusCode::Eof =>
                    {
                        break
                    }
                    Err(_) => {
                        let _ = raw.close(&handle).await;
                        return Err(Error::Ssh);
                    }
                }
            }
            let _ = raw.close(&handle).await;
            entries.sort_by(|a, b| a.name.cmp(&b.name));
            Ok(entries)
        };
        tokio::select! {biased; _=session.stop.cancelled()=>Err(Error::TerminalMissing), _=self.shutdown.cancelled()=>Err(Error::TerminalMissing), result=tokio::time::timeout(Duration::from_millis(1500),operation)=>result.map_err(|_|Error::ConnectTimeout)?}
    }
}
