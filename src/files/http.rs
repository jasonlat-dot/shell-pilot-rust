use super::*;
use crate::{
    error::{success, Envelope},
    http::AppState,
};
use axum::{
    extract::{
        multipart::MultipartRejection, rejection::QueryRejection, DefaultBodyLimit, Multipart,
        Path, Query, State,
    },
    routing::{get, post},
    Json, Router,
};
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/files",
            post(upload)
                .get(list)
                .layer(DefaultBodyLimit::max(model::MAX_REQUEST_BYTES)),
        )
        .route("/api/v1/files/storage", get(stats))
        .route("/api/v1/files/local", get(download))
        .route("/api/v1/files/{id}", get(metadata).delete(delete))
}
async fn upload(
    State(state): State<AppState>,
    multipart: std::result::Result<Multipart, MultipartRejection>,
) -> Result<Json<Envelope<model::UploadedFile>>> {
    let body =
        multipart.map_err(|_| Error::invalid("需要带 file 字段的 multipart/form-data 请求"))?;
    Ok(success(state.files.clone().upload(body).await?))
}
async fn metadata(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<model::UploadedFile>>> {
    Ok(success(state.files.metadata(&id).await?))
}
async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<()>>> {
    state.files.delete(&id).await?;
    Ok(success(()))
}
async fn stats(State(state): State<AppState>) -> Result<Json<Envelope<model::StorageStats>>> {
    Ok(success(state.files.stats().await?))
}
async fn download(
    State(state): State<AppState>,
    query: std::result::Result<Query<model::DownloadQuery>, QueryRejection>,
) -> Result<axum::response::Response> {
    state
        .files
        .clone()
        .download(query.map_err(|_| Error::link())?.0)
        .await
}

#[derive(serde::Deserialize)]
struct ListQuery {
    #[serde(default)]
    offset: usize,
}
async fn list(
    State(state): State<AppState>,
    query: std::result::Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Envelope<model::FilePage>>> {
    let query = query
        .map_err(|_| Error::invalid("文件列表查询参数不合法"))?
        .0;
    Ok(success(state.files.list(query.offset).await?))
}
