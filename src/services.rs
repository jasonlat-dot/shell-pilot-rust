//! Shared application services. Desktop IPC and HTTP are adapters over the same state.
use crate::{files, sftp, store::Store, terminal};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub struct Services {
    pub store: Arc<Store>,
    pub terminals: Arc<terminal::TerminalManager>,
    pub sftp: Arc<sftp::Manager>,
    pub files: Arc<files::Manager>,
    pub stop: CancellationToken,
}
impl Services {
    /// Embedded desktop services have no HTTP host or listening socket.
    pub fn native(store: Arc<Store>, settings: files::Settings, instance: uuid::Uuid) -> Arc<Self> {
        Arc::new(Self {
            terminals: Arc::new(terminal::TerminalManager::new(store.clone())),
            sftp: sftp::Manager::new(store.clone()),
            files: files::Manager::new_native(store.clone(), settings, instance),
            store,
            stop: CancellationToken::new(),
        })
    }
    pub fn new(store: Arc<Store>, settings: files::Settings, file_base_url: String) -> Arc<Self> {
        Arc::new(Self {
            terminals: Arc::new(terminal::TerminalManager::new(store.clone())),
            sftp: sftp::Manager::new(store.clone()),
            files: files::Manager::new(store.clone(), settings, file_base_url),
            store,
            stop: CancellationToken::new(),
        })
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.sftp.shutdown();
        self.terminals.shutdown().await;
        self.files.shutdown().await;
    }
}
