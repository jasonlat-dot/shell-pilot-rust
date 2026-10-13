use super::{
    model::{Kind, ManifestItem},
    Error, Result,
};
use std::collections::BTreeMap;
pub fn name(value: &str) -> Result<()> {
    if value.is_empty()
        || value == "."
        || value == ".."
        || value.len() > 255
        || value.contains(['/', '\\', '*', '?'])
        || value.chars().any(char::is_control)
    {
        return Err(Error::path("路径名称不合法或包含不支持的字符"));
    }
    Ok(())
}
pub fn absolute(value: &str) -> Result<String> {
    if !value.starts_with('/') || value.len() > 4096 {
        return Err(Error::path("远程路径必须是绝对路径且不超过 4096 字节"));
    }
    let mut parts = Vec::new();
    for part in value.split('/').filter(|p| !p.is_empty()) {
        name(part)?;
        parts.push(part);
    }
    if parts.len() > 64 {
        return Err(Error::limit());
    }
    Ok(format!("/{}", parts.join("/")))
}
pub fn relative(value: &str) -> Result<()> {
    if value.starts_with('/')
        || value.ends_with('/')
        || value.len() > 4096
        || value.split('/').count() > 64
    {
        return Err(Error::path("清单相对路径不合法"));
    }
    for part in value.split('/') {
        name(part)?;
    }
    Ok(())
}
pub fn inside(root: &str, path: &str) -> bool {
    root == "/" || path == root || path.strip_prefix(root).is_some_and(|p| p.starts_with('/'))
}
pub fn join(root: &str, part: &str) -> String {
    format!("{}/{}", root.trim_end_matches('/'), part)
}
pub fn parent(path: &str) -> &str {
    let pos = path.rfind('/').unwrap_or(0);
    if pos == 0 {
        "/"
    } else {
        &path[..pos]
    }
}
pub fn manifest(items: Vec<ManifestItem>, max: usize) -> Result<Vec<ManifestItem>> {
    if items.is_empty() || items.len() > max {
        return Err(Error::limit());
    }
    let mut map = BTreeMap::new();
    let mut bytes = 0u64;
    for item in items {
        relative(&item.relative_path)?;
        if !matches!(item.kind, Kind::File | Kind::Directory)
            || item.size > 1024 * 1024 * 1024 * 64
            || (item.kind == Kind::Directory && item.size != 0)
        {
            return Err(Error::invalid("清单文件类型或大小不合法"));
        }
        bytes = bytes
            .checked_add(item.size)
            .filter(|v| *v <= 1024 * 1024 * 1024 * 256)
            .ok_or_else(Error::limit)?;
        if map.insert(item.relative_path.clone(), item).is_some() {
            return Err(Error::invalid("清单路径重复"));
        }
    }
    let paths = map.keys().cloned().collect::<Vec<_>>();
    for path in paths {
        let parts = path.split('/').collect::<Vec<_>>();
        for end in 1..parts.len() {
            let parent = parts[..end].join("/");
            if let Some(item) = map.get(&parent) {
                if item.kind != Kind::Directory {
                    return Err(Error::invalid("文件路径同时被用作目录"));
                }
            } else {
                map.insert(
                    parent.clone(),
                    ManifestItem {
                        relative_path: parent,
                        kind: Kind::Directory,
                        size: 0,
                    },
                );
            }
            if map.len() > max {
                return Err(Error::limit());
            }
        }
    }
    let mut result = map.into_values().collect::<Vec<_>>();
    result.sort_by(|a, b| {
        a.relative_path
            .split('/')
            .count()
            .cmp(&b.relative_path.split('/').count())
            .then_with(|| a.relative_path.cmp(&b.relative_path))
    });
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundaries_and_manifest_conflicts() {
        assert!(!inside("/home/a", "/home/abc/x"));
        assert!(inside("/home/a", "/home/a/x"));
        for path in ["/a/../b", "relative", "/x/*", "/x/\u{0000}"] {
            assert!(absolute(path).is_err());
        }
        for path in ["../x", "/x", "a//b", "a/./b", "a\\b"] {
            assert!(relative(path).is_err());
        }
        let result = manifest(
            vec![ManifestItem {
                relative_path: "顶层/空目录/file".into(),
                kind: Kind::File,
                size: 0,
            }],
            10,
        )
        .unwrap();
        assert_eq!(result.len(), 3);
        assert!(manifest(
            vec![
                ManifestItem {
                    relative_path: "a".into(),
                    kind: Kind::File,
                    size: 1
                },
                ManifestItem {
                    relative_path: "a/b".into(),
                    kind: Kind::File,
                    size: 2
                }
            ],
            10
        )
        .is_err());
    }
}
