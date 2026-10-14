//! Persistent context, native model adapters, text turns and the D1 tool-loop kernel.
//! Desktop chat does not dispatch tools until target binding and approval are integrated.
pub mod budget;
pub mod chat;
pub mod compact;
pub mod context_status;
mod facts;
mod live;
pub mod model;
mod projects;
pub mod provider;
mod repository;
mod scopes;
pub mod ssh_tools;
pub mod tool_loop;
pub use projects::{Project, ProjectChat, ProjectChats};
mod turns;
pub use model::*;
pub use repository::Repository;
pub use scopes::{Scope, ScopeKind, ServerIdentity};

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("Agent session or event not found")]
    NotFound,
    #[error("Agent context changed; take a new snapshot")]
    Stale,
    #[error("Tool call does not match its bound task and SSH target")]
    TargetMismatch,
    #[error("Agent operation conflicts with an existing event")]
    Conflict,
    #[error("Invalid Agent context: {0}")]
    Invalid(String),
    #[error("Tool call/result sequence is incomplete or invalid")]
    ToolProtocol,
    #[error("Request cannot fit the model context window")]
    BudgetExceeded,
    #[error("Tool output exceeds capture limit")]
    OutputTooLarge,
    #[error("Tool output storage quota exceeded")]
    OutputQuota,
    #[error("Too many active tool output captures")]
    OutputBusy,
    #[error("Tool output capture is unavailable or expired")]
    OutputUnavailable,
    #[error("Tool output input failed")]
    OutputInput(#[from] std::io::Error),
    #[error("Agent database worker unavailable")]
    Worker,
    #[error("Agent database failure")]
    Database(#[from] rusqlite::Error),
    #[error("Agent context serialization failure")]
    Json(#[from] serde_json::Error),
}
pub type Result<T> = std::result::Result<T, Error>;

mod output;
mod output_model;
mod output_writer;
pub use output_model::*;
pub use output_writer::{CaptureStart, OutputWriter};

pub(crate) mod responses_store;
