use super::{model::*, *};
use crate::{
    error::{success, Envelope},
    http::{parse_json, parse_query, AppState},
};
use axum::{
    body::Body,
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Path, Query, State,
    },
    http::{header, HeaderMap},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse,
    },
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;
use std::convert::Infallible;
pub(crate) fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/sftp/sessions", post(open))
        .route("/api/v1/sftp/sessions/{id}", get(session).delete(close))
        .route(
            "/api/v1/sftp/sessions/{id}/entries",
            get(entries).delete(delete),
        )
        .route("/api/v1/sftp/sessions/{id}/directories", post(mkdir))
        .route("/api/v1/sftp/sessions/{id}/files", post(file))
        .route("/api/v1/sftp/sessions/{id}/transfers", get(tasks))
        .route("/api/v1/sftp/sessions/{id}/events", get(events))
        .route("/api/v1/sftp/transfers", post(create))
        .route("/api/v1/sftp/transfers/{id}", get(task))
        .route("/api/v1/sftp/transfers/{id}/cancel", post(cancel))
        .route(
            "/api/v1/sftp/transfers/{id}/items/{item}/content",
            get(download).put(upload),
        )
        .route(
            "/api/v1/sftp/transfers/{id}/items/{item}/confirm",
            post(confirm),
        )
}
fn json<T: serde::de::DeserializeOwned>(
    body: std::result::Result<Json<T>, JsonRejection>,
) -> Result<T> {
    parse_json(body).map_err(|_| Error::invalid("请求 JSON 或字段类型不合法"))
}
fn query<T: serde::de::DeserializeOwned>(
    value: std::result::Result<Query<T>, QueryRejection>,
) -> Result<T> {
    parse_query(value).map_err(|_| Error::invalid("查询参数不合法"))
}
async fn open(
    State(state): State<AppState>,
    body: std::result::Result<Json<OpenRequest>, JsonRejection>,
) -> Result<Json<Envelope<SessionView>>> {
    Ok(success(state.sftp.open(json(body)?).await?))
}
async fn session(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<SessionView>>> {
    Ok(success(state.sftp.session_view(&id)?))
}
async fn close(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<()>>> {
    state.sftp.close(&id)?;
    Ok(success(()))
}
#[derive(Deserialize)]
struct PathQuery {
    path: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathRequest {
    path: String,
}
async fn entries(
    State(state): State<AppState>,
    Path(id): Path<String>,
    params: std::result::Result<Query<PathQuery>, QueryRejection>,
) -> Result<Json<Envelope<Vec<Entry>>>> {
    let params = query(params)?;
    Ok(success(
        state.sftp.entries(&id, params.path.as_deref()).await?,
    ))
}
async fn mkdir(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: std::result::Result<Json<PathRequest>, JsonRejection>,
) -> Result<Json<Envelope<()>>> {
    state
        .sftp
        .change_entry(&id, &json(body)?.path, "mkdir")
        .await?;
    Ok(success(()))
}
async fn file(
    State(state): State<AppState>,
    Path(id): Path<String>,
    body: std::result::Result<Json<PathRequest>, JsonRejection>,
) -> Result<Json<Envelope<()>>> {
    state
        .sftp
        .change_entry(&id, &json(body)?.path, "file")
        .await?;
    Ok(success(()))
}
async fn delete(
    State(state): State<AppState>,
    Path(id): Path<String>,
    params: std::result::Result<Query<PathQuery>, QueryRejection>,
) -> Result<Json<Envelope<()>>> {
    let params = query(params)?;
    state
        .sftp
        .change_entry(
            &id,
            &params.path.ok_or_else(|| Error::invalid("缺少 path"))?,
            "delete",
        )
        .await?;
    Ok(success(()))
}
async fn tasks(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<Vec<TransferView>>>> {
    Ok(success(state.sftp.tasks(&id)?))
}
async fn create(
    State(state): State<AppState>,
    body: std::result::Result<Json<CreateRequest>, JsonRejection>,
) -> Result<Json<Envelope<TransferView>>> {
    Ok(success(state.sftp.create_transfer(json(body)?).await?))
}
async fn task(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<TransferView>>> {
    Ok(success(state.sftp.transfer_view(&id)?))
}
async fn cancel(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<Envelope<()>>> {
    state.sftp.cancel(&id)?;
    Ok(success(()))
}
async fn upload(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
    headers: HeaderMap,
    body: Body,
) -> Result<Json<Envelope<()>>> {
    if !headers
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        .is_some_and(|v| v.split(';').next() == Some("application/octet-stream"))
    {
        return Err(Error::invalid("上传需要 application/octet-stream"));
    }
    let length = headers
        .get(header::CONTENT_LENGTH)
        .map(|h| {
            h.to_str()
                .ok()
                .and_then(|s| s.parse::<u64>().ok())
                .ok_or_else(Error::size)
        })
        .transpose()?;
    state.sftp.upload(&id, &item, body, length).await?;
    Ok(success(()))
}
async fn download(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
) -> Result<Response> {
    state.sftp.download(&id, &item).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Confirm {
    saved: bool,
}
async fn confirm(
    State(state): State<AppState>,
    Path((id, item)): Path<(String, String)>,
    body: std::result::Result<Json<Confirm>, JsonRejection>,
) -> Result<Json<Envelope<()>>> {
    state.sftp.confirm(&id, &item, json(body)?.saved)?;
    Ok(success(()))
}
async fn events(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse> {
    let session = state.sftp.session(&id)?;
    let stop = CancellationToken::new();
    if let Some(previous) = session.subscription.lock().unwrap().replace(stop.clone()) {
        previous.cancel();
    }
    let stream = async_stream::stream! {
        let mut previous=String::new();
        loop {
            if session.stop.is_cancelled(){
                yield Ok::<Event,Infallible>(Event::default().event("session-closed").data(r#"{"code":"SFTP_NOT_FOUND","message":"文件管理会话已关闭"}"#));break;
            }
            if stop.is_cancelled(){break}
            let snapshot=session.tasks.lock().unwrap().values().map(|t|t.view().progress).collect::<Vec<_>>();
            let mut snapshot=snapshot;snapshot.sort_by(|a,b|a.transfer_id.cmp(&b.transfer_id));
            if let Ok(data)=serde_json::to_string(&snapshot){if previous!=data {previous=data.clone();yield Ok(Event::default().event("progress").data(data));}}
            tokio::select!{_ = session.stop.cancelled()=>{},_ = stop.cancelled()=>break,_=tokio::time::sleep(Duration::from_millis(300))=>{}}
        }
    };
    Ok((
        [("Cache-Control", "no-cache"), ("X-Accel-Buffering", "no")],
        Sse::new(stream).keep_alive(KeepAlive::new().interval(Duration::from_secs(15))),
    ))
}
