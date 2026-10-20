use super::{ConnectionState, OpenRequest, OpenResult, ReadResult};
use crate::{
    error::{success, Envelope, Error},
    http::{parse_json, parse_query},
};
use axum::{
    extract::{
        rejection::{JsonRejection, QueryRejection},
        Query, State,
    },
    routing::{get, post},
    Json, Router,
};
use serde::Deserialize;

pub(crate) fn routes() -> Router<crate::http::AppState> {
    Router::new()
        .route("/api/v1/ssh/terminal/connect", post(connect))
        .route("/api/v1/ssh/terminal/cancel_connect", post(cancel))
        .route("/api/v1/ssh/terminal/read", get(read))
        .route("/api/v1/ssh/terminal/connected", get(connected))
        .route("/api/v1/ssh/terminal/resources", get(resources))
        .route("/api/v1/ssh/terminal/write", post(write))
        .route("/api/v1/ssh/terminal/resize", post(resize))
        .route("/api/v1/ssh/terminal/close", post(close))
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SessionQuery {
    session_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct CancelQuery {
    connection_id: String,
    request_id: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct WriteRequest {
    session_id: String,
    input: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct ResizeRequest {
    session_id: String,
    cols: u32,
    rows: u32,
}

async fn connect(
    State(state): State<crate::http::AppState>,
    body: Result<Json<OpenRequest>, JsonRejection>,
) -> Result<Json<Envelope<OpenResult>>, Error> {
    Ok(success(state.terminals.connect(parse_json(body)?).await?))
}
async fn cancel(
    State(state): State<crate::http::AppState>,
    query: Result<Query<CancelQuery>, QueryRejection>,
) -> Result<Json<Envelope<()>>, Error> {
    let query = parse_query(query)?;
    state
        .terminals
        .cancel(&query.connection_id, &query.request_id)?;
    Ok(success(()))
}
async fn read(
    State(state): State<crate::http::AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Envelope<ReadResult>>, Error> {
    Ok(success(
        state.terminals.read(&parse_query(query)?.session_id).await,
    ))
}
async fn connected(
    State(state): State<crate::http::AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Envelope<ConnectionState>>, Error> {
    Ok(success(
        state.terminals.connected(&parse_query(query)?.session_id),
    ))
}
async fn resources(
    State(state): State<crate::http::AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Envelope<super::ResourceSnapshot>>, Error> {
    Ok(success(
        state
            .terminals
            .resources(&parse_query(query)?.session_id)
            .await?,
    ))
}
async fn write(
    State(state): State<crate::http::AppState>,
    body: Result<Json<WriteRequest>, JsonRejection>,
) -> Result<Json<Envelope<()>>, Error> {
    let body = parse_json(body)?;
    state.terminals.write(&body.session_id, body.input).await?;
    Ok(success(()))
}
async fn resize(
    State(state): State<crate::http::AppState>,
    body: Result<Json<ResizeRequest>, JsonRejection>,
) -> Result<Json<Envelope<()>>, Error> {
    let body = parse_json(body)?;
    state
        .terminals
        .resize(&body.session_id, body.cols, body.rows)
        .await?;
    Ok(success(()))
}
async fn close(
    State(state): State<crate::http::AppState>,
    query: Result<Query<SessionQuery>, QueryRejection>,
) -> Result<Json<Envelope<()>>, Error> {
    state.terminals.close(&parse_query(query)?.session_id);
    Ok(success(()))
}
