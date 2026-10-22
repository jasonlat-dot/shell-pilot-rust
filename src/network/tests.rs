use super::*;
use std::time::Duration;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
fn config(port: u16, protocol: Protocol) -> ProxyConfig {
    ProxyConfig {
        mode: Mode::Custom,
        protocol,
        host: "127.0.0.1".into(),
        port,
        username: "proxy-user".into(),
        password: "proxy-password".into(),
        bypass: Vec::new(),
    }
}
async fn header(socket: &mut TcpStream) -> String {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        assert!(bytes.len() < 8192);
        bytes.push(socket.read_u8().await.unwrap());
    }
    String::from_utf8(bytes).unwrap()
}
#[tokio::test]
async fn http_connect_preserves_ssh_banner_and_uses_only_proxy_authentication() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = config(listener.local_addr().unwrap().port(), Protocol::Http);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = header(&mut socket).await;
        assert!(request.starts_with("CONNECT remote.example:2222 HTTP/1.1\r\n"));
        assert!(request.contains("Proxy-Authorization: Basic cHJveHktdXNlcjpwcm94eS1wYXNzd29yZA=="));
        socket
            .write_all(b"HTTP/1.1 200 Connection established\r\n\r\nSSH-2.0-test\r\n")
            .await
            .unwrap();
    });
    let route = proxy.resolve("remote.example", 2222, "ssh").unwrap();
    let mut socket = tokio::time::timeout(
        Duration::from_secs(2),
        connect("remote.example", 2222, &route),
    )
    .await
    .unwrap()
    .unwrap();
    let mut banner = [0u8; 14];
    socket.read_exact(&mut banner).await.unwrap();
    assert_eq!(&banner, b"SSH-2.0-test\r\n");
    server.await.unwrap();
}
#[tokio::test]
async fn socks5_authenticates_and_sends_domain_for_proxy_side_dns() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = config(listener.local_addr().unwrap().port(), Protocol::Socks5);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        socket.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 2]);
        socket.write_all(&[5, 2]).await.unwrap();
        assert_eq!(socket.read_u8().await.unwrap(), 1);
        let mut user = vec![0; socket.read_u8().await.unwrap() as usize];
        socket.read_exact(&mut user).await.unwrap();
        assert_eq!(user, b"proxy-user");
        let mut password = vec![0; socket.read_u8().await.unwrap() as usize];
        socket.read_exact(&mut password).await.unwrap();
        assert_eq!(password, b"proxy-password");
        socket.write_all(&[1, 0]).await.unwrap();
        let mut command = [0; 4];
        socket.read_exact(&mut command).await.unwrap();
        assert_eq!(command, [5, 1, 0, 3]);
        let mut domain = vec![0; socket.read_u8().await.unwrap() as usize];
        socket.read_exact(&mut domain).await.unwrap();
        assert_eq!(domain, b"ssh.example.invalid");
        assert_eq!(socket.read_u16().await.unwrap(), 22);
        socket
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 22])
            .await
            .unwrap();
        socket.write_all(b"SSH").await.unwrap();
    });
    let mut socket = connect(
        "ssh.example.invalid",
        22,
        &proxy.resolve("ssh.example.invalid", 22, "ssh").unwrap(),
    )
    .await
    .unwrap();
    let mut bytes = [0; 3];
    socket.read_exact(&mut bytes).await.unwrap();
    assert_eq!(&bytes, b"SSH");
    server.await.unwrap();
}
#[tokio::test]
async fn model_http_builder_uses_explicit_proxy_and_loopback_always_bypasses_it() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = config(listener.local_addr().unwrap().port(), Protocol::Http);
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let request = header(&mut socket).await.to_ascii_lowercase();
        assert!(request.starts_with("get http://model.example.invalid/v1/models http/1.1"));
        assert!(request.contains("proxy-authorization: basic "));
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .await
            .unwrap();
    });
    let target = url::Url::parse("http://model.example.invalid/v1/models").unwrap();
    let client = proxy
        .http_client(reqwest::Client::builder(), &target)
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        client
            .get(target)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "OK"
    );
    server.await.unwrap();
    for host in ["localhost", "127.0.0.1", "::1", "[::1]"] {
        assert!(proxy.resolve(host, 22, "ssh").unwrap().is_none());
    }
    let direct = ProxyConfig {
        mode: Mode::Direct,
        ..proxy
    };
    assert!(direct
        .resolve("remote.example", 22, "ssh")
        .unwrap()
        .is_none());
}
#[tokio::test]
async fn rejected_connect_and_socks_auth_fail_without_direct_fallback() {
    for protocol in [Protocol::Http, Protocol::Socks5] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = config(listener.local_addr().unwrap().port(), protocol);
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            if protocol == Protocol::Http {
                let _ = header(&mut socket).await;
                socket
                    .write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\n\r\n")
                    .await
                    .unwrap();
            } else {
                let mut bytes = [0; 3];
                socket.read_exact(&mut bytes).await.unwrap();
                socket.write_all(&[5, 255]).await.unwrap();
            }
        });
        assert!(connect(
            "must-not-resolve.invalid",
            22,
            &proxy
                .resolve("must-not-resolve.invalid", 22, "ssh")
                .unwrap()
        )
        .await
        .is_err());
        server.await.unwrap();
    }
}
#[cfg(windows)]
#[test]
fn persisted_settings_keep_password_encrypted_and_never_return_it_in_views() {
    let store = Store::in_memory().unwrap();
    let request = serde_json::json!({"ai":{"mode":"custom","protocol":"http","host":"127.0.0.1","port":7890,"username":"proxy-user","password":"proxy-password"},"ssh":{"mode":"direct","protocol":"socks5","host":"","port":0,"username":""}});
    let view = serde_json::to_value(
        save(&store, serde_json::from_value(request.clone()).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(view["ai"]["passwordConfigured"], true);
    assert!(!view.to_string().contains("proxy-password"));
    assert!(view["ai"].get("password").is_none());
    let encrypted: Vec<u8> = store
        .database
        .lock()
        .unwrap()
        .query_row(
            "SELECT protected_config FROM client_network_settings WHERE id=1",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(!encrypted
        .windows(b"proxy-password".len())
        .any(|part| part == b"proxy-password"));
    let mut keep = request;
    keep["ai"].as_object_mut().unwrap().remove("password");
    save(&store, serde_json::from_value(keep.clone()).unwrap()).unwrap();
    assert_eq!(load(&store).unwrap().ai.password, "proxy-password");
    keep["ai"]["clearPassword"] = true.into();
    save(&store, serde_json::from_value(keep).unwrap()).unwrap();
    assert!(load(&store).unwrap().ai.password.is_empty());
}
#[test]
fn custom_proxy_rejects_paths_credentials_in_host_zero_port_and_control_characters() {
    let original = config(7890, Protocol::Http);
    for host in [
        "http://127.0.0.1",
        "user@host",
        "proxy/path",
        "proxy\r\nInjected: value",
        "host:7890",
    ] {
        let mut value = original.clone();
        value.host = host.into();
        assert!(value.endpoint().is_err());
    }
    let mut value = original;
    value.port = 0;
    assert!(value.endpoint().is_err());
    value.port = 7890;
    value.password = "bad\nvalue".into();
    assert!(value.endpoint().is_err());
}

#[test]
fn ipv6_proxy_address_builds_a_model_client() {
    let mut proxy = config(7890, Protocol::Http);
    proxy.host = "::1".into();
    let target = url::Url::parse("https://model.example.invalid/v1/").unwrap();
    assert!(proxy
        .http_client(reqwest::Client::builder(), &target)
        .and_then(|builder| builder.build().map_err(|_| invalid("client")))
        .is_ok());
}

#[tokio::test]
async fn model_requests_use_socks5_with_proxy_side_dns() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = ProxyConfig {
        username: String::new(),
        password: String::new(),
        ..config(listener.local_addr().unwrap().port(), Protocol::Socks5)
    };
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        socket.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 0]);
        socket.write_all(&[5, 0]).await.unwrap();
        let mut request = [0; 4];
        socket.read_exact(&mut request).await.unwrap();
        assert_eq!(request, [5, 1, 0, 3]);
        let mut domain = vec![0; socket.read_u8().await.unwrap() as usize];
        socket.read_exact(&mut domain).await.unwrap();
        assert_eq!(domain, b"model.example.invalid");
        assert_eq!(socket.read_u16().await.unwrap(), 80);
        socket
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 80])
            .await
            .unwrap();
        assert!(header(&mut socket)
            .await
            .starts_with("GET /v1/models HTTP/1.1"));
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK")
            .await
            .unwrap();
    });
    let target = url::Url::parse("http://model.example.invalid/v1/models").unwrap();
    let client = proxy
        .http_client(
            reqwest::Client::builder().timeout(Duration::from_secs(2)),
            &target,
        )
        .unwrap()
        .build()
        .unwrap();
    assert_eq!(
        client
            .get(target)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "OK"
    );
    server.await.unwrap();
}

#[test]
fn whitelist_overrides_system_and_custom_proxy_and_http_uses_effective_port() {
    for mode in [Mode::Custom, Mode::System] {
        let proxy = ProxyConfig {
            mode,
            bypass: vec!["api.example.com:443".into(), "192.168.1.10:2222".into()],
            host: "bad proxy".into(),
            ..Default::default()
        };
        assert!(proxy
            .resolve("192.168.1.10", 2222, "ssh")
            .unwrap()
            .is_none());
        assert!(proxy
            .http_client(
                reqwest::Client::builder(),
                &url::Url::parse("https://api.example.com/v1/").unwrap()
            )
            .is_ok());
        if mode == Mode::Custom {
            assert!(proxy.resolve("192.168.1.10", 22, "ssh").is_err());
            assert!(proxy
                .http_client(
                    reqwest::Client::builder(),
                    &url::Url::parse("https://api.example.com:8443/v1/").unwrap()
                )
                .is_err());
            assert!(proxy
                .http_client(
                    reqwest::Client::builder(),
                    &url::Url::parse("http://api.example.com/v1/").unwrap()
                )
                .is_err());
        }
    }
}

#[cfg(windows)]
#[test]
fn whitelist_persistence_is_independent_and_old_config_defaults_to_empty_rules() {
    let store = Store::in_memory().unwrap();
    let old = serde_json::json!({"ai":{"mode":"custom","protocol":"http","host":"127.0.0.1","port":7890,"username":"","password":""},"ssh":{"mode":"direct","protocol":"http","host":"","port":0,"username":"","password":""}});
    let encrypted = secrets::protect(&old.to_string(), ENTROPY).unwrap();
    store
        .database
        .lock()
        .unwrap()
        .execute(
            "INSERT INTO client_network_settings VALUES(1,?1)",
            [encrypted],
        )
        .unwrap();
    assert!(load(&store).unwrap().ai.bypass.is_empty());
    let mut update = old;
    update["ai"].as_object_mut().unwrap().remove("password");
    update["ssh"].as_object_mut().unwrap().remove("password");
    update["ai"]["bypass"] = serde_json::json!([" API.Example.com:443 ", "api.example.com:443"]);
    update["ssh"]["bypass"] = serde_json::json!(["192.168.1.10:22"]);
    let saved = serde_json::to_value(
        save(&store, serde_json::from_value(update.clone()).unwrap()).unwrap(),
    )
    .unwrap();
    assert_eq!(
        saved["ai"]["bypass"],
        serde_json::json!(["api.example.com:443"])
    );
    assert_eq!(load(&store).unwrap().ssh.bypass, ["192.168.1.10:22"]);
    update["ai"]["bypass"] = serde_json::json!(["host:99999"]);
    assert!(save(&store, serde_json::from_value(update.clone()).unwrap()).is_err());
    assert_eq!(load(&store).unwrap().ai.bypass, ["api.example.com:443"]);
    update["ai"]["bypass"] = serde_json::json!([]);
    save(&store, serde_json::from_value(update).unwrap()).unwrap();
    assert!(load(&store).unwrap().ai.bypass.is_empty());
    assert_eq!(load(&store).unwrap().ssh.bypass, ["192.168.1.10:22"]);
}

#[tokio::test]
async fn http_whitelist_sends_matching_requests_direct_and_other_ports_to_proxy() {
    let direct = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = direct.local_addr().unwrap();
    let remote = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy_address = remote.local_addr().unwrap();
    let direct_server = tokio::spawn(async move {
        let (mut socket, _) = direct.accept().await.unwrap();
        assert!(header(&mut socket)
            .await
            .starts_with("GET /v1/models HTTP/1.1"));
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\nDIRECT")
            .await
            .unwrap();
    });
    let proxy_server = tokio::spawn(async move {
        let (mut socket, _) = remote.accept().await.unwrap();
        assert!(header(&mut socket)
            .await
            .starts_with("GET http://service.example.invalid:"));
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 5\r\nConnection: close\r\n\r\nPROXY")
            .await
            .unwrap();
    });
    let target = url::Url::parse(&format!(
        "http://service.example.invalid:{}/v1/models",
        address.port()
    ))
    .unwrap();
    let mut proxy = config(proxy_address.port(), Protocol::Http);
    proxy.bypass = vec![format!("service.example.invalid:{}", address.port())];
    let request = |proxy: &ProxyConfig| {
        proxy
            .http_client(
                reqwest::Client::builder()
                    .timeout(Duration::from_secs(2))
                    .resolve("service.example.invalid", address),
                &target,
            )
            .unwrap()
            .build()
            .unwrap()
    };
    assert_eq!(
        request(&proxy)
            .get(target.clone())
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "DIRECT"
    );
    proxy.bypass = vec![format!(
        "service.example.invalid:{}",
        if address.port() == 65535 {
            65534
        } else {
            address.port() + 1
        }
    )];
    assert_eq!(
        request(&proxy)
            .get(target)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        "PROXY"
    );
    direct_server.await.unwrap();
    proxy_server.await.unwrap();
}
