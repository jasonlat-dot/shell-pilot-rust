use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
    Router,
};
use futures_util::StreamExt;
use serde_json::Value;
use shellpilot_server::{
    files::{
        model::{DownloadQuery, UploadedFile},
        Manager, Settings,
    },
    router_with_files,
    store::Store,
};
use std::{sync::Arc, time::Duration};
use tower::ServiceExt;
struct Fixture {
    app: Router,
    manager: Arc<Manager>,
    store: Arc<Store>,
    dir: tempfile::TempDir,
}
impl Fixture {
    fn new(settings: Settings) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Arc::new(Store::open(&dir.path().join("data.sqlite3")).unwrap());
        let manager = Manager::new(store.clone(), settings, "http://127.0.0.1:9001".into());
        let app = router_with_files(store.clone(), manager.clone());
        Self {
            dir,
            store,
            manager,
            app,
        }
    }
    async fn close(self) {
        self.manager.shutdown().await;
        drop(self.app);
        drop(self.manager);
        drop(self.store);
        drop(self.dir);
    }
    async fn upload(&self, name: &str, bytes: &[u8]) -> (StatusCode, Value) {
        let payload = multipart(name, bytes, false);
        call(
            &self.app,
            "POST",
            "/api/v1/files",
            Body::from(payload),
            "multipart/form-data; boundary=phase4-test",
        )
        .await
    }
    fn root(&self) -> std::path::PathBuf {
        self.dir.path().join("attachments")
    }
}
fn multipart(name: &str, bytes: &[u8], extra: bool) -> Vec<u8> {
    let mut body=format!("--phase4-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\nContent-Type: text/plain\r\n\r\n").into_bytes();
    body.extend_from_slice(bytes);
    body.extend_from_slice(b"\r\n");
    if extra {
        body.extend_from_slice(
            b"--phase4-test\r\nContent-Disposition: form-data; name=\"userId\"\r\n\r\nother\r\n",
        );
    }
    body.extend_from_slice(b"--phase4-test--\r\n");
    body
}
async fn call(
    app: &Router,
    method: &str,
    uri: &str,
    body: Body,
    content_type: &str,
) -> (StatusCode, Value) {
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("Content-Type", content_type)
                .header("Host", "untrusted.example")
                .body(body)
                .unwrap(),
        )
        .await
        .unwrap();
    let status = response.status();
    let bytes = to_bytes(response.into_body(), 32 * 1024 * 1024)
        .await
        .unwrap();
    (status, serde_json::from_slice(&bytes).unwrap())
}
fn relative(url: &str) -> String {
    let url = url::Url::parse(url).unwrap();
    format!("{}?{}", url.path(), url.query().unwrap())
}
async fn json_get(app: &Router, uri: &str) -> (StatusCode, Value) {
    call(app, "GET", uri, Body::empty(), "application/json").await
}
async fn eventually(mut check: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !check() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
async fn empty(fixture: &Fixture) {
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            if fixture.manager.stats().await.unwrap().file_count == 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    if fixture.root().exists() {
        assert_eq!(
            std::fs::read_dir(fixture.root())
                .unwrap()
                .filter(|entry| entry.as_ref().unwrap().file_name() != ".storage.lock")
                .count(),
            0
        );
    }
}
#[tokio::test]
async fn upload_hash_metadata_download_headers_and_host_are_compatible() {
    let fixture = Fixture::new(Settings::default());
    let (status, data) = fixture.upload("中文附件.TXT", b"hello phase four").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(data["code"], "SUCCESS_0000");
    let file: UploadedFile = serde_json::from_value(data["data"].clone()).unwrap();
    assert_eq!(file.status, "UPLOADED");
    assert_eq!(file.file_name, "中文附件.TXT");
    assert_eq!(file.size, 16);
    assert_eq!(file.sha256.len(), 64);
    assert!(file.download_url.starts_with("http://127.0.0.1:9001/"));
    let url = url::Url::parse(&file.download_url).unwrap();
    let query: DownloadQuery = serde_urlencoded_for_test(url.query().unwrap());
    assert_eq!(query.expires, file.url_expires_at.timestamp());
    assert_eq!(file.url_expires_at.timestamp_subsec_nanos(), 0);
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .uri(relative(&file.download_url))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["content-type"],
        "application/octet-stream"
    );
    assert_eq!(response.headers()["content-length"], "16");
    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
    assert!(response.headers()["content-disposition"]
        .to_str()
        .unwrap()
        .contains("filename*=UTF-8''"));
    assert_eq!(
        &to_bytes(response.into_body(), 100).await.unwrap()[..],
        b"hello phase four"
    );
    let (_, metadata) = json_get(&fixture.app, &format!("/api/v1/files/{}", file.file_id)).await;
    assert_eq!(metadata["data"]["sha256"], file.sha256);
    let (_, stats) = json_get(&fixture.app, "/api/v1/files/storage").await;
    assert_eq!(stats["data"]["usedBytes"], 16);
    assert_eq!(stats["data"]["reservedBytes"], 0);
    assert_eq!(stats["data"]["fileCount"], 1);
    fixture.close().await;
}
fn serde_urlencoded_for_test(query: &str) -> DownloadQuery {
    let params = url::form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .collect::<std::collections::HashMap<_, _>>();
    DownloadQuery {
        storage_id: params["storageId"].clone(),
        object_key: params["objectKey"].clone(),
        file_name: params["fileName"].clone(),
        expires: params["expires"].parse().unwrap(),
        signature: params["signature"].clone(),
    }
}
#[tokio::test]
async fn malformed_empty_disallowed_and_extra_fields_never_publish_files() {
    let fixture = Fixture::new(Settings::default());
    for (name, bytes, code) in [
        ("empty.txt", &b""[..], "FILE_INVALID"),
        ("program.exe", &b"x"[..], "FILE_TYPE_NOT_ALLOWED"),
        (".txt", &b"x"[..], "SUCCESS_0000"),
    ] {
        let (_, reply) = fixture.upload(name, bytes).await;
        assert_eq!(reply["code"], code);
        if code == "SUCCESS_0000" {
            fixture
                .manager
                .delete(reply["data"]["fileId"].as_str().unwrap())
                .await
                .unwrap();
        }
        empty(&fixture).await;
    }
    let (status, reply) = call(
        &fixture.app,
        "POST",
        "/api/v1/files",
        Body::from(multipart("safe.txt", b"secret", true)),
        "multipart/form-data; boundary=phase4-test",
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(reply["code"], "FILE_INVALID");
    empty(&fixture).await;
    for (body, type_) in [
        (Body::empty(), "application/json"),
        (
            Body::from("--broken"),
            "multipart/form-data; boundary=broken",
        ),
    ] {
        let (status, reply) = call(&fixture.app, "POST", "/api/v1/files", body, type_).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(reply["code"], "FILE_INVALID");
        empty(&fixture).await;
    }
    fixture.close().await;
}
#[tokio::test]
async fn size_and_total_request_limits_use_json_errors_and_release_reservations() {
    let fixture = Fixture::new(Settings {
        max_file_bytes: 4,
        ..Default::default()
    });
    let (status, reply) = fixture.upload("file.txt", b"12345").await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(reply["code"], "FILE_TOO_LARGE");
    empty(&fixture).await;
    let (status, reply) = fixture.upload("file.txt", b"1234").await;
    assert_eq!(status, StatusCode::OK);
    fixture
        .manager
        .delete(reply["data"]["fileId"].as_str().unwrap())
        .await
        .unwrap();
    empty(&fixture).await;
    // Limits apply to the complete multipart body, not just its first file field.
    let (status, reply) = call(
        &fixture.app,
        "POST",
        "/api/v1/files",
        Body::from(vec![b'x'; 22 * 1024 * 1024]),
        "multipart/form-data; boundary=phase4-test",
    )
    .await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(reply["code"], "FILE_TOO_LARGE");
    empty(&fixture).await;
    fixture.close().await;
}
#[tokio::test]
async fn signature_tampering_expiry_and_path_escape_are_rejected() {
    let fixture = Fixture::new(Settings::default());
    let (_, reply) = fixture.upload("safe.txt", b"contents").await;
    let file: UploadedFile = serde_json::from_value(reply["data"].clone()).unwrap();
    for (key, value, expected) in [
        ("fileName", "other.txt", StatusCode::BAD_REQUEST),
        ("objectKey", "../../secret.txt", StatusCode::BAD_REQUEST),
        ("storageId", "other", StatusCode::BAD_REQUEST),
        ("signature", "00", StatusCode::BAD_REQUEST),
        ("expires", "1", StatusCode::GONE),
    ] {
        let mut url = url::Url::parse(&file.download_url).unwrap();
        let mut pairs = url
            .query_pairs()
            .into_owned()
            .collect::<std::collections::BTreeMap<_, _>>();
        pairs.insert(key.into(), value.into());
        url.set_query(Some(
            &url::form_urlencoded::Serializer::new(String::new())
                .extend_pairs(pairs.iter())
                .finish(),
        ));
        let (status, _) = json_get(&fixture.app, &relative(url.as_str())).await;
        assert_eq!(status, expected);
    }
    assert_eq!(
        &std::fs::read(
            fixture.root().join(
                serde_urlencoded_for_test(
                    url::Url::parse(&file.download_url)
                        .unwrap()
                        .query()
                        .unwrap()
                )
                .object_key
            )
        )
        .unwrap()[..],
        b"contents"
    );
    fixture.close().await;
}
#[tokio::test]
async fn restart_keeps_metadata_and_reissues_links_while_old_signatures_expire() {
    let fixture = Fixture::new(Settings::default());
    let (_, reply) = fixture.upload("saved.txt", b"persistent").await;
    let old: UploadedFile = serde_json::from_value(reply["data"].clone()).unwrap();
    fixture.manager.shutdown().await;
    let next = Manager::new(
        fixture.store.clone(),
        Settings::default(),
        "http://127.0.0.1:9001".into(),
    );
    let app = router_with_files(fixture.store.clone(), next.clone());
    let (status, reply) = json_get(&app, &format!("/api/v1/files/{}", old.file_id)).await;
    assert_eq!(status, StatusCode::OK);
    let new: UploadedFile = serde_json::from_value(reply["data"].clone()).unwrap();
    assert_eq!(new.sha256, old.sha256);
    assert_ne!(new.download_url, old.download_url);
    assert_eq!(
        json_get(&app, &relative(&old.download_url)).await.0,
        StatusCode::BAD_REQUEST
    );
    let response = app
        .oneshot(
            Request::builder()
                .uri(relative(&new.download_url))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        &to_bytes(response.into_body(), 100).await.unwrap()[..],
        b"persistent"
    );
    next.shutdown().await;
    fixture.close().await;
}
#[tokio::test]
async fn quota_and_file_count_are_enforced_and_delete_releases_capacity() {
    let fixture = Fixture::new(Settings {
        max_file_bytes: 4,
        max_storage_bytes: 8,
        max_files: 2,
        ..Default::default()
    });
    let (_, one) = fixture.upload("one.txt", b"1234").await;
    let (_, two) = fixture.upload("two.txt", b"1234").await;
    let (status, error) = fixture.upload("three.txt", b"x").await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(error["code"], "FILE_STORAGE_QUOTA_EXCEEDED");
    let id = one["data"]["fileId"].as_str().unwrap();
    let old = one["data"]["downloadUrl"].as_str().unwrap();
    assert_eq!(
        call(
            &fixture.app,
            "DELETE",
            &format!("/api/v1/files/{id}"),
            Body::empty(),
            "application/json"
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        json_get(&fixture.app, &relative(old)).await.0,
        StatusCode::NOT_FOUND
    );
    assert_eq!(fixture.upload("three.txt", b"x").await.0, StatusCode::OK);
    assert_eq!(fixture.manager.stats().await.unwrap().file_count, 2);
    assert!(fixture
        .manager
        .metadata(two["data"]["fileId"].as_str().unwrap())
        .await
        .is_ok());
    fixture.close().await;
}
#[tokio::test]
async fn recovery_cleans_interrupted_uploads_but_preserves_live_and_foreign_files() {
    let fixture = Fixture::new(Settings {
        cleanup_interval: Duration::from_secs(3600),
        ..Default::default()
    });
    let (_, live) = fixture.upload("live.txt", b"keep").await;
    let id = uuid::Uuid::new_v4().to_string();
    let key = format!("{id}.txt");
    let db = rusqlite::Connection::open(fixture.dir.path().join("data.sqlite3")).unwrap();
    db.execute("INSERT INTO file_assets(id,owner_id,file_name,content_type,object_key,size,status,created_at) VALUES(?1,'default','interrupted.txt','text/plain',?2,20,'UPLOADING',1)",rusqlite::params![id,key]).unwrap();
    std::fs::write(fixture.root().join(format!(".{id}.uploading")), b"partial").unwrap();
    std::fs::write(fixture.root().join(&key), b"published-before-crash").unwrap();
    std::fs::write(fixture.root().join("user-note.txt"), b"foreign").unwrap();
    drop(db);
    fixture.manager.cleanup().await.unwrap();
    assert_eq!(fixture.manager.stats().await.unwrap().file_count, 1);
    assert!(!fixture.root().join(key).exists());
    assert!(!fixture.root().join(format!(".{id}.uploading")).exists());
    assert_eq!(
        std::fs::read(fixture.root().join("user-note.txt")).unwrap(),
        b"foreign"
    );
    assert!(fixture
        .manager
        .metadata(live["data"]["fileId"].as_str().unwrap())
        .await
        .is_ok());
    fixture.close().await;
}
#[tokio::test]
async fn optional_retention_removes_old_assets_and_default_retains_them() {
    let fixture = Fixture::new(Settings::default());
    let (_, reply) = fixture.upload("old.txt", b"old").await;
    let id = reply["data"]["fileId"].as_str().unwrap().to_string();
    let db = rusqlite::Connection::open(fixture.dir.path().join("data.sqlite3")).unwrap();
    db.execute("UPDATE file_assets SET created_at=1 WHERE id=?1", [&id])
        .unwrap();
    drop(db);
    fixture.manager.cleanup().await.unwrap();
    assert!(fixture.manager.metadata(&id).await.is_ok());
    fixture.manager.shutdown().await;
    let cleaner = Manager::new(
        fixture.store.clone(),
        Settings {
            retention: Some(Duration::from_secs(86400)),
            ..Default::default()
        },
        "http://127.0.0.1:9001".into(),
    );
    cleaner.cleanup().await.unwrap();
    assert_eq!(
        cleaner.metadata(&id).await.unwrap_err().code,
        "FILE_DOWNLOAD_NOT_FOUND"
    );
    assert_eq!(cleaner.stats().await.unwrap().used_bytes, 0);
    cleaner.shutdown().await;
    fixture.close().await;
}
#[tokio::test]
async fn disabled_invalid_storage_and_untrusted_origin_leave_no_assets() {
    for settings in [
        Settings {
            enabled: false,
            ..Default::default()
        },
        Settings {
            max_storage_bytes: 1,
            ..Default::default()
        },
    ] {
        let expected = if settings.enabled {
            "FILE_STORAGE_CONFIG_INVALID"
        } else {
            "FILE_STORAGE_NOT_CONFIGURED"
        };
        let fixture = Fixture::new(settings);
        let (status, reply) = fixture.upload("file.txt", b"x").await;
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(reply["code"], expected);
        assert!(!fixture.root().exists());
        fixture.close().await;
    }
    let fixture = Fixture::new(Settings::default());
    let response = fixture
        .app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/v1/files")
                .header("Content-Type", "multipart/form-data; boundary=phase4-test")
                .header("Origin", "https://evil.example")
                .body(Body::from(multipart("file.txt", b"forbidden", false)))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(fixture.manager.stats().await.unwrap().file_count, 0);
    fixture.close().await;
}
#[tokio::test]
async fn paginated_history_refresh_and_missing_files_are_handled() {
    let fixture = Fixture::new(Settings::default());
    for index in 0..21 {
        assert_eq!(
            fixture.upload(&format!("file{index}.txt"), b"x").await.0,
            StatusCode::OK
        );
    }
    let (_, page) = json_get(&fixture.app, "/api/v1/files?offset=0").await;
    assert_eq!(page["data"]["items"].as_array().unwrap().len(), 20);
    assert_eq!(page["data"]["hasMore"], true);
    let (_, page) = json_get(&fixture.app, "/api/v1/files?offset=20").await;
    assert_eq!(page["data"]["items"].as_array().unwrap().len(), 1);
    assert_eq!(page["data"]["hasMore"], false);
    assert_eq!(
        json_get(&fixture.app, "/api/v1/files?offset=-1").await.0,
        StatusCode::BAD_REQUEST
    );
    let id = page["data"]["items"][0]["fileId"].as_str().unwrap();
    let url = page["data"]["items"][0]["downloadUrl"].as_str().unwrap();
    let object =
        serde_urlencoded_for_test(url::Url::parse(url).unwrap().query().unwrap()).object_key;
    std::fs::remove_file(fixture.root().join(object)).unwrap();
    assert_eq!(
        json_get(&fixture.app, &format!("/api/v1/files/{id}"))
            .await
            .0,
        StatusCode::NOT_FOUND
    );
    fixture.manager.delete(id).await.unwrap();
    fixture.close().await;
}
#[tokio::test]
async fn stalled_upload_busy_rejection_and_shutdown_release_all_slots() {
    use futures_util::stream;
    let fixture = Fixture::new(Settings {
        max_uploads: 1,
        idle_timeout: Duration::from_millis(150),
        ..Default::default()
    });
    let prefix=b"--phase4-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"pending.txt\"\r\n\r\npartial".to_vec();
    let body = Body::from_stream(
        stream::once(async move { Ok::<_, std::io::Error>(prefix) }).chain(stream::pending()),
    );
    let app = fixture.app.clone();
    let pending = tokio::spawn(async move {
        call(
            &app,
            "POST",
            "/api/v1/files",
            body,
            "multipart/form-data; boundary=phase4-test",
        )
        .await
    });
    eventually(|| {
        fixture.root().exists()
            && std::fs::read_dir(fixture.root())
                .unwrap()
                .any(|entry| entry.unwrap().file_name() != ".storage.lock")
    })
    .await;
    let (status, reply) = fixture.upload("other.txt", b"x").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(reply["code"], "FILE_UPLOAD_BUSY");
    assert_eq!(pending.await.unwrap().0, StatusCode::BAD_REQUEST);
    empty(&fixture).await;
    assert_eq!(fixture.upload("after.txt", b"x").await.0, StatusCode::OK);
    fixture.close().await;
}
#[tokio::test]
async fn aborted_http_waiter_does_not_expose_partial_assets_and_shutdown_cancels_the_body() {
    use futures_util::stream;
    let fixture = Fixture::new(Settings {
        idle_timeout: Duration::from_secs(5),
        ..Default::default()
    });
    let prefix=b"--phase4-test\r\nContent-Disposition: form-data; name=\"file\"; filename=\"pending.txt\"\r\n\r\npartial".to_vec();
    let body = Body::from_stream(
        stream::once(async move { Ok::<_, std::io::Error>(prefix) }).chain(stream::pending()),
    );
    let app = fixture.app.clone();
    let pending = tokio::spawn(async move {
        call(
            &app,
            "POST",
            "/api/v1/files",
            body,
            "multipart/form-data; boundary=phase4-test",
        )
        .await
    });
    eventually(|| {
        fixture.root().exists()
            && std::fs::read_dir(fixture.root())
                .unwrap()
                .any(|entry| entry.unwrap().file_name() != ".storage.lock")
    })
    .await;
    pending.abort();
    let _ = pending.await;
    assert_eq!(fixture.manager.stats().await.unwrap().used_bytes, 0);
    fixture.manager.shutdown().await;
    empty(&fixture).await;
    fixture.close().await;
}
#[cfg(windows)]
#[tokio::test]
async fn junction_storage_root_is_rejected_without_touching_external_files() {
    use std::os::windows::process::CommandExt;
    // Set up the junction before starting the manager's recovery worker.
    let dir = tempfile::tempdir().unwrap();
    let external = tempfile::tempdir().unwrap();
    std::fs::write(external.path().join("keep.txt"), b"keep").unwrap();
    let status = std::process::Command::new("cmd")
        .creation_flags(0x08000000)
        .args(["/C", "mklink", "/J"])
        .arg(dir.path().join("attachments"))
        .arg(external.path())
        .status()
        .unwrap();
    assert!(status.success());
    let store = Arc::new(Store::open(&dir.path().join("data.sqlite3")).unwrap());
    let manager = Manager::new(
        store.clone(),
        Settings::default(),
        "http://127.0.0.1:9001".into(),
    );
    let fixture = Fixture {
        app: router_with_files(store.clone(), manager.clone()),
        manager,
        store,
        dir,
    };
    let (_, error) = fixture.upload("blocked.txt", b"blocked").await;
    assert_eq!(error["code"], "FILE_STORAGE_CONFIG_INVALID");
    assert_eq!(
        std::fs::read(external.path().join("keep.txt")).unwrap(),
        b"keep"
    );
    std::fs::remove_dir(fixture.root()).unwrap();
    fixture.close().await;
}

#[tokio::test]
async fn multipart_byte_boundaries_preserve_unicode_names_and_known_sha256() {
    let fixture = Fixture::new(Settings::default());
    let bytes = multipart("分块.txt", b"abc", false);
    let stream = futures_util::stream::iter(
        bytes
            .into_iter()
            .map(|byte| Ok::<_, std::io::Error>(vec![byte])),
    );
    let (status, reply) = call(
        &fixture.app,
        "POST",
        "/api/v1/files",
        Body::from_stream(stream),
        "multipart/form-data; boundary=phase4-test",
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["data"]["fileName"], "分块.txt");
    assert_eq!(
        reply["data"]["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    fixture.close().await;
}

#[tokio::test]
async fn two_managers_cannot_recover_or_write_the_same_storage_concurrently() {
    let fixture = Fixture::new(Settings::default());
    let (_, live) = fixture.upload("live.txt", b"keep").await;
    let second = Manager::new(
        fixture.store.clone(),
        Settings::default(),
        "http://127.0.0.1:9001".into(),
    );
    assert_eq!(
        second.cleanup().await.unwrap_err().code,
        "FILE_STORAGE_BUSY"
    );
    assert_eq!(
        second
            .metadata(live["data"]["fileId"].as_str().unwrap())
            .await
            .unwrap_err()
            .code,
        "FILE_STORAGE_BUSY"
    );
    fixture.manager.shutdown().await;
    assert!(second
        .metadata(live["data"]["fileId"].as_str().unwrap())
        .await
        .is_ok());
    second.shutdown().await;
    fixture.close().await;
}

#[tokio::test]
async fn committed_temporary_alias_is_cleaned_without_deleting_the_asset() {
    let fixture = Fixture::new(Settings::default());
    let (_, reply) = fixture.upload("keep.txt", b"keep").await;
    let file: UploadedFile = serde_json::from_value(reply["data"].clone()).unwrap();
    let key = serde_urlencoded_for_test(
        url::Url::parse(&file.download_url)
            .unwrap()
            .query()
            .unwrap(),
    )
    .object_key;
    std::fs::hard_link(
        fixture.root().join(key),
        fixture.root().join(format!(".{}.uploading", file.file_id)),
    )
    .unwrap();
    fixture.manager.cleanup().await.unwrap();
    assert!(!fixture
        .root()
        .join(format!(".{}.uploading", file.file_id))
        .exists());
    assert!(fixture.manager.open_attachment(&file.file_id).await.is_ok());
    fixture.close().await;
}
