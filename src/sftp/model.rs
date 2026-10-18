use serde::{Deserialize, Serialize};
use std::{sync::Mutex, time::Instant};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Kind {
    File,
    Directory,
    Symlink,
    Other,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Direction {
    Upload,
    Download,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Conflict {
    #[default]
    Fail,
    Skip,
    Replace,
}
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Status {
    Pending,
    Running,
    Sent,
    Completed,
    Skipped,
    Failed,
    Cancelled,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AccessMode {
    #[default]
    Login,
    SudoRoot,
}
#[derive(Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct OpenRequest {
    pub connection_id: String,
    pub root_path: Option<String>,
    #[serde(default)]
    pub access_mode: AccessMode,
    pub sudo_password: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub sftp_session_id: String,
    pub connection_id: String,
    pub root_path: String,
    pub login_username: String,
    pub access_username: String,
    pub access_mode: AccessMode,
    pub created_at: chrono::DateTime<chrono::Utc>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Entry {
    pub name: String,
    pub path: String,
    pub kind: Kind,
    pub size: u64,
    pub modified_at: u64,
    pub permissions: String,
}
#[derive(Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ManifestItem {
    pub relative_path: String,
    pub kind: Kind,
    pub size: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CreateRequest {
    pub sftp_session_id: String,
    pub direction: Direction,
    pub remote_path: String,
    #[serde(default)]
    pub conflict: Conflict,
    pub items: Option<Vec<ManifestItem>>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub item_id: String,
    pub relative_path: String,
    pub kind: Kind,
    pub size: u64,
    pub transferred_bytes: u64,
    pub status: Status,
    pub error_code: Option<String>,
    pub message: Option<String>,
}
impl From<ManifestItem> for Item {
    fn from(item: ManifestItem) -> Self {
        Self {
            item_id: uuid::Uuid::new_v4().to_string(),
            relative_path: item.relative_path,
            kind: item.kind,
            size: item.size,
            transferred_bytes: 0,
            status: Status::Pending,
            error_code: None,
            message: None,
        }
    }
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub transfer_id: String,
    pub direction: Direction,
    pub status: &'static str,
    pub total_bytes: u64,
    pub transferred_bytes: u64,
    pub completed_items: usize,
    pub total_items: usize,
    pub failed_items: usize,
    pub awaiting_confirmation: usize,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub active_items: Vec<Item>,
}
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TransferView {
    pub transfer_id: String,
    pub sftp_session_id: String,
    pub direction: Direction,
    pub remote_path: String,
    pub progress: Progress,
    pub items: Vec<Item>,
}
pub(crate) struct TaskData {
    pub items: Vec<Item>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
    pub touched: Instant,
}
pub(crate) struct Task {
    pub id: String,
    pub session_id: String,
    pub direction: Direction,
    pub remote: String,
    pub base: String,
    pub conflict: Conflict,
    pub stop: CancellationToken,
    pub data: Mutex<TaskData>,
    pub _capacity: tokio::sync::OwnedSemaphorePermit,
}
impl Task {
    pub fn view(&self) -> TransferView {
        let data = self.data.lock().unwrap();
        let completed = data
            .items
            .iter()
            .filter(|i| matches!(i.status, Status::Completed | Status::Skipped))
            .count();
        let failed = data
            .items
            .iter()
            .filter(|i| i.status == Status::Failed)
            .count();
        let sent = data
            .items
            .iter()
            .filter(|i| i.status == Status::Sent)
            .count();
        let running = data
            .items
            .iter()
            .filter(|i| i.status == Status::Running)
            .cloned()
            .collect::<Vec<_>>();
        let status = if self.stop.is_cancelled() {
            "CANCELLED"
        } else if !running.is_empty() {
            "RUNNING"
        } else if failed > 0 {
            "FAILED"
        } else if completed == data.items.len() {
            "COMPLETED"
        } else if sent > 0 {
            "AWAITING_CONFIRMATION"
        } else {
            "PENDING"
        };
        TransferView {
            transfer_id: self.id.clone(),
            sftp_session_id: self.session_id.clone(),
            direction: self.direction,
            remote_path: self.remote.clone(),
            progress: Progress {
                transfer_id: self.id.clone(),
                direction: self.direction,
                status,
                total_bytes: data
                    .items
                    .iter()
                    .filter(|i| i.status != Status::Skipped)
                    .map(|i| i.size)
                    .sum(),
                transferred_bytes: data
                    .items
                    .iter()
                    .filter(|i| i.status != Status::Skipped)
                    .map(|i| i.transferred_bytes.min(i.size))
                    .sum(),
                completed_items: completed,
                total_items: data.items.len(),
                failed_items: failed,
                awaiting_confirmation: sent,
                updated_at: data.updated_at,
                active_items: running,
            },
            items: data.items.clone(),
        }
    }
    pub fn cancel(&self) {
        self.stop.cancel();
        self.mutate(|items| {
            for item in items {
                if !matches!(item.status, Status::Completed | Status::Skipped) {
                    item.status = Status::Cancelled;
                    item.error_code = Some("SFTP_CANCELLED".into());
                    item.message = Some("传输已取消".into());
                }
            }
        });
    }
    pub fn mutate(&self, f: impl FnOnce(&mut Vec<Item>)) {
        let mut data = self.data.lock().unwrap();
        f(&mut data.items);
        data.updated_at = chrono::Utc::now();
        data.touched = Instant::now();
    }
    pub fn item(&self, id: &str) -> super::Result<Item> {
        self.data
            .lock()
            .unwrap()
            .items
            .iter()
            .find(|i| i.item_id == id)
            .cloned()
            .ok_or_else(super::Error::missing)
    }
    pub fn finish(&self, id: &str, status: Status, error: Option<&super::Error>) {
        self.mutate(|items| {
            if let Some(item) = items.iter_mut().find(|i| i.item_id == id) {
                if item.status == Status::Cancelled {
                    return;
                }
                item.status = status;
                item.error_code = error.map(|e| e.code.into());
                item.message = error.map(|e| e.message.clone());
            }
        });
    }
    pub fn progress(&self, id: &str, bytes: u64) {
        self.mutate(|items| {
            if let Some(item) = items.iter_mut().find(|i| i.item_id == id) {
                item.transferred_bytes = bytes.min(item.size);
            }
        });
    }
}
