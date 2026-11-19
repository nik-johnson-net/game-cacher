//! LRU index + eviction. Tracks bytes and last access per cached object in memory,
//! snapshots to `index/lru.json`, and evicts whole objects oldest-first.
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use bytes::Bytes;
use serde::{Deserialize, Serialize};

use super::key::object_prefix_of;
use crate::metrics::Metrics;
use crate::storage::DynStorage;

pub const INDEX_KEY: &str = "index/lru.json";
const DEFAULT_PERIOD: Duration = Duration::from_secs(60);

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

/// Called with the object prefix whenever an object leaves the index
/// (eviction, `remove_object`, `remove_identifier`).
pub type RemoveHook = Arc<dyn Fn(&str) + Send + Sync>;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    identifier: String,
    bytes: u64,
    /// Unix seconds.
    last_access: u64,
    /// Tie-breaker within the same second (not persisted).
    #[serde(skip)]
    seq: u64,
}

#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    objects: BTreeMap<String, Entry>,
}

#[derive(Default)]
struct Inner {
    objects: HashMap<String, Entry>,
    total: u64,
    seq: u64,
}

impl Inner {
    fn touch(&mut self, prefix: &str, identifier: &str) -> &mut Entry {
        self.seq += 1;
        let seq = self.seq;
        let e = self
            .objects
            .entry(prefix.to_string())
            .or_insert_with(|| Entry {
                identifier: identifier.to_string(),
                bytes: 0,
                last_access: 0,
                seq: 0,
            });
        e.last_access = now_secs();
        e.seq = seq;
        e
    }

    fn remove(&mut self, prefix: &str) -> bool {
        match self.objects.remove(prefix) {
            Some(e) => {
                self.total = self.total.saturating_sub(e.bytes);
                true
            }
            None => false,
        }
    }
}

/// Tracks size and last access per cached object (keyed by `ObjectKey::prefix()`),
/// persisted to `index/lru.json`.
pub struct LruIndex {
    storage: DynStorage,
    max_bytes: Option<u64>,
    metrics: Arc<Metrics>,
    inner: Mutex<Inner>,
    dirty: AtomicBool,
    on_remove: RwLock<Option<RemoveHook>>,
    evicting: tokio::sync::Mutex<()>,
    /// Woken when a write pushes the total over the limit, so eviction doesn't wait for the tick.
    over_limit: tokio::sync::Notify,
    /// True when the snapshot was loaded (not rebuilt) and should be reconciled with storage.
    needs_reconcile: AtomicBool,
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl LruIndex {
    /// Loads `index/lru.json`, or rebuilds by listing `cache/` when missing/corrupt.
    pub async fn load_or_rebuild(
        storage: DynStorage,
        max_bytes: Option<u64>,
        metrics: Arc<Metrics>,
    ) -> Result<Arc<Self>> {
        let loaded = match storage.get(INDEX_KEY).await.context("reading LRU index")? {
            Some(bytes) => match serde_json::from_slice::<Snapshot>(&bytes) {
                Ok(s) => Some(s.objects),
                Err(e) => {
                    log::warn!("corrupt {INDEX_KEY} ({e}), rebuilding from storage");
                    None
                }
            },
            None => None,
        };
        let rebuilt = loaded.is_none();
        let objects = match loaded {
            Some(o) => o,
            None => rebuild(&storage).await?,
        };
        let mut sorted: Vec<(String, Entry)> = objects.into_iter().collect();
        sorted.sort_by_key(|(_, e)| e.last_access);
        let mut inner = Inner::default();
        for (prefix, mut e) in sorted {
            inner.seq += 1;
            e.seq = inner.seq;
            inner.total += e.bytes;
            inner.objects.insert(prefix, e);
        }
        log::info!(
            "LRU index: {} objects, {} bytes{}",
            inner.objects.len(),
            inner.total,
            if rebuilt { " (rebuilt)" } else { "" }
        );
        Ok(Arc::new(Self {
            storage,
            max_bytes,
            metrics,
            inner: Mutex::new(inner),
            dirty: AtomicBool::new(rebuilt),
            on_remove: RwLock::new(None),
            evicting: tokio::sync::Mutex::new(()),
            over_limit: tokio::sync::Notify::new(),
            needs_reconcile: AtomicBool::new(!rebuilt),
        }))
    }

    /// Registers the callback run for every object that leaves the index.
    pub fn set_remove_hook(&self, hook: RemoveHook) {
        *self.on_remove.write().unwrap_or_else(|e| e.into_inner()) = Some(hook);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn notify_removed(&self, prefixes: &[String]) {
        let hook = self
            .on_remove
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(hook) = hook {
            prefixes.iter().for_each(|p| hook(p));
        }
    }

    /// Marks an object as used now.
    pub fn touch(&self, object_prefix: &str, identifier: &str) {
        self.lock().touch(object_prefix, identifier);
        self.dirty.store(true, Ordering::Relaxed);
    }

    /// Records `bytes` newly stored for an object (slice or meta write) and touches it.
    pub fn record_write(&self, object_prefix: &str, identifier: &str, bytes: u64) {
        let mut inner = self.lock();
        inner.touch(object_prefix, identifier).bytes += bytes;
        inner.total += bytes;
        let over = self.max_bytes.is_some_and(|m| inner.total > m);
        drop(inner);
        self.dirty.store(true, Ordering::Relaxed);
        if over {
            self.over_limit.notify_one();
        }
    }

    /// Brings a loaded snapshot in line with storage: objects written after the last
    /// snapshot (e.g. before a crash) are added, deleted ones dropped, sizes corrected.
    /// Entries touched after the listing started are left alone.
    pub async fn reconcile(&self) -> Result<()> {
        let started = now_secs();
        let on_disk = rebuild(&self.storage).await?;
        let (mut added, mut removed) = (0, Vec::new());
        {
            let mut inner = self.lock();
            let stale: Vec<String> = inner
                .objects
                .iter()
                .filter(|(p, e)| e.last_access < started && !on_disk.contains_key(*p))
                .map(|(p, _)| p.clone())
                .collect();
            for p in stale {
                inner.remove(&p);
                removed.push(p);
            }
            for (prefix, disk) in on_disk {
                match inner.objects.get_mut(&prefix) {
                    Some(e) if e.last_access < started && e.bytes != disk.bytes => {
                        let old = std::mem::replace(&mut e.bytes, disk.bytes);
                        inner.total = inner.total - old + disk.bytes;
                    }
                    Some(_) => {}
                    None => {
                        inner.total += disk.bytes;
                        inner.objects.insert(prefix, disk);
                        added += 1;
                    }
                }
            }
        }
        if added > 0 || !removed.is_empty() {
            log::info!(
                "LRU index reconciled: {added} objects added, {} removed",
                removed.len()
            );
            self.dirty.store(true, Ordering::Relaxed);
            self.notify_removed(&removed);
        }
        Ok(())
    }

    /// Forgets an object (after it was deleted from storage).
    pub fn remove_object(&self, object_prefix: &str) {
        if self.lock().remove(object_prefix) {
            self.dirty.store(true, Ordering::Relaxed);
        }
        self.notify_removed(&[object_prefix.to_string()]);
    }

    /// Forgets all objects of an identifier (after a platform purge).
    pub fn remove_identifier(&self, identifier: &str) {
        let removed: Vec<String> = {
            let mut inner = self.lock();
            let prefixes: Vec<String> = inner
                .objects
                .iter()
                .filter(|(_, e)| e.identifier == identifier)
                .map(|(p, _)| p.clone())
                .collect();
            prefixes.iter().for_each(|p| {
                inner.remove(p);
            });
            prefixes
        };
        self.dirty.store(true, Ordering::Relaxed);
        self.notify_removed(&removed);
    }

    pub fn usage(&self) -> Usage {
        let inner = self.lock();
        let mut usage = Usage {
            total_bytes: inner.total,
            objects: inner.objects.len() as u64,
            per_identifier: BTreeMap::new(),
        };
        for e in inner.objects.values() {
            let u = usage
                .per_identifier
                .entry(e.identifier.clone())
                .or_default();
            u.bytes += e.bytes;
            u.objects += 1;
        }
        usage
    }

    pub fn max_bytes(&self) -> Option<u64> {
        self.max_bytes
    }

    /// Writes `index/lru.json`.
    pub async fn snapshot(&self) -> Result<()> {
        self.dirty.store(false, Ordering::Relaxed);
        let json = {
            let inner = self.lock();
            let snap = Snapshot {
                version: 1,
                objects: inner
                    .objects
                    .iter()
                    .map(|(k, v)| (k.clone(), v.clone()))
                    .collect(),
            };
            serde_json::to_vec(&snap)?
        };
        let res = self.storage.put(INDEX_KEY, Bytes::from(json)).await;
        if res.is_err() {
            self.dirty.store(true, Ordering::Relaxed);
        }
        res.context("writing LRU index")
    }

    /// Picks least-recently-used objects to drop so the total ends at or below 90% of max.
    fn pick_victims(&self, max: u64) -> Vec<(String, String)> {
        let inner = self.lock();
        if inner.total <= max {
            return Vec::new();
        }
        let target = (max as u128 * 9 / 10) as u64;
        let mut entries: Vec<(&String, &Entry)> = inner.objects.iter().collect();
        entries.sort_by_key(|(_, e)| (e.last_access, e.seq));
        let mut total = inner.total;
        let mut victims = Vec::new();
        for (prefix, e) in entries {
            if total <= target {
                break;
            }
            total = total.saturating_sub(e.bytes);
            victims.push((prefix.clone(), e.identifier.clone()));
        }
        victims
    }

    /// If over the limit, deletes least-recently-used objects down to 90% of max.
    /// Returns the number of objects evicted.
    pub async fn evict_if_needed(&self) -> Result<u64> {
        let Some(max) = self.max_bytes else {
            return Ok(0);
        };
        let _guard = self.evicting.lock().await;
        let mut evicted = 0;
        for (prefix, identifier) in self.pick_victims(max) {
            if let Err(e) = self.storage.delete_prefix(&format!("{prefix}/")).await {
                log::warn!("evicting {prefix}: {e:#}");
                continue;
            }
            self.remove_object(&prefix);
            self.metrics.eviction(&identifier);
            evicted += 1;
        }
        if evicted > 0 {
            log::info!("evicted {evicted} objects (max {max} bytes)");
        }
        Ok(evicted)
    }

    /// One maintenance pass: evict, snapshot if changed, refresh storage gauges.
    /// `seen` remembers identifiers so emptied ones are reset to zero.
    pub async fn maintain(&self, seen: &mut BTreeSet<String>) {
        if let Err(e) = self.evict_if_needed().await {
            log::warn!("eviction failed: {e:#}");
        }
        if self.dirty.load(Ordering::Relaxed)
            && let Err(e) = self.snapshot().await
        {
            log::warn!("{e:#}");
        }
        let usage = self.usage();
        seen.extend(usage.per_identifier.keys().cloned());
        for id in seen.iter() {
            let u = usage.per_identifier.get(id).cloned().unwrap_or_default();
            self.metrics.set_storage(id, u.bytes, u.objects);
        }
        self.metrics.set_storage_max(self.max_bytes);
    }

    /// Spawns the periodic snapshot (60s) + eviction + metrics refresh task.
    pub fn spawn_background(self: &Arc<Self>) -> tokio::task::JoinHandle<()> {
        self.spawn_background_every(DEFAULT_PERIOD)
    }

    /// Like `spawn_background` with a custom period (tests). The first pass runs immediately.
    pub fn spawn_background_every(
        self: &Arc<Self>,
        period: Duration,
    ) -> tokio::task::JoinHandle<()> {
        let index = self.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(period);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let mut seen = BTreeSet::new();
            if index.needs_reconcile.swap(false, Ordering::Relaxed)
                && let Err(e) = index.reconcile().await
            {
                log::warn!("reconciling LRU index: {e:#}");
            }
            loop {
                tokio::select! {
                    _ = tick.tick() => {}
                    _ = index.over_limit.notified() => {}
                }
                index.maintain(&mut seen).await;
            }
        })
    }
}

/// Rebuilds the index by listing every key under `cache/`.
async fn rebuild(storage: &DynStorage) -> Result<BTreeMap<String, Entry>> {
    let now = now_secs();
    let mut objects: BTreeMap<String, Entry> = BTreeMap::new();
    for info in storage.list("cache/").await.context("listing cache/")? {
        let Some((id, prefix)) = object_prefix_of(&info.key) else {
            continue;
        };
        objects
            .entry(prefix.to_string())
            .or_insert_with(|| Entry {
                identifier: id.to_string(),
                bytes: 0,
                last_access: now,
                seq: 0,
            })
            .bytes += info.size;
    }
    Ok(objects)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cache::key::ObjectKey;
    use crate::storage::MemoryStorage;

    async fn put_object(storage: &DynStorage, id: &str, path: &str, size: usize) -> String {
        let k = ObjectKey::new(id, path);
        storage
            .put(&k.slice_key(0), Bytes::from(vec![0u8; size]))
            .await
            .unwrap();
        storage
            .put(&k.meta_key(), Bytes::from_static(b"{}"))
            .await
            .unwrap();
        k.prefix()
    }

    async fn index(storage: &DynStorage, max: Option<u64>) -> Arc<LruIndex> {
        LruIndex::load_or_rebuild(storage.clone(), max, Metrics::new())
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn rebuild_snapshot_and_corrupt() {
        let storage: DynStorage = Arc::new(MemoryStorage::new());
        put_object(&storage, "steam", "/a", 100).await;
        put_object(&storage, "epic", "/b", 50).await;
        storage
            .put("settings.json", Bytes::from_static(b"{}"))
            .await
            .unwrap();
        let idx = index(&storage, Some(1000)).await;
        let u = idx.usage();
        assert_eq!((u.total_bytes, u.objects), (154, 2));
        assert_eq!(u.per_identifier["steam"].bytes, 102);
        assert_eq!(idx.max_bytes(), Some(1000));

        idx.record_write("cache/x/aa/bb/c", "origin", 10);
        idx.snapshot().await.unwrap();
        let reloaded = index(&storage, None).await;
        assert_eq!(reloaded.usage(), idx.usage());

        storage
            .put(INDEX_KEY, Bytes::from_static(b"nope"))
            .await
            .unwrap();
        // Corrupt snapshot: rebuilt from storage, so the index-only object is gone.
        assert_eq!(index(&storage, None).await.usage().objects, 2);
    }

    #[tokio::test]
    async fn remove_and_hook() {
        let storage: DynStorage = Arc::new(MemoryStorage::new());
        let idx = index(&storage, None).await;
        let removed = Arc::new(Mutex::new(Vec::<String>::new()));
        let r = removed.clone();
        idx.set_remove_hook(Arc::new(move |p| r.lock().unwrap().push(p.to_string())));
        idx.record_write("p1", "steam", 10);
        idx.record_write("p2", "steam", 10);
        idx.record_write("p3", "epic", 5);
        idx.touch("p3", "epic");
        idx.remove_object("p3");
        assert_eq!(idx.usage().total_bytes, 20);
        idx.remove_identifier("steam");
        assert_eq!(idx.usage(), Usage::default());
        let mut got = removed.lock().unwrap().clone();
        got.sort();
        assert_eq!(got, vec!["p1", "p2", "p3"]);
        assert_eq!(idx.evict_if_needed().await.unwrap(), 0); // unbounded
    }

    #[tokio::test]
    async fn reconcile_adds_missing_and_drops_deleted() {
        let storage: DynStorage = Arc::new(MemoryStorage::new());
        let a = put_object(&storage, "steam", "/a", 100).await;
        let idx = index(&storage, None).await;
        idx.snapshot().await.unwrap();
        // Simulate a crash: objects written after the snapshot, and one deleted behind our back.
        let b = put_object(&storage, "epic", "/b", 50).await;
        storage.delete_prefix(&format!("{a}/")).await.unwrap();
        let idx = index(&storage, None).await;
        assert_eq!(
            idx.usage().total_bytes,
            102,
            "stale snapshot before reconcile"
        );
        // Make the loaded entry look older than the reconcile start.
        idx.lock().objects.get_mut(&a).unwrap().last_access = 1;
        idx.reconcile().await.unwrap();
        let u = idx.usage();
        assert_eq!((u.total_bytes, u.objects), (52, 1));
        assert!(u.per_identifier.contains_key("epic"));
        assert!(idx.lock().objects.contains_key(&b));
    }

    #[tokio::test]
    async fn write_over_limit_wakes_background_eviction() {
        let storage: DynStorage = Arc::new(MemoryStorage::new());
        let a = put_object(&storage, "steam", "/a", 100).await;
        let idx = index(&storage, Some(150)).await;
        let task = idx.spawn_background_every(Duration::from_secs(3600));
        tokio::time::sleep(Duration::from_millis(50)).await;
        let b = ObjectKey::new("steam", "/b").prefix();
        storage
            .put(&format!("{b}/0"), Bytes::from(vec![0u8; 100]))
            .await
            .unwrap();
        idx.record_write(&b, "steam", 100);
        for _ in 0..50 {
            if idx.usage().total_bytes <= 137 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(idx.usage().total_bytes, 100);
        assert!(
            storage.list(&format!("{a}/")).await.unwrap().is_empty(),
            "oldest evicted"
        );
        task.abort();
    }

    #[tokio::test]
    async fn evicts_oldest_first() {
        let storage: DynStorage = Arc::new(MemoryStorage::new());
        let a = put_object(&storage, "steam", "/a", 98).await;
        let b = put_object(&storage, "steam", "/b", 98).await;
        let c = put_object(&storage, "steam", "/c", 98).await;
        let idx = index(&storage, Some(250)).await;
        idx.touch(&b, "steam");
        idx.touch(&a, "steam");
        idx.touch(&c, "steam");
        // 300 > 250 → evict down to <= 225: drop b (oldest), leaving 200.
        assert_eq!(idx.evict_if_needed().await.unwrap(), 1);
        assert!(storage.list(&format!("{b}/")).await.unwrap().is_empty());
        assert!(!storage.list(&format!("{a}/")).await.unwrap().is_empty());
        assert_eq!(idx.usage().total_bytes, 200);
        assert_eq!(idx.evict_if_needed().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn background_snapshots_and_publishes() {
        let storage: DynStorage = Arc::new(MemoryStorage::new());
        let metrics = Metrics::new();
        let idx = LruIndex::load_or_rebuild(storage.clone(), Some(1 << 20), metrics.clone())
            .await
            .unwrap();
        idx.record_write("cache/steam/aa/bb/x", "steam", 42);
        let task = idx.spawn_background_every(Duration::from_millis(20));
        for _ in 0..100 {
            if storage.exists(INDEX_KEY).await.unwrap() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        task.abort();
        assert!(storage.exists(INDEX_KEY).await.unwrap());
        let text = metrics.encode();
        assert!(
            text.contains(r#"gamecacher_storage_used_bytes{platform="steam"} 42"#),
            "{text}"
        );
        assert!(
            text.contains("gamecacher_storage_max_bytes 1048576"),
            "{text}"
        );
    }
}
