use axum::{
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use serde::Serialize;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{0}")]
    Invalid(String),
    #[error("连接不存在")]
    NotFound,
    #[error("连接 ID 已存在")]
    Conflict,
    #[error("database: {0}")]
    Database(#[from] rusqlite::Error),
    #[error("serialization: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("credentials: {0}")]
    Credentials(#[from] std::io::Error),
    #[error("终端会话不存在或已关闭")]
    TerminalMissing,
    #[error("SSH 连接已取消")]
    Cancelled,
    #[error("SSH 连接超时")]
    ConnectTimeout,
    #[error("SSH 密码或私钥认证失败")]
    Authentication,
    #[error("服务器主机密钥与 known_hosts 不匹配、被撤销或不受支持")]
    HostKey,
    #[error("SSH 传输、PTY 或 shell 操作失败")]
    Ssh,
    #[error("终端会话或连接请求达到上限")]
    Busy,
    #[error("storage worker unavailable")]
    Worker,
}

#[derive(Serialize)]
pub struct Envelope<T: Serialize> {
    pub code: &'static str,
    pub info: String,
    pub data: Option<T>,
}

pub fn success<T: Serialize>(data: T) -> Json<Envelope<T>> {
    Json(Envelope {
        code: "SUCCESS_0000",
        info: "成功".into(),
        data: Some(data),
    })
}

pub fn failure(status: StatusCode, code: &'static str, info: impl Into<String>) -> Response {
    (
        status,
        Json(Envelope::<()> {
            code,
            info: info.into(),
            data: None,
        }),
    )
        .into_response()
}

impl IntoResponse for Error {
    fn into_response(self) -> Response {
        match self {
            Self::Invalid(info) => failure(StatusCode::BAD_REQUEST, "S0002", info),
            Self::NotFound => failure(StatusCode::NOT_FOUND, "S0001", "连接不存在"),
            Self::TerminalMissing => {
                failure(StatusCode::NOT_FOUND, "S0003", "终端会话不存在或已关闭")
            }
            Self::Cancelled => failure(
                StatusCode::CONFLICT,
                "SSH_CONNECT_CANCELLED",
                "SSH 连接已取消",
            ),
            Self::ConnectTimeout => failure(
                StatusCode::GATEWAY_TIMEOUT,
                "SSH_CONNECT_TIMEOUT",
                "SSH 连接超时",
            ),
            Self::Authentication => failure(
                StatusCode::BAD_GATEWAY,
                "SSH_AUTH_FAILED",
                "SSH 密码或私钥认证失败",
            ),
            Self::HostKey => failure(
                StatusCode::BAD_GATEWAY,
                "SSH_HOST_KEY_REJECTED",
                "服务器主机密钥校验失败",
            ),
            Self::Ssh => failure(
                StatusCode::BAD_GATEWAY,
                "SSH_TRANSPORT_FAILED",
                "SSH 传输、PTY 或 shell 操作失败",
            ),
            Self::Busy => failure(
                StatusCode::TOO_MANY_REQUESTS,
                "SSH_BUSY",
                "终端会话或连接请求达到上限",
            ),
            Self::Conflict => failure(StatusCode::CONFLICT, "S0002", "连接 ID 已存在"),
            error => {
                tracing::error!(%error, "backend operation failed");
                failure(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "ERROR_SERVER_0500",
                    "服务器内部错误",
                )
            }
        }
    }
}
