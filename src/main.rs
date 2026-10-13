use shellpilot_server::{files, serve_with_file_settings, store::Store};
use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let port: u16 = std::env::var("SHELLPILOT_PORT")
        .unwrap_or_else(|_| "8889".into())
        .parse()?;
    let data = std::env::var_os("SHELLPILOT_DATA_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("LOCALAPPDATA")
                .map(|p| PathBuf::from(p).join("ShellPilot").join("rust-server"))
        })
        .ok_or("set SHELLPILOT_DATA_DIR on non-Windows platforms")?;
    let store = Arc::new(Store::open(&data.join("shellpilot.sqlite3"))?);
    let listener =
        tokio::net::TcpListener::bind(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), port))
            .await?;
    tracing::info!(address = %listener.local_addr()?, "ShellPilot Rust backend ready");
    let file_settings = files::Settings {
        enabled: std::env::var("SHELLPILOT_FILES_ENABLED")
            .unwrap_or_else(|_| "true".into())
            .parse()?,
        max_storage_bytes: std::env::var("SHELLPILOT_FILE_MAX_STORAGE_MIB")
            .unwrap_or_else(|_| "1024".into())
            .parse::<u64>()?
            .checked_mul(1024 * 1024)
            .ok_or("attachment quota overflow")?,
        retention: match std::env::var("SHELLPILOT_FILE_RETENTION_DAYS")
            .unwrap_or_else(|_| "0".into())
            .parse::<u64>()?
        {
            0 => None,
            days => Some(std::time::Duration::from_secs(
                days.checked_mul(86400).ok_or("retention overflow")?,
            )),
        },
        download_ttl: std::time::Duration::from_secs(
            std::env::var("SHELLPILOT_FILE_LINK_SECONDS")
                .unwrap_or_else(|_| "900".into())
                .parse()?,
        ),
        ..Default::default()
    };
    file_settings.validate()?;
    serve_with_file_settings(listener, store, file_settings, async {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::error!(%error, "shutdown signal failed");
        }
    })
    .await?;
    Ok(())
}
