use shellpilot_server::{
    files::{Manager, Settings},
    store::Store,
};
use std::sync::Arc;
use tokio::io::AsyncReadExt;

#[tokio::test]
async fn native_file_handles_share_validation_hash_storage_and_delete_with_http() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("db.sqlite3")).unwrap());
    let manager = Manager::new_native(store, Settings::default(), uuid::Uuid::new_v4());
    let original = "原生文件🙂".repeat(10000).into_bytes();
    let uploaded = manager
        .clone()
        .upload_reader(
            "报告.txt".into(),
            "text/plain".into(),
            std::io::Cursor::new(original.clone()),
        )
        .await
        .unwrap();
    assert_eq!(uploaded.size, original.len() as u64);
    assert!(uploaded.download_url.starts_with("ipc://builtin/"));
    assert!(uploaded.download_url.ends_with(&format!("/files/{}", uploaded.file_id)));
    assert!(!uploaded.download_url.contains("signature="));
    let (_, mut file) = manager.open_attachment(&uploaded.file_id).await.unwrap();
    let mut saved = vec![];
    file.read_to_end(&mut saved).await.unwrap();
    assert_eq!(saved, original);
    drop(file);
    assert_eq!(manager.list(0).await.unwrap().items.len(), 1);
    manager.delete(&uploaded.file_id).await.unwrap();
    assert_eq!(manager.stats().await.unwrap().used_bytes, 0);
    for name in ["bad.exe", "bad"] {
        assert!(manager
            .clone()
            .upload_reader(
                name.into(),
                "".into(),
                std::io::Cursor::new(b"data".to_vec())
            )
            .await
            .is_err());
    }
    let cleaned = manager
        .clone()
        .upload_reader(
            "../escape.txt".into(),
            "".into(),
            std::io::Cursor::new(b"safe".to_vec()),
        )
        .await
        .unwrap();
    assert_eq!(cleaned.file_name, "escape.txt");
    assert!(!dir.path().join("escape.txt").exists());
    assert!(manager
        .clone()
        .upload_reader("empty.txt".into(), "".into(), std::io::Cursor::new(vec![]))
        .await
        .is_err());
    manager.shutdown().await;
}
#[tokio::test]
async fn native_oversize_and_shutdown_cleanup_reservations_and_unpublished_files() {
    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(Store::open(&dir.path().join("db.sqlite3")).unwrap());
    let manager = Manager::new_native(
        store,
        Settings {
            max_file_bytes: 1024,
            ..Settings::default()
        },
        uuid::Uuid::new_v4(),
    );
    let error = manager
        .clone()
        .upload_reader(
            "large.log".into(),
            "".into(),
            std::io::Cursor::new(vec![b'x'; 1025]),
        )
        .await
        .unwrap_err();
    assert_eq!(error.status.as_u16(), 413);
    let (writer, reader) = tokio::io::duplex(32);
    let active = manager.clone();
    let upload = tokio::spawn(async move {
        active
            .upload_reader("waiting.log".into(), "".into(), reader)
            .await
    });
    tokio::time::sleep(std::time::Duration::from_millis(30)).await;
    manager.shutdown().await;
    drop(writer);
    assert!(upload.await.unwrap().is_err());
    let store = Arc::new(Store::open(&dir.path().join("db.sqlite3")).unwrap());
    drop(manager);
    let recovered = Manager::new_native(store, Settings::default(), uuid::Uuid::new_v4());
    let stats = recovered.stats().await.unwrap();
    assert_eq!(stats.reserved_bytes, 0);
    assert_eq!(stats.file_count, 0);
    recovered.shutdown().await;
}
