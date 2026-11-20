//! Blizzard Battle.net: passive caching plus managed updates (TACT/NGDP prefill).

mod formats;
mod managed;
mod tact;

use super::{ManagedUpdates, Platform, domains};

const DEFAULT_PATCH_BASE: &str = "http://us.patch.battle.net:1119";

pub struct Blizzard {
    patch_base: String,
}

impl Blizzard {
    /// Platform identifier (`Platform::id`), also the cache key namespace.
    pub const ID: &'static str = "blizzard";

    pub fn new() -> Self {
        Self::with_patch_base(DEFAULT_PATCH_BASE)
    }

    /// Uses a different TACT patch server (tests).
    pub fn with_patch_base(url: &str) -> Self {
        Self {
            patch_base: url.trim_end_matches('/').to_string(),
        }
    }
}

impl Default for Blizzard {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for Blizzard {
    fn id(&self) -> &'static str {
        Self::ID
    }
    fn name(&self) -> &'static str {
        "Blizzard Battle.net"
    }
    fn description(&self) -> &'static str {
        "Caches Battle.net game downloads and patches from Blizzard's CDNs."
    }
    fn domains(&self) -> &'static [&'static str] {
        domains::BLIZZARD
    }
    fn hide_etag(&self) -> bool {
        true
    }
    fn managed(&self) -> Option<&dyn ManagedUpdates> {
        Some(self)
    }
}
