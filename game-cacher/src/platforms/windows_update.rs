//! Windows Update: passive caching + managed prefill of a user-entered URL list (TODO T12).
use super::{CachePolicy, Platform, domains};

pub struct WindowsUpdate {}

impl WindowsUpdate {
    pub fn new() -> Self {
        Self {}
    }
}

impl Default for WindowsUpdate {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for WindowsUpdate {
    fn id(&self) -> &'static str {
        "windowsupdate"
    }
    fn name(&self) -> &'static str {
        "Windows Update"
    }
    fn description(&self) -> &'static str {
        "Caches Windows Update and Office CDN downloads. Specific update files can be prefetched by URL."
    }
    fn domains(&self) -> &'static [&'static str] {
        domains::WINDOWS_UPDATE
    }
    fn cache_policy(&self, path: &str) -> CachePolicy {
        // Certificate trust lists change often and must never be served stale.
        let file = path
            .rsplit('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let file = file.split('?').next().unwrap_or_default();
        if [
            "authrootstl.cab",
            "pinrulesstl.cab",
            "disallowedcertstl.cab",
        ]
        .contains(&file)
        {
            CachePolicy::Bypass
        } else {
            CachePolicy::Cache
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bypasses_trust_lists() {
        let w = WindowsUpdate::new();
        assert_eq!(
            w.cache_policy("/msdownload/update/v3/static/trustedr/en/authrootstl.cab"),
            CachePolicy::Bypass
        );
        assert_eq!(
            w.cache_policy("/d/msdownload/update/software/x.cab"),
            CachePolicy::Cache
        );
    }
}
