//! Blizzard Battle.net: passive caching; managed updates (TACT) TODO(T9).
use super::{Platform, domains};

pub struct Blizzard {}

impl Blizzard {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for Blizzard {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for Blizzard {
    fn id(&self) -> &'static str {
        "blizzard"
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
}
