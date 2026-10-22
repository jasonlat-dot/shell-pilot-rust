use super::{model::Kind, path, wire, Error, Manager, Result};

// Validate ancestors without following the final component: a link may be renamed
// or unlinked, but must never become a route into another directory tree.
async fn target(wire: &wire::Wire, root: &str, value: &str) -> Result<String> {
    let value = path::absolute(value)?;
    if value == root || !path::inside(root, &value) {
        return Err(Error::path("不能修改会话根目录或根目录之外的条目"));
    }
    wire.checked(root, path::parent(&value), false).await?;
    Ok(value)
}

impl Manager {
    /// SFTP v3 rename fails when the destination already exists; do not use the
    /// overwrite-capable posix-rename extension for interactive file operations.
    pub async fn rename_entry(&self, id: &str, from: &str, to: &str) -> Result<()> {
        let session = self.session(id)?;
        let _active = session.begin();
        wire::operation(&session, async {
            let _lock = session
                .metadata
                .acquire()
                .await
                .map_err(|_| Error::cancelled())?;
            let wire = wire::Wire::new(&session).await?;
            let source = target(&wire, &session.view.root_path, from).await?;
            let destination = target(&wire, &session.view.root_path, to).await?;
            let attrs = wire.stat(&source).await?.ok_or_else(Error::missing)?;
            if source == destination || (attrs.is_dir() && path::inside(&source, &destination)) {
                return Err(Error::invalid("不能移动到原路径或目录自身的子目录"));
            }
            if wire.stat(&destination).await?.is_some() {
                return Err(Error::conflict("目标已存在，请更换名称或目标目录"));
            }
            wire.raw.rename(source, destination).await?;
            Ok(())
        })
        .await
    }

    /// Explicit recursive deletion. First inspect the complete bounded tree so
    /// unsupported entries or excessive depth do not start a partial deletion.
    pub async fn delete_tree(&self, id: &str, value: &str) -> Result<()> {
        let session = self.session(id)?;
        let _active = session.begin();
        wire::operation(&session, async {
            let _lock = session
                .metadata
                .acquire()
                .await
                .map_err(|_| Error::cancelled())?;
            let wire = wire::Wire::new(&session).await?;
            let root = &session.view.root_path;
            let source = target(&wire, root, value).await?;
            let mut pending = vec![source];
            let mut entries = Vec::new();
            while let Some(path) = pending.pop() {
                let path = target(&wire, root, &path).await?;
                let attrs = wire.stat(&path).await?.ok_or_else(Error::missing)?;
                if entries.len() >= self.settings.max_items {
                    return Err(Error::limit());
                }
                if attrs.is_dir() && !attrs.is_symlink() {
                    let children = wire.list(&path, self.settings.max_items).await?;
                    if entries.len() + pending.len() + children.len() + 1 > self.settings.max_items
                    {
                        return Err(Error::limit());
                    }
                    pending.extend(children.into_iter().map(|entry| entry.path));
                    entries.push((path, Kind::Directory));
                } else if attrs.is_regular() || attrs.is_symlink() {
                    entries.push((path, Kind::File));
                } else {
                    return Err(Error::path("目录包含不支持删除的特殊文件"));
                }
            }
            for (path, kind) in entries.into_iter().rev() {
                let path = target(&wire, root, &path).await?;
                let attrs = wire.stat(&path).await?.ok_or_else(Error::missing)?;
                if kind == Kind::Directory {
                    if !attrs.is_dir() || attrs.is_symlink() {
                        return Err(Error::conflict("删除期间目录类型已变化，请刷新后重试"));
                    }
                    wire.raw.rmdir(path).await?;
                } else {
                    if !attrs.is_regular() && !attrs.is_symlink() {
                        return Err(Error::conflict("删除期间文件类型已变化，请刷新后重试"));
                    }
                    wire.raw.remove(path).await?;
                }
            }
            Ok(())
        })
        .await
    }
}
