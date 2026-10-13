use super::{
    model::{Entry, Kind},
    path, Error, Result, Session,
};
use russh_sftp::{
    client::RawSftpSession,
    protocol::{FileAttributes, OpenFlags, StatusCode},
};
use std::{sync::Arc, time::Duration};
pub(crate) struct Wire {
    pub raw: RawSftpSession,
    pub hardlink: bool,
    pub posix_rename: bool,
}
impl Wire {
    pub async fn new(session: &Session) -> Result<Self> {
        let permit = session
            .openings
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::busy())?;
        let stream = session
            .transport
            .clone()
            .sftp_stream(session.connect_timeout, permit)
            .await
            .map_err(Error::ssh)?;
        let raw = RawSftpSession::new_with_config(
            stream,
            russh_sftp::client::Config {
                request_timeout_secs: 120,
                max_packet_len: 262144,
                ..Default::default()
            },
        );
        let version = raw.init().await?;
        Ok(Self {
            hardlink: version
                .extensions
                .get("hardlink@openssh.com")
                .is_some_and(|v| v == "1"),
            posix_rename: version
                .extensions
                .get("posix-rename@openssh.com")
                .is_some_and(|v| v == "1"),
            raw,
        })
    }
    pub async fn realpath(&self, path: &str) -> Result<String> {
        let reply = self.raw.realpath(path).await?;
        if reply.files.len() != 1 {
            return Err(Error::invalid("服务器返回的规范路径不合法"));
        }
        path::absolute(&reply.files[0].filename)
    }
    pub async fn stat(&self, path: &str) -> Result<Option<FileAttributes>> {
        match self.raw.lstat(path).await {
            Ok(reply) => Ok(Some(reply.attrs)),
            Err(russh_sftp::client::error::Error::Status(s))
                if s.status_code == StatusCode::NoSuchFile =>
            {
                Ok(None)
            }
            Err(e) => Err(e.into()),
        }
    }
    // Check every ancestor with lstat, not just the last component or realpath.
    pub async fn checked(&self, root: &str, value: &str, missing_leaf: bool) -> Result<String> {
        let target = path::absolute(value)?;
        if !path::inside(root, &target) {
            return Err(Error::path("路径超出当前文件管理根目录"));
        }
        let mut current = String::new();
        let parts = target
            .split('/')
            .filter(|p| !p.is_empty())
            .collect::<Vec<_>>();
        for (index, part) in parts.iter().enumerate() {
            current.push('/');
            current.push_str(part);
            let leaf = index + 1 == parts.len();
            match self.stat(&current).await? {
                None if leaf && missing_leaf => {}
                None => return Err(Error::missing()),
                Some(attrs) => {
                    if attrs.is_symlink() {
                        return Err(Error::path("不允许跟随符号链接"));
                    }
                    if !leaf && !attrs.is_dir() {
                        return Err(Error::path("路径父级不是目录"));
                    }
                }
            }
        }
        let existing = if missing_leaf && self.stat(&target).await?.is_none() {
            path::parent(&target)
        } else {
            &target
        };
        let canonical = self.realpath(existing).await?;
        if !path::inside(root, &canonical) {
            return Err(Error::path("规范路径超出会话根目录"));
        }
        Ok(target)
    }
    pub async fn list(&self, path: &str, limit: usize) -> Result<Vec<Entry>> {
        let handle = self.raw.opendir(path).await?.handle;
        let result = async {
            let mut entries = Vec::new();
            loop {
                match self.raw.readdir(&handle).await {
                    Ok(reply) => {
                        if reply.files.is_empty() {
                            return Err(Error::invalid("服务器返回空目录批次"));
                        }
                        for file in reply.files {
                            if file.filename == "." || file.filename == ".." {
                                continue;
                            }
                            path::name(&file.filename)?;
                            entries.push(entry(
                                file.filename.clone(),
                                path::join(path, &file.filename),
                                &file.attrs,
                            ));
                            if entries.len() > limit {
                                return Err(Error::limit());
                            }
                        }
                    }
                    Err(russh_sftp::client::error::Error::Status(s))
                        if s.status_code == StatusCode::Eof =>
                    {
                        break
                    }
                    Err(e) => return Err(e.into()),
                }
            }
            entries.sort_by(|a, b| {
                (a.kind != Kind::Directory)
                    .cmp(&(b.kind != Kind::Directory))
                    .then_with(|| a.name.cmp(&b.name))
            });
            Ok(entries)
        }
        .await;
        let closed = self.raw.close(&handle).await;
        let entries = result?;
        closed?;
        Ok(entries)
    }
    pub async fn mkdirs(&self, root: &str, target: &str) -> Result<()> {
        let target = path::absolute(target)?;
        if !path::inside(root, &target) {
            return Err(Error::path("目录超出会话根目录"));
        }
        let suffix = target
            .strip_prefix(root.trim_end_matches('/'))
            .ok_or_else(|| Error::path("目录超出根目录"))?;
        let mut current = root.to_string();
        self.checked(root, root, false).await?;
        for part in suffix.split('/').filter(|p| !p.is_empty()) {
            current = path::join(&current, part);
            self.checked(root, &current, true).await?;
            match self.stat(&current).await? {
                Some(meta) if meta.is_dir() => {}
                Some(_) => return Err(Error::conflict("目标父目录已被文件占用")),
                None => {
                    self.raw.mkdir(&current, FileAttributes::default()).await?;
                }
            }
        }
        Ok(())
    }
    pub async fn empty_file(&self, target: &str) -> Result<()> {
        let handle = self
            .raw
            .open(
                target,
                OpenFlags::WRITE | OpenFlags::CREATE | OpenFlags::EXCLUDE,
                FileAttributes {
                    permissions: Some(0o100600),
                    ..Default::default()
                },
            )
            .await?
            .handle;
        self.raw.close(handle).await?;
        Ok(())
    }
}
pub(crate) fn entry(name: String, path: String, attrs: &FileAttributes) -> Entry {
    let kind = if attrs.is_symlink() {
        Kind::Symlink
    } else if attrs.is_dir() {
        Kind::Directory
    } else if attrs.is_regular() {
        Kind::File
    } else {
        Kind::Other
    };
    Entry {
        name,
        path,
        kind,
        size: if kind == Kind::File {
            attrs.size.unwrap_or(0)
        } else {
            0
        },
        modified_at: u64::from(attrs.mtime.unwrap_or(0)) * 1000,
        permissions: format!("{:04o}", attrs.permissions.unwrap_or(0) & 0o7777),
    }
}
pub(crate) async fn operation<T>(
    session: &Arc<Session>,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! { biased;_ =session.stop.cancelled()=>Err(Error::cancelled()),value=tokio::time::timeout(Duration::from_secs(30),future)=>value.map_err(|_|Error::timeout())? }
}
