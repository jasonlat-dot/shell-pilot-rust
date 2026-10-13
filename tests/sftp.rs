#![cfg(windows)]
#[path = "sftp_support/mod.rs"]
mod support;
use axum::body::{to_bytes, Body};
use shellpilot_server::{
    model::ConnectionRequest,
    router,
    sftp::{model::*, Manager, Settings},
    terminal::{OpenRequest as TerminalOpen, TerminalManager},
};
use std::{sync::atomic::Ordering, time::Duration};
use support::*;
fn create(
    session: &str,
    direction: Direction,
    remote: &str,
    items: Option<Vec<ManifestItem>>,
) -> CreateRequest {
    CreateRequest {
        sftp_session_id: session.into(),
        direction,
        remote_path: remote.into(),
        conflict: Conflict::Fail,
        items,
    }
}
fn file(path: &str, size: u64) -> ManifestItem {
    ManifestItem {
        relative_path: path.into(),
        kind: Kind::File,
        size,
    }
}
fn directory(path: &str) -> ManifestItem {
    ManifestItem {
        relative_path: path.into(),
        kind: Kind::Directory,
        size: 0,
    }
}
fn no_part(fixture: &Fixture) -> bool {
    std::fs::read_dir(fixture.root.path().join("home"))
        .unwrap()
        .all(|e| {
            !e.unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".sftp-")
        })
}

#[tokio::test]
async fn sessions_private_key_host_identity_and_terminal_independence() {
    let fixture = Fixture::new().await;
    let (store, connection) = fixture.saved(true);
    let manager = Manager::new(store.clone());
    let terminal = TerminalManager::new(store.clone());
    let shell = terminal
        .connect(TerminalOpen {
            connection_id: connection.connection_id.clone(),
            request_id: "sftp-independence".into(),
            cols: None,
            rows: None,
        })
        .await
        .unwrap();
    let opened = manager
        .open(OpenRequest {
            connection_id: connection.connection_id.clone(),
            root_path: None,
        })
        .await
        .unwrap();
    assert_eq!(opened.root_path, "/home");
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 2);
    terminal.close(&shell.session_id);
    manager
        .change_entry(&opened.sftp_session_id, "/home/alive", "file")
        .await
        .unwrap();
    assert_eq!(
        manager
            .entries(&opened.sftp_session_id, None)
            .await
            .unwrap()
            .len(),
        1
    );
    let other = terminal
        .connect(TerminalOpen {
            connection_id: connection.connection_id.clone(),
            request_id: "sftp-independent-close".into(),
            cols: None,
            rows: None,
        })
        .await
        .unwrap();
    manager.close(&opened.sftp_session_id).unwrap();
    assert!(terminal.connected(&other.session_id).connected);
    terminal
        .write(&other.session_id, "still alive".into())
        .await
        .unwrap();
    terminal.shutdown().await;
    let mut update:ConnectionRequest=serde_json::from_value(serde_json::json!({"connectionId":connection.connection_id,"connectionName":"wrong key","host":"127.0.0.1","port":fixture.port,"username":"tester","knownHosts":format!("[127.0.0.1]:{} {}",fixture.port,russh::keys::PrivateKey::random(&mut russh::keys::key::safe_rng(),russh::keys::Algorithm::Ed25519).unwrap().public_key().to_openssh().unwrap())})).unwrap();
    update.strict_host_key_check = Some(true);
    store.save(update, true).unwrap();
    assert_eq!(
        manager
            .open(OpenRequest {
                connection_id: connection.connection_id,
                root_path: None
            })
            .await
            .unwrap_err()
            .code,
        "SFTP_UNAVAILABLE"
    );
    manager.shutdown();
    eventually(|| fixture.stats.live.load(Ordering::SeqCst) == 0).await;
}
#[tokio::test]
async fn directory_operations_reject_traversal_links_permissions_and_recursive_delete() {
    let fixture = Fixture::new().await;
    let (manager, _, id) = fixture.manager(Settings::default()).await;
    manager
        .change_entry(&id, "/home/子目录", "mkdir")
        .await
        .unwrap();
    manager
        .change_entry(&id, "/home/子目录/空文件", "file")
        .await
        .unwrap();
    let entries = manager.entries(&id, Some("/home/子目录")).await.unwrap();
    assert_eq!(entries[0].size, 0);
    assert_eq!(entries[0].kind, Kind::File);
    assert!(manager
        .change_entry(&id, "/home/子目录/空文件", "file")
        .await
        .is_err());
    assert!(manager
        .change_entry(&id, "/home/子目录", "delete")
        .await
        .is_err());
    for path in ["/home/../outside", "/outside", "/home/link/escape"] {
        assert_eq!(
            manager
                .change_entry(&id, path, "file")
                .await
                .unwrap_err()
                .code,
            "SFTP_PATH_FORBIDDEN"
        );
    }
    assert_eq!(
        manager
            .entries(&id, Some("/home/denied"))
            .await
            .unwrap_err()
            .code,
        "SFTP_FORBIDDEN"
    );
    assert!(manager.change_entry(&id, "/home", "delete").await.is_err());
    manager
        .change_entry(&id, "/home/子目录/空文件", "delete")
        .await
        .unwrap();
    manager
        .change_entry(&id, "/home/子目录", "delete")
        .await
        .unwrap();
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
    manager.shutdown();
}
#[tokio::test]
async fn nested_upload_and_download_include_empty_files_and_directories() {
    let fixture = Fixture::new().await;
    let (manager, _, id) = fixture.manager(Settings::default()).await;
    let task = manager
        .create_transfer(create(
            &id,
            Direction::Upload,
            "/home",
            Some(vec![
                file("tree/嵌套/data", 5),
                file("tree/zero", 0),
                directory("tree/empty"),
            ]),
        ))
        .await
        .unwrap();
    for item in task.items.iter().filter(|i| i.kind == Kind::File) {
        let body = if item.size == 0 {
            Body::empty()
        } else {
            Body::from("hello")
        };
        manager
            .upload(&task.transfer_id, &item.item_id, body, Some(item.size))
            .await
            .unwrap();
    }
    assert_eq!(
        manager
            .transfer_view(&task.transfer_id)
            .unwrap()
            .progress
            .status,
        "COMPLETED"
    );
    assert_eq!(fixture.read("/home/tree/嵌套/data"), b"hello");
    assert!(fixture.root.path().join("home/tree/empty").is_dir());
    let download = manager
        .create_transfer(create(&id, Direction::Download, "/home/tree", None))
        .await
        .unwrap();
    for item in &download.items {
        if item.kind == Kind::File {
            let response = manager
                .download(&download.transfer_id, &item.item_id)
                .await
                .unwrap();
            let bytes = to_bytes(response.into_body(), 1024).await.unwrap();
            assert_eq!(bytes.len() as u64, item.size);
            assert_eq!(
                manager
                    .transfer_view(&download.transfer_id)
                    .unwrap()
                    .items
                    .iter()
                    .find(|i| i.item_id == item.item_id)
                    .unwrap()
                    .status,
                Status::Sent
            );
        }
        manager
            .confirm(&download.transfer_id, &item.item_id, true)
            .unwrap();
        manager
            .confirm(&download.transfer_id, &item.item_id, true)
            .unwrap();
    }
    assert_eq!(
        manager
            .transfer_view(&download.transfer_id)
            .unwrap()
            .progress
            .status,
        "COMPLETED"
    );
    manager.shutdown();
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
}
#[tokio::test]
async fn upload_conflicts_preserve_existing_files_and_support_skip_or_explicit_replace() {
    let fixture = Fixture::new().await;
    fixture.file("/home/file", b"original");
    let (manager, _, id) = fixture.manager(Settings::default()).await;
    let failed = manager
        .create_transfer(create(
            &id,
            Direction::Upload,
            "/home",
            Some(vec![file("file", 3)]),
        ))
        .await
        .unwrap();
    assert_eq!(failed.items[0].status, Status::Failed);
    assert!(manager
        .upload(
            &failed.transfer_id,
            &failed.items[0].item_id,
            Body::from("new"),
            Some(3)
        )
        .await
        .is_err());
    assert_eq!(fixture.read("/home/file"), b"original");
    for conflict in [Conflict::Skip, Conflict::Replace] {
        let mut request = create(&id, Direction::Upload, "/home", Some(vec![file("file", 3)]));
        request.conflict = conflict;
        let task = manager.create_transfer(request).await.unwrap();
        if conflict == Conflict::Skip {
            assert_eq!(task.items[0].status, Status::Skipped);
            assert_eq!(task.progress.total_bytes, 0);
        } else {
            manager
                .upload(
                    &task.transfer_id,
                    &task.items[0].item_id,
                    Body::from("new"),
                    Some(3),
                )
                .await
                .unwrap();
            assert_eq!(fixture.read("/home/file"), b"new");
        }
    }
    assert!(no_part(&fixture));
    manager.shutdown();
}
#[tokio::test]
async fn length_errors_do_not_commit_and_failed_items_can_retry() {
    let fixture = Fixture::new().await;
    let (manager, _, id) = fixture.manager(Settings::default()).await;
    let task = manager
        .create_transfer(create(
            &id,
            Direction::Upload,
            "/home",
            Some(vec![file("file", 3)]),
        ))
        .await
        .unwrap();
    let item = &task.items[0];
    assert_eq!(
        manager
            .upload(&task.transfer_id, &item.item_id, Body::from("bad"), Some(4))
            .await
            .unwrap_err()
            .code,
        "SFTP_SIZE_MISMATCH"
    );
    for bytes in ["x", "too long"] {
        assert_eq!(
            manager
                .upload(&task.transfer_id, &item.item_id, Body::from(bytes), None)
                .await
                .unwrap_err()
                .code,
            "SFTP_SIZE_MISMATCH"
        );
        assert!(!fixture.root.path().join("home/file").exists());
        assert!(no_part(&fixture));
    }
    manager
        .upload(&task.transfer_id, &item.item_id, Body::from("yes"), Some(3))
        .await
        .unwrap();
    assert!(manager
        .upload(&task.transfer_id, &item.item_id, Body::from("bad"), Some(3))
        .await
        .is_err());
    assert_eq!(fixture.read("/home/file"), b"yes");
    manager.shutdown();
}
#[tokio::test]
async fn download_checks_source_size_requires_confirmation_and_retries_local_save_failure() {
    let fixture = Fixture::new().await;
    fixture.file("/home/file", b"old");
    let (manager, _, id) = fixture.manager(Settings::default()).await;
    let task = manager
        .create_transfer(create(&id, Direction::Download, "/home/file", None))
        .await
        .unwrap();
    let item = &task.items[0];
    assert!(manager
        .confirm(&task.transfer_id, &item.item_id, true)
        .is_err());
    fixture.file("/home/file", b"changed");
    assert_eq!(
        manager
            .download(&task.transfer_id, &item.item_id)
            .await
            .unwrap_err()
            .code,
        "SFTP_SOURCE_CHANGED"
    );
    fixture.file("/home/file", b"old");
    let response = manager
        .download(&task.transfer_id, &item.item_id)
        .await
        .unwrap();
    assert_eq!(
        &to_bytes(response.into_body(), 100).await.unwrap()[..],
        b"old"
    );
    assert_eq!(
        manager
            .transfer_view(&task.transfer_id)
            .unwrap()
            .progress
            .status,
        "AWAITING_CONFIRMATION"
    );
    manager
        .confirm(&task.transfer_id, &item.item_id, false)
        .unwrap();
    let response = manager
        .download(&task.transfer_id, &item.item_id)
        .await
        .unwrap();
    to_bytes(response.into_body(), 100).await.unwrap();
    manager
        .confirm(&task.transfer_id, &item.item_id, true)
        .unwrap();
    manager.shutdown();
}
#[tokio::test]
async fn cancellation_stops_upload_cleans_temp_and_keeps_sibling_session_usable() {
    let fixture = Fixture::new().await;
    let (manager, _, id) = fixture.manager(Settings::default()).await;
    let task = manager
        .create_transfer(create(
            &id,
            Direction::Upload,
            "/home",
            Some(vec![file("big", 1024 * 1024)]),
        ))
        .await
        .unwrap();
    *fixture.delay.lock().unwrap() = Duration::from_millis(80);
    let upload_manager = manager.clone();
    let transfer = task.transfer_id.clone();
    let item = task.items[0].item_id.clone();
    let upload = tokio::spawn(async move {
        upload_manager
            .upload(
                &transfer,
                &item,
                Body::from(vec![b'x'; 1024 * 1024]),
                Some(1024 * 1024),
            )
            .await
    });
    eventually(|| !no_part(&fixture)).await;
    assert_eq!(
        manager
            .upload(
                &task.transfer_id,
                &task.items[0].item_id,
                Body::empty(),
                None
            )
            .await
            .unwrap_err()
            .code,
        "SFTP_CONFLICT"
    );
    manager.cancel(&task.transfer_id).unwrap();
    assert_eq!(upload.await.unwrap().unwrap_err().code, "SFTP_CANCELLED");
    assert_eq!(
        manager
            .transfer_view(&task.transfer_id)
            .unwrap()
            .progress
            .status,
        "CANCELLED"
    );
    assert!(!fixture.root.path().join("home/big").exists());
    assert!(no_part(&fixture));
    assert!(manager
        .upload(
            &task.transfer_id,
            &task.items[0].item_id,
            Body::empty(),
            None
        )
        .await
        .is_err());
    *fixture.delay.lock().unwrap() = Duration::ZERO;
    manager
        .change_entry(&id, "/home/still-alive", "file")
        .await
        .unwrap();
    manager.shutdown();
}
#[tokio::test]
async fn dropped_or_stalled_download_releases_channel_and_concurrency_slots() {
    let fixture = Fixture::new().await;
    fixture.file("/home/big", &vec![b'x'; 1024 * 1024]);
    let (manager, _, id) = fixture
        .manager(Settings {
            stream_idle_timeout: Duration::from_millis(100),
            ..Default::default()
        })
        .await;
    for drop_response in [true, false] {
        let task = manager
            .create_transfer(create(&id, Direction::Download, "/home/big", None))
            .await
            .unwrap();
        let response = manager
            .download(&task.transfer_id, &task.items[0].item_id)
            .await
            .unwrap();
        if drop_response {
            drop(response);
        } else {
            tokio::time::sleep(Duration::from_millis(220)).await;
            drop(response);
        }
        eventually(|| {
            manager.transfer_view(&task.transfer_id).unwrap().items[0].status == Status::Failed
        })
        .await;
        eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
    }
    manager
        .change_entry(&id, "/home/usable", "file")
        .await
        .unwrap();
    manager.shutdown();
}
#[tokio::test]
async fn idle_reaping_capacity_and_task_retention_are_bounded() {
    let fixture = Fixture::new().await;
    let settings = Settings {
        max_sessions: 1,
        max_tasks: 1,
        max_items: 2,
        max_tracked_items: 2,
        idle_timeout: Duration::from_millis(180),
        retention: Duration::from_millis(50),
        reaper_interval: Duration::from_millis(10),
        ..Default::default()
    };
    let (manager, connection, id) = fixture.manager(settings).await;
    assert_eq!(
        manager
            .open(OpenRequest {
                connection_id: connection.connection_id.clone(),
                root_path: None
            })
            .await
            .unwrap_err()
            .code,
        "SFTP_BUSY"
    );
    let task = manager
        .create_transfer(create(
            &id,
            Direction::Upload,
            "/home",
            Some(vec![file("one", 0)]),
        ))
        .await
        .unwrap();
    assert_eq!(
        manager
            .create_transfer(create(
                &id,
                Direction::Upload,
                "/home",
                Some(vec![file("two", 0)])
            ))
            .await
            .unwrap_err()
            .code,
        "SFTP_BUSY"
    );
    eventually(|| manager.transfer_view(&task.transfer_id).is_err()).await;
    let _ = manager
        .create_transfer(create(
            &id,
            Direction::Upload,
            "/home",
            Some(vec![file("two", 0)]),
        ))
        .await
        .unwrap();
    // State queries must not keep an abandoned window alive.
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            if manager.session_view(&id).is_err() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let opened = manager
        .open(OpenRequest {
            connection_id: connection.connection_id,
            root_path: None,
        })
        .await
        .unwrap();
    manager.close(&opened.sftp_session_id).unwrap();
    manager.shutdown();
}
#[tokio::test]
async fn manifest_limits_and_other_owners_are_rejected() {
    let fixture = Fixture::new().await;
    let (store, connection) = fixture.saved(false);
    let manager = Manager::with_settings(
        store.clone(),
        Settings {
            max_items: 2,
            ..Default::default()
        },
    );
    let session = manager
        .open(OpenRequest {
            connection_id: connection.connection_id.clone(),
            root_path: Some("/home".into()),
        })
        .await
        .unwrap();
    for items in [
        vec![file("../escape", 1)],
        vec![file("a", 0), file("a/b", 0)],
        vec![file("a", 0), file("b", 0), file("c", 0)],
        vec![file("a", u64::MAX)],
    ] {
        assert!(manager
            .create_transfer(create(
                &session.sftp_session_id,
                Direction::Upload,
                "/home",
                Some(items)
            ))
            .await
            .is_err());
    }
    let mut update:ConnectionRequest=serde_json::from_value(serde_json::json!({"connectionId":connection.connection_id,"connectionName":"other","host":"127.0.0.1","port":fixture.port,"username":"tester","userId":"other"})).unwrap();
    update.strict_host_key_check = Some(false);
    store.save(update, true).unwrap();
    assert_eq!(
        manager
            .open(OpenRequest {
                connection_id: connection.connection_id,
                root_path: None
            })
            .await
            .unwrap_err()
            .code,
        "SFTP_NOT_FOUND"
    );
    manager.shutdown();
}
#[tokio::test]
async fn http_contract_and_sse_snapshot_replacement_close_notification() {
    use axum::http::{Request, StatusCode};
    use futures_util::StreamExt;
    use tower::ServiceExt;
    let fixture = Fixture::new().await;
    let (store, connection) = fixture.saved(false);
    let app = router(store);
    async fn json(
        app: &axum::Router,
        method: &str,
        uri: &str,
        body: Option<serde_json::Value>,
    ) -> (StatusCode, serde_json::Value) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(uri)
                    .header("Content-Type", "application/json")
                    .body(
                        body.map(|b| Body::from(b.to_string()))
                            .unwrap_or_else(Body::empty),
                    )
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }
    let (status, opened) = json(
        &app,
        "POST",
        "/api/v1/sftp/sessions",
        Some(serde_json::json!({"connectionId":connection.connection_id,"rootPath":"/home"})),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let session = opened["data"]["sftpSessionId"].as_str().unwrap();
    let uri = format!("/api/v1/sftp/sessions/{session}");
    let (_, view) = json(&app, "GET", &uri, None).await;
    assert_eq!(view["data"]["rootPath"], "/home");
    let events = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{uri}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(events.headers()["content-type"], "text/event-stream");
    let mut first = events.into_body().into_data_stream();
    let initial = first.next().await.unwrap().unwrap();
    assert!(String::from_utf8_lossy(&initial).contains("event: progress"));
    let events = app
        .clone()
        .oneshot(
            Request::builder()
                .uri(format!("{uri}/events"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let mut second = events.into_body().into_data_stream();
    second.next().await.unwrap().unwrap();
    assert!(tokio::time::timeout(Duration::from_secs(1), first.next())
        .await
        .unwrap()
        .is_none());
    let (_,created)=json(&app,"POST","/api/v1/sftp/transfers",Some(serde_json::json!({"sftpSessionId":session,"direction":"UPLOAD","remotePath":"/home","items":[{"relativePath":"empty","kind":"FILE","size":0}]}))).await;
    let task = created["data"]["transferId"].as_str().unwrap();
    let item = created["data"]["items"][0]["itemId"].as_str().unwrap();
    let content = format!("/api/v1/sftp/transfers/{task}/items/{item}/content");
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("PUT")
                .uri(content)
                .header("Content-Type", "application/octet-stream")
                .header("Content-Length", "0")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let (_, tasks) = json(&app, "GET", &format!("{uri}/transfers"), None).await;
    assert_eq!(tasks["data"][0]["progress"]["status"], "COMPLETED");
    let (status, _) = json(&app, "DELETE", &uri, None).await;
    assert_eq!(status, StatusCode::OK);
    let closed = tokio::time::timeout(Duration::from_secs(1), second.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&closed).contains("session-closed"));
    let (status, error) = json(&app, "GET", &uri, None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error["code"], "SFTP_NOT_FOUND");
    eventually(|| fixture.stats.live.load(Ordering::SeqCst) == 0).await;
}

#[tokio::test]
async fn empty_directory_save_failure_can_be_confirmed_after_retry() {
    let fixture = Fixture::new().await;
    std::fs::create_dir(fixture.root.path().join("home/empty")).unwrap();
    let (manager, _, id) = fixture.manager(Settings::default()).await;
    let task = manager
        .create_transfer(create(&id, Direction::Download, "/home/empty", None))
        .await
        .unwrap();
    assert_eq!(task.progress.status, "AWAITING_CONFIRMATION");
    let item = &task.items[0];
    manager
        .confirm(&task.transfer_id, &item.item_id, false)
        .unwrap();
    assert_eq!(
        manager.transfer_view(&task.transfer_id).unwrap().items[0].status,
        Status::Failed
    );
    manager
        .confirm(&task.transfer_id, &item.item_id, true)
        .unwrap();
    assert_eq!(
        manager
            .transfer_view(&task.transfer_id)
            .unwrap()
            .progress
            .status,
        "COMPLETED"
    );
    let result = manager
        .create_transfer(create(
            &id,
            Direction::Upload,
            "/home",
            Some(vec![directory("denied/sub")]),
        ))
        .await;
    assert!(result.is_err());
    assert_eq!(manager.tasks(&id).unwrap().len(), 1);
    manager.shutdown();
}
