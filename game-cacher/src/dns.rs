//! DNS server: answers A/AAAA for enabled platforms' domains with the cache IPs,
//! forwards everything else to `--dns-forward` (or REFUSED when unset). TODO(T3).
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use anyhow::Result;

use crate::metrics::Metrics;
use crate::platforms::Platforms;
use crate::settings::SettingsStore;

pub struct DnsServer {}

impl DnsServer {
    pub fn new(
        _cache_ips: Vec<IpAddr>,
        _forwarders: Vec<SocketAddr>,
        _platforms: Arc<Platforms>,
        _settings: Arc<SettingsStore>,
        _metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        todo!("T3")
    }

    /// Binds UDP and TCP on `addr` (port 0 → same ephemeral port for both) and
    /// serves in background tasks. Returns the bound address.
    pub async fn bind(self: &Arc<Self>, _addr: SocketAddr) -> Result<SocketAddr> {
        todo!("T3")
    }
}
