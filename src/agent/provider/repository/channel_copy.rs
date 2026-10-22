use super::{channels::channel_view, *};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelCopyDraft {
    pub display_name: String,
    pub changes: Option<ChannelCopyChanges>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ChannelCopyChanges {
    pub update: ModelConfigUpdate,
    pub model_ids: Vec<String>,
}

fn copy_name(db: &Connection, user: &str, original: &str) -> Result<String> {
    for number in 1..=33 {
        let suffix = if number == 1 {
            "（副本）".to_owned()
        } else {
            format!("（副本 {number}）")
        };
        let prefix = original
            .chars()
            .take(64 - suffix.chars().count())
            .collect::<String>();
        let name = format!("{prefix}{suffix}");
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM agent_model_channels WHERE user_id=?1 AND display_name=?2 UNION ALL SELECT 1 FROM agent_model_names WHERE user_id=?1 AND display_name=?2)",
            params![user, name], |row| row.get(0),
        )?;
        if !exists {
            return Ok(name);
        }
    }
    Err(ModelError::Invalid("cannot create a unique copy name"))
}

impl ModelRepository {
    /// Clone the current configuration in one transaction. Credentials are
    /// re-encrypted for fresh IDs; no plaintext key is returned to the WebView.
    pub fn copy_channel(
        &self,
        source_id: &str,
        user: &str,
        expected_revision: u64,
        id: &str,
    ) -> Result<ModelChannel> {
        self.copy_channel_draft(source_id, user, expected_revision, id, None)
    }

    /// Commit the copy and any editor changes together; a rejected draft leaves
    /// no channel, profile or credential behind.
    pub fn copy_channel_draft(
        &self,
        source_id: &str,
        user: &str,
        expected_revision: u64,
        id: &str,
        draft: Option<ChannelCopyDraft>,
    ) -> Result<ModelChannel> {
        checked_id(source_id)?;
        checked_id(user)?;
        checked_id(id)?;
        let mut db = self
            .store
            .database
            .lock()
            .map_err(|_| ModelError::Database)?;
        let tx = db.transaction()?;
        let source = channel_view(&tx, source_id, user)?;
        if source.profile.revision != expected_revision {
            return Err(ModelError::Stale);
        }
        if source_id == id || row(&tx, id, user)?.is_some() {
            return Err(ModelError::Stale);
        }
        if channel_count(&tx, user)? >= 32 {
            return Err(ModelError::Invalid("at most 32 channels per user"));
        }
        let total: usize = tx.query_row(
            "SELECT COUNT(*) FROM agent_model_profiles WHERE user_id=?1",
            [user],
            |row| row.get(0),
        )?;
        if total + source.models.len() > 4096 {
            return Err(ModelError::Invalid("model profile storage limit reached"));
        }
        let name = copy_name(&tx, user, &source.profile.display_name)?;
        tx.execute(
            "INSERT INTO agent_model_channels(user_id,id,display_name,revision) VALUES(?1,?2,?3,1)",
            params![user, id, name],
        )?;
        for (position, model) in source.models.iter().enumerate() {
            let (_, config, encrypted) = row(&tx, &model.id, user)?.ok_or(ModelError::NotFound)?;
            let profile_id = if position == 0 {
                id.to_owned()
            } else {
                uuid::Uuid::new_v4().to_string()
            };
            // Entropy includes the profile identity, so copying the old blob
            // directly would make the new configuration's key unreadable.
            let key = encrypted
                .map(|encrypted| {
                    let plain = secrets::unprotect(&encrypted, &entropy(user, &model.id))
                        .map_err(|_| ModelError::Credentials)?;
                    secrets::protect(&plain, &entropy(user, &profile_id))
                        .map_err(|_| ModelError::Credentials)
                })
                .transpose()?;
            tx.execute("INSERT INTO agent_model_profiles(id,user_id,revision,config_json,api_key,updated_at) VALUES(?1,?2,1,?3,?4,?5)", params![profile_id, user, config, key, chrono::Utc::now().timestamp()])?;
            tx.execute(
                "INSERT INTO agent_model_names(user_id,profile_id,display_name) VALUES(?1,?2,?3)",
                params![user, profile_id, name],
            )?;
            tx.execute("INSERT INTO agent_model_channel_members(user_id,channel_id,profile_id,model,position,active) VALUES(?1,?2,?3,?4,?5,1)", params![user, id, profile_id, model.model, position])?;
        }
        tx.execute(
            "INSERT INTO agent_model_channel_states(user_id,channel_id,enabled) VALUES(?1,?2,?3)",
            params![user, id, source.profile.enabled],
        )?;
        let result = if let Some(draft) = draft {
            if let Some(changes) = draft.changes {
                channels::save_channel_tx(
                    &tx,
                    id,
                    user,
                    1,
                    changes.update,
                    &draft.display_name,
                    changes.model_ids,
                )?
            } else {
                let name = draft.display_name.trim();
                if name.is_empty()
                    || name.chars().count() > 64
                    || name.len() > 256
                    || name.chars().any(char::is_control)
                {
                    return Err(ModelError::Invalid(
                        "channel name must contain 1 to 64 printable characters",
                    ));
                }
                tx.execute(
                    "UPDATE agent_model_channels SET display_name=?3 WHERE user_id=?1 AND id=?2",
                    params![user, id, name],
                )?;
                tx.execute("UPDATE agent_model_names SET display_name=?3 WHERE user_id=?1 AND profile_id IN (SELECT profile_id FROM agent_model_channel_members WHERE user_id=?1 AND channel_id=?2)", params![user,id,name])?;
                channel_view(&tx, id, user)?
            }
        } else {
            channel_view(&tx, id, user)?
        };
        tx.commit()?;
        Ok(result)
    }
}
