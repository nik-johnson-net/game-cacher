use super::{Platform, domains};

/// EA app / Origin: passive caching only (no usable open download protocol).
pub struct Origin;

impl Platform for Origin {
    fn id(&self) -> &'static str {
        "origin"
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
