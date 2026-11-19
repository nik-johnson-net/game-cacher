use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;

use super::{ObjectInfo, Storage, validate_key};

const TMP_DIR: &str = ".tmp";

/// Stores each key as a file under `root`; writes go to a temp file then rename.
pub struct DiskStorage {
    root: PathBuf,
}

impl DiskStorage {
    pub async fn new(root: &Path) -> Result<Self> {
        tokio::fs::create_dir_all(root.join(TMP_DIR))
            .await
            .with_context(|| format!("creating storage root {}", root.display()))?;
        Ok(Self {
            root: root.to_path_buf(),
        })
    }

    fn path(&self, key: &str) -> Result<PathBuf> {
        validate_key(key)?;
        if key.split('/').next() == Some(TMP_DIR) {
            anyhow::bail!("invalid storage key: {key:?}");
        }
        Ok(self.root.join(key))
    }

    /// Lists files whose key starts with `prefix` (which may end mid-component).
    async fn walk(&self, prefix: &str) -> Result<Vec<ObjectInfo>> {
        if prefix.starts_with('/')
            || prefix.contains('\\')
            || prefix.split('/').any(|c| c == "." || c == "..")
        {
            anyhow::bail!("invalid storage prefix: {prefix:?}");
        }
        let root = self.root.clone();
        let prefix = prefix.to_string();
        tokio::task::spawn_blocking(move || walk_blocking(&root, &prefix)).await?
    }

    async fn remove_pruning(&self, key: &str) -> Result<()> {
        let path = self.root.join(key);
        match tokio::fs::remove_file(&path).await {
            Ok(()) => {}
            Err(e) if e.kind() == ErrorKind::NotFound => {}
            Err(e) => return Err(e).with_context(|| format!("deleting {key}")),
        }
        let mut dir = path.parent();
        while let Some(d) = dir {
            if d == self.root || !d.starts_with(&self.root) {
                break;
            }
            // Fails (and stops pruning) when the directory is not empty.
            if tokio::fs::remove_dir(d).await.is_err() {
                break;
            }
            dir = d.parent();
        }
        Ok(())
    }
}

fn walk_blocking(root: &Path, prefix: &str) -> Result<Vec<ObjectInfo>> {
    let dir_part = prefix.rfind('/').map(|i| &prefix[..i]).unwrap_or("");
    let start = if dir_part.is_empty() {
        root.to_path_buf()
    } else {
        root.join(dir_part)
    };
    let mut out = Vec::new();
    let mut stack = vec![start];
    while let Some(dir) = stack.pop() {
        let entries = match std::fs::read_dir(&dir) {
            Ok(e) => e,
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                continue;
            }
            Err(e) => return Err(e).with_context(|| format!("reading {}", dir.display())),
        };
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let key = path
                .strip_prefix(root)?
                .components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/");
            if key == TMP_DIR {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(e) if e.kind() == ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            if meta.is_dir() {
                stack.push(path);
            } else if key.starts_with(prefix) {
                out.push(ObjectInfo {
                    key,
                    size: meta.len(),
                });
            }
        }
    }
    Ok(out)
}

#[async_trait]
impl Storage for DiskStorage {
    fn name(&self) -> &'static str {
        "disk"
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let path = self.path(key)?;
        match tokio::fs::read(&path).await {
            Ok(b) => Ok(Some(Bytes::from(b))),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                Ok(None)
            }
            Err(e) => Err(e).with_context(|| format!("reading {key}")),
        }
    }

    async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        let path = self.path(key)?;
        let tmp = self
            .root
            .join(TMP_DIR)
            .join(uuid::Uuid::new_v4().to_string());
        tokio::fs::write(&tmp, &data)
            .await
            .with_context(|| format!("writing temp file for {key}"))?;
        let mut attempts = 0;
        loop {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await?;
            }
            match tokio::fs::rename(&tmp, &path).await {
                Ok(()) => return Ok(()),
                // A concurrent delete may have pruned the parent dir just now.
                Err(e) if e.kind() == ErrorKind::NotFound && attempts < 5 => attempts += 1,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&tmp).await;
                    return Err(e).with_context(|| format!("storing {key}"));
                }
            }
        }
    }

    async fn delete(&self, key: &str) -> Result<()> {
        self.path(key)?;
        self.remove_pruning(key).await
    }

    async fn delete_prefix(&self, prefix: &str) -> Result<(u64, u64)> {
        let mut count = 0;
        let mut bytes = 0;
        for obj in self.walk(prefix).await? {
            self.remove_pruning(&obj.key).await?;
            count += 1;
            bytes += obj.size;
        }
        Ok((count, bytes))
    }

    async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>> {
        self.walk(prefix).await
    }

    async fn exists(&self, key: &str) -> Result<bool> {
        let path = self.path(key)?;
        match tokio::fs::metadata(&path).await {
            Ok(m) => Ok(m.is_file()),
            Err(e) if matches!(e.kind(), ErrorKind::NotFound | ErrorKind::NotADirectory) => {
                Ok(false)
            }
            Err(e) => Err(e).with_context(|| format!("stat {key}")),
        }
    }
}
