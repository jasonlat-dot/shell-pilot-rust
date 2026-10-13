pub fn router_with_files(store: Arc<Store>, files: Arc<crate::files::Manager>) -> Router {
    let terminals = Arc::new(crate::terminal::TerminalManager::new(store.clone()));
    let sftp = crate::sftp::Manager::new(store.clone());
    router_with_all_services(store, terminals, sftp, files)
}
use crate::{
    error::{failure, success, Envelope, Error},
    model::{Connection, ConnectionRequest},
    store::Store,
};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Query, Request, State,
    },
    http::{header, HeaderValue, Method, StatusCode},
    middleware::{self, Next},
    response::Response,
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use serde_json::json;
use std::sync::Arc;
use tower_http::cors::CorsLayer;

#[derive(Clone)]
pub(crate) struct AppState {
    pub store: Arc<Store>,
    pub terminals: Arc<crate::terminal::TerminalManager>,
    pub sftp: Arc<crate::sftp::Manager>,
    pub files: Arc<crate::files::Manager>,
}
const ORIGINS: &[&str] = &[
    "http://localhost:1420",
    "http://127.0.0.1:1420",
    "http://localhost:5173",
    "http://127.0.0.1:5173",
    "tauri://localhost",
    "http://tauri.localhost",
    "https://tauri.localhost",
];

pub fn router(store: Arc<Store>) -> Router {
    let terminals = Arc::new(crate::terminal::TerminalManager::new(store.clone()));
    router_with_terminals(store, terminals)
}
pub(crate) fn router_with_terminals(
    store: Arc<Store>,
    terminals: Arc<crate::terminal::TerminalManager>,
) -> Router {
    let sftp = crate::sftp::Manager::new(store.clone());
    router_with_services(store, terminals, sftp)
}
pub(crate) fn router_with_services(
    store: Arc<Store>,
    terminals: Arc<crate::terminal::TerminalManager>,
    sftp: Arc<crate::sftp::Manager>,
) -> Router {
    let files = crate::files::Manager::new(
        store.clone(),
        crate::files::Settings::default(),
        "http://127.0.0.1:8889".into(),
    );
    router_with_all_services(store, terminals, sftp, files)
}
pub(crate) fn router_with_all_services(
    store: Arc<Store>,
    terminals: Arc<crate::terminal::TerminalManager>,
    sftp: Arc<crate::sftp::Manager>,
    files: Arc<crate::files::Manager>,
) -> Router {
    let state = AppState {
        store,
        terminals,
        sftp,
        files,
    };
    Router::new()
        .route(
            "/api/health",
            get(|| async { Json(json!({"status":"UP", "checkedAt": chrono::Utc::now()})) }),
        )
        .route("/api/v1/ssh/connection_list", get(list))
        .route("/api/v1/ssh/get_connection", get(get_connection))
        .route("/api/v1/ssh/create_connection", post(create))
        .route("/api/v1/ssh/update_connection", post(update))
        .route("/api/v1/ssh/delete_connection", post(delete))
        .merge(crate::terminal::routes())
        .merge(crate::sftp::routes())
        .merge(crate::files::routes())
        .fallback(unimplemented)
        .method_not_allowed_fallback(|| async {
            failure(
                StatusCode::METHOD_NOT_ALLOWED,
                "SERVER_ERROR_001",
                "不支持的请求方法",
            )
        })
        .layer(middleware::from_fn(origin_guard))
        .layer(
            CorsLayer::new()
                .allow_origin(
                    ORIGINS
                        .iter()
                        .map(|s| s.parse::<HeaderValue>().expect("static origin"))
                        .collect::<Vec<_>>(),
                )
                .allow_methods([Method::GET, Method::POST, Method::DELETE, Method::PUT])
                .allow_headers([header::CONTENT_TYPE]),
        )
        .with_state(state)
}

// CORS alone does not prevent cross-origin POSTs. Reject before mutation.
async fn origin_guard(request: Request, next: Next) -> Response {
    if let Some(origin) = request.headers().get(header::ORIGIN) {
        if !origin.to_str().is_ok_and(|s| ORIGINS.contains(&s)) {
            return failure(
                StatusCode::FORBIDDEN,
                "ORIGIN_FORBIDDEN",
                "不允许此来源访问本地后端",
            );
        }
    }
    next.run(request).await
}

async fn unimplemented(request: Request) -> Response {
    let path = request.uri().path();
    if path.starts_with("/agent/")
        || path.starts_with("/api/v1/ssh/terminal/")
        || path.starts_with("/api/v1/sftp/")
    {
        failure(
            StatusCode::NOT_IMPLEMENTED,
            "FEATURE_NOT_MIGRATED",
            "该功能尚未迁移到 Rust 后端",
        )
    } else {
        failure(StatusCode::NOT_FOUND, "NOT_FOUND", "接口不存在")
    }
}

async fn worker<T: Send + 'static>(
    store: AppState,
    action: impl FnOnce(&Store) -> Result<T, Error> + Send + 'static,
) -> Result<T, Error> {
    tokio::task::spawn_blocking(move || action(&store.store))
        .await
        .map_err(|_| Error::Worker)?
}

pub(crate) fn parse_json<T>(value: Result<Json<T>, JsonRejection>) -> Result<T, Error> {
    value
        .map(|Json(v)| v)
        .map_err(|_| Error::Invalid("请求 JSON 格式或字段类型不合法".into()))
}
pub(crate) fn parse_query<T>(value: Result<Query<T>, QueryRejection>) -> Result<T, Error> {
    value
        .map(|Query(v)| v)
        .map_err(|_| Error::Invalid("查询参数不合法".into()))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct IdQuery {
    connection_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct UserQuery {
    user_id: Option<String>,
}

async fn list(
    State(store): State<AppState>,
    query: Result<Query<UserQuery>, QueryRejection>,
) -> Result<Json<Envelope<Vec<Connection>>>, Error> {
    let user = parse_query(query)?
        .user_id
        .filter(|s| !s.trim().is_empty())
        .unwrap_or_else(|| "default".into());
    Ok(success(worker(store, move |s| s.list(&user)).await?))
}
async fn get_connection(
    State(store): State<AppState>,
    query: Result<Query<IdQuery>, QueryRejection>,
) -> Result<Json<Envelope<Connection>>, Error> {
    let id = parse_query(query)?.connection_id;
    Ok(success(worker(store, move |s| s.get(&id)).await?))
}
async fn create(
    State(store): State<AppState>,
    body: Result<Json<ConnectionRequest>, JsonRejection>,
) -> Result<Json<Envelope<Connection>>, Error> {
    let request = parse_json(body)?;
    Ok(success(
        worker(store, move |s| s.save(request, false)).await?,
    ))
}
async fn update(
    State(store): State<AppState>,
    body: Result<Json<ConnectionRequest>, JsonRejection>,
) -> Result<Json<Envelope<Connection>>, Error> {
    let request = parse_json(body)?;
    Ok(success(store.terminals.update_connection(request).await?))
}
async fn delete(
    State(store): State<AppState>,
    query: Result<Query<IdQuery>, QueryRejection>,
) -> Result<Json<Envelope<()>>, Error> {
    let id = parse_query(query)?.connection_id;
    store.terminals.delete_connection(id).await?;
    Ok(success(()))
}
