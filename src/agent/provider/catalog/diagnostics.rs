//! Fixed metadata and schema locations only; never record credentials or raw catalog bodies.
use super::*;
use crate::diagnostics::{event, Level};
use serde_json::json;
use std::time::Instant;

pub(super) fn kind(value: Option<&Value>) -> &'static str {
    match value {
        None => "missing",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}
pub(super) fn invalid(path: &str, expected: &str, value: Option<&Value>) -> ModelError {
    event(
        Level::Error,
        "model.catalog.parse_failed",
        json!({"stage":"catalog_schema","jsonPath":path,"expected":expected,"actualType":kind(value)}),
    );
    ModelError::Response
}
pub(super) fn json_error(error: &serde_json::Error) -> ModelError {
    event(
        Level::Error,
        "model.catalog.parse_failed",
        json!({"stage":"json","line":error.line(),"column":error.column()}),
    );
    ModelError::Response
}
pub(super) fn request(url: &Url, page: usize, headers: &header::HeaderMap) {
    let authentication = if headers.contains_key(header::AUTHORIZATION) {
        "bearer"
    } else if headers.contains_key("x-api-key") {
        "x-api-key"
    } else if headers.contains_key("x-goog-api-key") {
        "x-goog-api-key"
    } else {
        "none"
    };
    let mut safe = url.clone();
    let pairs: Vec<_> = url
        .query_pairs()
        .map(|(key, value)| {
            let value = if matches!(key.as_ref(), "pageSize" | "limit")
                && value.bytes().all(|b| b.is_ascii_digit())
            {
                value.into_owned()
            } else {
                "REDACTED".into()
            };
            (key.into_owned(), value)
        })
        .collect();
    safe.set_query(None);
    if !pairs.is_empty() {
        let mut query = safe.query_pairs_mut();
        for (key, value) in pairs {
            query.append_pair(&key, &value);
        }
    }
    event(
        Level::Info,
        "model.catalog.http_request",
        json!({"method":"GET","url":safe.as_str(),"page":page,"authentication":authentication,"hasBody":false}),
    );
}
pub(super) fn response(response: &reqwest::Response, page: usize) {
    let content_type = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .unwrap_or("")
        .trim();
    let media = match content_type {
        "application/json" => "json",
        "text/html" => "html",
        "" => "missing",
        _ => "other",
    };
    event(
        Level::Info,
        "model.catalog.http_response",
        json!({"page":page,"httpStatus":response.status().as_u16(),"mediaType":media,"contentLength":response.content_length()}),
    );
}
pub(super) fn shape(value: &Value, page: usize, bytes: usize) {
    let first = value
        .get("models")
        .or_else(|| value.get("data"))
        .and_then(Value::as_array)
        .and_then(|v| v.first());
    event(
        Level::Debug,
        "model.catalog.response_shape",
        json!({"page":page,"bytes":bytes,"rootType":kind(Some(value)),"modelsType":kind(value.get("models")),"dataType":kind(value.get("data")),"firstItem":{
            "nameType":kind(first.and_then(|v|v.get("name"))),"idType":kind(first.and_then(|v|v.get("id"))),
            "supportedGenerationMethodsType":kind(first.and_then(|v|v.get("supportedGenerationMethods"))),
            "nameHasModelsPrefix":first.and_then(|v|v.get("name")).and_then(Value::as_str).map(|v|v.starts_with("models/"))
        }}),
    );
}
pub(super) fn pagination(reason: &'static str, page: usize) {
    event(
        Level::Error,
        "model.catalog.pagination_failed",
        json!({"reason":reason,"page":page}),
    );
}
fn code(error: &ModelError) -> &'static str {
    match error {
        ModelError::Cancelled => "AGENT_CANCELLED",
        ModelError::Timeout => "MODEL_TIMEOUT",
        ModelError::Http(_) => "MODEL_HTTP_ERROR",
        ModelError::Network => "MODEL_NETWORK_ERROR",
        ModelError::ResponseTooLarge => "MODEL_RESPONSE_LIMIT",
        ModelError::Response => "MODEL_RESPONSE_INVALID",
        _ => "MODEL_CATALOG_FAILED",
    }
}
pub(super) async fn observe(
    client: &CatalogClient,
    future: impl std::future::Future<Output = Result<Vec<CatalogModel>>>,
) -> Result<Vec<CatalogModel>> {
    let started = Instant::now();
    event(
        Level::Info,
        "model.catalog.started",
        json!({"provider":client.provider,"timeoutSeconds":30,"maxPages":20,"maxResponseBytes":8*1024*1024}),
    );
    let result = future.await;
    match &result {
        Ok(models) => event(
            Level::Info,
            "model.catalog.completed",
            json!({"durationMs":started.elapsed().as_millis(),"modelCount":models.len()}),
        ),
        Err(error) => event(
            if matches!(error, ModelError::Cancelled) {
                Level::Warn
            } else {
                Level::Error
            },
            "model.catalog.failed",
            json!({"durationMs":started.elapsed().as_millis(),"errorCode":code(error),"httpStatus":if let ModelError::Http(status)=error{Some(*status)}else{None}}),
        ),
    }
    result
}

pub(super) fn unadvertised(count: usize) {
    if count > 0 {
        event(
            Level::Debug,
            "model.catalog.entries_skipped",
            json!({"reason":"missing_or_empty_generation_methods","count":count}),
        );
    }
}
