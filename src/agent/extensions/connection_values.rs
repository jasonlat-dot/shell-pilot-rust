//! User-entered HTTP headers / stdio environment values, encrypted separately from public settings.
use super::{
    config::{Config, Server, Transport},
    *,
};
use crate::{agent::provider::KeyChange, secrets};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

const SAVED: &str = "__SHELLPILOT_SAVED__";
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Values {
    pub headers: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
}
impl Values {
    pub(super) fn validate(&self, transport: &Transport) -> Result<()> {
        if self.headers.len() > 32 || self.env.len() > 32 {
            return Err(Error::Invalid);
        }
        match transport {
            Transport::Stdio { env, .. }
                if self.headers.is_empty() && env.len() + self.env.len() <= 32 => {}
            Transport::StreamableHttp { .. } | Transport::Sse { .. } if self.env.is_empty() => {}
            _ => return Err(Error::Invalid),
        }
        let mut seen = std::collections::HashSet::new();
        for (name, value) in &self.headers {
            let name = name.to_ascii_lowercase();
            if name.len() > 128
                || !seen.insert(name.clone())
                || matches!(
                    name.as_str(),
                    "host"
                        | "content-length"
                        | "content-type"
                        | "accept"
                        | "connection"
                        | "transfer-encoding"
                        | "mcp-session-id"
                        | "mcp-protocol-version"
                )
                || reqwest::header::HeaderName::from_bytes(name.as_bytes()).is_err()
                || value.len() > 8192
                || !value.bytes().all(|b| b == b'\t' || (32..=126).contains(&b))
                || reqwest::header::HeaderValue::from_str(value).is_err()
            {
                return Err(Error::Invalid);
            }
        }
        for (name, value) in &self.env {
            if name.is_empty()
                || name.len() > 128
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                || value.len() > 8192
                || value.contains('\0')
            {
                return Err(Error::Invalid);
            }
        }
        Ok(())
    }
}
fn target(server: &Server) -> String {
    match &server.transport {
        Transport::Stdio { command, args, env } => json!(["stdio", command, args, env]).to_string(),
        Transport::StreamableHttp { url, .. } => json!(["streamableHttp", url]).to_string(),
        Transport::Sse { url, .. } => json!(["sse", url]).to_string(),
    }
}
fn context(user: &str, name: &str, target: &str) -> String {
    format!("mcp-values:{}", json!([user, name, target]))
}
fn read(db: &Connection, user: &str, name: &str) -> Result<Option<(String, Values)>> {
    let row:Option<(String,Vec<u8>)>=db.query_row("SELECT target,encrypted_values FROM agent_mcp_connection_values WHERE user_id=?1 AND server_name=?2",params![user,name],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    row.map(|(target, blob)| {
        let raw = secrets::unprotect(&blob, &context(user, name, &target))
            .map_err(|_| Error::Unavailable)?;
        let values = serde_json::from_str(&raw).map_err(|_| Error::Storage)?;
        Ok((target, values))
    })
    .transpose()
}
pub(super) fn decorate(db: &Connection, user: &str, config: &mut Config) -> Result<()> {
    for server in &mut config.servers {
        server.connection_values = None;
        server.saved_headers.clear();
        server.saved_env.clear();
        if let Some((stored, values)) = read(db, user, &server.name)? {
            if stored != target(server) {
                return Err(Error::Changed);
            }
            server.saved_headers = values.headers.into_keys().collect();
            server.saved_env = values.env.into_keys().collect();
        }
    }
    Ok(())
}
pub(super) fn update(db: &Connection, user: &str, config: &mut Config) -> Result<()> {
    let mut keep = vec![];
    for server in &mut config.servers {
        let destination = target(server);
        let previous = read(db, user, &server.name)?;
        let mut values = if let Some(mut values) = server.connection_values.take() {
            values.headers = values
                .headers
                .into_iter()
                .map(|(key, value)| (key.to_ascii_lowercase(), value))
                .collect();
            let old = previous
                .as_ref()
                .filter(|(stored, _)| *stored == destination)
                .map(|(_, v)| v);
            for (key, value) in &mut values.headers {
                if value == SAVED {
                    if let Some(previous) = old.and_then(|v| v.headers.get(key)) {
                        *value = previous.clone();
                    } else if key == "authorization" {
                        let Transport::StreamableHttp { url, .. } = &server.transport else {
                            return Err(Error::Invalid);
                        };
                        let legacy:Option<(String,Vec<u8>)>=db.query_row("SELECT endpoint,bearer_token FROM agent_extension_credentials WHERE user_id=?1 AND server_name=?2",params![user,&server.name],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
                        let Some((endpoint, blob)) = legacy.filter(|(endpoint, _)| endpoint == url)
                        else {
                            return Err(Error::Invalid);
                        };
                        let entropy =
                            format!("mcp-bearer:{}", json!([user, &server.name, endpoint]));
                        *value = format!(
                            "Bearer {}",
                            secrets::unprotect(&blob, &entropy).map_err(|_| Error::Unavailable)?
                        );
                    } else {
                        return Err(Error::Invalid);
                    }
                }
            }
            for (key, value) in &mut values.env {
                if value == SAVED {
                    *value = old
                        .and_then(|v| v.env.get(key))
                        .cloned()
                        .ok_or(Error::Invalid)?;
                }
            }
            if matches!(server.transport, Transport::StreamableHttp { .. }) {
                let legacy:bool=db.query_row("SELECT EXISTS(SELECT 1 FROM agent_extension_credentials WHERE user_id=?1 AND server_name=?2)",params![user,&server.name],|r|r.get(0))?;
                if legacy {
                    server.bearer_token = KeyChange::Clear;
                }
            }
            values
        } else {
            match previous {
                Some((stored, values)) if stored == destination => values,
                Some(_) => return Err(Error::Invalid),
                None => Values::default(),
            }
        };
        values.validate(&server.transport)?;
        if values.headers.contains_key("authorization") {
            match &mut server.transport {
                Transport::StreamableHttp { bearer_env, .. }
                | Transport::Sse { bearer_env, .. } => bearer_env.clear(),
                _ => {}
            }
        }
        server.saved_headers = values.headers.keys().cloned().collect();
        server.saved_env = values.env.keys().cloned().collect();
        if !values.headers.is_empty() || !values.env.is_empty() {
            let raw = serde_json::to_string(&values).map_err(|_| Error::Storage)?;
            let blob = secrets::protect(&raw, &context(user, &server.name, &destination))
                .map_err(|_| Error::Unavailable)?;
            keep.push((server.name.clone(), destination, blob));
        }
        values.headers.clear();
        values.env.clear();
    }
    db.execute(
        "DELETE FROM agent_mcp_connection_values WHERE user_id=?1",
        [user],
    )?;
    for (name, destination, blob) in keep {
        db.execute("INSERT INTO agent_mcp_connection_values(user_id,server_name,target,encrypted_values) VALUES(?1,?2,?3,?4)",params![user,name,destination,blob])?;
    }
    Ok(())
}
pub(super) fn resolve(store: &Store, user: &str, server: &Server, revision: u64) -> Result<Values> {
    let db = store.database.lock().map_err(|_| Error::Storage)?;
    if super::config::settings_revision(&db, user)? != revision {
        return Err(Error::Changed);
    }
    let values = match read(&db, user, &server.name)? {
        Some((stored, values)) if stored == target(server) => values,
        Some(_) => return Err(Error::Changed),
        None => Values::default(),
    };
    values.validate(&server.transport)?;
    Ok(values)
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    fn input() -> config::Settings {
        serde_json::from_value(json!({"revision":0,"config":{"servers":[{"name":"search","description":"搜索服务","enabled":false,"transport":"streamableHttp","url":"https://example.com/mcp","connectionValues":{"headers":{"Authorization":"Bearer fixture-header"},"env":{}}},{"name":"files","enabled":false,"transport":"stdio","command":"node","args":["server.js"],"env":[],"connectionValues":{"headers":{},"env":{"TOKEN":"fixture-env"}}}]}})).unwrap()
    }
    #[test]
    fn json_mcp_values_encrypt_roundtrip_preserve_markers_and_clear_removed_keys_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.db");
        let store = Store::open(&path).unwrap();
        let saved = config::save(&store, "alice", input()).unwrap();
        let raw = serde_json::to_string(&saved).unwrap();
        assert!(!raw.contains("fixture-header"));
        assert!(!raw.contains("fixture-env"));
        assert_eq!(saved.config.servers[0].saved_headers, vec!["authorization"]);
        assert_eq!(saved.config.servers[1].saved_env, vec!["TOKEN"]);
        drop(store);
        let store = Store::open(&path).unwrap();
        let mut loaded = config::settings(&store, "alice").unwrap();
        loaded.config.servers[0].connection_values = Some(Values {
            headers: BTreeMap::from([("Authorization".into(), SAVED.into())]),
            env: BTreeMap::new(),
        });
        loaded.config.servers[1].connection_values = Some(Values {
            headers: BTreeMap::new(),
            env: BTreeMap::from([("TOKEN".into(), SAVED.into())]),
        });
        let kept = config::save(&store, "alice", loaded).unwrap();
        assert_eq!(
            resolve(&store, "alice", &kept.config.servers[0], kept.revision)
                .unwrap()
                .headers["authorization"],
            "Bearer fixture-header"
        );
        assert_eq!(
            resolve(&store, "alice", &kept.config.servers[1], kept.revision)
                .unwrap()
                .env["TOKEN"],
            "fixture-env"
        );
        let db = store.database.lock().unwrap();
        let raw: String = db
            .query_row(
                "SELECT config_json FROM agent_extension_settings WHERE user_id='alice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("fixture-header"));
        assert!(!raw.contains("fixture-env"));
        drop(db);
        let mut cleared = kept;
        for server in &mut cleared.config.servers {
            server.connection_values = Some(Values::default());
        }
        let cleared = config::save(&store, "alice", cleared).unwrap();
        assert!(cleared.config.servers[0].saved_headers.is_empty());
        assert!(cleared.config.servers[1].saved_env.is_empty());
        assert!(read(&store.database.lock().unwrap(), "alice", "search")
            .unwrap()
            .is_none());
    }
    #[test]
    fn json_mcp_values_refuse_unknown_markers_target_changes_and_duplicate_headers_without_committing(
    ) {
        let store = Store::in_memory().unwrap();
        let mut first = input();
        first.config.servers[0]
            .connection_values
            .as_mut()
            .unwrap()
            .headers
            .insert("authorization".into(), "duplicate".into());
        assert!(config::save(&store, "alice", first).is_err());
        let saved = config::save(&store, "alice", input()).unwrap();
        let mut edit = saved.clone();
        edit.config.servers[0].connection_values = Some(Values {
            headers: BTreeMap::from([("x-new".into(), SAVED.into())]),
            env: BTreeMap::new(),
        });
        assert!(config::save(&store, "alice", edit).is_err());
        let mut edit = saved.clone();
        if let Transport::StreamableHttp { url, .. } = &mut edit.config.servers[0].transport {
            *url = "https://other.example/mcp".into();
        }
        edit.config.servers[0].connection_values = Some(Values {
            headers: BTreeMap::from([("authorization".into(), SAVED.into())]),
            env: BTreeMap::new(),
        });
        assert!(config::save(&store, "alice", edit).is_err());
        assert_eq!(
            config::settings(&store, "alice").unwrap().revision,
            saved.revision
        );
        assert!(resolve(&store, "bob", &saved.config.servers[0], saved.revision).is_err());
        let mut edit = saved;
        edit.config.servers.clear();
        config::save(&store, "alice", edit).unwrap();
        assert!(read(&store.database.lock().unwrap(), "alice", "files")
            .unwrap()
            .is_none());
    }
    #[test]
    fn json_mcp_values_migrate_a_legacy_bearer_token_from_an_authorization_placeholder() {
        let store = Store::in_memory().unwrap();
        let mut first = input();
        first.config.servers.truncate(1);
        first.config.servers[0].connection_values = None;
        first.config.servers[0].bearer_token =
            serde_json::from_value(json!({"action":"set","value":"fixture-legacy-token"})).unwrap();
        let mut saved = config::save(&store, "alice", first).unwrap();
        saved.config.servers[0].connection_values = Some(Values {
            headers: BTreeMap::from([("Authorization".into(), SAVED.into())]),
            env: BTreeMap::new(),
        });
        let migrated = config::save(&store, "alice", saved).unwrap();
        assert!(!migrated.config.servers[0].bearer_token_configured);
        assert_eq!(
            resolve(
                &store,
                "alice",
                &migrated.config.servers[0],
                migrated.revision
            )
            .unwrap()
            .headers["authorization"],
            "Bearer fixture-legacy-token"
        );
        assert!(credentials::resolve(
            &store,
            "alice",
            &migrated.config.servers[0],
            migrated.revision
        )
        .unwrap()
        .is_none());
    }
}
