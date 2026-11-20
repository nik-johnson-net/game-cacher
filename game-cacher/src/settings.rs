//! UI-editable settings (`settings.json`) and per-platform state documents
//! (`state/<platform>.json`), both kept in storage — no database.

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use async_trait::async_trait;
use bytes::Bytes;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;

use crate::platforms::StateStore;
use crate::schedule::Schedule;
use crate::storage::DynStorage;

const SETTINGS_KEY: &str = "settings.json";

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlatformSettings {
    #[serde(default = "default_true")]
    pub enabled: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManagedSettings {
    #[serde(default)]
    pub selected: Vec<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub platforms: BTreeMap<String, PlatformSettings>,
    #[serde(default)]
    pub managed: BTreeMap<String, ManagedSettings>,
    #[serde(default)]
    pub update_schedule: Schedule,
}

impl Settings {
    /// Platforms are enabled unless explicitly disabled.
    pub fn platform_enabled(&self, id: &str) -> bool {
        self.platforms.get(id).map(|p| p.enabled).unwrap_or(true)
    }

    pub fn selected(&self, id: &str) -> Vec<String> {
        self.managed
            .get(id)
            .map(|m| m.selected.clone())
            .unwrap_or_default()
    }
}

fn default_true() -> bool {
    true
}

/// Cached settings document with write-through persistence.
pub struct SettingsStore {
    storage: DynStorage,
    current: RwLock<Settings>,
}

impl SettingsStore {
    pub async fn load(storage: DynStorage) -> Result<Self> {
        let current = match storage.get(SETTINGS_KEY).await? {
            Some(bytes) => serde_json::from_slice(&bytes).context("parsing settings.json")?,
            None => Settings::default(),
        };
        Ok(Self {
            storage,
            current: RwLock::new(current),
        })
    }

    pub async fn get(&self) -> Settings {
        self.current.read().await.clone()
    }

    /// Applies `f` and persists the result atomically with respect to other updates.
    pub async fn update<F>(&self, f: F) -> Result<Settings>
    where
        F: FnOnce(&mut Settings),
    {
        let mut guard = self.current.write().await;
        let mut next = guard.clone();
        f(&mut next);
        let json = serde_json::to_vec_pretty(&next)?;
        self.storage.put(SETTINGS_KEY, Bytes::from(json)).await?;
        *guard = next.clone();
        Ok(next)
    }

    pub fn state_store(&self, platform_id: &str) -> Arc<dyn StateStore> {
        Arc::new(StorageStateStore {
            storage: self.storage.clone(),
            key: format!("state/{platform_id}.json"),
        })
    }
}

struct StorageStateStore {
    storage: DynStorage,
    key: String,
}

#[async_trait]
impl StateStore for StorageStateStore {
    async fn load(&self) -> Result<serde_json::Value> {
        Ok(match self.storage.get(&self.key).await? {
            Some(b) => {
                serde_json::from_slice(&b).with_context(|| format!("parsing {}", self.key))?
            }
            None => serde_json::Value::Object(Default::default()),
        })
    }

    async fn save(&self, value: &serde_json::Value) -> Result<()> {
        self.storage
            .put(&self.key, Bytes::from(serde_json::to_vec_pretty(value)?))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::MemoryStorage;

    #[tokio::test]
    async fn defaults_and_persistence() {
        let storage: DynStorage = Arc::new(MemoryStorage::new());
        let s = SettingsStore::load(storage.clone()).await.unwrap();
        assert!(s.get().await.platform_enabled("steam"));
        assert_eq!(s.get().await.update_schedule, Schedule::default());

        s.update(|s| {
            s.platforms
                .insert("steam".into(), PlatformSettings { enabled: false });
            s.managed.insert(
                "epic".into(),
                ManagedSettings {
                    selected: vec!["Fortnite".into()],
                },
            );
            s.update_schedule = Schedule::parse("Europe/London", &["02:00", "16:00"]).unwrap();
            s.update_schedule.changed = Some(chrono::Utc::now());
        })
        .await
        .unwrap();

        let reloaded = SettingsStore::load(storage.clone())
            .await
            .unwrap()
            .get()
            .await;
        assert!(!reloaded.platform_enabled("steam"));
        assert_eq!(reloaded.selected("epic"), vec!["Fortnite".to_string()]);
        assert!(reloaded.selected("steam").is_empty());
        assert_eq!(reloaded.update_schedule, s.get().await.update_schedule);
        assert_eq!(reloaded.update_schedule.times.len(), 2);
    }

    #[tokio::test]
    async fn tolerant_parsing() {
        let storage: DynStorage = Arc::new(MemoryStorage::new());
        storage
            .put(
                SETTINGS_KEY,
                Bytes::from_static(br#"{"platforms":{"epic":{}},"update_interval_minutes":60}"#),
            )
            .await
            .unwrap();
        let s = SettingsStore::load(storage.clone())
            .await
            .unwrap()
            .get()
            .await;
        assert!(s.platform_enabled("epic"));
        // The retired interval setting is ignored in favour of the default schedule.
        assert_eq!(s.update_schedule, Schedule::default());

        storage
            .put(SETTINGS_KEY, Bytes::from_static(b"not json"))
            .await
            .unwrap();
        assert!(SettingsStore::load(storage).await.is_err());
    }

    #[tokio::test]
    async fn state_store_roundtrip() {
        let storage: DynStorage = Arc::new(MemoryStorage::new());
        let s = SettingsStore::load(storage).await.unwrap();
        let st = s.state_store("blizzard");
        assert_eq!(st.load().await.unwrap(), serde_json::json!({}));
        st.save(&serde_json::json!({"wow": "abc"})).await.unwrap();
        assert_eq!(st.load().await.unwrap()["wow"], "abc");
    }
}
