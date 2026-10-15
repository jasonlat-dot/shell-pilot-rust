//! Exec on the originating terminal's authenticated transport, never a profile reconnect.
use super::*;

#[derive(Clone)]
pub(crate) struct Lease {
    session: Arc<Session>,
}
pub(crate) struct PreparedExec {
    opening: OpeningChannel,
    session: Arc<Session>,
    _transport: Arc<PooledTransport>,
    _slot: tokio::sync::OwnedSemaphorePermit,
    shutdown: CancellationToken,
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
}
pub(crate) struct ExecResult {
    pub exit_code: u32,
    pub stdout: String,
    pub stderr: String,
    pub truncated: bool,
}
impl Lease {
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
    ) -> Result<PreparedExec, ExecError> {
        self.check_agent_lease(lease)?;
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
        })
    }
}
impl PreparedExec {
    pub async fn run(
        mut self,
        command: &str,
        timeout: Duration,
        cancel: &CancellationToken,
    ) -> Result<ExecResult, ExecError> {
        if cancel.is_cancelled() || self.session.stop.is_cancelled() || self.shutdown.is_cancelled()
        {
            return Err(ExecError::Unavailable);
        }
        let operation = async {
            let channel = self.opening.0.as_mut().expect("owned exec channel");
            channel
                .exec(true, command.as_bytes())
                .await
                .map_err(|_| ExecError::Unknown)?;
            // No stdin/PTY: this shell is independent of the interactive terminal's cwd/env.
            channel.eof().await.map_err(|_| ExecError::Unknown)?;
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            let mut truncated = false;
            let mut accepted = false;
            let mut exit_code = None;
            loop {
                match channel.wait().await {
                    Some(ChannelMsg::Success) => accepted = true,
                    Some(ChannelMsg::Failure) if !accepted && exit_code.is_none() => {
                        return Err(ExecError::Rejected)
                    }
                    Some(ChannelMsg::Data { data }) => {
                        capture(&mut stdout, &stderr, &data, &mut truncated)
                    }
                    Some(ChannelMsg::ExtendedData { data, ext: 1 }) => {
                        capture(&mut stderr, &stdout, &data, &mut truncated)
                    }
                    Some(ChannelMsg::ExitStatus { exit_status }) if exit_code.is_none() => {
                        exit_code = Some(exit_status)
                    }
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
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(ExecError::Unknown),
            _ = self.session.stop.cancelled() => Err(ExecError::Unknown),
            _ = self.shutdown.cancelled() => Err(ExecError::Unknown),
            result = tokio::time::timeout(timeout, operation) => result.unwrap_or(Err(ExecError::Unknown)),
        }
        // OpeningChannel drops on every path and closes only this exec channel.
    }
}
fn capture(output: &mut Vec<u8>, other: &[u8], data: &[u8], truncated: &mut bool) {
    // D4 will connect full-output persistence. D2 keeps a bounded combined preview,
    // continues draining, and never claims truncated data is the complete output.
    let take = data
        .len()
        .min(1024usize.saturating_sub(output.len() + other.len()));
    output.extend_from_slice(&data[..take]);
    *truncated |= take < data.len();
}
