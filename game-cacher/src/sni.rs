//! TLS passthrough: peek the ClientHello SNI, resolve via upstream DNS, splice TCP. TODO(T3).
use std::sync::Arc;

use tokio::net::TcpListener;

use crate::metrics::Metrics;
use crate::upstream::Upstream;

/// Serves passthrough connections from `listener` to `<sni>:<upstream_port>` (443 in production).
pub fn spawn(
    _listener: TcpListener,
    _upstream: Upstream,
    _upstream_port: u16,
    _metrics: Arc<Metrics>,
) -> tokio::task::JoinHandle<()> {
    todo!("T3")
}
