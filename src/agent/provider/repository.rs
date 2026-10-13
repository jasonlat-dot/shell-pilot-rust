use super::*;
use crate::{agent::model::identifier, secrets, store::Store};
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Arc;

#[derive(Clone)]
pub struct ModelRepository {
    store: Arc<Store>,
}
/// Native-only runtime settings. Never serialize this into IPC or the event log.
pub struct RuntimeConfig {
    pub config: ModelConfig,
    pub(crate) api_key: Option<SecretKey>,
}
impl std::fmt::Debug for RuntimeConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RuntimeConfig")
            .field("config", &self.config)
            .field("api_key", &self.api_key)
            .finish()
    }
}
type Row = (u64, String, Option<Vec<u8>>);
fn checked_id(value: &str) -> Result<()> {
    identifier(value).map_err(|_| ModelError::Invalid("invalid profile/user ID"))
}
fn entropy(user: &str, id: &str) -> String {
    // Length prefixes prevent collisions when IDs contain punctuation.
    format!(
        "shellpilot-model:{}:{user}:{}:{id}:api-key",
        user.len(),
        id.len()
    )
}
fn row(db: &Connection, id: &str, user: &str) -> Result<Option<Row>> {
    Ok(db.query_row("SELECT revision,config_json,api_key FROM agent_model_profiles WHERE user_id=?1 AND id=?2",
        params![user,id], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?)
}
fn display_name(db: &Connection, id: &str, user: &str) -> Result<String> {
    Ok(db
        .query_row(
            "SELECT display_name FROM agent_model_names WHERE user_id=?1 AND profile_id=?2",
            params![user, id],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_else(|| id.to_owned()))
}
fn view(id: &str, row: Row, display_name: String) -> Result<ModelProfile> {
    let (revision, json, key) = row;
    let config: ModelConfig = serde_json::from_str(&json).map_err(|_| ModelError::Database)?;
    let config = config.normalized()?;
    Ok(ModelProfile {
        id: id.into(),
        display_name,
        revision,
        ready: !config.require_api_key || key.is_some(),
        config,
        api_key_configured: key.is_some(),
    })
}
impl ModelRepository {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }
    pub fn get(&self, id: &str, user: &str) -> Result<ModelProfile> {
        checked_id(id)?;
        checked_id(user)?;
        let db = self
            .store
            .database
            .lock()
            .map_err(|_| ModelError::Database)?;
        view(
            id,
            row(&db, id, user)?.ok_or(ModelError::NotFound)?,
            display_name(&db, id, user)?,
        )
    }
    pub fn list(&self, user: &str) -> Result<Vec<ModelProfile>> {
        checked_id(user)?;
        let db = self
            .store
            .database
            .lock()
            .map_err(|_| ModelError::Database)?;
        let mut statement = db.prepare("SELECT id,revision,config_json,api_key FROM agent_model_profiles WHERE user_id=?1 ORDER BY id LIMIT 32")?;
        let rows = statement.query_map([user], |r| {
            Ok((r.get::<_, String>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
        })?;
        rows.map(|r| {
            let (id, row) = r?;
            let name = display_name(&db, &id, user)?;
            view(&id, row, name)
        })
        .collect()
    }
    /// revision=0 creates a new profile; edits/deletes use the last displayed revision.
    pub fn save(
        &self,
        id: &str,
        user: &str,
        expected_revision: u64,
        update: ModelConfigUpdate,
    ) -> Result<ModelProfile> {
        self.save_named(id, user, expected_revision, update, None)
    }
    /// Label and configuration share one revision/transaction; identity never changes.
    pub fn save_named(
        &self,
        id: &str,
        user: &str,
        expected_revision: u64,
        update: ModelConfigUpdate,
        name: Option<&str>,
    ) -> Result<ModelProfile> {
        checked_id(id)?;
        checked_id(user)?;
        let name = name.map(str::trim);
        if name.is_some_and(|name| {
            name.is_empty()
                || name.len() > 256
                || name.chars().count() > 64
                || name.chars().any(char::is_control)
        }) {
            return Err(ModelError::Invalid(
                "configuration name must contain 1 to 64 printable characters",
            ));
        }
        let config = update.config.normalized()?;
        let json = serde_json::to_string(&config).map_err(|_| ModelError::Database)?;
        let mut db = self
            .store
            .database
            .lock()
            .map_err(|_| ModelError::Database)?;
        let tx = db.transaction()?;
        let previous = row(&tx, id, user)?;
        let revision = previous.as_ref().map(|r| r.0).unwrap_or(0);
        if revision != expected_revision {
            return Err(ModelError::Stale);
        }
        if previous.is_none()
            && tx.query_row(
                "SELECT COUNT(*) FROM agent_model_profiles WHERE user_id=?1",
                [user],
                |r| r.get::<_, usize>(0),
            )? >= 32
        {
            return Err(ModelError::Invalid("at most 32 model profiles per user"));
        }
        let key = match update.api_key {
            KeyChange::Keep => {
                if let Some((_, old_json, Some(_))) = &previous {
                    let old: ModelConfig =
                        serde_json::from_str(old_json).map_err(|_| ModelError::Database)?;
                    if old.base_url != config.base_url {
                        return Err(ModelError::Invalid(
                            "changing API base URL requires an explicit key set/clear",
                        ));
                    }
                }
                previous.and_then(|r| r.2)
            }
            KeyChange::Set { value } => {
                let value = SecretKey::new(value.0)?;
                Some(
                    secrets::protect(&value.0, &entropy(user, id))
                        .map_err(|_| ModelError::Credentials)?,
                )
            }
            KeyChange::Clear => None,
        };
        let next = revision
            .checked_add(1)
            .filter(|n| *n <= i64::MAX as u64)
            .ok_or(ModelError::Stale)?;
        tx.execute("INSERT INTO agent_model_profiles(id,user_id,revision,config_json,api_key,updated_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(user_id,id) DO UPDATE SET revision=excluded.revision,config_json=excluded.config_json,api_key=excluded.api_key,updated_at=excluded.updated_at",
            params![id,user,next,json,key,chrono::Utc::now().timestamp()])?;
        if let Some(name) = name {
            tx.execute("INSERT INTO agent_model_names(user_id,profile_id,display_name) VALUES(?1,?2,?3) ON CONFLICT(user_id,profile_id) DO UPDATE SET display_name=excluded.display_name", params![user,id,name])?;
        }
        let result = view(
            id,
            row(&tx, id, user)?.ok_or(ModelError::Database)?,
            display_name(&tx, id, user)?,
        )?;
        tx.commit()?;
        Ok(result)
    }
    pub fn delete(&self, id: &str, user: &str, expected_revision: u64) -> Result<()> {
        checked_id(id)?;
        checked_id(user)?;
        let mut db = self
            .store
            .database
            .lock()
            .map_err(|_| ModelError::Database)?;
        let tx = db.transaction()?;
        let previous = row(&tx, id, user)?.ok_or(ModelError::NotFound)?;
        if previous.0 != expected_revision {
            return Err(ModelError::Stale);
        }
        tx.execute(
            "DELETE FROM agent_model_names WHERE user_id=?1 AND profile_id=?2",
            params![user, id],
        )?;
        tx.execute(
            "DELETE FROM agent_model_profiles WHERE user_id=?1 AND id=?2 AND revision=?3",
            params![user, id, expected_revision],
        )?;
        tx.commit()?;
        Ok(())
    }
    pub fn runtime(&self, id: &str, user: &str) -> Result<RuntimeConfig> {
        checked_id(id)?;
        checked_id(user)?;
        let db = self
            .store
            .database
            .lock()
            .map_err(|_| ModelError::Database)?;
        let (revision, json, key) = row(&db, id, user)?.ok_or(ModelError::NotFound)?;
        let profile = view(id, (revision, json, None), id.to_owned())?;
        let api_key = key
            .map(|key| {
                let plain = secrets::unprotect(&key, &entropy(user, id))
                    .map_err(|_| ModelError::Credentials)?;
                SecretKey::new(plain).map_err(|_| ModelError::Credentials)
            })
            .transpose()?;
        if profile.config.require_api_key && api_key.is_none() {
            return Err(ModelError::KeyMissing);
        }
        Ok(RuntimeConfig {
            config: profile.config,
            api_key,
        })
    }
}
