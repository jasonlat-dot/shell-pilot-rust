pub mod agent;
pub mod error;
pub mod files;
mod http;
pub mod model;
mod secrets;
pub mod sftp;
pub mod store;
pub mod terminal;

pub use http::{router, router_with_files};

/// Standalone executable and future Tauri host share this entry point.
/// Binding before calling lets Tauri choose port 0 and publish the actual port.
pub async fn serve(
    listener: tokio::net::TcpListener,
    store: std::sync::Arc<store::Store>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    serve_with_file_settings(listener, store, files::Settings::default(), shutdown).await
}
pub async fn serve_with_file_settings(
    listener: tokio::net::TcpListener,
    store: std::sync::Arc<store::Store>,
    file_settings: files::Settings,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "desktop backend must bind to loopback",
        ));
    }
    let terminals = std::sync::Arc::new(terminal::TerminalManager::new(store.clone()));
    let sftp = sftp::Manager::new(store.clone());
    let files = files::Manager::new(
        store.clone(),
        file_settings,
        format!("http://{}", listener.local_addr()?),
    );
    let router =
        http::router_with_all_services(store, terminals.clone(), sftp.clone(), files.clone());
    axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            files.shutdown().await;
            sftp.shutdown();
            terminals.shutdown().await;
        })
        .await
}
