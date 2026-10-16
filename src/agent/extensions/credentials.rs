//! Native-only MCP credentials. IPC returns presence, never stored token contents.
use super::{
    config::{Config, Server, Transport},
    *,
};
use crate::{agent::provider::KeyChange, secrets};
use rusqlite::{params, Connection, OptionalExtension};

fn context(user: &str, name: &str, url: &str) -> String {
    format!("mcp-bearer:{}", json!([user, name, url]))
}
fn row(db: &Connection, user: &str, name: &str) -> Result<Option<(String, Vec<u8>)>> {
    Ok(db.query_row("SELECT endpoint,bearer_token FROM agent_extension_credentials WHERE user_id=?1 AND server_name=?2", params![user,name], |r| Ok((r.get(0)?,r.get(1)?))).optional()?)
}
pub(super) fn decorate(db: &Connection, user: &str, config: &mut Config) -> Result<()> {
    for server in &mut config.servers {
        server.bearer_token = KeyChange::Keep;
        server.bearer_token_configured = match &server.transport {
            Transport::StreamableHttp { url, .. } => {
                row(db, user, &server.name)?.is_some_and(|(endpoint, _)| endpoint == *url)
            }
            _ => false,
        };
    }
    Ok(())
}
pub(super) fn update(db: &Connection, user: &str, config: &mut Config) -> Result<()> {
    let mut keep = vec![];
    for server in &mut config.servers {
        server.bearer_token_configured = false;
        if let Transport::StreamableHttp { url, bearer_env } = &mut server.transport {
            let encrypted = match &server.bearer_token {
                KeyChange::Keep => match row(db, user, &server.name)? {
                    Some((endpoint, key)) if endpoint == *url => Some(key),
                    Some(_) => return Err(Error::Invalid), // Never carry a saved key to a changed endpoint.
                    None => None,
                },
                KeyChange::Set { value } => {
                    bearer_env.clear();
                    Some(
                        secrets::protect(&value.0, &context(user, &server.name, url))
                            .map_err(|_| Error::Unavailable)?,
                    )
                }
                KeyChange::Clear => {
                    bearer_env.clear();
                    None
                }
            };
            server.bearer_token_configured = encrypted.is_some();
            if let Some(key) = encrypted {
                keep.push((server.name.clone(), url.clone(), key));
            }
        }
        server.bearer_token = KeyChange::Keep;
    }
    db.execute(
        "DELETE FROM agent_extension_credentials WHERE user_id=?1",
        [user],
    )?;
    for (name, url, key) in keep {
        db.execute("INSERT INTO agent_extension_credentials(user_id,server_name,endpoint,bearer_token) VALUES(?1,?2,?3,?4)",params![user,name,url,key])?;
    }
    Ok(())
}
pub(super) fn resolve(
    store: &Store,
    user: &str,
    server: &Server,
    revision: u64,
) -> Result<Option<String>> {
    let Transport::StreamableHttp { url, .. } = &server.transport else {
        return Ok(None);
    };
    let db = store.database.lock().map_err(|_| Error::Storage)?;
    if super::config::settings_revision(&db, user)? != revision {
        return Err(Error::Changed);
    }
    match row(&db, user, &server.name)? {
        Some((endpoint, key)) if endpoint == *url => {
            secrets::unprotect(&key, &context(user, &server.name, url))
                .map(Some)
                .map_err(|_| Error::Unavailable)
        }
        Some(_) => Err(Error::Changed),
        None => Ok(None),
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;
    fn input() -> config::Settings {
        serde_json::from_value(json!({"revision":0,"config":{"servers":[{"name":"baidu-search","enabled":true,"transport":"streamableHttp","url":"https://example.com/mcp","bearerEnv":"","bearerToken":{"action":"set","value":"fixture-token/a+b.c"}}]}})).unwrap()
    }
    fn token(store: &Store, user: &str) -> Option<String> {
        let settings = config::settings(store, user).unwrap();
        resolve(store, user, &settings.config.servers[0], settings.revision).unwrap()
    }
    #[test]
    fn mcp_client_token_is_encrypted_redacted_preserved_on_edit_and_clearable_after_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.db");
        let store = Store::open(&path).unwrap();
        let saved = config::save(&store, "alice", input()).unwrap();
        assert!(saved.config.servers[0].bearer_token_configured);
        let response = serde_json::to_string(&saved).unwrap();
        assert!(!response.contains("fixture-token"));
        assert!(!response.contains("\"bearerToken\":"));
        let db = store.database.lock().unwrap();
        let raw: String = db
            .query_row(
                "SELECT config_json FROM agent_extension_settings WHERE user_id='alice'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert!(!raw.contains("fixture-token"));
        let (_, blob) = row(&db, "alice", "baidu-search").unwrap().unwrap();
        assert!(!blob
            .windows(b"fixture-token".len())
            .any(|chunk| chunk == b"fixture-token"));
        drop(db);
        drop(store);
        let store = Store::open(&path).unwrap();
        assert_eq!(
            token(&store, "alice").as_deref(),
            Some("fixture-token/a+b.c")
        );
        let mut edit = config::settings(&store, "alice").unwrap();
        edit.config.execution_limits.model_rounds = 48;
        config::save(&store, "alice", edit).unwrap();
        assert_eq!(
            token(&store, "alice").as_deref(),
            Some("fixture-token/a+b.c")
        );
        let mut edit = config::settings(&store, "alice").unwrap();
        edit.config.servers[0].bearer_token = KeyChange::Clear;
        let cleared = config::save(&store, "alice", edit).unwrap();
        assert!(!cleared.config.servers[0].bearer_token_configured);
        assert!(token(&store, "alice").is_none());
        assert!(
            row(&store.database.lock().unwrap(), "alice", "baidu-search")
                .unwrap()
                .is_none()
        );
    }
    #[test]
    fn mcp_client_token_is_owner_endpoint_and_revision_bound_and_removing_server_cleans_it() {
        let store = Store::in_memory().unwrap();
        config::save(&store, "alice", input()).unwrap();
        config::save(&store, "bob", input()).unwrap();
        let original = config::settings(&store, "alice").unwrap();
        let mut edited = original.clone();
        if let Transport::StreamableHttp { url, .. } = &mut edited.config.servers[0].transport {
            *url = "https://other.example/mcp".into();
        }
        assert!(config::save(&store, "alice", edited).is_err());
        assert_eq!(
            config::settings(&store, "alice").unwrap().revision,
            original.revision
        );
        let mut edit = original.clone();
        edit.config.servers.clear();
        config::save(&store, "alice", edit).unwrap();
        assert!(matches!(
            resolve(
                &store,
                "alice",
                &original.config.servers[0],
                original.revision
            ),
            Err(Error::Changed)
        ));
        let db = store.database.lock().unwrap();
        assert!(row(&db, "alice", "baidu-search").unwrap().is_none());
        assert!(row(&db, "bob", "baidu-search").unwrap().is_some());
        // Swapping a ciphertext across user records cannot bypass DPAPI entropy binding.
        db.execute(
            "UPDATE agent_extension_credentials SET user_id='mallory' WHERE user_id='bob'",
            [],
        )
        .unwrap();
        let (_, blob) = row(&db, "mallory", "baidu-search").unwrap().unwrap();
        assert!(secrets::unprotect(
            &blob,
            &context("mallory", "baidu-search", "https://example.com/mcp")
        )
        .is_err());
    }
    #[test]
    fn mcp_client_token_rejects_control_characters_and_can_explicitly_replace_for_new_endpoint() {
        let store = Store::in_memory().unwrap();
        let mut bad = input();
        bad.config.servers[0].bearer_token =
            serde_json::from_value(json!({"action":"set","value":"fixture\r\ninjected"})).unwrap();
        assert!(config::save(&store, "alice", bad).is_err());
        config::save(&store, "alice", input()).unwrap();
        let mut replacement = config::settings(&store, "alice").unwrap();
        if let Transport::StreamableHttp { url, .. } = &mut replacement.config.servers[0].transport
        {
            *url = "https://other.example/mcp".into();
        }
        replacement.config.servers[0].bearer_token =
            serde_json::from_value(json!({"action":"set","value":"new-fixture-token"})).unwrap();
        config::save(&store, "alice", replacement).unwrap();
        assert_eq!(token(&store, "alice").as_deref(), Some("new-fixture-token"));
    }
}
