mod database;
mod http;
pub mod model;
use crate::store::Store;
use axum::{
    body::{Body, Bytes},
    extract::Multipart,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
};
use hmac::{Hmac, KeyInit, Mac};
pub(crate) use http::routes;
pub use model::{Settings, StorageStats, UploadedFile};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{Mutex as AsyncMutex, Notify, OnceCell, OwnedSemaphorePermit, Semaphore},
};
use tokio_util::sync::CancellationToken;
pub type Result<T> = std::result::Result<T, Error>;
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct Error {
    pub code: &'static str,
    pub message: String,
    pub status: StatusCode,
}
impl Error {
    fn new(code: &'static str, message: impl Into<String>, status: StatusCode) -> Self {
        Self {
            code,
            message: message.into(),
            status,
        }
    }
    fn invalid(message: impl Into<String>) -> Self {
        Self::new("FILE_INVALID", message, StatusCode::BAD_REQUEST)
    }
    fn kind() -> Self {
        Self::new(
            "FILE_TYPE_NOT_ALLOWED",
            "不支持此文件扩展名",
            StatusCode::BAD_REQUEST,
        )
    }
    fn large() -> Self {
        Self::new(
            "FILE_TOO_LARGE",
            "文件或 multipart 请求超过大小限制",
            StatusCode::PAYLOAD_TOO_LARGE,
        )
    }
    fn quota() -> Self {
        Self::new(
            "FILE_STORAGE_QUOTA_EXCEEDED",
            "附件存储容量或文件数量已满，请删除文件或调整存储限额",
            StatusCode::PAYLOAD_TOO_LARGE,
        )
    }
    fn disabled() -> Self {
        Self::new(
            "FILE_STORAGE_NOT_CONFIGURED",
            "未启用本地附件存储",
            StatusCode::SERVICE_UNAVAILABLE,
        )
    }
    fn config() -> Self {
        Self::new(
            "FILE_STORAGE_CONFIG_INVALID",
            "本地附件存储设置或目录不合法",
            StatusCode::SERVICE_UNAVAILABLE,
        )
    }
    fn unavailable() -> Self {
        Self::new(
            "FILE_STORAGE_UNAVAILABLE",
            "本地附件存储读写失败，请检查磁盘空间和目录权限",
            StatusCode::SERVICE_UNAVAILABLE,
        )
    }
    fn failed() -> Self {
        Self::new(
            "FILE_UPLOAD_FAILED",
            "文件处理或元数据保存失败",
            StatusCode::INTERNAL_SERVER_ERROR,
        )
    }
    fn busy() -> Self {
        Self::new(
            "FILE_UPLOAD_BUSY",
            "文件正在处理或并发已满，请稍后重试",
            StatusCode::TOO_MANY_REQUESTS,
        )
    }
    fn link() -> Self {
        Self::new(
            "FILE_DOWNLOAD_LINK_INVALID",
            "下载链接参数或签名无效",
            StatusCode::BAD_REQUEST,
        )
    }
    fn not_found() -> Self {
        Self::new(
            "FILE_DOWNLOAD_NOT_FOUND",
            "附件不存在或已清理",
            StatusCode::NOT_FOUND,
        )
    }
    fn expired() -> Self {
        Self::new(
            "FILE_DOWNLOAD_LINK_EXPIRED",
            "下载链接已过期，请刷新链接",
            StatusCode::GONE,
        )
    }
    fn occupied() -> Self {
        Self::new(
            "FILE_STORAGE_BUSY",
            "同一附件目录正在由另一个客户端实例使用，请关闭该实例后重试",
            StatusCode::SERVICE_UNAVAILABLE,
        )
    }
    fn cancelled() -> Self {
        Self::new(
            "FILE_UPLOAD_FAILED",
            "附件服务已停止或请求已中断",
            StatusCode::SERVICE_UNAVAILABLE,
        )
    }
}
impl IntoResponse for Error {
    fn into_response(self) -> Response {
        crate::error::failure(self.status, self.code, self.message)
    }
}
impl From<rusqlite::Error> for Error {
    fn from(_: rusqlite::Error) -> Self {
        Self::failed()
    }
}
impl From<axum::extract::multipart::MultipartError> for Error {
    fn from(e: axum::extract::multipart::MultipartError) -> Self {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            Self::large()
        } else {
            Self::invalid("multipart 不完整或文件流已中断")
        }
    }
}
pub struct Manager {
    pub(crate) store: Arc<Store>,
    pub settings: Settings,
    pub(crate) root_path: Option<PathBuf>,
    root: OnceCell<PathBuf>,
    lease: Mutex<Option<std::fs::File>>,
    key: [u8; 32],
    base_url: String,
    valid: bool,
    uploads: Arc<Semaphore>,
    downloads: Arc<Semaphore>,
    active: Mutex<HashSet<String>>,
    changed: Notify,
    cleanup_lock: AsyncMutex<()>,
    stop: CancellationToken,
}
impl Manager {
    pub fn new(store: Arc<Store>, settings: Settings, base_url: String) -> Arc<Self> {
        let mut key = [0u8; 32];
        let url_valid = url::Url::parse(&base_url).is_ok_and(|url| {
            url.scheme() == "http"
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
                && url.fragment().is_none()
                && url.path() == "/"
                && url.host_str().is_some_and(|s| {
                    s.trim_matches(['[', ']'])
                        .parse::<std::net::IpAddr>()
                        .is_ok_and(|ip| ip.is_loopback())
                })
        });
        let valid = settings.validate().is_ok() && getrandom::fill(&mut key).is_ok() && url_valid;
        let manager = Arc::new(Self {
            root_path: store.data_directory.as_ref().map(|p| p.join("attachments")),
            store,
            uploads: Arc::new(Semaphore::new(settings.max_uploads.clamp(1, 64))),
            downloads: Arc::new(Semaphore::new(8)),
            settings,
            key,
            base_url: base_url.trim_end_matches('/').into(),
            valid,
            root: OnceCell::new(),
            lease: Mutex::new(None),
            active: Mutex::new(HashSet::new()),
            changed: Notify::new(),
            cleanup_lock: AsyncMutex::new(()),
            stop: CancellationToken::new(),
        });
        if tokio::runtime::Handle::try_current().is_ok() {
            let weak = Arc::downgrade(&manager);
            let token = manager.stop.clone();
            let interval = manager
                .settings
                .cleanup_interval
                .max(Duration::from_millis(10));
            tokio::spawn(async move {
                // Delay startup recovery until the HTTP host has been assembled.
                tokio::select! {_=token.cancelled()=>return,_=tokio::time::sleep(Duration::from_millis(100))=>{}}
                loop {
                    let Some(manager) = weak.upgrade() else { break };
                    if manager.settings.enabled && manager.root_path.is_some() {
                        if let Err(error) = manager.cleanup().await {
                            tracing::warn!(code = error.code, "attachment cleanup postponed");
                        }
                    }
                    drop(manager);
                    tokio::select! {_=token.cancelled()=>break,_=tokio::time::sleep(interval)=>{}}
                }
            });
        }
        manager
    }
    fn enabled(&self) -> Result<()> {
        if !self.settings.enabled || self.root_path.is_none() {
            return Err(Error::disabled());
        }
        if !self.valid {
            return Err(Error::config());
        }
        if self.stop.is_cancelled() {
            return Err(Error::cancelled());
        }
        Ok(())
    }
    async fn root(&self) -> Result<&PathBuf> {
        if self.stop.is_cancelled() && self.lease.lock().unwrap().is_none() {
            return Err(Error::cancelled());
        }
        if !self.valid {
            return Err(Error::config());
        }
        let root = self
            .root
            .get_or_try_init(|| async {
                let path = self.root_path.clone().ok_or_else(Error::disabled)?;
                let canonical = tokio::task::spawn_blocking(move || {
                    if let Ok(metadata) = std::fs::symlink_metadata(&path) {
                        if reparse(&metadata) || !metadata.is_dir() {
                            return Err(Error::config());
                        }
                    }
                    std::fs::create_dir_all(&path).map_err(|_| Error::unavailable())?;
                    let metadata =
                        std::fs::symlink_metadata(&path).map_err(|_| Error::unavailable())?;
                    if reparse(&metadata) || !metadata.is_dir() {
                        return Err(Error::config());
                    }
                    let canonical =
                        std::fs::canonicalize(path).map_err(|_| Error::unavailable())?;
                    let mut options = std::fs::OpenOptions::new();
                    options.read(true).write(true).create(true).truncate(false);
                    #[cfg(windows)]
                    {
                        use std::os::windows::fs::OpenOptionsExt;
                        options.custom_flags(0x00200000).share_mode(3);
                    }
                    let lease = options
                        .open(canonical.join(".storage.lock"))
                        .map_err(|_| Error::unavailable())?;
                    if reparse(&lease.metadata().map_err(|_| Error::unavailable())?) {
                        return Err(Error::config());
                    }
                    lease.try_lock().map_err(|error| match error {
                        std::fs::TryLockError::WouldBlock => Error::occupied(),
                        _ => Error::unavailable(),
                    })?;
                    Ok((canonical, lease))
                })
                .await
                .map_err(|_| Error::unavailable())??;
                let (canonical, lease) = canonical;
                *self.lease.lock().unwrap() = Some(lease);
                Ok::<_, Error>(canonical)
            })
            .await?;
        let metadata = tokio::fs::symlink_metadata(root)
            .await
            .map_err(|_| Error::unavailable())?;
        if reparse(&metadata) || !metadata.is_dir() {
            return Err(Error::config());
        }
        Ok(root)
    }
    async fn wait<T>(&self, future: impl std::future::Future<Output = Result<T>>) -> Result<T> {
        tokio::select! {biased;_=self.stop.cancelled()=>Err(Error::cancelled()),result=tokio::time::timeout(self.settings.idle_timeout,future)=>result.map_err(|_|Error::invalid("上传流长时间无进展"))?}
    }
    pub async fn upload(self: Arc<Self>, multipart: Multipart) -> Result<UploadedFile> {
        self.enabled()?;
        let permit = self
            .uploads
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        // Own the stream through commit: a lost HTTP response is an uncertain outcome,
        // while an interrupted body still triggers cleanup. The client never retries automatically.
        tokio::spawn(async move { self.upload_owned(multipart, permit).await })
            .await
            .map_err(|_| Error::failed())?
    }
    async fn upload_owned(
        self: Arc<Self>,
        mut multipart: Multipart,
        permit: OwnedSemaphorePermit,
    ) -> Result<UploadedFile> {
        let root = self.root().await?.clone();
        let mut field = self
            .wait(async { Ok(multipart.next_field().await?) })
            .await?
            .ok_or_else(|| Error::invalid("缺少 file 字段"))?;
        if field.name() != Some("file") {
            return Err(Error::invalid("请求只能包含一个 file 字段"));
        }
        let (name, extension) = model::file_name(
            field
                .file_name()
                .ok_or_else(|| Error::invalid("缺少文件名"))?,
        )?;
        let id = uuid::Uuid::new_v4().to_string();
        let asset = model::Asset {
            id: id.clone(),
            file_name: name,
            content_type: model::content_type(field.content_type()),
            object_key: format!("{id}.{extension}"),
            size: self.settings.max_file_bytes,
            sha256: String::new(),
            status: "UPLOADING".into(),
            created_at: chrono::Utc::now().timestamp(),
        };
        self.active.lock().unwrap().insert(id.clone());
        let mut guard = UploadGuard {
            manager: self.clone(),
            asset: asset.clone(),
            permit: Some(permit),
            file: None,
            complete: false,
        };
        self.reserve(asset.clone()).await?;
        let temp = root.join(format!(".{id}.uploading"));
        guard.file = Some(
            tokio::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(temp)
                .await
                .map_err(|_| Error::unavailable())?,
        );
        let mut hash = Sha256::new();
        let mut size = 0u64;
        while let Some(chunk) = self.wait(async { Ok(field.chunk().await?) }).await? {
            size = size
                .checked_add(chunk.len() as u64)
                .filter(|n| *n <= self.settings.max_file_bytes)
                .ok_or_else(Error::large)?;
            // Axum/multer chunks are streamed; disk writes and hashing never need the full file.
            for bytes in chunk.chunks(65536) {
                guard
                    .file
                    .as_mut()
                    .unwrap()
                    .write_all(bytes)
                    .await
                    .map_err(|_| Error::unavailable())?;
                Digest::update(&mut hash, bytes);
            }
        }
        drop(field);
        if self
            .wait(async { Ok(multipart.next_field().await?) })
            .await?
            .is_some()
        {
            return Err(Error::invalid(
                "请求只能包含一个 file 字段，不接受 userId 或额外文件",
            ));
        }
        if size == 0 {
            return Err(Error::invalid("不能上传空文件"));
        }
        guard
            .file
            .as_mut()
            .unwrap()
            .flush()
            .await
            .map_err(|_| Error::unavailable())?;
        guard
            .file
            .as_mut()
            .unwrap()
            .sync_all()
            .await
            .map_err(|_| Error::unavailable())?;
        guard.file.take();
        // Atomic, non-overwriting publication in the same directory.
        self.root().await?;
        tokio::fs::hard_link(
            root.join(format!(".{id}.uploading")),
            root.join(&asset.object_key),
        )
        .await
        .map_err(|_| Error::unavailable())?;
        let digest = model::hex(&hash.finalize());
        self.finish(id.clone(), size, digest.clone()).await?;
        // Publication is committed before replying. Cleanup of a duplicate temp name is retryable.
        guard.complete = true;
        let _ = tokio::fs::remove_file(root.join(format!(".{id}.uploading"))).await;
        let mut saved = asset;
        saved.size = size;
        saved.sha256 = digest;
        saved.status = "UPLOADED".into();
        self.signed(&saved)
    }
    fn signed(&self, asset: &model::Asset) -> Result<UploadedFile> {
        let seconds = chrono::Utc::now()
            .timestamp()
            .checked_add(self.settings.download_ttl.as_secs() as i64)
            .ok_or_else(Error::config)?;
        let expires = chrono::DateTime::from_timestamp(seconds, 0).ok_or_else(Error::config)?;
        let signature = self.signature(&asset.object_key, &asset.file_name, expires.timestamp())?;
        let query = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("storageId", model::STORAGE_ID)
            .append_pair("objectKey", &asset.object_key)
            .append_pair("fileName", &asset.file_name)
            .append_pair("expires", &expires.timestamp().to_string())
            .append_pair("signature", &model::hex(&signature))
            .finish();
        Ok(UploadedFile {
            file_id: asset.id.clone(),
            file_name: asset.file_name.clone(),
            content_type: asset.content_type.clone(),
            size: asset.size,
            sha256: asset.sha256.clone(),
            status: asset.status.clone(),
            download_url: format!("{}/api/v1/files/local?{query}", self.base_url),
            url_expires_at: expires,
        })
    }
    fn signature(&self, key: &str, name: &str, expires: i64) -> Result<Vec<u8>> {
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| Error::config())?;
        Mac::update(
            &mut mac,
            format!("{}\n{key}\n{name}\n{expires}", model::STORAGE_ID).as_bytes(),
        );
        Ok(mac.finalize().into_bytes().to_vec())
    }
    pub async fn metadata(&self, id: &str) -> Result<UploadedFile> {
        self.enabled()?;
        let asset = self.get(id).await?;
        if asset.status != "UPLOADED" {
            return Err(Error::not_found());
        }
        let _ = self.open_asset(&asset).await?;
        self.signed(&asset)
    }
    async fn open_asset(&self, asset: &model::Asset) -> Result<tokio::fs::File> {
        if model::object_id(&asset.object_key)? != asset.id {
            return Err(Error::link());
        }
        let root = self.root().await?.clone();
        let path = root.join(&asset.object_key);
        let size = asset.size;
        let file = tokio::task::spawn_blocking(move || {
            let meta = std::fs::symlink_metadata(&path).map_err(|_| Error::not_found())?;
            if reparse(&meta) || !meta.is_file() || meta.len() != size {
                return Err(Error::not_found());
            }
            let mut options = std::fs::OpenOptions::new();
            options.read(true);
            #[cfg(windows)]
            {
                use std::os::windows::fs::OpenOptionsExt;
                options.custom_flags(0x00200000);
            } // FILE_FLAG_OPEN_REPARSE_POINT
            let file = options.open(&path).map_err(|_| Error::not_found())?;
            let meta = file.metadata().map_err(|_| Error::not_found())?;
            if reparse(&meta) || !meta.is_file() || meta.len() != size {
                return Err(Error::not_found());
            }
            if std::fs::canonicalize(path)
                .map_err(|_| Error::not_found())?
                .parent()
                != Some(root.as_path())
            {
                return Err(Error::not_found());
            }
            Ok(file)
        })
        .await
        .map_err(|_| Error::unavailable())??;
        Ok(tokio::fs::File::from_std(file))
    }
    pub async fn download(self: Arc<Self>, query: model::DownloadQuery) -> Result<Response> {
        self.enabled()?;
        if query.storage_id != model::STORAGE_ID {
            return Err(Error::link());
        }
        let id = model::object_id(&query.object_key)?;
        if query.expires <= chrono::Utc::now().timestamp() {
            return Err(Error::expired());
        }
        let mut mac = Hmac::<Sha256>::new_from_slice(&self.key).map_err(|_| Error::config())?;
        Mac::update(
            &mut mac,
            format!(
                "{}\n{}\n{}\n{}",
                model::STORAGE_ID,
                query.object_key,
                query.file_name,
                query.expires
            )
            .as_bytes(),
        );
        mac.verify_slice(&model::unhex(&query.signature)?)
            .map_err(|_| Error::link())?;
        let asset = self.get(&id).await?;
        if asset.status != "UPLOADED"
            || asset.object_key != query.object_key
            || asset.file_name != query.file_name
        {
            return Err(Error::not_found());
        }
        let permit = self
            .downloads
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let mut file = self.open_asset(&asset).await?;
        let disposition = format!(
            "attachment; filename=\"download.{}\"; filename*=UTF-8''{}",
            asset.object_key.rsplit('.').next().unwrap_or("bin"),
            percent_encoding::utf8_percent_encode(
                &asset.file_name,
                percent_encoding::NON_ALPHANUMERIC
            )
        );
        let (sender, mut receiver) = tokio::sync::mpsc::channel::<Result<Bytes>>(2);
        let host = self.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let mut sent = 0u64;
            let mut buffer = vec![0u8; 65536];
            let result: Result<()> = async {
                loop {
                    let count = host
                        .wait(async {
                            file.read(&mut buffer)
                                .await
                                .map_err(|_| Error::unavailable())
                        })
                        .await?;
                    if count == 0 {
                        break;
                    }
                    sent += count as u64;
                    if sent > asset.size {
                        return Err(Error::unavailable());
                    }
                    host.wait(async {
                        sender
                            .send(Ok(Bytes::copy_from_slice(&buffer[..count])))
                            .await
                            .map_err(|_| Error::cancelled())
                    })
                    .await?;
                }
                if sent != asset.size {
                    return Err(Error::unavailable());
                }
                Ok(())
            }
            .await;
            if let Err(error) = result {
                tokio::select! {
                    _=host.stop.cancelled()=>{},
                    _=tokio::time::timeout(host.settings.idle_timeout,sender.send(Err(error)))=>{}
                }
            }
        });
        let stream =
            async_stream::stream! {while let Some(bytes)=receiver.recv().await{yield bytes;}};
        Response::builder()
            .header(header::CONTENT_TYPE, "application/octet-stream")
            .header(header::CONTENT_LENGTH, asset.size)
            .header(header::CONTENT_DISPOSITION, disposition)
            .header(header::CACHE_CONTROL, "private, no-store")
            .header("X-Content-Type-Options", "nosniff")
            .body(Body::from_stream(stream))
            .map_err(|_| Error::failed())
    }
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.enabled()?;
        let _lock = self.cleanup_lock.lock().await;
        self.root().await?;
        let asset = self.get(id).await?;
        if self.active.lock().unwrap().contains(id) {
            return Err(Error::busy());
        }
        let record = id.to_string();
        self.db(move |db| {
            db.execute(
                "UPDATE file_assets SET status='DELETING' WHERE id=?1",
                [record],
            )?;
            Ok(())
        })
        .await?;
        self.cleanup_asset(&asset).await
    }
    pub async fn cleanup(&self) -> Result<()> {
        let _lock = self.cleanup_lock.lock().await;
        self.enabled()?;
        self.root().await?;
        self.cleanup_committed_temps().await?;
        let records = self.candidates().await?;
        for record in records {
            if self.active.lock().unwrap().contains(&record.id) {
                continue;
            }
            if let Err(error) = self.cleanup_asset(&record).await {
                tracing::warn!(file_id=%record.id,code=error.code,"attachment cleanup requires retry");
            }
        }
        Ok(())
    }
    async fn cleanup_committed_temps(&self) -> Result<()> {
        let mut entries = tokio::fs::read_dir(self.root().await?)
            .await
            .map_err(|_| Error::unavailable())?;
        let mut visited = 0usize;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(|_| Error::unavailable())?
        {
            visited += 1;
            if visited
                > self
                    .settings
                    .max_files
                    .saturating_mul(2)
                    .saturating_add(100)
            {
                break;
            }
            let name = entry.file_name().to_string_lossy().to_string();
            let Some(id) = name
                .strip_prefix('.')
                .and_then(|n| n.strip_suffix(".uploading"))
            else {
                continue;
            };
            if self.active.lock().unwrap().contains(id) {
                continue;
            }
            if let Ok(asset) = self.get(id).await {
                if asset.status == "UPLOADED" {
                    if let Err(error) = tokio::fs::remove_file(entry.path()).await {
                        if error.kind() != std::io::ErrorKind::NotFound {
                            tracing::warn!(file_id=%id,"committed attachment temporary name requires cleanup");
                        }
                    }
                }
            }
        }
        Ok(())
    }
    pub async fn open_attachment(&self, id: &str) -> Result<(model::Asset, tokio::fs::File)> {
        self.enabled()?;
        let asset = self.get(id).await?;
        if asset.status != "UPLOADED" {
            return Err(Error::not_found());
        }
        let file = self.open_asset(&asset).await?;
        Ok((asset, file))
    }
    async fn cleanup_asset(&self, asset: &model::Asset) -> Result<()> {
        if model::object_id(&asset.object_key)? != asset.id {
            return Err(Error::config());
        }
        let root = self.root().await?;
        for path in [
            root.join(format!(".{}.uploading", asset.id)),
            root.join(&asset.object_key),
        ] {
            if let Err(error) = tokio::fs::remove_file(path).await {
                if error.kind() != std::io::ErrorKind::NotFound {
                    let _ = self.cleanup_required(asset.id.clone()).await;
                    return Err(Error::unavailable());
                }
            }
        }
        self.remove_record(asset.id.clone()).await
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        let _ = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let changed = self.changed.notified();
                if self.active.lock().unwrap().is_empty() {
                    break;
                }
                changed.await;
            }
        })
        .await;
        let _cleanup = self.cleanup_lock.lock().await;
        self.lease.lock().unwrap().take();
    }
}
impl Drop for Manager {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
struct UploadGuard {
    manager: Arc<Manager>,
    asset: model::Asset,
    permit: Option<OwnedSemaphorePermit>,
    file: Option<tokio::fs::File>,
    complete: bool,
}
impl Drop for UploadGuard {
    fn drop(&mut self) {
        self.file.take();
        let manager = self.manager.clone();
        let asset = self.asset.clone();
        let permit = self.permit.take();
        if self.complete {
            manager.active.lock().unwrap().remove(&asset.id);
            manager.changed.notify_waiters();
            return;
        }
        if let Ok(runtime) = tokio::runtime::Handle::try_current() {
            runtime.spawn(async move{
            let _permit=permit;
            if let Err(error)=manager.cleanup_asset(&asset).await{tracing::warn!(file_id=%asset.id,code=error.code,"interrupted upload requires cleanup");}
            manager.active.lock().unwrap().remove(&asset.id);manager.changed.notify_waiters();
        });
        } else {
            manager.active.lock().unwrap().remove(&asset.id);
            manager.changed.notify_waiters();
        }
    }
}
fn reparse(meta: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        meta.file_type().is_symlink() || meta.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}
