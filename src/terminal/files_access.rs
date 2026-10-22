use super::*;

/// Captures one terminal's authenticated identity, never another tab's sudo state.
#[derive(Clone)]
pub struct TerminalFileAccess {
    session: Arc<Session>,
    access: directory_access::Access,
    pub(crate) transport: Arc<Transport>,
    _pooled: Arc<PooledTransport>,
}
impl TerminalManager {
    pub fn file_access(
        &self,
        id: &str,
        connection: &str,
        user: &str,
    ) -> Result<TerminalFileAccess, Error> {
        let session = self.session(id)?;
        if session.connection_id != connection
            || session.identity.user_id != user
            || !session.state().connected
        {
            return Err(Error::TerminalMissing);
        }
        let pooled = session.transport.upgrade().ok_or(Error::TerminalMissing)?;
        let access = session.directory_access.lock().unwrap().access.clone();
        if access.elevated && access.bridge.is_none() {
            return Err(directory_access::access_error());
        }
        Ok(TerminalFileAccess {
            session,
            access,
            transport: pooled.transport.clone(),
            _pooled: pooled,
        })
    }
}
impl TerminalFileAccess {
    pub(crate) fn elevated(&self) -> bool {
        self.access.elevated
    }
    pub(crate) fn username(&self) -> &str {
        &self.session.identity.username
    }
    pub(crate) fn connection_id(&self) -> &str {
        &self.session.connection_id
    }
    pub(crate) fn stop(&self) -> CancellationToken {
        self.session.stop.child_token()
    }
    pub(crate) fn valid(&self) -> bool {
        !self.session.stop.is_cancelled()
            && !self.identity_changed()
    }
    pub(crate) fn client_closed(&self) -> bool {
        self.session.state().disconnect_reason == Some(DisconnectReason::ClientClosed)
    }
    pub(crate) fn identity_changed(&self) -> bool {
        self.session.directory_access.lock().unwrap().access != self.access
    }
    pub(crate) async fn invalidated(&self) {
        loop {
            let changed = self.session.changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            if !self.valid() { return }
            tokio::select! { _ = self.session.stop.cancelled() => return, _ = changed => {} }
        }
    }
    pub(crate) async fn stream(
        &self,
        limit: Duration,
        slot: tokio::sync::OwnedSemaphorePermit,
    ) -> Result<russh::ChannelStream<client::Msg>, Error> {
        if !self.valid() {
            return Err(Error::TerminalMissing);
        }
        if !self.elevated() {
            return self.transport.clone().sftp_stream(limit, slot).await;
        }
        let bridge = self
            .access
            .bridge
            .as_deref()
            .ok_or_else(directory_access::access_error)?;
        let token = uuid::Uuid::new_v4().simple().to_string();
        let command = include_str!("files_collect.sh")
            .replace("\r\n", "\n")
            .replace("__BRIDGE__", bridge)
            .replace("__REQUEST__", &token);
        let mut opening = open_channel(self.transport.clone(), limit, slot, None).await?;
        let channel = opening.0.as_mut().unwrap();
        let setup = async {
            channel
                .exec(true, command.as_bytes())
                .await
                .map_err(|_| Error::Ssh)?;
            let ready = b"SHELLPILOT_TERMINAL_SFTP_READY\n";
            let mut output = Vec::new();
            loop {
                match channel.wait().await {
                    Some(ChannelMsg::Data { data }) => {
                        output.extend_from_slice(&data);
                        if output == ready { return Ok(()) }
                        if !ready.starts_with(&output) { return Err(Error::Ssh) }
                    }
                    Some(ChannelMsg::Failure | ChannelMsg::Eof | ChannelMsg::Close | ChannelMsg::ExitStatus { .. }) | None => return Err(Error::Invalid("无法复用终端文件权限，请确认服务器安装了 OpenSSH sftp-server，并退出 root 后重新 sudo -i".into())),
                    _ => {}
                }
            }
        };
        tokio::select! { biased;
            _ = self.invalidated() => return Err(Error::TerminalMissing),
            result = tokio::time::timeout(limit, setup) => result.map_err(|_| Error::ConnectTimeout)??,
        }
        Ok(opening.0.take().unwrap().into_stream())
    }
}
