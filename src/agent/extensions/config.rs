use super::*;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub servers: Vec<Server>,
    #[serde(default)]
    pub skill_roots: Vec<String>,
    #[serde(default)]
    pub sub_agents: bool,
    #[serde(default)]
    pub execution_limits: ExecutionLimits,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct ExecutionLimits {
    pub model_rounds: usize,
    pub tool_calls: usize,
    pub child_model_rounds: usize,
    pub child_tool_calls: usize,
    pub max_child_tasks: usize,
}
impl Default for ExecutionLimits {
    fn default() -> Self {
        let main = tool_loop::Limits::default();
        Self {
            model_rounds: main.model_rounds,
            tool_calls: main.tool_calls,
            child_model_rounds: 24,
            child_tool_calls: 64,
            max_child_tasks: 6,
        }
    }
}
impl ExecutionLimits {
    fn validate(&self) -> Result<()> {
        if !(2..=256).contains(&self.model_rounds)
            || !(1..=1024).contains(&self.tool_calls)
            || !(2..=256).contains(&self.child_model_rounds)
            || !(1..=1024).contains(&self.child_tool_calls)
            || !(1..=16).contains(&self.max_child_tasks)
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
    pub(crate) fn main_loop(&self) -> tool_loop::Limits {
        tool_loop::Limits {
            model_rounds: self.model_rounds,
            tool_calls: self.tool_calls,
            tool_timeout: std::time::Duration::from_secs(300),
            ..Default::default()
        }
    }
    pub(crate) fn child_loop(&self) -> tool_loop::Limits {
        tool_loop::Limits {
            model_rounds: self.child_model_rounds,
            tool_calls: self.child_tool_calls,
            tool_timeout: std::time::Duration::from_secs(300),
            ..Default::default()
        }
    }
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Server {
    pub name: String,
    pub enabled: bool,
    #[serde(default, rename = "bearerToken", skip_serializing)]
    pub bearer_token: crate::agent::provider::KeyChange,
    #[serde(
        default,
        rename = "bearerTokenConfigured",
        skip_serializing_if = "std::ops::Not::not"
    )]
    pub bearer_token_configured: bool,
    #[serde(flatten)]
    pub transport: Transport,
}
#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "transport", rename_all = "camelCase", deny_unknown_fields)]
pub enum Transport {
    Stdio {
        command: String,
        #[serde(default)]
        args: Vec<String>,
        #[serde(default)]
        env: Vec<String>,
    },
    #[serde(rename = "streamableHttp")]
    StreamableHttp {
        url: String,
        #[serde(default, rename = "bearerEnv")]
        bearer_env: String,
    },
    Sse {
        url: String,
        #[serde(default, rename = "bearerEnv")]
        bearer_env: String,
        #[serde(default = "default_mcp_timeout", rename = "timeoutSeconds")]
        timeout_seconds: u64,
    },
}
fn default_mcp_timeout() -> u64 {
    45
}
fn validate_http(url: &str, bearer_env: &str, legacy: bool) -> Result<()> {
    let u = url::Url::parse(url).map_err(|_| Error::Invalid)?;
    let local = u.host_str().is_some_and(|h| {
        h == "localhost" || h.parse::<std::net::IpAddr>().is_ok_and(|i| i.is_loopback())
    });
    if url.len() > 4096
        || u.host().is_none()
        || !(u.scheme() == "https" || (u.scheme() == "http" && (legacy || local)))
        || !u.username().is_empty()
        || u.password().is_some()
        || u.fragment().is_some()
        || (!bearer_env.is_empty() && !valid_name(bearer_env, 128))
    {
        return Err(Error::Invalid);
    }
    Ok(())
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub revision: u64,
    pub config: Config,
}
pub(super) fn settings_revision(db: &rusqlite::Connection, user: &str) -> Result<u64> {
    Ok(db
        .query_row(
            "SELECT revision FROM agent_extension_settings WHERE user_id=?1",
            [user],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0))
}
fn valid_name(s: &str, max: usize) -> bool {
    !s.is_empty() && s.len() <= max && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        self.execution_limits.validate()?;
        if self.servers.len() > 8 || self.skill_roots.len() > 16 {
            return Err(Error::Invalid);
        }
        let mut names = std::collections::HashSet::new();
        for s in &self.servers {
            if let crate::agent::provider::KeyChange::Set { value } = &s.bearer_token {
                if !matches!(s.transport, Transport::StreamableHttp { .. }) {
                    return Err(Error::Invalid);
                }
                crate::agent::provider::SecretKey::new(value.0.clone())
                    .map_err(|_| Error::Invalid)?;
            }
            if !(s.name.len() <= 24
                && !s.name.is_empty()
                && s.name
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-'))
                || !names.insert(&s.name)
            {
                return Err(Error::Invalid);
            }
            match &s.transport {
                Transport::Stdio { command, args, env } => {
                    if command.trim().is_empty()
                        || command.len() > 2048
                        || command.contains(['\0', '\n', '\r'])
                        || args.len() > 64
                        || args.iter().any(|a| a.len() > 8192 || a.contains('\0'))
                        || env.len() > 32
                        || env.iter().any(|k| !valid_name(k, 128))
                    {
                        return Err(Error::Invalid);
                    }
                }
                Transport::StreamableHttp { url, bearer_env } => {
                    validate_http(url, bearer_env, false)?
                }
                Transport::Sse {
                    url,
                    bearer_env,
                    timeout_seconds,
                } => {
                    validate_http(url, bearer_env, true)?;
                    if !(1..=300).contains(timeout_seconds) {
                        return Err(Error::Invalid);
                    }
                }
            }
        }
        if self
            .skill_roots
            .iter()
            .any(|p| p.len() > 4096 || !std::path::Path::new(p).is_absolute())
        {
            return Err(Error::Invalid);
        }
        Ok(())
    }
}
pub fn settings(store: &Store, user: &str) -> Result<Settings> {
    super::super::model::identifier(user).map_err(|_| Error::Invalid)?;
    let db = store.database.lock().map_err(|_| Error::Storage)?;
    let row: Option<(u64, String)> = db
        .query_row(
            "SELECT revision,config_json FROM agent_extension_settings WHERE user_id=?1",
            [user],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    match row {
        Some((revision, raw)) => {
            let mut config = serde_json::from_str(&raw).map_err(|_| Error::Storage)?;
            super::credentials::decorate(&db, user, &mut config)?;
            Ok(Settings { revision, config })
        }
        None => Ok(Settings {
            revision: 0,
            config: Config::default(),
        }),
    }
}
pub fn save(store: &Store, user: &str, mut value: Settings) -> Result<Settings> {
    value.config.validate()?;
    super::super::model::identifier(user).map_err(|_| Error::Invalid)?;
    let mut db = store.database.lock().map_err(|_| Error::Storage)?;
    let tx = db.transaction()?;
    let old: u64 = tx
        .query_row(
            "SELECT revision FROM agent_extension_settings WHERE user_id=?1",
            [user],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(0);
    if value.revision != old {
        return Err(Error::Changed);
    }
    let next = old.checked_add(1).ok_or(Error::Storage)?;
    super::credentials::update(&tx, user, &mut value.config)?;
    tx.execute("INSERT INTO agent_extension_settings(user_id,revision,config_json) VALUES(?1,?2,?3) ON CONFLICT(user_id) DO UPDATE SET revision=excluded.revision,config_json=excluded.config_json",rusqlite::params![user,next,serde_json::to_string(&value.config).map_err(|_|Error::Invalid)?])?;
    // Editing/removing an MCP server invalidates every unconsumed exact-call approval.
    tx.execute("UPDATE agent_extension_approvals SET state='cancelled' WHERE user_id=?1 AND state IN ('pending','approved')",[user])?;
    tx.commit()?;
    Ok(Settings {
        revision: next,
        config: value.config,
    })
}
