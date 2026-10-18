use russh::{
    keys::{key::safe_rng, Algorithm, PrivateKey, PublicKey},
    server::{self, ChannelOpenHandle, Server as _, Session},
    Channel, ChannelId, Pty,
};
use russh_sftp::protocol::{self as p, FileAttributes, OpenFlags, StatusCode};
use shellpilot_server::{
    model::{Connection, ConnectionRequest},
    sftp::{model::OpenRequest, Manager, Settings},
    store::Store,
};
use std::{
    collections::{HashMap, HashSet},
    path::{Component, PathBuf},
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::{Duration, UNIX_EPOCH},
};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub struct Stats {
    pub channels: AtomicUsize,
    pub live: AtomicUsize,
    pub connections: AtomicUsize,
    pub root_commands: Mutex<Vec<String>>,
    pub password_prompts: AtomicUsize,
}
#[derive(Clone, Copy, Default)]
pub enum RootAccess {
    #[default]
    Denied,
    Passwordless,
    Password,
}
pub struct Fixture {
    pub root: tempfile::TempDir,
    pub port: u16,
    pub host: PrivateKey,
    pub client: PrivateKey,
    pub stats: Arc<Stats>,
    pub delay: Arc<Mutex<Duration>>,
    pub root_access: Arc<Mutex<RootAccess>>,
    stop: CancellationToken,
}
impl Fixture {
    pub async fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("home")).unwrap();
        std::fs::create_dir(root.path().join("root")).unwrap();
        let host = PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).unwrap();
        let client = PrivateKey::random(&mut safe_rng(), Algorithm::Ed25519).unwrap();
        let stats = Arc::new(Stats::default());
        let delay = Arc::new(Mutex::new(Duration::ZERO));
        let root_access = Arc::new(Mutex::new(RootAccess::default()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let mut server = Server {
            root: Arc::new(root.path().to_path_buf()),
            stats: stats.clone(),
            delay: delay.clone(),
            client: client.public_key().clone(),
            root_access: root_access.clone(),
        };
        let config = Arc::new(server::Config {
            keys: vec![host.clone()],
            auth_rejection_time: Duration::ZERO,
            auth_rejection_time_initial: Some(Duration::ZERO),
            ..Default::default()
        });
        let stop = CancellationToken::new();
        let stopping = stop.clone();
        tokio::spawn(async move {
            let running = server.run_on_socket(config, &listener);
            let handle = running.handle();
            tokio::spawn(async move {
                stopping.cancelled().await;
                handle.shutdown("done".into());
            });
            let _ = running.await;
        });
        Self {
            root,
            port,
            host,
            client,
            stats,
            delay,
            root_access,
            stop,
        }
    }
    pub fn saved(&self, key: bool) -> (Arc<Store>, Connection) {
        let store = Arc::new(Store::in_memory().unwrap());
        let connection = store
            .save(
                ConnectionRequest {
                    connection_name: "temporary SFTP".into(),
                    host: "127.0.0.1".into(),
                    port: self.port,
                    username: "tester".into(),
                    auth_type: Some(if key { 2 } else { 1 }),
                    password: (!key).then(|| "test-password".into()),
                    private_key: key.then(|| {
                        self.client
                            .to_openssh(russh::keys::ssh_key::LineEnding::LF)
                            .unwrap()
                            .to_string()
                    }),
                    strict_host_key_check: Some(true),
                    known_hosts: Some(format!(
                        "[127.0.0.1]:{} {}",
                        self.port,
                        self.host.public_key().to_openssh().unwrap()
                    )),
                    ..Default::default()
                },
                false,
            )
            .unwrap();
        (store, connection)
    }
    pub async fn manager(&self, settings: Settings) -> (Arc<Manager>, Connection, String) {
        let (store, connection) = self.saved(false);
        let manager = Manager::with_settings(store, settings);
        let session = manager
            .open(OpenRequest {
                connection_id: connection.connection_id.clone(),
                root_path: Some("/home".into()),
                ..Default::default()
            })
            .await
            .unwrap();
        (manager, connection, session.sftp_session_id)
    }
    pub fn file(&self, path: &str, bytes: &[u8]) {
        let path = self.root.path().join(path.trim_start_matches('/'));
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    pub fn read(&self, path: &str) -> Vec<u8> {
        std::fs::read(self.root.path().join(path.trim_start_matches('/'))).unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.stop.cancel();
    }
}
struct Server {
    root: Arc<PathBuf>,
    stats: Arc<Stats>,
    delay: Arc<Mutex<Duration>>,
    client: PublicKey,
    root_access: Arc<Mutex<RootAccess>>,
}
impl server::Server for Server {
    type Handler = Ssh;
    fn new_client(&mut self, _: Option<std::net::SocketAddr>) -> Ssh {
        self.stats.live.fetch_add(1, Ordering::SeqCst);
        self.stats.connections.fetch_add(1, Ordering::SeqCst);
        Ssh {
            root: self.root.clone(),
            stats: self.stats.clone(),
            delay: self.delay.clone(),
            client: self.client.clone(),
            channels: HashMap::new(),
            ids: HashSet::new(),
            sftp: HashSet::new(),
            root_access: self.root_access.clone(),
            password_pending: HashMap::new(),
        }
    }
}
struct Ssh {
    root: Arc<PathBuf>,
    stats: Arc<Stats>,
    delay: Arc<Mutex<Duration>>,
    client: PublicKey,
    channels: HashMap<ChannelId, Channel<server::Msg>>,
    ids: HashSet<ChannelId>,
    sftp: HashSet<ChannelId>,
    root_access: Arc<Mutex<RootAccess>>,
    password_pending: HashMap<ChannelId, Vec<u8>>,
}
impl Ssh {
    fn start_sftp(
        &mut self,
        id: ChannelId,
        session: &mut Session,
        elevated: bool,
    ) -> Result<(), russh::Error> {
        self.sftp.insert(id);
        let channel = self.channels.remove(&id).unwrap();
        if elevated {
            // Fragmented readiness and password prompts exercise incremental parsing.
            session.data(id, b"SHELLPILOT_ROOT_".to_vec())?;
            session.data(id, b"SFTP_READY\n".to_vec())?;
        }
        let handler = Fs {
            root: self.root.clone(),
            delay: self.delay.clone(),
            handles: HashMap::new(),
            elevated,
        };
        tokio::spawn(async move {
            russh_sftp::server::run(channel.into_stream(), handler).await;
        });
        Ok(())
    }
}
impl Drop for Ssh {
    fn drop(&mut self) {
        self.stats
            .channels
            .fetch_sub(self.ids.len(), Ordering::SeqCst);
        self.stats.live.fetch_sub(1, Ordering::SeqCst);
    }
}
impl server::Handler for Ssh {
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
            if user == "tester" && key.key_data() == self.client.key_data() {
                server::Auth::Accept
            } else {
                server::Auth::reject()
            },
        )
    }
    async fn channel_open_session(
        &mut self,
        channel: Channel<server::Msg>,
        reply: ChannelOpenHandle,
        _: &mut Session,
    ) -> Result<(), Self::Error> {
        self.stats.channels.fetch_add(1, Ordering::SeqCst);
        self.ids.insert(channel.id());
        self.channels.insert(channel.id(), channel);
        reply.accept().await;
        Ok(())
    }
    async fn channel_close(&mut self, id: ChannelId, _: &mut Session) -> Result<(), Self::Error> {
        if self.ids.remove(&id) {
            self.stats.channels.fetch_sub(1, Ordering::SeqCst);
        }
        self.channels.remove(&id);
        self.sftp.remove(&id);
        Ok(())
    }
    async fn channel_eof(
        &mut self,
        id: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if self.ids.remove(&id) {
            self.stats.channels.fetch_sub(1, Ordering::SeqCst);
        }
        self.channels.remove(&id);
        session.close(id)?;
        Ok(())
    }
    async fn subsystem_request(
        &mut self,
        id: ChannelId,
        name: &str,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if name != "sftp" {
            session.channel_failure(id)?;
            return Ok(());
        }
        session.channel_success(id)?;
        self.start_sftp(id, session, false)?;
        Ok(())
    }
    async fn exec_request(
        &mut self,
        id: ChannelId,
        command: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        let command = String::from_utf8_lossy(command).into_owned();
        self.stats
            .root_commands
            .lock()
            .unwrap()
            .push(command.clone());
        if !command.starts_with("sudo ")
            || !command.contains("test \"$(id -u)\" = 0")
            || !command.contains("sftp-server")
        {
            session.channel_failure(id)?;
            return Ok(());
        }
        session.channel_success(id)?;
        let access = *self.root_access.lock().unwrap();
        match access {
            RootAccess::Passwordless => self.start_sftp(id, session, true)?,
            RootAccess::Password if command.contains("sudo -S ") => {
                self.stats.password_prompts.fetch_add(1, Ordering::SeqCst);
                self.password_pending.insert(id, Vec::new());
                session.extended_data(id, 1, b"SHELLPILOT_SUDO_".to_vec())?;
                session.extended_data(id, 1, b"PASSWORD:".to_vec())?;
            }
            _ => {
                session.exit_status_request(id, 1)?;
                session.eof(id)?;
                session.close(id)?;
            }
        }
        Ok(())
    }
    #[allow(clippy::too_many_arguments)]
    async fn pty_request(
        &mut self,
        id: ChannelId,
        _: &str,
        _: u32,
        _: u32,
        _: u32,
        _: u32,
        _: &[(Pty, u32)],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(id)?;
        Ok(())
    }
    async fn shell_request(
        &mut self,
        id: ChannelId,
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        session.channel_success(id)?;
        session.data(id, b"ready\r\n".to_vec())?;
        Ok(())
    }
    async fn data(
        &mut self,
        id: ChannelId,
        bytes: &[u8],
        session: &mut Session,
    ) -> Result<(), Self::Error> {
        if let Some(password) = self.password_pending.get_mut(&id) {
            // russh delivers data both to this callback and the channel receiver.
            // Consume sudo's input before handing that receiver to the SFTP server.
            while let Some(message) = self.channels.get_mut(&id).unwrap().wait().await {
                if matches!(message, russh::ChannelMsg::Data { .. }) {
                    break;
                }
            }
            password.extend_from_slice(bytes);
            if password.ends_with(b"\n") {
                let correct = password == b"sudo-test-password\n";
                self.password_pending.remove(&id);
                if correct {
                    self.start_sftp(id, session, true)?;
                } else {
                    // sudo asks again after a wrong password; the client must stop.
                    session.extended_data(id, 1, b"SHELLPILOT_SUDO_PASSWORD:".to_vec())?;
                }
            }
            return Ok(());
        }
        if !self.sftp.contains(&id) {
            session.data(id, bytes.to_vec())?;
        }
        Ok(())
    }
}
enum Handle {
    File(tokio::fs::File),
    Dir(Vec<p::File>, usize),
}
struct Fs {
    root: Arc<PathBuf>,
    delay: Arc<Mutex<Duration>>,
    handles: HashMap<String, Handle>,
    elevated: bool,
}
fn io(error: std::io::Error) -> StatusCode {
    match error.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    }
}
fn attrs(meta: &std::fs::Metadata) -> FileAttributes {
    FileAttributes {
        size: Some(meta.len()),
        permissions: Some(if meta.is_dir() { 0o040755 } else { 0o100644 }),
        mtime: Some(
            meta.modified()
                .unwrap_or(UNIX_EPOCH)
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs() as u32,
        ),
        atime: Some(0),
        ..Default::default()
    }
}
fn ok(id: u32) -> p::Status {
    p::Status {
        id,
        status_code: StatusCode::Ok,
        error_message: String::new(),
        language_tag: "en".into(),
    }
}
impl Fs {
    fn path(&self, value: &str) -> Result<PathBuf, StatusCode> {
        let value = if value == "." {
            if self.elevated {
                "/root"
            } else {
                "/home"
            }
        } else {
            value
        };
        if !self.elevated && (value == "/root" || value.starts_with("/root/")) {
            return Err(StatusCode::PermissionDenied);
        }
        if !value.starts_with('/') || value.contains('\\') {
            return Err(StatusCode::PermissionDenied);
        }
        if PathBuf::from(value)
            .components()
            .any(|p| matches!(p, Component::ParentDir))
        {
            return Err(StatusCode::PermissionDenied);
        }
        Ok(self.root.join(value.trim_start_matches('/')))
    }
    fn handle(&mut self, file: Handle) -> String {
        let id = uuid::Uuid::new_v4().to_string();
        self.handles.insert(id.clone(), file);
        id
    }
    async fn inspect_attrs(&self, path: &str) -> Result<FileAttributes, StatusCode> {
        // Deliberate virtual symlink and permission boundary for path protection tests.
        if path == "/home/link" {
            return Ok(FileAttributes {
                permissions: Some(0o120777),
                ..Default::default()
            });
        }
        if path.starts_with("/home/denied") {
            return Err(StatusCode::PermissionDenied);
        }
        Ok(attrs(
            &tokio::fs::metadata(self.path(path)?).await.map_err(io)?,
        ))
    }
    async fn delay(&self) {
        let delay = *self.delay.lock().unwrap();
        tokio::time::sleep(delay).await;
    }
}
impl russh_sftp::server::Handler for Fs {
    type Error = StatusCode;
    fn unimplemented(&self) -> Self::Error {
        StatusCode::OpUnsupported
    }
    async fn init(
        &mut self,
        _: u32,
        _: HashMap<String, String>,
    ) -> Result<p::Version, Self::Error> {
        let mut version = p::Version::new();
        version
            .extensions
            .insert("hardlink@openssh.com".into(), "1".into());
        version
            .extensions
            .insert("posix-rename@openssh.com".into(), "1".into());
        Ok(version)
    }
    async fn realpath(&mut self, id: u32, value: String) -> Result<p::Name, Self::Error> {
        let path = if value == "." {
            if self.elevated {
                "/root".into()
            } else {
                "/home".into()
            }
        } else if value.starts_with("/home/link") {
            "/outside".into()
        } else {
            value
        };
        self.inspect_attrs(&path).await?;
        Ok(p::Name {
            id,
            files: vec![p::File::dummy(path)],
        })
    }
    async fn lstat(&mut self, id: u32, path: String) -> Result<p::Attrs, Self::Error> {
        Ok(p::Attrs {
            id,
            attrs: self.inspect_attrs(&path).await?,
        })
    }
    async fn stat(&mut self, id: u32, path: String) -> Result<p::Attrs, Self::Error> {
        Ok(p::Attrs {
            id,
            attrs: self.inspect_attrs(&path).await?,
        })
    }
    async fn fstat(&mut self, id: u32, handle: String) -> Result<p::Attrs, Self::Error> {
        match self.handles.get(&handle) {
            Some(Handle::File(file)) => Ok(p::Attrs {
                id,
                attrs: attrs(&file.metadata().await.map_err(io)?),
            }),
            _ => Err(StatusCode::Failure),
        }
    }
    async fn open(
        &mut self,
        id: u32,
        path: String,
        flags: OpenFlags,
        _: FileAttributes,
    ) -> Result<p::Handle, Self::Error> {
        let path = self.path(&path)?;
        let file = tokio::fs::OpenOptions::new()
            .read(flags.contains(OpenFlags::READ))
            .write(flags.contains(OpenFlags::WRITE))
            .create(flags.contains(OpenFlags::CREATE) && !flags.contains(OpenFlags::EXCLUDE))
            .create_new(flags.contains(OpenFlags::EXCLUDE))
            .truncate(flags.contains(OpenFlags::TRUNCATE))
            .open(path)
            .await
            .map_err(io)?;
        // Simulate OPEN creating a file before its delayed acknowledgment reaches the client.
        self.delay().await;
        Ok(p::Handle {
            id,
            handle: self.handle(Handle::File(file)),
        })
    }
    async fn close(&mut self, id: u32, handle: String) -> Result<p::Status, Self::Error> {
        self.handles.remove(&handle).ok_or(StatusCode::Failure)?;
        Ok(ok(id))
    }
    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<p::Data, Self::Error> {
        self.delay().await;
        let Some(Handle::File(file)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io)?;
        let mut data = vec![0; len.min(32768) as usize];
        let size = file.read(&mut data).await.map_err(io)?;
        if size == 0 {
            return Err(StatusCode::Eof);
        }
        data.truncate(size);
        Ok(p::Data { id, data })
    }
    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<p::Status, Self::Error> {
        self.delay().await;
        let Some(Handle::File(file)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(io)?;
        file.write_all(&data).await.map_err(io)?;
        file.flush().await.map_err(io)?;
        Ok(ok(id))
    }
    async fn opendir(&mut self, id: u32, path: String) -> Result<p::Handle, Self::Error> {
        let mut listing = tokio::fs::read_dir(self.path(&path)?).await.map_err(io)?;
        let mut entries = Vec::new();
        while let Some(entry) = listing.next_entry().await.map_err(io)? {
            entries.push(p::File::new(
                entry.file_name().to_string_lossy().to_string(),
                attrs(&entry.metadata().await.map_err(io)?),
            ));
        }
        Ok(p::Handle {
            id,
            handle: self.handle(Handle::Dir(entries, 0)),
        })
    }
    async fn readdir(&mut self, id: u32, handle: String) -> Result<p::Name, Self::Error> {
        let Some(Handle::Dir(entries, pos)) = self.handles.get_mut(&handle) else {
            return Err(StatusCode::Failure);
        };
        if *pos >= entries.len() {
            return Err(StatusCode::Eof);
        }
        let file = entries[*pos].clone();
        *pos += 1;
        Ok(p::Name {
            id,
            files: vec![file],
        })
    }
    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        _: FileAttributes,
    ) -> Result<p::Status, Self::Error> {
        tokio::fs::create_dir(self.path(&path)?).await.map_err(io)?;
        Ok(ok(id))
    }
    async fn rmdir(&mut self, id: u32, path: String) -> Result<p::Status, Self::Error> {
        tokio::fs::remove_dir(self.path(&path)?).await.map_err(io)?;
        Ok(ok(id))
    }
    async fn remove(&mut self, id: u32, path: String) -> Result<p::Status, Self::Error> {
        tokio::fs::remove_file(self.path(&path)?)
            .await
            .map_err(io)?;
        Ok(ok(id))
    }
    async fn rename(
        &mut self,
        id: u32,
        old: String,
        new: String,
    ) -> Result<p::Status, Self::Error> {
        let old = self.path(&old)?;
        let new = self.path(&new)?;
        tokio::fs::hard_link(&old, &new).await.map_err(io)?;
        tokio::fs::remove_file(old).await.map_err(io)?;
        Ok(ok(id))
    }
    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<p::Packet, Self::Error> {
        fn part(bytes: &mut &[u8]) -> Result<String, StatusCode> {
            if bytes.len() < 4 {
                return Err(StatusCode::BadMessage);
            }
            let len = u32::from_be_bytes(bytes[..4].try_into().unwrap()) as usize;
            *bytes = &bytes[4..];
            if bytes.len() < len {
                return Err(StatusCode::BadMessage);
            }
            let s = String::from_utf8(bytes[..len].to_vec()).map_err(|_| StatusCode::BadMessage)?;
            *bytes = &bytes[len..];
            Ok(s)
        }
        let mut bytes = data.as_slice();
        let old = self.path(&part(&mut bytes)?)?;
        let new = self.path(&part(&mut bytes)?)?;
        if request == "hardlink@openssh.com" {
            tokio::fs::hard_link(old, new).await.map_err(io)?;
        } else if request == "posix-rename@openssh.com" {
            tokio::fs::rename(old, new).await.map_err(io)?;
        } else {
            return Err(StatusCode::OpUnsupported);
        }
        Ok(p::Packet::Status(ok(id)))
    }
}
pub async fn eventually(mut f: impl FnMut() -> bool) {
    tokio::time::timeout(Duration::from_secs(4), async {
        while !f() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("condition did not converge")
}
