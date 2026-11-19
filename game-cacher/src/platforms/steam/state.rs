//! `state/steam.json`:
//! `{ "account": "name", "apps": { "<appid>": { "buildid": "..", "depots": { "<depotid>": "<manifest gid>" } } } }`

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::platforms::StateStore;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SteamState {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account: Option<String>,
    #[serde(default)]
    pub apps: BTreeMap<String, AppState>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppState {
    /// Public-branch build id that was fully prefetched.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub buildid: Option<String>,
    /// depot id → manifest gid (decimal strings) that were prefetched.
    #[serde(default)]
    pub depots: BTreeMap<String, String>,
}

pub async fn load(store: &dyn StateStore) -> Result<SteamState> {
    let value = store.load().await?;
    serde_json::from_value(value).context("parsing steam state")
}

pub async fn save(store: &dyn StateStore, state: &SteamState) -> Result<()> {
    store.save(&serde_json::to_value(state)?).await
}
