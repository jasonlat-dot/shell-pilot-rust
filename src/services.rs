//! Shared application services. Desktop IPC and HTTP are adapters over the same state.
use crate::{agent::chat, files, sftp, store::Store, terminal};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub struct Services {
    pub store: Arc<Store>,
    pub terminals: Arc<terminal::TerminalManager>,
    pub sftp: Arc<sftp::Manager>,
    pub files: Arc<files::Manager>,
    pub agent: Arc<chat::Manager>,
    pub stop: CancellationToken,
}
impl Services {
    /// Embedded desktop services have no HTTP host or listening socket.
    pub fn native(store: Arc<Store>, settings: files::Settings, instance: uuid::Uuid) -> Arc<Self> {
        let stop = CancellationToken::new();
        let terminals = Arc::new(terminal::TerminalManager::new(store.clone()));
        let files = files::Manager::new_native(store.clone(), settings, instance);
        Arc::new(Self {
            terminals: terminals.clone(),
            sftp: sftp::Manager::new(store.clone()),
            files: files.clone(),
            agent: Arc::new(
                chat::Manager::with_terminals(store.clone(), stop.clone(), terminals)
                    .with_files(files),
            ),
            store,
            stop,
        })
    }
    pub fn new(store: Arc<Store>, settings: files::Settings, file_base_url: String) -> Arc<Self> {
        let stop = CancellationToken::new();
        let terminals = Arc::new(terminal::TerminalManager::new(store.clone()));
        let files = files::Manager::new(store.clone(), settings, file_base_url);
        Arc::new(Self {
            terminals: terminals.clone(),
            sftp: sftp::Manager::new(store.clone()),
            files: files.clone(),
            agent: Arc::new(
                chat::Manager::with_terminals(store.clone(), stop.clone(), terminals)
                    .with_files(files),
            ),
            store,
            stop,
        })
    }
    pub async fn shutdown(&self) {
        self.stop.cancel();
        self.agent.shutdown();
        self.sftp.shutdown();
        self.terminals.shutdown().await;
        self.files.shutdown().await;
    }
}
