#![cfg(windows)]
mod support;
use shellpilot_server::{
    error::Error,
    store::Store,
    terminal::{DisconnectReason, TerminalManager, TerminalSettings},
};
use std::{
    sync::{atomic::Ordering, Arc},
    time::Duration,
};
use support::*;

#[tokio::test]
async fn password_pty_write_resize_and_independent_windows() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let first = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    let second = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    assert_ne!(first.session_id, second.session_id);
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.stats.channels.load(Ordering::SeqCst), 2);
    output_contains(&manager, &first.session_id, "ready").await;
    output_contains(&manager, &second.session_id, "ready").await;
    manager
        .write(&first.session_id, "__unicode__".into())
        .await
        .unwrap();
    output_contains(&manager, &first.session_id, "你好世界").await;
    manager.resize(&first.session_id, 91, 32).await.unwrap();
    eventually(|| fixture.stats.sizes.lock().unwrap().contains(&(91, 32))).await;
    assert!(matches!(
        manager
            .delete_connection(connection.connection_id.clone())
            .await,
        Err(Error::Invalid(_))
    ));
    manager.close(&first.session_id);
    let state = manager.connected(&first.session_id);
    assert!(!state.connected);
    assert_eq!(
        state.disconnect_reason,
        Some(DisconnectReason::ClientClosed)
    );
    assert!(!state.reconnect_allowed);
    assert!(manager.connected(&second.session_id).connected);
    manager
        .write(&second.session_id, "other window".into())
        .await
        .unwrap();
    output_contains(&manager, &second.session_id, "other window").await;
    manager.close(&second.session_id);
    manager
        .delete_connection(connection.connection_id)
        .await
        .unwrap();
    manager.shutdown().await;
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
}

#[tokio::test]
async fn ed25519_private_key_authentication_and_startup_command() {
    let fixture = Fixture::new(Spec::default()).await;
    let store = Arc::new(Store::in_memory().unwrap());
    let mut request = fixture.request(true);
    request.startup_command = Some("startup-test".into());
    request.compression = Some(true);
    let connection = store.save(request, false).unwrap();
    let manager = TerminalManager::new(store);
    let terminal = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    output_contains(&manager, &terminal.session_id, "startup-test\n").await;
    manager.shutdown().await;
}

#[tokio::test]
async fn supported_host_and_login_key_algorithms_interoperate() {
    use russh::keys::{Algorithm, EcdsaCurve, HashAlg};
    for algorithm in [
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        },
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP384,
        },
        Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP521,
        },
        Algorithm::Rsa {
            hash: Some(HashAlg::Sha512),
        },
    ] {
        let fixture = Fixture::with_algorithms(Spec::default(), algorithm.clone(), algorithm).await;
        let (store, connection) = fixture.saved(true);
        let manager = TerminalManager::new(store);
        let terminal = manager
            .connect(open(&connection.connection_id))
            .await
            .unwrap();
        output_contains(&manager, &terminal.session_id, "ready").await;
        manager.shutdown().await;
    }
}

#[tokio::test]
async fn wrong_password_wrong_host_key_and_invalid_private_key_fail() {
    let fixture = Fixture::new(Spec::default()).await;
    let store = Arc::new(Store::in_memory().unwrap());
    let mut request = fixture.request(false);
    request.password = Some("incorrect".into());
    let connection = store.save(request, false).unwrap();
    let manager = TerminalManager::new(store.clone());
    assert!(matches!(
        manager.connect(open(&connection.connection_id)).await,
        Err(Error::Authentication)
    ));
    let mut request = fixture.request(false);
    let other = russh::keys::PrivateKey::random(
        &mut russh::keys::key::safe_rng(),
        russh::keys::Algorithm::Ed25519,
    )
    .unwrap();
    request.known_hosts = Some(format!(
        "[127.0.0.1]:{} {}",
        fixture.port,
        other.public_key().to_openssh().unwrap()
    ));
    let connection = store.save(request, false).unwrap();
    assert!(matches!(
        manager.connect(open(&connection.connection_id)).await,
        Err(Error::HostKey)
    ));
    let mut request = fixture.request(true);
    request.private_key = Some("invalid-key".into());
    let connection = store.save(request, false).unwrap();
    assert!(matches!(
        manager.connect(open(&connection.connection_id)).await,
        Err(Error::Invalid(_))
    ));
    assert_eq!(fixture.stats.channels.load(Ordering::SeqCst), 0);
    manager.shutdown().await;
}

#[tokio::test]
async fn cancellation_before_during_and_after_connection_never_leaks_shells() {
    let fixture = Fixture::new(Spec {
        shell_delay: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let request = open(&connection.connection_id);
    manager
        .cancel(&request.connection_id, &request.request_id)
        .unwrap();
    assert!(matches!(
        manager.connect(request).await,
        Err(Error::Cancelled)
    ));
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 0);
    let request = open(&connection.connection_id);
    let request_id = request.request_id.clone();
    let running_manager = manager.clone();
    let running = tokio::spawn(async move { running_manager.connect(request).await });
    eventually(|| fixture.stats.shells.load(Ordering::SeqCst) == 1).await;
    manager
        .cancel(&connection.connection_id, &request_id)
        .unwrap();
    assert!(matches!(running.await.unwrap(), Err(Error::Cancelled)));
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
    let request = open(&connection.connection_id);
    let request_id = request.request_id.clone();
    let terminal = manager.connect(request).await.unwrap();
    manager
        .cancel(&connection.connection_id, &request_id)
        .unwrap();
    assert_eq!(
        manager.connected(&terminal.session_id).disconnect_reason,
        Some(DisconnectReason::ClientClosed)
    );
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
    manager.shutdown().await;
}

#[tokio::test]
async fn dropping_connect_future_closes_partially_opened_channel() {
    let fixture = Fixture::new(Spec {
        shell_delay: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let running_manager = manager.clone();
    let request = open(&connection.connection_id);
    let running = tokio::spawn(async move { running_manager.connect(request).await });
    eventually(|| fixture.stats.shells.load(Ordering::SeqCst) == 1).await;
    running.abort();
    let _ = running.await;
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
    manager.shutdown().await;
}

#[tokio::test]
async fn new_long_poll_replaces_old_poll_and_quiet_poll_times_out() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings {
        poll_wait: Duration::from_millis(120),
        ..Default::default()
    });
    let terminal = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    output_contains(&manager, &terminal.session_id, "ready").await;
    let waiting_manager = manager.clone();
    let id = terminal.session_id.clone();
    let pending = tokio::spawn(async move { waiting_manager.read(&id).await });
    tokio::time::sleep(Duration::from_millis(20)).await;
    let result = manager.read(&terminal.session_id).await;
    assert_eq!(pending.await.unwrap().status, "REPLACED");
    assert_eq!(result.status, "TIMEOUT");
    assert!(result.timeout && result.connected);
    manager.close(&terminal.session_id);
    let result = manager.read(&terminal.session_id).await;
    assert_eq!(
        result.disconnect_reason,
        Some(DisconnectReason::ClientClosed)
    );
    assert!(!result.reconnect_allowed);
    manager.shutdown().await;
}

#[tokio::test]
async fn output_is_bounded_and_idle_sessions_close_without_reconnection() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings {
        poll_wait: Duration::from_millis(100),
        idle_timeout: Duration::from_millis(200),
        buffer_bytes: 128,
        ..Default::default()
    });
    let terminal = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    output_contains(&manager, &terminal.session_id, "ready").await;
    manager
        .write(&terminal.session_id, "__burst__".into())
        .await
        .unwrap();
    let output = manager.read(&terminal.session_id).await;
    assert_eq!(output.status, "DATA");
    assert!(output.buffer_overflow);
    assert!(output.output.len() <= 128);
    eventually(|| !manager.connected(&terminal.session_id).connected).await;
    let result = manager.read(&terminal.session_id).await;
    assert_eq!(
        result.disconnect_reason,
        Some(DisconnectReason::IdleTimeout)
    );
    assert!(!result.reconnect_allowed);
    manager.shutdown().await;
}

#[tokio::test]
async fn remote_transport_disconnect_updates_all_windows_and_new_attempt_reconnects() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let first = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    let second = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    output_contains(&manager, &first.session_id, "ready").await;
    output_contains(&manager, &second.session_id, "ready").await;
    manager
        .write(&first.session_id, "__disconnect__".into())
        .await
        .unwrap();
    eventually(|| {
        !manager.connected(&first.session_id).connected
            && !manager.connected(&second.session_id).connected
    })
    .await;
    let result = manager.read(&first.session_id).await;
    assert_eq!(
        result.disconnect_reason,
        Some(DisconnectReason::ChannelDisconnected)
    );
    assert!(result.reconnect_allowed);
    let reopened = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    assert_ne!(reopened.session_id, first.session_id);
    assert!(manager.connected(&reopened.session_id).connected);
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 2);
    manager.shutdown().await;
}

#[tokio::test]
async fn rejected_pty_or_shell_does_not_publish_a_session() {
    for spec in [
        Spec {
            reject_pty: true,
            ..Default::default()
        },
        Spec {
            reject_shell: true,
            ..Default::default()
        },
    ] {
        let fixture = Fixture::new(spec).await;
        let (manager, connection) = fixture.manager(TerminalSettings::default());
        assert!(matches!(
            manager.connect(open(&connection.connection_id)).await,
            Err(Error::Ssh)
        ));
        eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
        manager
            .delete_connection(connection.connection_id)
            .await
            .unwrap();
        manager.shutdown().await;
    }
}

#[tokio::test]
async fn changed_configuration_applies_to_new_windows_without_disconnecting_existing_ones() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let terminal = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    let mut update = fixture.request(false);
    update.connection_id = Some(connection.connection_id.clone());
    update.password = Some("incorrect".into());
    manager.update_connection(update).await.unwrap();
    assert!(matches!(
        manager.connect(open(&connection.connection_id)).await,
        Err(Error::Authentication)
    ));
    assert!(manager.connected(&terminal.session_id).connected);
    manager
        .write(&terminal.session_id, "still-alive".into())
        .await
        .unwrap();
    output_contains(&manager, &terminal.session_id, "still-alive").await;
    manager.shutdown().await;
}

#[tokio::test]
async fn stalled_ssh_handshake_is_bounded_by_connection_timeout() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let store = Arc::new(Store::in_memory().unwrap());
    let connection = store
        .save(
            shellpilot_server::model::ConnectionRequest {
                connection_name: "stall".into(),
                host: "127.0.0.1".into(),
                port: listener.local_addr().unwrap().port(),
                username: "tester".into(),
                password: Some("test-password".into()),
                strict_host_key_check: Some(false),
                connect_timeout: Some(1),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    let manager = TerminalManager::new(store);
    let started = std::time::Instant::now();
    assert!(matches!(
        manager.connect(open(&connection.connection_id)).await,
        Err(Error::ConnectTimeout)
    ));
    assert!(started.elapsed() < Duration::from_secs(3));
    manager.shutdown().await;
}

#[tokio::test]
async fn unknown_closed_sessions_and_duplicate_request_ids_have_explicit_states() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let missing = manager.read("unknown").await;
    assert_eq!(
        missing.disconnect_reason,
        Some(DisconnectReason::SessionNotFound)
    );
    assert!(!missing.reconnect_allowed);
    assert!(matches!(
        manager.write("unknown", "x".into()).await,
        Err(Error::TerminalMissing)
    ));
    let request = open(&connection.connection_id);
    let request_id = request.request_id.clone();
    let terminal = manager.connect(request).await.unwrap();
    let mut duplicate = open(&connection.connection_id);
    duplicate.request_id = request_id;
    assert!(matches!(
        manager.connect(duplicate).await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        manager.resize(&terminal.session_id, 0, 24).await,
        Err(Error::Invalid(_))
    ));
    manager.close(&terminal.session_id);
    manager.close(&terminal.session_id);
    manager.shutdown().await;
}

#[tokio::test]
async fn cancel_before_channel_confirmation_keeps_other_windows_alive() {
    let fixture = Fixture::new(Spec {
        open_delay: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let first = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    output_contains(&manager, &first.session_id, "ready").await;
    let request = open(&connection.connection_id);
    let request_id = request.request_id.clone();
    let connecting = manager.clone();
    let pending = tokio::spawn(async move { connecting.connect(request).await });
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 2).await;
    manager
        .cancel(&connection.connection_id, &request_id)
        .unwrap();
    assert!(matches!(pending.await.unwrap(), Err(Error::Cancelled)));
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 1).await;
    assert!(manager.connected(&first.session_id).connected);
    manager
        .write(&first.session_id, "first-is-alive".into())
        .await
        .unwrap();
    output_contains(&manager, &first.session_id, "first-is-alive").await;
    manager.shutdown().await;
}

#[tokio::test]
async fn timeout_during_key_exchange_closes_the_actual_tcp_socket() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let peer = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        socket.write_all(b"SSH-2.0-stalled-kex\r\n").await.unwrap();
        let mut buffer = [0; 4096];
        loop {
            match socket.read(&mut buffer).await {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
        }
    });
    let store = Arc::new(Store::in_memory().unwrap());
    let connection = store
        .save(
            shellpilot_server::model::ConnectionRequest {
                connection_name: "stall-kex".into(),
                host: "127.0.0.1".into(),
                port,
                username: "tester".into(),
                password: Some("test-password".into()),
                strict_host_key_check: Some(false),
                connect_timeout: Some(1),
                ..Default::default()
            },
            false,
        )
        .unwrap();
    let manager = TerminalManager::new(store);
    assert!(matches!(
        manager.connect(open(&connection.connection_id)).await,
        Err(Error::ConnectTimeout)
    ));
    tokio::time::timeout(Duration::from_secs(2), peer)
        .await
        .expect("TCP socket leaked after timeout")
        .unwrap();
    manager.shutdown().await;
}

#[tokio::test]
async fn remote_channel_eof_only_disconnects_its_own_window() {
    let fixture = Fixture::new(Spec::default()).await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let first = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    let second = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    output_contains(&manager, &first.session_id, "ready").await;
    output_contains(&manager, &second.session_id, "ready").await;
    manager
        .write(&first.session_id, "__channel_close__".into())
        .await
        .unwrap();
    eventually(|| !manager.connected(&first.session_id).connected).await;
    assert!(manager.connected(&second.session_id).connected);
    assert_eq!(
        manager.connected(&first.session_id).disconnect_reason,
        Some(DisconnectReason::ChannelDisconnected)
    );
    manager.shutdown().await;
}

#[tokio::test]
async fn all_terminal_http_endpoints_match_existing_client_payloads() {
    use axum::{
        body::{to_bytes, Body},
        http::{Request, StatusCode},
        Router,
    };
    use serde_json::{json, Value};
    use tower::ServiceExt;
    async fn call(
        app: &Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json")
            .body(
                body.map(|v| Body::from(v.to_string()))
                    .unwrap_or_else(Body::empty),
            )
            .unwrap();
        let response = tokio::time::timeout(Duration::from_secs(3), app.clone().oneshot(request))
            .await
            .unwrap()
            .unwrap();
        let status = response.status();
        let json =
            serde_json::from_slice(&to_bytes(response.into_body(), 1024 * 1024).await.unwrap())
                .unwrap();
        (status, json)
    }
    let fixture = Fixture::new(Spec::default()).await;
    let (store, connection) = fixture.saved(false);
    let app = shellpilot_server::router(store);
    let request_id = uuid::Uuid::new_v4().to_string();
    let (status, connected) = call(&app, "POST", "/api/v1/ssh/terminal/connect",
        Some(json!({"connectionId":connection.connection_id, "requestId":request_id,"cols":120,"rows":24}))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(connected["code"], "SUCCESS_0000");
    assert_eq!(connected["data"]["connectionId"], connection.connection_id);
    assert!(connected["data"]["initialOutput"].is_string());
    let id = connected["data"]["sessionId"].as_str().unwrap();
    let (_, state) = call(
        &app,
        "GET",
        &format!("/api/v1/ssh/terminal/connected?sessionId={id}"),
        None,
    )
    .await;
    assert_eq!(state["data"]["connected"], true);
    let (resource_status, resources) = call(
        &app,
        "GET",
        &format!("/api/v1/ssh/terminal/resources?sessionId={id}"),
        None,
    )
    .await;
    assert_eq!(resource_status, StatusCode::OK);
    assert_eq!(resources["code"], "SUCCESS_0000");
    assert_eq!(resources["data"]["supported"], true);
    assert_eq!(resources["data"]["memoryUsedBytes"], 614400);
    let (_, result) = call(
        &app,
        "POST",
        "/api/v1/ssh/terminal/write",
        Some(json!({"sessionId":id,"input":"http-echo"})),
    )
    .await;
    assert_eq!(result["code"], "SUCCESS_0000");
    let mut output = connected["data"]["initialOutput"]
        .as_str()
        .unwrap()
        .to_string();
    while !output.contains("http-echo") {
        let (_, read) = call(
            &app,
            "GET",
            &format!("/api/v1/ssh/terminal/read?sessionId={id}"),
            None,
        )
        .await;
        assert_eq!(read["data"]["status"], "DATA");
        assert!(read["data"]["hasData"].as_bool().unwrap());
        assert!(read["data"]["bufferOverflow"].is_boolean());
        output.push_str(read["data"]["output"].as_str().unwrap());
    }
    let (_, result) = call(
        &app,
        "POST",
        "/api/v1/ssh/terminal/resize",
        Some(json!({"sessionId":id,"cols":88,"rows":30})),
    )
    .await;
    assert_eq!(result["code"], "SUCCESS_0000");
    let (status, _) = call(
        &app,
        "POST",
        &format!(
            "/api/v1/ssh/delete_connection?connectionId={}",
            connection.connection_id
        ),
        None,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (_, result) = call(
        &app,
        "POST",
        &format!(
            "/api/v1/ssh/terminal/cancel_connect?connectionId={}&requestId={request_id}",
            connection.connection_id
        ),
        None,
    )
    .await;
    assert_eq!(result["code"], "SUCCESS_0000");
    let (_, result) = call(
        &app,
        "POST",
        &format!("/api/v1/ssh/terminal/close?sessionId={id}"),
        None,
    )
    .await;
    assert_eq!(result["code"], "SUCCESS_0000");
    let (_, state) = call(
        &app,
        "GET",
        &format!("/api/v1/ssh/terminal/connected?sessionId={id}"),
        None,
    )
    .await;
    assert_eq!(state["data"]["disconnectReason"], "CLIENT_CLOSED");
    assert_eq!(state["data"]["reconnectAllowed"], false);
    drop(app);
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
}

#[tokio::test]
async fn shutdown_wakes_long_poll_and_cancels_pending_connections() {
    let fixture = Fixture::new(Spec {
        open_delay: Duration::from_millis(200),
        ..Default::default()
    })
    .await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let first = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    output_contains(&manager, &first.session_id, "ready").await;
    let reading = manager.clone();
    let id = first.session_id.clone();
    let poll = tokio::spawn(async move { reading.read(&id).await });
    let connecting = manager.clone();
    let request = open(&connection.connection_id);
    let pending = tokio::spawn(async move { connecting.connect(request).await });
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 2).await;
    tokio::time::timeout(Duration::from_secs(1), manager.shutdown())
        .await
        .unwrap();
    assert!(matches!(pending.await.unwrap(), Err(Error::Cancelled)));
    let result = tokio::time::timeout(Duration::from_secs(1), poll)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        result.disconnect_reason,
        Some(DisconnectReason::ClientClosed)
    );
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 0).await;
}

#[tokio::test]
async fn pooled_connections_open_twenty_four_windows_under_openssh_ten_session_limit() {
    let fixture = Fixture::new(Spec {
        max_sessions: Some(10),
        ..Default::default()
    })
    .await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    let mut windows = Vec::new();
    for index in 0..24 {
        let opened = manager
            .connect(open(&connection.connection_id))
            .await
            .unwrap();
        manager
            .write(&opened.session_id, format!("window-{index}\n"))
            .await
            .unwrap();
        output_contains(&manager, &opened.session_id, &format!("window-{index}")).await;
        windows.push(opened);
    }
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.stats.rejected_channels.load(Ordering::SeqCst), 0);
    assert!(windows
        .iter()
        .all(|window| manager.connected(&window.session_id).connected));
    manager.close(&windows[0].session_id);
    eventually(|| fixture.stats.channels.load(Ordering::SeqCst) == 23).await;
    let replacement = manager
        .connect(open(&connection.connection_id))
        .await
        .unwrap();
    assert!(manager.connected(&replacement.session_id).connected);
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 3);
    manager.shutdown().await;
    eventually(|| fixture.stats.live_connections.load(Ordering::SeqCst) == 0).await;
}

#[tokio::test]
async fn lower_server_limits_are_learned_without_reexecuting_a_startup_command() {
    let fixture = Fixture::new(Spec {
        max_sessions: Some(2),
        ..Default::default()
    })
    .await;
    let (store, mut connection) = fixture.saved(false);
    let mut updated = fixture.request(false);
    updated.connection_id = Some(connection.connection_id.clone());
    updated.startup_command = Some("startup-once".into());
    connection = store.save(updated, true).unwrap();
    let manager = TerminalManager::new(store);
    let mut windows = Vec::new();
    for _ in 0..7 {
        let opened = manager
            .connect(open(&connection.connection_id))
            .await
            .unwrap();
        let mut output = opened.initial_output.clone();
        output.push_str(&output_contains(&manager, &opened.session_id, "startup-once").await);
        assert_eq!(output.matches("startup-once").count(), 1);
        windows.push(opened);
    }
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 4);
    assert_eq!(fixture.stats.rejected_channels.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.stats.shells.load(Ordering::SeqCst), 7);
    manager
        .write(&windows[0].session_id, "still-usable".into())
        .await
        .unwrap();
    output_contains(&manager, &windows[0].session_id, "still-usable").await;
    manager.shutdown().await;
    eventually(|| fixture.stats.live_connections.load(Ordering::SeqCst) == 0).await;
}

#[tokio::test]
async fn pooled_transport_failure_is_isolated_and_pool_limit_is_explicit() {
    let fixture = Fixture::new(Spec {
        max_sessions: Some(10),
        ..Default::default()
    })
    .await;
    let (manager, connection) = fixture.manager(TerminalSettings {
        channels_per_transport: 2,
        transports_per_connection: 2,
        ..Default::default()
    });
    let mut windows = Vec::new();
    for _ in 0..4 {
        windows.push(
            manager
                .connect(open(&connection.connection_id))
                .await
                .unwrap(),
        );
    }
    assert!(matches!(
        manager.connect(open(&connection.connection_id)).await,
        Err(Error::Busy)
    ));
    manager
        .write(&windows[0].session_id, "__disconnect__".into())
        .await
        .unwrap();
    eventually(|| {
        !manager.connected(&windows[0].session_id).connected
            && !manager.connected(&windows[1].session_id).connected
    })
    .await;
    assert!(manager.connected(&windows[2].session_id).connected);
    manager
        .write(&windows[2].session_id, "other-transport".into())
        .await
        .unwrap();
    output_contains(&manager, &windows[2].session_id, "other-transport").await;
    assert!(manager
        .connect(open(&connection.connection_id))
        .await
        .is_ok());
    manager.shutdown().await;
}

#[tokio::test]
async fn server_rejecting_all_channels_returns_specific_error_with_no_reauthentication_loop() {
    let fixture = Fixture::new(Spec {
        max_sessions: Some(0),
        ..Default::default()
    })
    .await;
    let (manager, connection) = fixture.manager(TerminalSettings::default());
    assert!(matches!(
        manager.connect(open(&connection.connection_id)).await,
        Err(Error::ChannelRejected)
    ));
    assert_eq!(fixture.stats.connections.load(Ordering::SeqCst), 1);
    manager.shutdown().await;
    eventually(|| fixture.stats.live_connections.load(Ordering::SeqCst) == 0).await;
}
