//! Read-only sidebar browsing, using the current shell's authenticated elevation.
use super::*;
use russh_sftp::{client::RawSftpSession, protocol::StatusCode};

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryEntry {
    name: String,
    directory: bool,
    symlink: bool,
    size: Option<u64>,
}
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryListing {
    path: String,
    entries: Vec<DirectoryEntry>,
    truncated: bool,
}

impl TerminalManager {
    pub async fn directory_list(
        &self,
        id: &str,
        user: &str,
        path: &str,
    ) -> Result<DirectoryListing, Error> {
        // Empty means SFTP login home, never a guessed terminal working directory.
        if (!path.is_empty() && !path.starts_with('/'))
            || path.len() > 4096
            || path.chars().any(char::is_control)
        {
            return Err(Error::Invalid("文件目录需要绝对路径".into()));
        }
        let session = self.session(id)?;
        if session.identity.user_id != user
            || !session.state().connected
            || session.stop.is_cancelled()
        {
            return Err(Error::TerminalMissing);
        }
        let pooled = session.transport.upgrade().ok_or(Error::TerminalMissing)?;
        let access = session.directory_access.lock().unwrap().access.clone();
        let _slot = pooled
            .slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let permit = self
            .opening_slots
            .clone()
            .try_acquire_owned()
            .map_err(|_| Error::Busy)?;
        let operation = async {
            if access.elevated {
                let bridge = access
                    .bridge
                    .as_deref()
                    .ok_or_else(super::directory_access::access_error)?;
                let data = pooled
                    .transport
                    .clone()
                    .elevated_directory(bridge, if path.is_empty() { "/" } else { path }, permit)
                    .await?;
                return parse_elevated(&data);
            }
            let stream = pooled
                .transport
                .clone()
                .sftp_stream(Duration::from_secs(3), permit)
                .await?;
            let raw = RawSftpSession::new_with_config(
                stream,
                russh_sftp::client::Config {
                    request_timeout_secs: 3,
                    max_packet_len: 262144,
                    ..Default::default()
                },
            );
            raw.init().await.map_err(|_| Error::Ssh)?;
            let canonical = raw
                .realpath(if path.is_empty() { "." } else { path })
                .await
                .map_err(|_| Error::Ssh)?;
            let canonical = canonical
                .files
                .first()
                .map(|file| file.filename.clone())
                .filter(|path| {
                    path.starts_with('/')
                        && path.len() <= 4096
                        && !path.chars().any(char::is_control)
                })
                .ok_or_else(|| Error::Invalid("服务器未返回有效目录".into()))?;
            let handle = raw
                .opendir(&canonical)
                .await
                .map_err(|_| {
                    Error::Invalid("无法读取目录，请检查路径和 SSH 登录账号的权限".into())
                })?
                .handle;
            let mut entries = Vec::new();
            let mut truncated = false;
            let mut bytes = 0;
            loop {
                match raw.readdir(&handle).await {
                    Ok(reply) => {
                        if reply.files.is_empty() {
                            break;
                        }
                        for file in reply.files {
                            let name = file.filename;
                            if name == "."
                                || name == ".."
                                || name.is_empty()
                                || name.len() > 1024
                                || name.contains('/')
                                || name.chars().any(char::is_control)
                            {
                                continue;
                            }
                            if entries.len() >= 4096 || bytes + name.len() > 1024 * 1024 {
                                truncated = true;
                                break;
                            }
                            bytes += name.len();
                            entries.push(DirectoryEntry {
                                name,
                                directory: file.attrs.is_dir(),
                                symlink: file.attrs.is_symlink(),
                                size: file.attrs.size,
                            });
                        }
                        if truncated {
                            break;
                        }
                    }
                    Err(russh_sftp::client::error::Error::Status(status))
                        if status.status_code == StatusCode::Eof =>
                    {
                        break
                    }
                    Err(_) => {
                        let _ = raw.close(&handle).await;
                        return Err(Error::Ssh);
                    }
                }
            }
            let _ = raw.close(&handle).await;
            entries.sort_by(|a, b| {
                b.directory
                    .cmp(&a.directory)
                    .then_with(|| a.name.cmp(&b.name))
            });
            Ok(DirectoryListing {
                path: canonical,
                entries,
                truncated,
            })
        };
        let listing = tokio::select! { biased;
            _ = session.stop.cancelled() => Err(Error::TerminalMissing),
            _ = self.shutdown.cancelled() => Err(Error::TerminalMissing),
            result = tokio::time::timeout(Duration::from_secs(5), operation) => result.map_err(|_| Error::ConnectTimeout)?,
        }?;
        // An exiting/nested sudo shell invalidates in-flight metadata as well.
        if session.directory_access.lock().unwrap().access != access {
            return Err(Error::Invalid("终端访问身份已切换，请重新读取目录".into()));
        }
        Ok(listing)
    }
}

fn parse_elevated(data: &[u8]) -> Result<DirectoryListing, Error> {
    let mut parts = data.split(|b| *b == 0);
    if parts.next() != Some(b"OK".as_slice()) {
        return Err(Error::Invalid(
            "无法以当前终端身份读取目录，请检查路径或目录是否仍然存在".into(),
        ));
    }
    let path = std::str::from_utf8(parts.next().unwrap_or_default())
        .ok()
        .filter(|s| s.starts_with('/') && s.len() <= 4096 && !s.chars().any(char::is_control))
        .ok_or(Error::Ssh)?
        .to_owned();
    let mut entries = Vec::new();
    let mut bytes = 0;
    let mut truncated = data.len().saturating_sub(path.len() + 4) >= 1_048_576;
    while let Some(name) = parts.next() {
        let Some(kind) = parts.next() else { break };
        let Some(size) = parts.next() else { break };
        let Ok(name) = std::str::from_utf8(name) else {
            continue;
        };
        if name.is_empty()
            || name == "."
            || name == ".."
            || name.len() > 1024
            || name.contains('/')
            || name.chars().any(char::is_control)
        {
            continue;
        }
        if entries.len() >= 4096 || bytes + name.len() > 1_048_576 {
            truncated = true;
            break;
        }
        bytes += name.len();
        entries.push(DirectoryEntry {
            name: name.into(),
            directory: kind == b"d",
            symlink: kind == b"l",
            size: std::str::from_utf8(size).ok().and_then(|s| s.parse().ok()),
        });
    }
    entries.sort_by(|a, b| {
        b.directory
            .cmp(&a.directory)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(DirectoryListing {
        path,
        entries,
        truncated,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn root_metadata_is_bounded_filtered_and_has_no_file_content() {
        let listing = parse_elevated(
            b"OK\x00/root\x00text\x00f\x0012\x00folder\x00d\x000\x00.hidden\x00f\x001\x00bad/name\x00f\x001\x00",
        )
        .unwrap();
        let json = serde_json::to_value(listing).unwrap();
        assert_eq!(json["path"], "/root");
        assert_eq!(json["entries"][0]["name"], "folder");
        assert_eq!(json["entries"].as_array().unwrap().len(), 3);
        assert!(parse_elevated(b"ERR\0").is_err());
        assert!(parse_elevated(b"OK\0relative\0").is_err());
    }
}
