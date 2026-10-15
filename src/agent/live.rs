//! Host-owned scope identity; offline text is allowed, executable leases remain separate.
use super::{
    chat::{Error, Request, Result},
    Repository, ScopeKind, ServerIdentity, Target,
};
use crate::{store::Store, terminal::TerminalManager};
use serde_json::{json, Value};
use std::sync::Arc;
#[derive(Clone)]
pub(crate) struct Source {
    store: Arc<Store>,
    terminals: Option<Arc<TerminalManager>>,
}
pub(crate) struct Live {
    pub environment: Value,
}
impl Source {
    pub fn new(store: Arc<Store>, terminals: Option<Arc<TerminalManager>>) -> Self {
        Self { store, terminals }
    }
    pub fn server_identity(&self, terminal: &str, user: &str) -> Result<(String, ServerIdentity)> {
        let terminals = self.terminals.as_ref().ok_or(Error::TargetUnavailable)?;
        let (identity, connected) = terminals
            .agent_identity(terminal)
            .ok_or(Error::TargetUnavailable)?;
        if !connected {
            return Err(Error::TargetUnavailable);
        }
        if identity.user_id != user
            || self
                .store
                .get(&identity.connection_id)
                .map_err(|_| Error::TargetUnavailable)?
                .user_id
                != user
        {
            return Err(Error::TargetMismatch);
        }
        Ok((
            identity.connection_id,
            ServerIdentity {
                host: identity.host.trim_matches(['[', ']']).to_ascii_lowercase(),
                port: identity.port,
                username: identity.username,
                host_key: identity.host_key,
            },
        ))
    }
    pub fn capture_for(&self, request: &Request, target: &Option<Target>) -> Result<Live> {
        let session =
            Repository::new(self.store.clone()).session(&request.session_id, &request.user_id)?;
        if session.agent_id != request.agent_id {
            return Err(Error::NotFound);
        }
        let ssh = match &session.scope {
            Some(scope) if scope.kind == ScopeKind::General => {
                if target.is_some() || !request.terminal_session_id.is_empty() {
                    return Err(Error::TargetMismatch);
                }
                Value::Null
            }
            Some(scope) => {
                if target.is_some()
                    && target != &session.task.as_ref().and_then(|t| t.target.clone())
                {
                    return Err(Error::TargetMismatch);
                }
                let expected = scope.identity.as_ref().ok_or(Error::Storage)?;
                let connection = scope.connection_id.as_ref().ok_or(Error::Storage)?;
                let mut connected = false;
                if !request.terminal_session_id.is_empty() {
                    if let Some((actual, live)) = self
                        .terminals
                        .as_ref()
                        .and_then(|t| t.agent_identity(&request.terminal_session_id))
                    {
                        let actual_identity = ServerIdentity {
                            host: actual.host.trim_matches(['[', ']']).to_ascii_lowercase(),
                            port: actual.port,
                            username: actual.username,
                            host_key: actual.host_key,
                        };
                        if &actual.connection_id != connection
                            || actual.user_id != request.user_id
                            || &actual_identity != expected
                        {
                            return Err(Error::TargetMismatch);
                        }
                        if live {
                            self.server_identity(&request.terminal_session_id, &request.user_id)?;
                            connected = true;
                        }
                    } else if scope.origin_terminal_id.as_deref()
                        != Some(&request.terminal_session_id)
                    {
                        return Err(Error::TargetMismatch);
                    }
                }
                json!({"verified":true,"connected":connected,"connectionId":connection,
                    "terminalSessionId":if connected{Some(&request.terminal_session_id)}else{None},
                    "host":expected.host,"port":expected.port,"username":expected.username,
                    "hostKey":expected.host_key,"os":null,"cwd":null})
            }
            None => {
                // Legacy history never becomes ordinary shared context or adopts an edited profile.
                if let Some(target) = target {
                    match self
                        .terminals
                        .as_ref()
                        .and_then(|t| t.agent_identity(&target.terminal_session_id))
                    {
                        Some((identity, connected)) => {
                            if identity.connection_id != target.connection_id
                                || identity.user_id != request.user_id
                            {
                                return Err(Error::TargetMismatch);
                            }
                            if connected
                                && self
                                    .store
                                    .get(&target.connection_id)
                                    .map_err(|_| Error::TargetUnavailable)?
                                    .user_id
                                    != request.user_id
                            {
                                return Err(Error::TargetMismatch);
                            }
                            json!({"verified":true,"connected":connected,"connectionId":identity.connection_id,
                                "terminalSessionId":target.terminal_session_id,"host":identity.host,"port":identity.port,
                                "username":identity.username,"os":null,"cwd":null})
                        }
                        None => {
                            json!({"verified":false,"connected":false,"connectionId":target.connection_id,"os":null,"cwd":null})
                        }
                    }
                } else {
                    Value::Null
                }
            }
        };
        Ok(Live {
            environment: json!({"source":"nativeHost","desktopOs":std::env::consts::OS,"ssh":ssh,
            "observation":"连接状态是本次请求准备时的快照；断开后可分析已有证据，实时读取/执行需另行核实连接与授权。远端 os/cwd 未探测时未知。",
            "permissions":{"mode":"textOnly","sshExecute":false,"localExecute":false,"tools":[],"approvals":[]}}),
        })
    }
}
