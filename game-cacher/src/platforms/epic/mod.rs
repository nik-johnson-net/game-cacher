//! Epic Games Store: passive caching plus managed updates (auth-code login,
//! launcher assets, manifest-driven chunk prefill).
use tokio::sync::Mutex;

use super::{ManagedUpdates, Platform, domains};

mod client;
mod managed;
#[doc(hidden)]
pub mod manifest;

pub use client::Endpoints;

pub struct Epic {
    endpoints: Endpoints,
    token: Mutex<Option<managed::CachedToken>>,
    /// Serializes load-modify-save cycles of the state document. Never held across network calls.
    state_lock: Mutex<()>,
}

impl Epic {
    /// Platform identifier (`Platform::id`), also the cache key namespace.
    pub const ID: &'static str = "epic";

    pub fn new() -> Self {
        Self::with_endpoints(Endpoints::default())
    }

    /// Custom API base URLs (scheme + host), used by tests.
    pub fn with_endpoints(endpoints: Endpoints) -> Self {
        Self {
            endpoints,
            token: Mutex::new(None),
            state_lock: Mutex::new(()),
        }
    }
}

impl Default for Epic {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for Epic {
    fn id(&self) -> &'static str {
        Self::ID
    }
    fn name(&self) -> &'static str {
        "Epic Games"
    }
    fn description(&self) -> &'static str {
        "Caches Epic Games Store chunk downloads from Epic's CDNs."
    }
    fn domains(&self) -> &'static [&'static str] {
        domains::EPIC
    }
    fn managed(&self) -> Option<&dyn ManagedUpdates> {
        Some(self)
    }
}
