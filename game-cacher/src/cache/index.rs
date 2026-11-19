//! LRU index + eviction. TODO(T7).
use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::Result;

use crate::metrics::Metrics;
use crate::storage::DynStorage;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct IdentifierUsage {
    pub bytes: u64,
    pub objects: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct Usage {
    pub total_bytes: u64,
    pub objects: u64,
    pub per_identifier: BTreeMap<String, IdentifierUsage>,
}

/// Tracks size and last access per cached object (keyed by `ObjectKey::prefix()`),
/// persisted to `index/lru.json`.
pub struct LruIndex {}

impl LruIndex {
    /// Loads `index/lru.json`, or rebuilds by listing `cache/` when missing/corrupt.
    pub async fn load_or_rebuild(
        _storage: DynStorage,
        _max_bytes: Option<u64>,
        _metrics: Arc<Metrics>,
    ) -> Result<Arc<Self>> {
        todo!("T7")
    }
    /// Marks an object as used now.
    pub fn touch(&self, _object_prefix: &str, _identifier: &str) {
        todo!("T7")
    }
    /// Records `bytes` newly stored for an object (slice or meta write) and touches it.
    pub fn record_write(&self, _object_prefix: &str, _identifier: &str, _bytes: u64) {
        todo!("T7")
    }
    /// Forgets an object (after it was deleted from storage).
    pub fn remove_object(&self, _object_prefix: &str) {
        todo!("T7")
    }
    /// Forgets all objects of an identifier (after a platform purge).
    pub fn remove_identifier(&self, _identifier: &str) {
        todo!("T7")
    }
    pub fn usage(&self) -> Usage {
        todo!("T7")
    }
    pub fn max_bytes(&self) -> Option<u64> {
        todo!("T7")
    }
    /// Writes `index/lru.json`.
    pub async fn snapshot(&self) -> Result<()> {
        todo!("T7")
    }
    /// If over the limit, deletes least-recently-used objects down to 90% of max.
    /// Returns the number of objects evicted.
    pub async fn evict_if_needed(&self) -> Result<u64> {
        todo!("T7")
    }
    /// Spawns the periodic snapshot (60s) + eviction + metrics refresh task.
    pub fn spawn_background(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        todo!("T7")
    }
}
