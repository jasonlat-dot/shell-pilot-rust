use super::{client, open_channel, ChannelMsg, Error, Transport};
use std::{sync::Arc, time::Duration};

const READY: &[u8] = b"SHELLPILOT_ROOT_SFTP_READY\n";
const PASSWORD_PROMPT: &[u8] = b"SHELLPILOT_SUDO_PASSWORD:";
// Fixed command only: neither paths, usernames nor passwords are interpolated.
const ROOT_SERVER: &str = r#"test "$(id -u)" = 0 || exit 126; cd /root || exit 126; for p in /usr/lib/openssh/sftp-server /usr/libexec/openssh/sftp-server /usr/lib/ssh/sftp-server /usr/libexec/sftp-server; do if [ -x "$p" ]; then printf "SHELLPILOT_ROOT_SFTP_READY\n"; exec "$p"; fi; done; exit 127"#;

impl Transport {
    pub(crate) async fn root_sftp_stream(
        self: Arc<Self>,
        limit: Duration,
        slot: tokio::sync::OwnedSemaphorePermit,
        password: Option<&str>,
    ) -> Result<russh::ChannelStream<client::Msg>, Error> {
        let mut opening = open_channel(self.clone(), limit, slot, None).await?;
        let channel = opening.0.as_mut().unwrap();
        let command = if password.is_some() {
            format!("sudo -S -p 'SHELLPILOT_SUDO_PASSWORD:' -u root -- sh -c '{ROOT_SERVER}'")
        } else {
            format!("sudo -n -u root -- sh -c '{ROOT_SERVER}'")
        };
        let setup = async {
            channel.exec(true, command).await.map_err(|_| Error::Ssh)?;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut sent_password = false;
            loop {
                match channel.wait().await {
                    Some(ChannelMsg::Success) => {}
                    Some(ChannelMsg::Data { data }) => {
                        stdout.extend_from_slice(&data);
                        if stdout == READY {
                            return Ok(());
                        }
                        if !READY.starts_with(&stdout) {
                            return Err(root_error());
                        }
                    }
                    Some(ChannelMsg::ExtendedData { data, .. }) => {
                        if stderr.len() + data.len() > 8192 {
                            return Err(root_error());
                        }
                        stderr.extend_from_slice(&data);
                        if stderr
                            .windows(PASSWORD_PROMPT.len())
                            .any(|part| part == PASSWORD_PROMPT)
                        {
                            if sent_password {
                                return Err(root_error());
                            }
                            let password = password.ok_or_else(root_error)?;
                            // Send only when sudo actually requests it. NOPASSWD must not
                            // receive password bytes in the subsequent binary SFTP stream.
                            channel
                                .data(format!("{password}\n").as_bytes())
                                .await
                                .map_err(|_| Error::Ssh)?;
                            sent_password = true;
                            stderr.clear();
                        }
                    }
                    Some(
                        ChannelMsg::Failure
                        | ChannelMsg::Close
                        | ChannelMsg::Eof
                        | ChannelMsg::ExitStatus { .. },
                    )
                    | None => return Err(root_error()),
                    _ => {}
                }
            }
        };
        tokio::time::timeout(limit, setup)
            .await
            .map_err(|_| root_error())??;
        Ok(opening.0.take().unwrap().into_stream())
    }
}

fn root_error() -> Error {
    Error::Invalid("无法以 root 打开文件管理：请确认当前登录账号有 sudo 权限；需要密码时请填写该账号的 sudo 密码。服务器还需安装独立的 OpenSSH sftp-server，且 sudo 不能要求 TTY。也可改用 root SSH 连接。".into())
}
