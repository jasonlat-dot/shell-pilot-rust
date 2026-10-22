#![cfg(windows)]
#[path = "sftp_support/mod.rs"]
#[allow(dead_code)]
mod support;
use shellpilot_server::terminal::{OpenRequest, TerminalManager};
use std::{sync::atomic::Ordering, time::Duration};
use support::*;

#[tokio::test]
async fn sidebar_uses_original_transport_lists_metadata_and_cannot_elevate_or_cross_sessions() {
    let fixture = Fixture::new().await;
    fixture.file("/home/.hidden", b"PRIVATE_CONTENT");
    fixture.file("/home/space 中文.txt", b"1234");
    std::fs::create_dir(fixture.root.path().join("home/scripts")).unwrap();
    let (store, connection) = fixture.saved(false);
    let terminals = TerminalManager::new(store);
    let terminal = terminals
        .connect(OpenRequest {
            connection_id: connection.connection_id,
            request_id: uuid::Uuid::new_v4().to_string(),
            cols: None,
            rows: None,
        })
        .await
        .unwrap();
    let listing = terminals
        .directory_list(&terminal.session_id, &connection.user_id, "")
        .await
        .unwrap();
    let json = serde_json::to_value(listing).unwrap();
    assert_eq!(json["path"], "/home");
    assert_eq!(json["truncated"], false);
    let entries = json["entries"].as_array().unwrap();
    assert_eq!(entries[0]["name"], "scripts");
    assert_eq!(entries[0]["directory"], true);
    assert!(entries.iter().any(|entry| entry["name"] == ".hidden"));
    assert!(entries
        .iter()
        .any(|entry| entry["name"] == "space 中文.txt" && entry["size"] == 4));
    assert!(!json.to_string().contains("PRIVATE_CONTENT"));
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 1);
    assert!(fixture.stats.root_commands.lock().unwrap().is_empty());
    assert!(terminals
        .directory_list(&terminal.session_id, "other-user", "/home")
        .await
        .is_err());
    for path in ["relative", "/root", "/missing", "/home\n"] {
        assert!(
            terminals
                .directory_list(&terminal.session_id, &connection.user_id, path)
                .await
                .is_err(),
            "{path}"
        );
    }
    assert!(terminals.connected(&terminal.session_id).connected);
    terminals.close(&terminal.session_id);
    assert!(terminals
        .directory_list(&terminal.session_id, &connection.user_id, "/home")
        .await
        .is_err());
    terminals.shutdown().await;
}

#[tokio::test]
async fn cancelling_sidebar_directory_releases_channel_and_keeps_shell_alive() {
    let fixture = Fixture::new().await;
    let (store, connection) = fixture.saved(false);
    let terminals = std::sync::Arc::new(TerminalManager::new(store));
    let terminal = terminals
        .connect(OpenRequest {
            connection_id: connection.connection_id,
            request_id: uuid::Uuid::new_v4().to_string(),
            cols: None,
            rows: None,
        })
        .await
        .unwrap();
    *fixture.directory_delay.lock().unwrap() = Duration::from_secs(30);
    let worker = terminals.clone();
    let id = terminal.session_id.clone();
    let user = connection.user_id;
    let task = tokio::spawn(async move { worker.directory_list(&id, &user, "/home").await });
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) >= 2).await;
    task.abort();
    let _ = task.await;
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 1).await;
    assert!(terminals.connected(&terminal.session_id).connected);
    terminals.shutdown().await;
}

#[tokio::test]
async fn sidebar_reuses_root_shell_auth_and_revokes_inflight_access_on_exit() {
    let fixture = Fixture::new().await;
    let (store, connection) = fixture.saved(false);
    let terminals = std::sync::Arc::new(TerminalManager::new(store));
    let terminal = terminals
        .connect_with_completion(
            OpenRequest {
                connection_id: connection.connection_id.clone(),
                request_id: uuid::Uuid::new_v4().to_string(),
                cols: None,
                rows: None,
            },
            Some(shellpilot_server::terminal::completion::ShellIntegration {
                nonce: "a".repeat(32),
                script: "true".into(),
            }),
        )
        .await
        .unwrap();
    assert!(terminals
        .directory_list(&terminal.session_id, &connection.user_id, "/root")
        .await
        .is_err());
    terminals
        .write(&terminal.session_id, "TEST_ROOT".into())
        .await
        .unwrap();
    let report = terminals.read(&terminal.session_id).await;
    assert!(serde_json::to_value(report).unwrap()["output"]
        .as_str()
        .unwrap()
        .contains(";F;0;1000;"));
    let listing = terminals
        .directory_list(&terminal.session_id, &connection.user_id, "/root")
        .await
        .unwrap();
    let json = serde_json::to_value(listing).unwrap();
    assert_eq!(json["entries"][0]["name"], "folder");
    assert_eq!(fixture.stats.password_prompts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 1);
    assert!(fixture
        .stats
        .root_commands
        .lock()
        .unwrap()
        .iter()
        .all(|s| !s.contains("sudo ")));
    assert!(terminals
        .directory_list(&terminal.session_id, "other-user", "/root")
        .await
        .is_err());

    let other = terminals
        .connect_with_completion(
            OpenRequest {
                connection_id: connection.connection_id.clone(),
                request_id: uuid::Uuid::new_v4().to_string(),
                cols: None,
                rows: None,
            },
            Some(shellpilot_server::terminal::completion::ShellIntegration {
                nonce: "b".repeat(32),
                script: "true".into(),
            }),
        )
        .await
        .unwrap();
    assert!(
        terminals
            .directory_list(&other.session_id, &connection.user_id, "/root")
            .await
            .is_err(),
        "其他终端不继承 root 权限"
    );
    *fixture.directory_delay.lock().unwrap() = Duration::from_millis(200);
    let count = fixture.stats.root_commands.lock().unwrap().len();
    let worker = terminals.clone();
    let id = terminal.session_id.clone();
    let user = connection.user_id.clone();
    let pending = tokio::spawn(async move { worker.directory_list(&id, &user, "/root").await });
    eventually(|| fixture.stats.root_commands.lock().unwrap().len() > count).await;
    terminals
        .write(&terminal.session_id, "TEST_LOGIN".into())
        .await
        .unwrap();
    let _ = terminals.read(&terminal.session_id).await;
    assert!(
        pending.await.unwrap().is_err(),
        "退出 root 后丢弃在途提权结果"
    );
    assert!(terminals
        .directory_list(&terminal.session_id, &connection.user_id, "/root")
        .await
        .is_err());
    assert!(terminals.connected(&terminal.session_id).connected);
    terminals.shutdown().await;
}

#[tokio::test]
async fn file_manager_opens_current_root_via_terminal_auth_and_close_keeps_ssh_alive() {
    use shellpilot_server::sftp::{model::OpenRequest as FilesOpen, Manager};
    let fixture = Fixture::new().await;
    fixture.file("/root/project/private.txt", b"root-only");
    let (store, connection) = fixture.saved(false);
    let terminals = TerminalManager::new(store.clone());
    let files = Manager::new(store);
    let terminal = terminals
        .connect_with_completion(
            OpenRequest {
                connection_id: connection.connection_id.clone(),
                request_id: uuid::Uuid::new_v4().to_string(),
                cols: None,
                rows: None,
            },
            Some(shellpilot_server::terminal::completion::ShellIntegration {
                nonce: "e".repeat(32),
                script: "true".into(),
            }),
        )
        .await
        .unwrap();
    let login = terminals
        .file_access(
            &terminal.session_id,
            &connection.connection_id,
            &connection.user_id,
        )
        .unwrap();
    let login_files = files
        .open_terminal(
            FilesOpen {
                connection_id: connection.connection_id.clone(),
                root_path: Some("/".into()),
                ..Default::default()
            },
            login,
        )
        .await
        .unwrap();
    assert_eq!(login_files.access_username, "tester");
    files
        .entries(&login_files.sftp_session_id, Some("/home"))
        .await
        .unwrap();
    files.close(&login_files.sftp_session_id).unwrap();
    assert!(terminals.connected(&terminal.session_id).connected);
    assert!(terminals
        .file_access(&terminal.session_id, "another-host", &connection.user_id)
        .is_err());
    terminals
        .write(&terminal.session_id, "TEST_ROOT".into())
        .await
        .unwrap();
    let _ = terminals.read(&terminal.session_id).await;
    let access = terminals
        .file_access(
            &terminal.session_id,
            &connection.connection_id,
            &connection.user_id,
        )
        .unwrap();
    let opened = files
        .open_terminal(
            FilesOpen {
                connection_id: connection.connection_id.clone(),
                root_path: Some("/".into()),
                ..Default::default()
            },
            access,
        )
        .await
        .unwrap();
    assert_eq!(opened.access_username, "root");
    let entries = files
        .entries(&opened.sftp_session_id, Some("/root/project"))
        .await
        .unwrap();
    assert_eq!(entries[0].name, "private.txt");
    files
        .change_entry(&opened.sftp_session_id, "/root/project/new.txt", "file")
        .await
        .unwrap();
    assert_eq!(fixture.read("/root/project/new.txt"), b"");
    assert_eq!(fixture.stats.password_prompts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 1);
    assert!(fixture
        .stats
        .root_commands
        .lock()
        .unwrap()
        .iter()
        .all(|s| !s.contains("sudo ")));
    files.close(&opened.sftp_session_id).unwrap();
    assert!(
        terminals.connected(&terminal.session_id).connected,
        "关闭文件窗口不能关闭复用的 SSH transport"
    );

    let access = terminals
        .file_access(
            &terminal.session_id,
            &connection.connection_id,
            &connection.user_id,
        )
        .unwrap();
    let opened = files
        .open_terminal(
            FilesOpen {
                connection_id: connection.connection_id.clone(),
                root_path: Some("/root/project".into()),
                ..Default::default()
            },
            access,
        )
        .await
        .unwrap();
    terminals
        .write(&terminal.session_id, "TEST_LOGIN".into())
        .await
        .unwrap();
    let _ = terminals.read(&terminal.session_id).await;
    assert_eq!(
        files
            .entries(&opened.sftp_session_id, Some("/root/project"))
            .await
            .unwrap_err().code,
        "SFTP_IDENTITY_CHANGED",
        "退出 root 后文件窗口权限立即失效"
    );
    files.close(&opened.sftp_session_id).unwrap();
    files.shutdown();
    terminals.shutdown().await;
}
