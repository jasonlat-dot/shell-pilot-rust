use super::{Error, Result};
use serde::{Deserialize, Serialize};
use std::time::Duration;
pub const EXTENSIONS: &[&str] = &[
    "png", "jpg", "jpeg", "webp", "pdf", "txt", "log", "csv", "json", "yaml", "yml", "md", "docx",
    "xlsx",
];
pub const STORAGE_ID: &str = "local-rust-main";
pub const MAX_FILE_BYTES: u64 = 20 * 1024 * 1024;
pub const MAX_REQUEST_BYTES: usize = 21 * 1024 * 1024;
#[derive(Clone)]
pub struct Settings {
    pub enabled: bool,
    pub max_file_bytes: u64,
    pub max_storage_bytes: u64,
    pub max_files: usize,
    pub max_uploads: usize,
    pub download_ttl: Duration,
    pub retention: Option<Duration>,
    pub idle_timeout: Duration,
    pub cleanup_interval: Duration,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            enabled: true,
            max_file_bytes: MAX_FILE_BYTES,
            max_storage_bytes: 1024 * 1024 * 1024,
            max_files: 10000,
            max_uploads: 4,
            download_ttl: Duration::from_secs(900),
            retention: None,
            idle_timeout: Duration::from_secs(120),
            cleanup_interval: Duration::from_secs(60),
        }
    }
}
impl Settings {
    pub fn validate(&self) -> Result<()> {
        if self.max_file_bytes == 0
            || self.max_file_bytes > MAX_FILE_BYTES
            || self.max_storage_bytes < self.max_file_bytes
            || self.max_storage_bytes > i64::MAX as u64
            || self.max_files == 0
            || self.max_uploads == 0
            || self.download_ttl.as_secs() == 0
            || self.download_ttl.as_secs() > 604800
            || self.idle_timeout.is_zero()
            || self.cleanup_interval.is_zero()
        {
            return Err(Error::config());
        }
        Ok(())
    }
}
#[derive(Clone, Debug)]
pub struct Asset {
    pub id: String,
    pub file_name: String,
    pub content_type: String,
    pub object_key: String,
    pub size: u64,
    pub sha256: String,
    pub status: String,
    pub created_at: i64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UploadedFile {
    pub file_id: String,
    pub file_name: String,
    pub content_type: String,
    pub size: u64,
    pub sha256: String,
    pub status: String,
    pub download_url: String,
    pub url_expires_at: chrono::DateTime<chrono::Utc>,
}
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StorageStats {
    pub enabled: bool,
    pub used_bytes: u64,
    pub reserved_bytes: u64,
    pub max_storage_bytes: u64,
    pub file_count: usize,
    pub max_files: usize,
    pub max_file_bytes: u64,
    pub retention_days: u64,
    pub download_link_seconds: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DownloadQuery {
    pub storage_id: String,
    pub object_key: String,
    pub file_name: String,
    pub expires: i64,
    pub signature: String,
}
pub fn file_name(value: &str) -> Result<(String, String)> {
    let value = value.replace('\\', "/");
    let name = value.rsplit('/').next().unwrap_or("").trim();
    if name.is_empty()
        || name.chars().count() > 255
        || name.len() > 1024
        || name.chars().any(char::is_control)
    {
        return Err(Error::invalid("文件名不合法"));
    }
    let extension = name
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    if !EXTENSIONS.contains(&extension.as_str()) {
        return Err(Error::kind());
    }
    Ok((name.into(), extension))
}
pub fn object_id(key: &str) -> Result<String> {
    let (id, ext) = key.rsplit_once('.').ok_or_else(Error::link)?;
    if !EXTENSIONS.contains(&ext) || !uuid::Uuid::parse_str(id).is_ok_and(|u| u.to_string() == id) {
        return Err(Error::link());
    }
    Ok(id.into())
}
pub fn content_type(value: Option<&str>) -> String {
    let value = value.unwrap_or("").trim();
    if value.len() > 127
        || value.split('/').count() != 2
        || value.split('/').any(|p| {
            p.is_empty()
                || !p
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
        })
    {
        return "application/octet-stream".into();
    }
    value.to_ascii_lowercase()
}
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
pub(crate) fn unhex(value: &str) -> Result<Vec<u8>> {
    if value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::link());
    }
    (0..value.len())
        .step_by(2)
        .map(|p| u8::from_str_radix(&value[p..p + 2], 16).map_err(|_| Error::link()))
        .collect()
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FilePage {
    pub items: Vec<UploadedFile>,
    pub offset: usize,
    pub has_more: bool,
}
