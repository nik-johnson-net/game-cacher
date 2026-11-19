//! Storage abstraction: a flat key → bytes store (disk directory or S3 bucket).
//! Keys are `/`-separated relative paths (see docs/ARCHITECTURE.md "Storage layout").

#[cfg(test)]
mod conformance;
mod disk;
mod memory;
mod s3;

use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;

pub use disk::DiskStorage;
pub use memory::MemoryStorage;
pub use s3::S3Storage;

use crate::config::{Config, StorageKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectInfo {
    pub key: String,
    pub size: u64,
}

#[async_trait]
pub trait Storage: Send + Sync + 'static {
    /// Backend name for display (`disk`, `s3`, `memory`).
    fn name(&self) -> &'static str;
    async fn get(&self, key: &str) -> Result<Option<Bytes>>;
    /// Atomic replace: readers see the old or the new value, never a partial write.
    async fn put(&self, key: &str, data: Bytes) -> Result<()>;
    /// Deleting a missing key is not an error.
    async fn delete(&self, key: &str) -> Result<()>;
    /// Deletes every key starting with `prefix`; returns (count, bytes).
    async fn delete_prefix(&self, prefix: &str) -> Result<(u64, u64)>;
    /// Lists all keys under `prefix` (recursive), unordered.
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>>;
    async fn exists(&self, key: &str) -> Result<bool>;
}

pub type DynStorage = Arc<dyn Storage>;

/// Rejects keys that could escape the storage root.
pub fn validate_key(key: &str) -> Result<()> {
    if key.is_empty()
        || key.starts_with('/')
        || key.contains('\\')
        || key
            .split('/')
            .any(|c| c.is_empty() || c == "." || c == "..")
    {
        anyhow::bail!("invalid storage key: {key:?}");
    }
    Ok(())
}

pub async fn from_config(config: &Config) -> Result<DynStorage> {
    Ok(match config.storage {
        StorageKind::Disk => Arc::new(DiskStorage::new(&config.storage_path).await?),
        StorageKind::S3 => Arc::new(S3Storage::from_config(config)?),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys() {
        validate_key("cache/steam/ab/cd/x/0").unwrap();
        for bad in ["", "/abs", "a/../b", "a//b", "a/./b", "a\\b", "a/"] {
            assert!(validate_key(bad).is_err(), "{bad}");
        }
    }
}
