use super::{model::Asset, *};
use rusqlite::{params, Connection, OptionalExtension};
pub(crate) fn read(row: &rusqlite::Row<'_>) -> rusqlite::Result<Asset> {
    Ok(Asset {
        id: row.get(0)?,
        file_name: row.get(1)?,
        content_type: row.get(2)?,
        object_key: row.get(3)?,
        size: row.get(4)?,
        sha256: row.get(5)?,
        status: row.get(6)?,
        created_at: row.get(7)?,
    })
}
const SELECT:&str="SELECT id,file_name,content_type,object_key,size,sha256,status,created_at FROM file_assets WHERE owner_id='default'";
impl Manager {
    pub(crate) async fn db<T: Send + 'static>(
        &self,
        action: impl FnOnce(&mut Connection) -> Result<T> + Send + 'static,
    ) -> Result<T> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || {
            let mut db = store.database.lock().map_err(|_| Error::failed())?;
            action(&mut db)
        })
        .await
        .map_err(|_| Error::failed())?
    }
    pub(crate) async fn reserve(&self, asset: Asset) -> Result<()> {
        let max_bytes = self.settings.max_storage_bytes;
        let max_files = self.settings.max_files;
        self.db(move|db|{
            let tx=db.transaction()?;
            let (used,count):(u64,usize)=tx.query_row("SELECT COALESCE(SUM(size),0),COUNT(*) FROM file_assets WHERE status IN ('UPLOADING','UPLOADED','DELETING','CLEANUP_REQUIRED')",[],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if used.checked_add(asset.size).is_none_or(|n|n>max_bytes)||count>=max_files{return Err(Error::quota())}
            tx.execute("INSERT INTO file_assets(id,owner_id,file_name,content_type,object_key,size,sha256,status,created_at) VALUES(?1,'default',?2,?3,?4,?5,'','UPLOADING',?6)",params![asset.id,asset.file_name,asset.content_type,asset.object_key,asset.size,asset.created_at])?;
            tx.commit()?;Ok(())
        }).await
    }
    pub(crate) async fn finish(&self, id: String, size: u64, digest: String) -> Result<()> {
        self.db(move|db|{
            if db.execute("UPDATE file_assets SET size=?2,sha256=?3,status='UPLOADED' WHERE id=?1 AND status='UPLOADING'",params![id,size,digest])?!=1{return Err(Error::failed())}Ok(())
        }).await
    }
    pub(crate) async fn get(&self, id: &str) -> Result<Asset> {
        if !uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id) {
            return Err(Error::not_found());
        }
        let id = id.to_owned();
        self.db(move |db| {
            db.query_row(&format!("{SELECT} AND id=?1"), [id], read)
                .optional()?
                .ok_or_else(Error::not_found)
        })
        .await
    }
    pub(crate) async fn remove_record(&self, id: String) -> Result<()> {
        self.db(move |db| {
            db.execute("DELETE FROM file_assets WHERE id=?1", [id])?;
            Ok(())
        })
        .await
    }
    pub(crate) async fn cleanup_required(&self, id: String) -> Result<()> {
        self.db(move |db| {
            db.execute(
                "UPDATE file_assets SET status='CLEANUP_REQUIRED' WHERE id=?1",
                [id],
            )?;
            Ok(())
        })
        .await
    }
    pub async fn stats(&self) -> Result<model::StorageStats> {
        let (used,reserved,count)=self.db(|db|{
            Ok(db.query_row("SELECT COALESCE(SUM(CASE WHEN status!='UPLOADING' THEN size ELSE 0 END),0),COALESCE(SUM(CASE WHEN status='UPLOADING' THEN size ELSE 0 END),0),COUNT(*) FROM file_assets WHERE status IN ('UPLOADING','UPLOADED','DELETING','CLEANUP_REQUIRED')",[],|r|Ok((r.get::<_,u64>(0)?,r.get::<_,u64>(1)?,r.get::<_,usize>(2)?)))?)
        }).await?;
        Ok(model::StorageStats {
            enabled: self.settings.enabled && self.root_path.is_some(),
            used_bytes: used,
            reserved_bytes: reserved,
            max_storage_bytes: self.settings.max_storage_bytes,
            file_count: count,
            max_files: self.settings.max_files,
            max_file_bytes: self.settings.max_file_bytes,
            retention_days: self
                .settings
                .retention
                .map(|d| d.as_secs() / 86400)
                .unwrap_or(0),
            download_link_seconds: self.settings.download_ttl.as_secs(),
        })
    }
    pub(crate) async fn candidates(&self) -> Result<Vec<Asset>> {
        let cutoff = self
            .settings
            .retention
            .map(|duration| {
                chrono::Utc::now()
                    .timestamp()
                    .saturating_sub(duration.as_secs().min(i64::MAX as u64) as i64)
            })
            .unwrap_or(i64::MIN);
        self.db(move|db|{
            let mut query=db.prepare(&format!("{SELECT} AND (status IN ('UPLOADING','DELETING','CLEANUP_REQUIRED') OR (status='UPLOADED' AND created_at<=?1)) ORDER BY created_at,id LIMIT 100"))?;
            let records=query.query_map([cutoff],read)?.collect::<std::result::Result<Vec<_>,_>>()?;Ok(records)
        }).await
    }
}

impl Manager {
    pub async fn list(&self, offset: usize) -> Result<model::FilePage> {
        self.enabled()?;
        self.root().await?;
        if offset > self.settings.max_files {
            return Err(Error::invalid("文件列表偏移超出范围"));
        }
        let assets=self.db(move|db|{
            let mut statement=db.prepare(&format!("{SELECT} AND status='UPLOADED' ORDER BY created_at DESC,id DESC LIMIT 21 OFFSET ?1"))?;
            let assets=statement.query_map([offset],read)?.collect::<std::result::Result<Vec<_>,_>>()?;
            Ok(assets)
        }).await?;
        let has_more = assets.len() > 20;
        let items = assets
            .into_iter()
            .take(20)
            .map(|asset| self.signed(&asset))
            .collect::<Result<Vec<_>>>()?;
        Ok(model::FilePage {
            items,
            offset,
            has_more,
        })
    }
}
