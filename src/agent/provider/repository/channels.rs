//! A channel edits its model profiles atomically. Stable profile IDs keep existing sessions intact.
use super::*;
use std::collections::HashSet;

fn channel_view(db: &Connection, id: &str, user: &str) -> Result<ModelChannel> {
    let channel:Option<(String,u64)>=db.query_row("SELECT display_name,revision FROM agent_model_channels WHERE user_id=?1 AND id=?2",params![user,id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
    let Some((name,revision))=channel else {
        let profile=profile_view(db,id,user,row(db,id,user)?.ok_or(ModelError::NotFound)?)?;
        if profile.channel_id.is_some() {return Err(ModelError::NotFound);}
        let models=vec![ChannelModel{id:profile.id.clone(),model:profile.config.model.clone()}];
        return Ok(ModelChannel{profile,models});
    };
    let mut query=db.prepare("SELECT profile_id,model FROM agent_model_channel_members WHERE user_id=?1 AND channel_id=?2 AND active=1 ORDER BY position,profile_id")?;
    let models=query.query_map(params![user,id],|r|Ok(ChannelModel{id:r.get(0)?,model:r.get(1)?}))?.collect::<std::result::Result<Vec<_>,_>>()?;
    let first=models.first().ok_or(ModelError::Database)?;
    let mut profile=profile_view(db,&first.id,user,row(db,&first.id,user)?.ok_or(ModelError::Database)?)?;
    profile.id=id.into();profile.display_name=name;profile.revision=revision;
    Ok(ModelChannel{profile,models})
}

impl ModelRepository {
    pub fn channels(&self,user:&str)->Result<Vec<ModelChannel>> {
        checked_id(user)?;
        let db=self.store.database.lock().map_err(|_|ModelError::Database)?;
        let mut query=db.prepare("SELECT id FROM agent_model_channels WHERE user_id=?1 UNION ALL SELECT p.id FROM agent_model_profiles p WHERE p.user_id=?1 AND NOT EXISTS(SELECT 1 FROM agent_model_channel_members m WHERE m.user_id=p.user_id AND m.profile_id=p.id) ORDER BY id LIMIT 32")?;
        let ids=query.query_map([user],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;
        ids.into_iter().map(|id|channel_view(&db,&id,user)).collect()
    }

    pub fn save_channel(&self,id:&str,user:&str,expected_revision:u64,mut update:ModelConfigUpdate,name:&str,models:Vec<String>)->Result<ModelChannel> {
        checked_id(id)?;checked_id(user)?;
        let name=name.trim();
        if name.is_empty()||name.chars().count()>64||name.len()>256||name.chars().any(char::is_control) {return Err(ModelError::Invalid("channel name must contain 1 to 64 printable characters"));}
        if models.is_empty()||models.len()>64 {return Err(ModelError::Invalid("a channel requires 1 to 64 model IDs"));}
        let models=models.into_iter().map(|model|model.trim().to_owned()).collect::<Vec<_>>();
        let mut unique=HashSet::new();
        for model in &models {
            if !unique.insert(model.clone()) {return Err(ModelError::Invalid("model IDs in one channel must be unique"));}
            update.config.model=model.clone();update.config.clone().normalized()?;
        }
        update.config.model=models[0].clone();
        let base=update.config.normalized()?;
        let mut db=self.store.database.lock().map_err(|_|ModelError::Database)?;
        let tx=db.transaction()?;
        let owner:Option<String>=tx.query_row("SELECT channel_id FROM agent_model_channel_members WHERE user_id=?1 AND profile_id=?2",params![user,id],|r|r.get(0)).optional()?;
        if owner.as_deref().is_some_and(|owner|owner!=id) {return Err(ModelError::Invalid("profile belongs to another channel"));}
        let previous=row(&tx,id,user)?;
        let channel_revision:Option<u64>=tx.query_row("SELECT revision FROM agent_model_channels WHERE user_id=?1 AND id=?2",params![user,id],|r|r.get(0)).optional()?;
        let revision=channel_revision.or_else(||previous.as_ref().map(|r|r.0)).unwrap_or(0);
        if revision!=expected_revision {return Err(ModelError::Stale);}
        if previous.is_none()&&channel_count(&tx,user)? >=32 {return Err(ModelError::Invalid("at most 32 channels per user"));}
        let next=revision.checked_add(1).filter(|n|*n<=i64::MAX as u64).ok_or(ModelError::Stale)?;
        let key=match update.api_key {
            KeyChange::Keep=>previous.as_ref().and_then(|r|r.2.as_ref()).map(|key|secrets::unprotect(key,&entropy(user,id)).map_err(|_|ModelError::Credentials)).transpose()?,
            KeyChange::Set{value}=>Some(SecretKey::new(value.0)?.0),
            KeyChange::Clear=>None,
        };
        // Retain removed entries as inactive so re-adding the same model reuses its history identity.
        let mut entries:Vec<(String,String,bool,usize)>={
            let mut query=tx.prepare("SELECT profile_id,model FROM agent_model_channel_members WHERE user_id=?1 AND channel_id=?2 ORDER BY position,profile_id")?;
            let rows=query.query_map(params![user,id],|r|Ok((r.get(0)?,r.get(1)?,false,0)))?.collect::<std::result::Result<Vec<_>,_>>()?;
            rows
        };
        if entries.is_empty() {
            let model=if let Some((_,json,_))=&previous {serde_json::from_str::<ModelConfig>(json).map_err(|_|ModelError::Database)?.model}else{models[0].clone()};
            entries.push((id.into(),model,false,0));
        }
        for (position,model) in models.iter().enumerate() {
            if let Some(entry)=entries.iter_mut().find(|entry|entry.1==*model) {entry.2=true;entry.3=position;}
            else {entries.push((uuid::Uuid::new_v4().to_string(),model.clone(),true,position));}
        }
        let mut added=0;
        for entry in &entries {if row(&tx,&entry.0,user)?.is_none(){added+=1;}}
        let total:usize=tx.query_row("SELECT COUNT(*) FROM agent_model_profiles WHERE user_id=?1",[user],|r|r.get(0))?;
        if total+added>4096 {return Err(ModelError::Invalid("model profile storage limit reached"));}
        tx.execute("INSERT INTO agent_model_channels(user_id,id,display_name,revision) VALUES(?1,?2,?3,?4) ON CONFLICT(user_id,id) DO UPDATE SET display_name=excluded.display_name,revision=excluded.revision",params![user,id,name,next])?;
        for (profile,model,active,position) in entries {
            let mut config=base.clone();config.model=model.clone();let config=config.normalized()?;
            let version=row(&tx,&profile,user)?.map(|r|r.0).unwrap_or(0).checked_add(1).filter(|n|*n<=i64::MAX as u64).ok_or(ModelError::Stale)?;
            let encrypted=key.as_ref().map(|key|secrets::protect(key,&entropy(user,&profile)).map_err(|_|ModelError::Credentials)).transpose()?;
            let json=serde_json::to_string(&config).map_err(|_|ModelError::Database)?;
            tx.execute("INSERT INTO agent_model_profiles(id,user_id,revision,config_json,api_key,updated_at) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(user_id,id) DO UPDATE SET revision=excluded.revision,config_json=excluded.config_json,api_key=excluded.api_key,updated_at=excluded.updated_at",params![profile,user,version,json,encrypted,chrono::Utc::now().timestamp()])?;
            tx.execute("INSERT INTO agent_model_names(user_id,profile_id,display_name) VALUES(?1,?2,?3) ON CONFLICT(user_id,profile_id) DO UPDATE SET display_name=excluded.display_name",params![user,profile,name])?;
            tx.execute("INSERT INTO agent_model_channel_members(user_id,channel_id,profile_id,model,position,active) VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(user_id,profile_id) DO UPDATE SET position=excluded.position,active=excluded.active",params![user,id,profile,model,position,active])?;
        }
        let result=channel_view(&tx,id,user)?;
        tx.commit()?;Ok(result)
    }

    pub fn delete_channel(&self,id:&str,user:&str,expected_revision:u64)->Result<()> {
        checked_id(id)?;checked_id(user)?;
        let mut db=self.store.database.lock().map_err(|_|ModelError::Database)?;
        let tx=db.transaction()?;
        let channel=channel_view(&tx,id,user)?;
        if channel.profile.revision!=expected_revision {return Err(ModelError::Stale);}
        let mut ids={
            let mut query=tx.prepare("SELECT profile_id FROM agent_model_channel_members WHERE user_id=?1 AND channel_id=?2")?;
            let rows=query.query_map(params![user,id],|r|r.get::<_,String>(0))?.collect::<std::result::Result<Vec<_>,_>>()?;rows
        };
        if ids.is_empty(){ids.push(id.into());}
        tx.execute("DELETE FROM agent_model_channel_members WHERE user_id=?1 AND channel_id=?2",params![user,id])?;
        tx.execute("DELETE FROM agent_model_channels WHERE user_id=?1 AND id=?2",params![user,id])?;
        for profile in ids {
            tx.execute("DELETE FROM agent_model_names WHERE user_id=?1 AND profile_id=?2",params![user,profile])?;
            tx.execute("DELETE FROM agent_model_profiles WHERE user_id=?1 AND id=?2",params![user,profile])?;
        }
        tx.commit()?;Ok(())
    }
}
