//! Provider strategies normalize wire protocols; HTTP and Agent execution remain separate.
mod adapter;
mod supplier;
pub use supplier::SupplierKind;
mod catalog;
mod anthropic;
mod google;
mod config;
pub(crate) mod continuation;
mod dispatch;
mod diagnostics;
mod protocol;
mod repository;
pub mod responses;
mod stream;
mod tool_stream;
mod transport;
pub use config::*;
pub use catalog::{CatalogClient, CatalogModel, CatalogRequest};
pub use continuation::Continuation;
pub use protocol::*;
pub use repository::{ChannelCopyChanges, ChannelCopyDraft, ModelRepository, RuntimeConfig};
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
    #[error("此模型渠道已禁用，请在模型设置中启用，或选择其他模型。")]
    Disabled,
    #[error("Model API key protection or decryption failed")]
    Credentials,
    #[error("Model database operation failed")]
    Database,
    #[error("此模型配置无法读取或当前版本不支持，请在模型设置中检查；原始数据已保留。")]
    StoredConfig,
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
