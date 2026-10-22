pub mod agent;
pub mod diagnostics;
pub mod error;
pub mod files;
mod http;
pub mod model;
pub mod network;
mod secrets;
pub mod services;
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
    let services = services::Services::new(
        store,
        file_settings,
        format!("http://{}", listener.local_addr()?),
    );
    serve_services(listener, services, shutdown).await
}

/// Optional HTTP adapter; the desktop holds the same services for direct IPC calls.
pub async fn serve_services(
    listener: tokio::net::TcpListener,
    services: std::sync::Arc<services::Services>,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<()> {
    if !listener.local_addr()?.ip().is_loopback() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "desktop backend must bind to loopback",
        ));
    }
    let router = http::router_with_all_services(
        services.store.clone(),
        services.terminals.clone(),
        services.sftp.clone(),
        services.files.clone(),
    );
    let shutting_down = services.clone();
    let result = axum::serve(listener, router)
        .with_graceful_shutdown(async move {
            shutdown.await;
            shutting_down.shutdown().await;
        })
        .await;
    if !services.stop.is_cancelled() {
        services.shutdown().await;
    }
    result
}
