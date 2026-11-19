//! Steam: passive caching keyed by the Steam client user agent; managed updates TODO(T11).
use super::{Platform, domains};

pub struct Steam {}

impl Steam {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for Steam {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for Steam {
    fn id(&self) -> &'static str {
        "steam"
    }
    fn name(&self) -> &'static str {
        "Steam"
    }
    fn description(&self) -> &'static str {
        "Caches Steam depot downloads. The Steam client switches to HTTP when lancache.steamcontent.com resolves to the cache."
    }
    fn domains(&self) -> &'static [&'static str] {
        domains::STEAM
    }
    fn matches_user_agent(&self, user_agent: &str) -> bool {
        user_agent.starts_with("Valve/Steam HTTP Client 1.0")
    }
}
