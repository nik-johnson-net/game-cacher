use super::{Platform, domains};

/// EA app / Origin: passive caching only (no usable open download protocol).
pub struct Origin;

impl Origin {
    /// Platform identifier (`Platform::id`), also the cache key namespace.
    pub const ID: &'static str = "origin";
}

impl Platform for Origin {
    fn id(&self) -> &'static str {
        Self::ID
    }
    fn name(&self) -> &'static str {
        "EA app / Origin"
    }
    fn description(&self) -> &'static str {
        "Caches EA app and Origin downloads served over HTTP. HTTPS traffic is passed through."
    }
    fn domains(&self) -> &'static [&'static str] {
        domains::ORIGIN
    }
}
