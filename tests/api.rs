use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use serde_json::{json, Value};
use shellpilot_server::{model::ConnectionRequest, router, store::Store};
use std::sync::Arc;
use tower::ServiceExt;

fn payload() -> Value {
    json!({"connectionName":"test-host","host":"192.0.2.1","port":22,"username":"tester","authType":1,"userId":"default","strictHostKeyCheck":false})
}

async fn call(app: &Router, method: &str, uri: &str, body: Option<Value>) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json")
        .body(
            body.map(|v| Body::from(v.to_string()))
                .unwrap_or_else(Body::empty),
        )
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn connection_crud_matches_client_contract() {
    let app = router(Arc::new(Store::in_memory().unwrap()));
    let (status, created) = call(
        &app,
        "POST",
        "/api/v1/ssh/create_connection",
        Some(payload()),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(created["code"], "SUCCESS_0000");
    assert!(created["data"].get("password").is_none());
    assert!(created["data"].get("privateKey").is_none());
    assert_eq!(created["data"]["connectTimeout"], 10);
    let id = created["data"]["connectionId"].as_str().unwrap();
    let (_, listed) = call(
        &app,
        "GET",
        "/api/v1/ssh/connection_list?userId=default",
        None,
    )
    .await;
    assert_eq!(listed["data"].as_array().unwrap().len(), 1);
    let (_, other_user) = call(
        &app,
        "GET",
        "/api/v1/ssh/connection_list?userId=other",
        None,
    )
    .await;
    assert_eq!(other_user["data"], json!([]));
    let (_, fetched) = call(
        &app,
        "GET",
        &format!("/api/v1/ssh/get_connection?connectionId={id}"),
        None,
    )
    .await;
    assert_eq!(fetched["data"], created["data"]);
    let mut update = payload();
    update["connectionId"] = json!(id);
    update["connectionName"] = json!("renamed");
    update["connectTimeout"] = json!(25);
    let (_, updated) = call(&app, "POST", "/api/v1/ssh/update_connection", Some(update)).await;
    assert_eq!(updated["data"]["connectionName"], "renamed");
    assert_eq!(updated["data"]["createdAt"], created["data"]["createdAt"]);
    let (_, removed) = call(
        &app,
        "POST",
        &format!("/api/v1/ssh/delete_connection?connectionId={id}"),
        None,
    )
    .await;
    assert_eq!(removed["code"], "SUCCESS_0000");
    let (status, missing) = call(
        &app,
        "GET",
        &format!("/api/v1/ssh/get_connection?connectionId={id}"),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(missing["code"], "S0001");
}

#[tokio::test]
async fn health_and_unmigrated_features_are_distinct() {
    let app = router(Arc::new(Store::in_memory().unwrap()));
    let (_, health) = call(&app, "GET", "/api/health", None).await;
    assert_eq!(health["status"], "UP");
    assert!(health["checkedAt"].as_str().is_some());
    let (status, terminal) = call(&app, "POST", "/agent/api/get_config", Some(json!({}))).await;
    assert_eq!(status, StatusCode::NOT_IMPLEMENTED);
    assert_eq!(terminal["code"], "FEATURE_NOT_MIGRATED");
    let (status, _) = call(&app, "GET", "/not-a-route", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn validation_errors_use_json_envelopes_and_do_not_write() {
    let store = Arc::new(Store::in_memory().unwrap());
    let app = router(store.clone());
    for change in [
        json!({"port":0}),
        json!({"port":65536}),
        json!({"authType":3}),
        json!({"host":" "}),
        json!({"strictHostKeyCheck":true}),
        json!({"connectTimeout":0}),
    ] {
        let mut body = payload();
        for (key, value) in change.as_object().unwrap() {
            body[key] = value.clone();
        }
        let (status, error) = call(&app, "POST", "/api/v1/ssh/create_connection", Some(body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(error["code"], "S0002");
    }
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/ssh/create_connection")
        .header("content-type", "application/json")
        .body(Body::from("{broken"))
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let body: Value =
        serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
    assert_eq!(body["code"], "S0002");
    assert!(store.list("default").unwrap().is_empty());
}

#[tokio::test]
async fn untrusted_browser_origin_cannot_modify_local_state() {
    let store = Arc::new(Store::in_memory().unwrap());
    let app = router(store.clone());
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/ssh/create_connection")
        .header("content-type", "application/json")
        .header("origin", "https://untrusted.example")
        .body(Body::from(payload().to_string()))
        .unwrap();
    assert_eq!(
        app.oneshot(request).await.unwrap().status(),
        StatusCode::FORBIDDEN
    );
    assert!(store.list("default").unwrap().is_empty());
}

#[tokio::test]
async fn tauri_origin_can_preflight_and_create_connections() {
    let app = router(Arc::new(Store::in_memory().unwrap()));
    let request = Request::builder()
        .method("OPTIONS")
        .uri("/api/v1/ssh/create_connection")
        .header("origin", "http://tauri.localhost")
        .header("access-control-request-method", "POST")
        .header("access-control-request-headers", "content-type")
        .body(Body::empty())
        .unwrap();
    let response = app.clone().oneshot(request).await.unwrap();
    assert_eq!(
        response.headers()["access-control-allow-origin"],
        "http://tauri.localhost"
    );
    let request = Request::builder()
        .method("POST")
        .uri("/api/v1/ssh/create_connection")
        .header("origin", "http://tauri.localhost")
        .header("content-type", "application/json")
        .body(Body::from(payload().to_string()))
        .unwrap();
    assert_eq!(app.oneshot(request).await.unwrap().status(), StatusCode::OK);
}

#[test]
fn sqlite_survives_restart_and_invalid_update_is_atomic() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("connections.sqlite3");
    let store = Store::open(&database).unwrap();
    let request: ConnectionRequest = serde_json::from_value(payload()).unwrap();
    let connection = store.save(request, false).unwrap();
    let mut invalid = payload();
    invalid["connectionId"] = json!(connection.connection_id);
    invalid["host"] = json!("");
    assert!(store
        .save(serde_json::from_value(invalid).unwrap(), true)
        .is_err());
    drop(store);
    let reopened = Store::open(&database).unwrap();
    assert_eq!(
        reopened.get(&connection.connection_id).unwrap().host,
        "192.0.2.1"
    );
}

#[cfg(windows)]
#[test]
fn credentials_are_encrypted_and_blank_updates_preserve_them() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("connections.sqlite3");
    let store = Store::open(&database).unwrap();
    let mut body = payload();
    body["password"] = json!("test-only-password");
    body["privateKey"] = json!("test-only-private-key");
    let connection = store
        .save(serde_json::from_value(body).unwrap(), false)
        .unwrap();
    let raw = rusqlite::Connection::open(&database).unwrap();
    let (password, private_key, metadata): (Vec<u8>, Vec<u8>, String) = raw
        .query_row(
            "SELECT password,private_key,metadata FROM ssh_connections",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap();
    assert_ne!(password, b"test-only-password");
    assert_ne!(private_key, b"test-only-private-key");
    assert!(!metadata.contains("test-only-password"));
    assert!(!metadata.contains("test-only-private-key"));
    let mut update = payload();
    update["connectionId"] = json!(connection.connection_id);
    update["password"] = json!("");
    store
        .save(serde_json::from_value(update).unwrap(), true)
        .unwrap();
    drop(store);
    let store = Store::open(&database).unwrap();
    let credentials = store.credentials(&connection.connection_id).unwrap();
    assert_eq!(credentials.password.as_deref(), Some("test-only-password"));
    assert_eq!(
        credentials.private_key.as_deref(),
        Some("test-only-private-key")
    );
    let mut update = payload();
    update["connectionId"] = json!(connection.connection_id);
    update["password"] = json!("replacement");
    store
        .save(serde_json::from_value(update).unwrap(), true)
        .unwrap();
    assert_eq!(
        store
            .credentials(&connection.connection_id)
            .unwrap()
            .password
            .as_deref(),
        Some("replacement")
    );
}

#[test]
fn update_preserves_host_key_settings_and_duplicate_id_is_rejected() {
    let store = Store::in_memory().unwrap();
    let mut body = payload();
    body["connectionId"] = json!("fixed-id");
    body["strictHostKeyCheck"] = json!(true);
    body["knownHosts"] = json!("example.test ssh-ed25519 placeholder");
    store
        .save(serde_json::from_value(body.clone()).unwrap(), false)
        .unwrap();
    assert!(store
        .save(serde_json::from_value(body).unwrap(), false)
        .is_err());
    let mut update = payload();
    update["connectionId"] = json!("fixed-id");
    update.as_object_mut().unwrap().remove("strictHostKeyCheck");
    let updated = store
        .save(serde_json::from_value(update).unwrap(), true)
        .unwrap();
    assert!(updated.strict_host_key_check);
    assert_eq!(
        updated.known_hosts.as_deref(),
        Some("example.test ssh-ed25519 placeholder")
    );
}

#[tokio::test]
async fn embedded_server_rejects_non_loopback_binding() {
    let listener = tokio::net::TcpListener::bind("0.0.0.0:0").await.unwrap();
    let result =
        shellpilot_server::serve(listener, Arc::new(Store::in_memory().unwrap()), async {}).await;
    assert_eq!(result.unwrap_err().kind(), std::io::ErrorKind::InvalidInput);
}
