use russh::{
    keys::{key::safe_rng, Algorithm, PrivateKey, PublicKey},
    server::{self, Server as _, Session},
    Channel, ChannelId, Pty,
};
use shellpilot_server::{
    model::{Connection, ConnectionRequest},
    store::Store,
    terminal::{OpenRequest, TerminalManager, TerminalSettings},
};
use std::{
    collections::HashSet,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Stats {
    pub connections: AtomicUsize,
    pub live_connections: AtomicUsize,
    pub channels: AtomicUsize,
    pub shells: AtomicUsize,
    pub sizes: Mutex<Vec<(u32, u32)>>,
    pub rejected_channels: AtomicUsize,
    pub execs: Mutex<Vec<String>>,
}
#[derive(Clone, Default)]
pub struct Spec {
    pub shell_delay: Duration,
    pub open_delay: Duration,
    pub reject_pty: bool,
    pub reject_shell: bool,
    pub max_sessions: Option<usize>,
}
struct TestServer {
    stats: Arc<Stats>,
    spec: Spec,
    client_key: PublicKey,
}
impl server::Server for TestServer {
    type Handler = TestHandler;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> TestHandler {
        self.stats.connections.fetch_add(1, Ordering::SeqCst);
        self.stats.live_connections.fetch_add(1, Ordering::SeqCst);
        TestHandler {
            stats: self.stats.clone(),
            spec: self.spec.clone(),
            client_key: self.client_key.clone(),
            channels: HashSet::new(),
        }
    }
}
struct TestHandler {
    stats: Arc<Stats>,
    spec: Spec,
    client_key: PublicKey,
    channels: HashSet<ChannelId>,
}
impl Drop for TestHandler {
    fn drop(&mut self) {
        self.stats
            .channels
            .fetch_sub(self.channels.len(), Ordering::SeqCst);
        self.stats.live_connections.fetch_sub(1, Ordering::SeqCst);
    }
}
impl server::Handler for TestHandler {
    type Error = russh::Error;
    async fn auth_password(
        &mut self,
        user: &str,
        password: &str,
    ) -> Result<server::Auth, Self::Error> {
        Ok(if user == "tester" && password == "test-password" {
            server::Auth::Accept
        } else {
            server::Auth::reject()
        })
    }
    async fn auth_publickey(
        &mut self,
        user: &str,
        key: &PublicKey,
    ) -> Result<server::Auth, Self::Error> {
        Ok(
            if user == "tester" && key.key_data() == self.client_key.key_data() {
                server::Auth::Accept
            } else {
                server::Auth::reject()
            },
        )
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: server::ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        if self
            .spec
            .max_sessions
            .is_some_and(|limit| self.channels.len() >= limit)
        {
            self.stats.rejected_channels.fetch_add(1, Ordering::SeqCst);
            reply
                .reject(russh::ChannelOpenFailure::AdministrativelyProhibited)
                .await;
            return Ok(());
        }
        self.channels.insert(channel.id());
        self.stats.channels.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.spec.open_delay).await;
        reply.accept().await;
        Ok(())
    }
    async fn channel_close(
        &mut self,
        channel: ChannelId,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.channels.remove(&channel) {
            self.stats.channels.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        channel: ChannelId,
        _: &str,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stats.sizes.lock().unwrap().push((cols, rows));
        if self.spec.reject_pty {
            session.channel_failure(channel)?;
        } else {
            session.channel_success(channel)?;
        }
        Ok(())
    }
    async fn shell_request(
        &mut self,
        channel: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stats.shells.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.spec.shell_delay).await;
        if self.spec.reject_shell {
            session.channel_failure(channel)?;
        } else {
            session.channel_success(channel)?;
            session.data(channel, b"ready\r\n".to_vec())?;
        }
        Ok(())
    }
    async fn window_change_request(
        &mut self,
        _: ChannelId,
        cols: u32,
        rows: u32,
        _: u32,
        _: u32,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stats.sizes.lock().unwrap().push((cols, rows));
        Ok(())
    }
    async fn exec_request(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(data).into_owned();
        self.stats.execs.lock().unwrap().push(command.clone());
        if command == "__exec_reject__" {
            session.channel_failure(channel)?;
            if self.channels.remove(&channel) {
                self.stats.channels.fetch_sub(1, Ordering::SeqCst);
            }
            session.close(channel)?;
            return Ok(());
        }
        session.channel_success(channel)?;
        if command == "__exec_quiet__" {
            return Ok(());
        }
        let stdout = if command == "__exec_large__" {
            vec![b'x'; 4096]
        } else {
            "执行完成\n".as_bytes().to_vec()
        };
        // Split a multi-byte code point across SSH data messages.
        session.data(channel, stdout[..2].to_vec())?;
        session.data(channel, stdout[2..].to_vec())?;
        session.extended_data(channel, 1, b"test stderr\n".to_vec())?;
        if command != "__exec_no_status__" {
            session.exit_status_request(channel, if command == "__exec_fail__" { 7 } else { 0 })?;
        }
        session.eof(channel)?;
        if self.channels.remove(&channel) {
            self.stats.channels.fetch_sub(1, Ordering::SeqCst);
        }
        session.close(channel)?;
        Ok(())
    }
    async fn data(
        &mut self,
        channel: ChannelId,
        data: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        match data {
            b"__disconnect__" => return Err(russh::Error::Disconnect),
            b"__channel_close__" => {
                session.eof(channel)?;
                session.close(channel)?;
            }
            b"__burst__" => {
                session.data(channel, vec![b'x'; 4096])?;
            }
            b"__unicode__" => {
                let bytes = "你好世界\r\n".as_bytes();
                session.data(channel, bytes[..2].to_vec())?;
                session.data(channel, bytes[2..].to_vec())?;
            }
            b"__quiet__" => {}
            _ => {
                session.data(channel, data.to_vec())?;
            }
        }
        Ok(())
    }
}

pub struct Fixture {
    pub port: u16,
    pub host_key: PrivateKey,
    pub client_key: PrivateKey,
    pub stats: Arc<Stats>,
    stop: CancellationToken,
    _task: tokio::task::JoinHandle<()>,
}
impl Fixture {
    pub async fn new(spec: Spec) -> Self {
        Self::with_algorithms(spec, Algorithm::Ed25519, Algorithm::Ed25519).await
    }
    pub async fn with_algorithms(
        spec: Spec,
        host_algorithm: Algorithm,
        client_algorithm: Algorithm,
    ) -> Self {
        let host_key = PrivateKey::random(&mut safe_rng(), host_algorithm).unwrap();
        let client_key = PrivateKey::random(&mut safe_rng(), client_algorithm).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let stats = Arc::new(Stats::default());
        let mut server = TestServer {
            stats: stats.clone(),
            spec,
            client_key: client_key.public_key().clone(),
        };
        let config = Arc::new(server::Config {
            keys: vec![host_key.clone()],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        let task = tokio::spawn(async move {
            let running = server.run_on_socket(config, &listener);
            let handle = running.handle();
            tokio::spawn(async move {
                stopping.cancelled().await;
                handle.shutdown("test done".into());
            });
            let _ = running.await;
        });
        Self {
            port,
            host_key,
            client_key,
            stats,
            stop,
            _task: task,
        }
    }
    pub fn request(&self, key_auth: bool) -> ConnectionRequest {
        ConnectionRequest {
            connection_name: "SSH protocol test".into(),
            host: "127.0.0.1".into(),
            port: self.port,
            username: "tester".into(),
            auth_type: Some(if key_auth { 2 } else { 1 }),
            user_id: Some("default".into()),
            password: (!key_auth).then(|| "test-password".into()),
            private_key: key_auth.then(|| {
                self.client_key
                    .to_openssh(russh::keys::ssh_key::LineEnding::LF)
                    .unwrap()
                    .to_string()
            }),
            strict_host_key_check: Some(true),
            known_hosts: Some(format!(
                "[127.0.0.1]:{} {}",
                self.port,
                self.host_key.public_key().to_openssh().unwrap()
            )),
            ..Default::default()
        }
    }
    pub fn saved(&self, key_auth: bool) -> (Arc<Store>, Connection) {
        let store = Arc::new(Store::in_memory().unwrap());
        let connection = store.save(self.request(key_auth), false).unwrap();
        (store, connection)
    }
    pub fn manager(&self, settings: TerminalSettings) -> (Arc<TerminalManager>, Connection) {
        let (store, connection) = self.saved(false);
        (
            Arc::new(TerminalManager::with_settings(store, settings)),
            connection,
        )
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}

pub fn open(id: &str) -> OpenRequest {
    OpenRequest {
        connection_id: id.into(),
        request_id: uuid::Uuid::new_v4().to_string(),
        cols: Some(120),
        rows: Some(24),
    }
}
pub async fn eventually(mut predicate: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(3), async {
        while !predicate() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition did not become true");
}
pub async fn output_contains(manager: &TerminalManager, id: &str, expected: &str) -> String {
    tokio::time::timeout(Duration::from_secs(3), async {
        let mut output = String::new();
        loop {
            let result = manager.read(id).await;
            output.push_str(&result.output);
            if output.contains(expected) {
                return output;
            }
            assert!(
                result.connected,
                "terminal disconnected before expected data"
            );
        }
    })
    .await
    .expect("expected terminal output did not arrive")
}
