use std::sync::Arc;
use std::time::Instant;

use crate::api::auth::Auth;
use crate::cache::{CacheEngine, LruIndex};
use crate::config::Config;
use crate::metrics::Metrics;
use crate::platforms::Platforms;
use crate::secrets::DynSecrets;
use crate::settings::SettingsStore;
use crate::storage::DynStorage;
use crate::updates::UpdateManager;
use crate::upstream::Upstream;

/// Shared handles for the admin API.
pub struct AppState {
    pub config: Arc<Config>,
    pub storage: DynStorage,
    pub secrets: DynSecrets,
    pub settings: Arc<SettingsStore>,
    pub platforms: Arc<Platforms>,
    pub metrics: Arc<Metrics>,
    pub index: Arc<LruIndex>,
    pub engine: Arc<CacheEngine>,
    pub upstream: Upstream,
    pub updates: Arc<UpdateManager>,
    pub auth: Arc<Auth>,
    pub started: Instant,
}
