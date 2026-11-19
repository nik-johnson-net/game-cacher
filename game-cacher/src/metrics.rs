//! OpenMetrics registry. Modules record through the typed helpers on `Metrics`.

use std::sync::Arc;

use prometheus_client::encoding::EncodeLabelSet;
use prometheus_client::encoding::text::encode;
use prometheus_client::metrics::counter::Counter;
use prometheus_client::metrics::family::Family;
use prometheus_client::metrics::gauge::Gauge;
use prometheus_client::registry::Registry;

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct RequestLabels {
    pub platform: String,
    pub result: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct BytesLabels {
    pub platform: String,
    pub source: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct PlatformLabel {
    pub platform: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
pub struct ResultLabel {
    pub result: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CacheResult {
    Hit,
    Miss,
    Bypass,
}

impl CacheResult {
    pub fn as_str(self) -> &'static str {
        match self {
            CacheResult::Hit => "hit",
            CacheResult::Miss => "miss",
            CacheResult::Bypass => "bypass",
        }
    }
}

pub struct Metrics {
    registry: Registry,
    requests: Family<RequestLabels, Counter>,
    bytes_served: Family<BytesLabels, Counter>,
    upstream_errors: Family<PlatformLabel, Counter>,
    storage_used: Family<PlatformLabel, Gauge>,
    storage_objects: Family<PlatformLabel, Gauge>,
    storage_max: Gauge,
    evictions: Family<PlatformLabel, Counter>,
    dns_queries: Family<ResultLabel, Counter>,
    sni_connections: Family<ResultLabel, Counter>,
    sni_active: Gauge,
    update_runs: Family<RequestLabels, Counter>,
    update_bytes: Family<PlatformLabel, Counter>,
}

/// Per-platform counters read back for the dashboard.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize)]
pub struct PlatformCounters {
    pub requests: u64,
    pub hits: u64,
    pub misses: u64,
    pub bypass: u64,
    pub bytes_from_cache: u64,
    pub bytes_from_upstream: u64,
}

fn p(platform: &str) -> PlatformLabel {
    PlatformLabel {
        platform: platform.to_string(),
    }
}

fn r(result: &str) -> ResultLabel {
    ResultLabel {
        result: result.to_string(),
    }
}

impl Metrics {
    pub fn new() -> Arc<Self> {
        let mut registry = Registry::with_prefix("gamecacher");
        let m = Metrics {
            requests: Family::default(),
            bytes_served: Family::default(),
            upstream_errors: Family::default(),
            storage_used: Family::default(),
            storage_objects: Family::default(),
            storage_max: Gauge::default(),
            evictions: Family::default(),
            dns_queries: Family::default(),
            sni_connections: Family::default(),
            sni_active: Gauge::default(),
            update_runs: Family::default(),
            update_bytes: Family::default(),
            registry: Registry::default(),
        };
        registry.register(
            "requests",
            "Cache proxy requests by result",
            m.requests.clone(),
        );
        registry.register(
            "bytes_served",
            "Bytes served to clients by source",
            m.bytes_served.clone(),
        );
        registry.register(
            "upstream_errors",
            "Failed upstream fetches",
            m.upstream_errors.clone(),
        );
        registry.register(
            "storage_used_bytes",
            "Cached bytes per platform",
            m.storage_used.clone(),
        );
        registry.register(
            "storage_objects",
            "Cached objects per platform",
            m.storage_objects.clone(),
        );
        registry.register(
            "storage_max_bytes",
            "Configured cache size limit (0 = unbounded)",
            m.storage_max.clone(),
        );
        registry.register("evictions", "Objects evicted by LRU", m.evictions.clone());
        registry.register(
            "dns_queries",
            "DNS queries by result",
            m.dns_queries.clone(),
        );
        registry.register(
            "sni_connections",
            "SNI passthrough connections by result",
            m.sni_connections.clone(),
        );
        registry.register(
            "sni_active_connections",
            "Open SNI passthrough connections",
            m.sni_active.clone(),
        );
        registry.register(
            "managed_update_runs",
            "Managed update runs by result",
            m.update_runs.clone(),
        );
        registry.register(
            "managed_update_bytes",
            "Bytes downloaded by managed updates",
            m.update_bytes.clone(),
        );
        Arc::new(Metrics { registry, ..m })
    }

    pub fn encode(&self) -> String {
        let mut out = String::new();
        encode(&mut out, &self.registry).expect("encoding metrics to String cannot fail");
        out
    }

    pub fn request(&self, platform: &str, result: CacheResult) {
        self.requests
            .get_or_create(&RequestLabels {
                platform: platform.into(),
                result: result.as_str().into(),
            })
            .inc();
    }

    pub fn bytes_served(&self, platform: &str, from_cache: bool, bytes: u64) {
        let source = if from_cache { "cache" } else { "upstream" };
        self.bytes_served
            .get_or_create(&BytesLabels {
                platform: platform.into(),
                source: source.into(),
            })
            .inc_by(bytes);
    }

    pub fn upstream_error(&self, platform: &str) {
        self.upstream_errors.get_or_create(&p(platform)).inc();
    }

    pub fn set_storage(&self, platform: &str, bytes: u64, objects: u64) {
        self.storage_used
            .get_or_create(&p(platform))
            .set(bytes as i64);
        self.storage_objects
            .get_or_create(&p(platform))
            .set(objects as i64);
    }

    pub fn set_storage_max(&self, bytes: Option<u64>) {
        self.storage_max.set(bytes.unwrap_or(0) as i64);
    }

    pub fn eviction(&self, platform: &str) {
        self.evictions.get_or_create(&p(platform)).inc();
    }

    /// `result`: override | forward | refused | error
    pub fn dns_query(&self, result: &str) {
        self.dns_queries.get_or_create(&r(result)).inc();
    }

    /// `result`: ok | error | loop | rejected
    pub fn sni_connection(&self, result: &str) {
        self.sni_connections.get_or_create(&r(result)).inc();
    }

    pub fn sni_active_delta(&self, delta: i64) {
        self.sni_active.inc_by(delta);
    }

    pub fn update_run(&self, platform: &str, ok: bool) {
        let result = if ok { "ok" } else { "error" };
        self.update_runs
            .get_or_create(&RequestLabels {
                platform: platform.into(),
                result: result.into(),
            })
            .inc();
    }

    pub fn update_bytes(&self, platform: &str, bytes: u64) {
        self.update_bytes.get_or_create(&p(platform)).inc_by(bytes);
    }

    pub fn platform_counters(&self, platform: &str) -> PlatformCounters {
        let req = |res: CacheResult| {
            self.requests
                .get_or_create(&RequestLabels {
                    platform: platform.into(),
                    result: res.as_str().into(),
                })
                .get()
        };
        let bytes = |src: &str| {
            self.bytes_served
                .get_or_create(&BytesLabels {
                    platform: platform.into(),
                    source: src.into(),
                })
                .get()
        };
        let (hits, misses, bypass) = (
            req(CacheResult::Hit),
            req(CacheResult::Miss),
            req(CacheResult::Bypass),
        );
        PlatformCounters {
            requests: hits + misses + bypass,
            hits,
            misses,
            bypass,
            bytes_from_cache: bytes("cache"),
            bytes_from_upstream: bytes("upstream"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_and_encodes() {
        let m = Metrics::new();
        m.request("steam", CacheResult::Hit);
        m.request("steam", CacheResult::Miss);
        m.bytes_served("steam", true, 100);
        m.dns_query("override");
        m.set_storage("steam", 1000, 2);
        let c = m.platform_counters("steam");
        assert_eq!(
            (c.requests, c.hits, c.misses, c.bytes_from_cache),
            (2, 1, 1, 100)
        );
        let text = m.encode();
        assert!(
            text.contains(r#"gamecacher_requests_total{platform="steam",result="hit"} 1"#),
            "{text}"
        );
        assert!(text.contains("gamecacher_storage_used_bytes"), "{text}");
        assert!(text.ends_with("# EOF\n"));
    }
}
