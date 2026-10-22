#![cfg(windows)]
#[path = "sftp_support/mod.rs"]
#[allow(dead_code)]
mod support;
use shellpilot_server::{
    model::ConnectionRequest,
    network,
    sftp::{model::OpenRequest, Manager},
    terminal::TerminalManager,
};
use std::{sync::atomic::Ordering, time::Duration};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

#[tokio::test]
async fn ssh_login_and_independent_sftp_use_proxy_and_setting_changes_do_not_reuse_old_routes() {
    tokio::time::timeout(Duration::from_secs(20), async {
        let fixture=support::Fixture::new().await;fixture.file("/home/proxy.txt",b"fixture");
        let (store,previous)=fixture.saved(false);
        let connection=store.save(ConnectionRequest {connection_id:Some(previous.connection_id),connection_name:"proxy fixture".into(),host:"ssh.example.invalid".into(),port:fixture.port,username:"tester".into(),known_hosts:Some(format!("[ssh.example.invalid]:{} {}",fixture.port,fixture.host.public_key().to_openssh().unwrap())),..Default::default()},true).unwrap();
        let listener=TcpListener::bind("127.0.0.1:0").await.unwrap();let proxy_port=listener.local_addr().unwrap().port();let ssh_port=fixture.port;
        let forwarding=tokio::spawn(async move {
            loop {let (mut socket,_)=listener.accept().await.unwrap();tokio::spawn(async move {
                let mut header=Vec::new();while !header.ends_with(b"\r\n\r\n"){assert!(header.len()<8192);header.push(socket.read_u8().await.unwrap());}
                assert!(String::from_utf8(header).unwrap().starts_with(&format!("CONNECT ssh.example.invalid:{ssh_port} HTTP/1.1")));
                let mut target=TcpStream::connect(("127.0.0.1",ssh_port)).await.unwrap();socket.write_all(b"HTTP/1.1 200 OK\r\n\r\n").await.unwrap();let _=tokio::io::copy_bidirectional(&mut socket,&mut target).await;
            });}
        });
        let settings=|port|serde_json::from_value(serde_json::json!({"ai":{"mode":"direct","protocol":"http","host":"","port":0},"ssh":{"mode":"custom","protocol":"http","host":"127.0.0.1","port":port,"bypass":["ssh.example.invalid:22"]}})).unwrap();
        network::save(&store,settings(proxy_port)).unwrap();
        let terminals=TerminalManager::new(store.clone());
        let open=||shellpilot_server::terminal::OpenRequest{connection_id:connection.connection_id.clone(),request_id:uuid::Uuid::new_v4().to_string(),cols:None,rows:None};
        let first=terminals.connect(open()).await.unwrap();let second=terminals.connect(open()).await.unwrap();
        assert_eq!(fixture.stats.connections.load(Ordering::SeqCst),1);
        let files=Manager::new(store.clone());let session=files.open(OpenRequest{connection_id:connection.connection_id.clone(),root_path:Some("/home".into()),..Default::default()}).await.unwrap();
        assert!(files.entries(&session.sftp_session_id,None).await.unwrap().iter().any(|e|e.name=="proxy.txt"));
        let closed=TcpListener::bind("127.0.0.1:0").await.unwrap();let closed_port=closed.local_addr().unwrap().port();drop(closed);
        network::save(&store,settings(closed_port)).unwrap();assert!(terminals.connect(open()).await.is_err());
        assert!(terminals.connected(&first.session_id).connected);assert!(terminals.connected(&second.session_id).connected);
        assert!(files.entries(&session.sftp_session_id,None).await.is_ok());
        files.shutdown();terminals.close(&first.session_id);terminals.close(&second.session_id);forwarding.abort();
    }).await.unwrap();
}
