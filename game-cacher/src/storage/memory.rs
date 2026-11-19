use std::collections::BTreeMap;

use anyhow::Result;
use async_trait::async_trait;
use bytes::Bytes;
use tokio::sync::RwLock;

use super::{ObjectInfo, Storage, validate_key};

/// In-memory storage for tests.
#[derive(Default)]
pub struct MemoryStorage {
    data: RwLock<BTreeMap<String, Bytes>>,
}

impl MemoryStorage {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl Storage for MemoryStorage {
    fn name(&self) -> &'static str {
        "memory"
    }
    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        validate_key(key)?;
        Ok(self.data.read().await.get(key).cloned())
    }
    async fn put(&self, key: &str, data: Bytes) -> Result<()> {
        validate_key(key)?;
        self.data.write().await.insert(key.to_string(), data);
        Ok(())
    }
    async fn delete(&self, key: &str) -> Result<()> {
        validate_key(key)?;
        self.data.write().await.remove(key);
        Ok(())
    }
    async fn delete_prefix(&self, prefix: &str) -> Result<(u64, u64)> {
        let mut data = self.data.write().await;
        let keys: Vec<String> = data
            .keys()
            .filter(|k| k.starts_with(prefix))
            .cloned()
            .collect();
        let mut bytes = 0;
        for k in &keys {
            bytes += data.remove(k).map(|b| b.len() as u64).unwrap_or(0);
        }
        Ok((keys.len() as u64, bytes))
    }
    async fn list(&self, prefix: &str) -> Result<Vec<ObjectInfo>> {
        Ok(self
            .data
            .read()
            .await
            .iter()
            .filter(|(k, _)| k.starts_with(prefix))
            .map(|(k, v)| ObjectInfo {
                key: k.clone(),
                size: v.len() as u64,
            })
            .collect())
    }
    async fn exists(&self, key: &str) -> Result<bool> {
        validate_key(key)?;
        Ok(self.data.read().await.contains_key(key))
    }
}
