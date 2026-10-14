//! Model configuration and protocol adapter. No Agent tool loop or UI is enabled here.
mod config;
mod protocol;
mod repository;
pub mod responses;
mod stream;
mod transport;
pub use config::*;
pub use protocol::*;
pub use repository::{ModelRepository, RuntimeConfig};
pub use transport::ModelClient;

#[derive(Clone, Debug, thiserror::Error)]
pub enum ModelError {
    #[error("Invalid model configuration: {0}")]
    Invalid(&'static str),
    #[error("Model configuration not found")]
    NotFound,
    #[error("Model configuration changed; refresh before saving")]
    Stale,
    #[error("Model API key is not configured")]
    KeyMissing,
    #[error("Model API key protection or decryption failed")]
    Credentials,
    #[error("Model database operation failed")]
    Database,
    #[error("Model request exceeds the configured context window")]
    BudgetExceeded,
    #[error("Model tool-call protocol is invalid or incomplete")]
    ToolProtocol,
    #[error("Model request was cancelled")]
    Cancelled,
    #[error("Model request timed out")]
    Timeout,
    #[error("Model connection or TLS verification failed")]
    Network,
    #[error("Model provider returned HTTP {0}")]
    Http(u16),
    #[error("Model response exceeded the configured size limit")]
    ResponseTooLarge,
    #[error("Stored Responses context belongs to another model endpoint or credential; start a new conversation")]
    ContextMismatch,
    #[error(
        "The configured Responses provider does not support token counting or remote compaction"
    )]
    RemoteUnsupported,
    #[error("Model provider returned an invalid response")]
    Response,
    #[error("Model response was incomplete or filtered")]
    Incomplete,
}
pub type Result<T> = std::result::Result<T, ModelError>;

// Database/provider diagnostics may contain request data or secrets: never include their text.
impl From<rusqlite::Error> for ModelError {
    fn from(_: rusqlite::Error) -> Self {
        Self::Database
    }
}
