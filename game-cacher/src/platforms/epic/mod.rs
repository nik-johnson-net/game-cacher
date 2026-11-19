//! Epic Games Store: passive caching; managed updates TODO(T10).
use super::{Platform, domains};

pub struct Epic {}

impl Epic {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for Epic {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for Epic {
    fn id(&self) -> &'static str {
        "epic"
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
}
