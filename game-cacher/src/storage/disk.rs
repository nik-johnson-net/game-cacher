//! TODO(T1): implement.
use std::path::{Path, PathBuf};

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

use super::{ObjectInfo, Storage};

/// Stores each key as a file under `root`; writes go to a temp file then rename.
pub struct DiskStorage {
    #[allow(dead_code)]
    root: PathBuf,
}

impl DiskStorage {
    pub async fn new(root: &Path) -> Result<Self> {
        Ok(Self {
            root: root.to_path_buf(),
        })
    }
}

#[async_trait]
impl Storage for DiskStorage {
    fn name(&self) -> &'static str {
        "disk"
    }
    async fn get(&self, _key: &str) -> Result<Option<Bytes>> {
        todo!("T1")
    }
    async fn put(&self, _key: &str, _data: Bytes) -> Result<()> {
        todo!("T1")
    }
    async fn delete(&self, _key: &str) -> Result<()> {
        todo!("T1")
    }
    async fn delete_prefix(&self, _prefix: &str) -> Result<(u64, u64)> {
        todo!("T1")
    }
    async fn list(&self, _prefix: &str) -> Result<Vec<ObjectInfo>> {
        todo!("T1")
    }
    async fn exists(&self, _key: &str) -> Result<bool> {
        todo!("T1")
    }
}
