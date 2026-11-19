//! Upstream resolution and HTTP client. Upstream CDNs are always resolved via
//! `--upstream-dns` (never our own DNS server, which would point back at us),
//! and any answer that is one of our cache IPs is rejected as a loop.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use async_trait::async_trait;

#[async_trait]
pub trait HostResolver: Send + Sync + 'static {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>>;
}

/// Resolves every host to a fixed IP. Used by tests.
pub struct StaticResolver(pub IpAddr);

#[async_trait]
impl HostResolver for StaticResolver {
    async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>> {
        Ok(vec![self.0])
    }
}

/// Hickory resolver pointed at the configured upstream DNS servers. TODO(T3).
pub struct DnsResolver {}

impl DnsResolver {
    pub fn new(_servers: &[SocketAddr]) -> Result<Self> {
        todo!("T3")
    }
}

#[async_trait]
impl HostResolver for DnsResolver {
    async fn resolve(&self, _host: &str) -> Result<Vec<IpAddr>> {
        todo!("T3")
    }
}

/// Wraps a resolver and drops addresses that are our own cache IPs.
pub struct LoopGuard {
    inner: Arc<dyn HostResolver>,
    own: Vec<IpAddr>,
}

impl LoopGuard {
    pub fn new(inner: Arc<dyn HostResolver>, own: Vec<IpAddr>) -> Self {
        Self { inner, own }
    }
}

#[async_trait]
impl HostResolver for LoopGuard {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>> {
        let addrs = self.inner.resolve(host).await?;
        let filtered: Vec<IpAddr> = addrs
            .iter()
            .copied()
            .filter(|a| !self.own.contains(a))
            .collect();
        if filtered.is_empty() {
            if addrs.is_empty() {
                anyhow::bail!("no addresses for {host}");
            }
            return Err(LoopDetected(host.to_string()).into());
        }
        Ok(filtered)
    }
}

/// Upstream resolved to this cache: refusing to proxy to ourselves.
#[derive(Debug, thiserror::Error)]
#[error("loop detected: {0} resolves to this cache")]
pub struct LoopDetected(pub String);

struct ReqwestAdapter(Arc<dyn HostResolver>);

impl reqwest::dns::Resolve for ReqwestAdapter {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let inner = self.0.clone();
        Box::pin(async move {
            let ips = inner
                .resolve(name.as_str())
                .await
                .map_err(|e| -> Box<dyn std::error::Error + Send + Sync> { e.into() })?;
            let addrs: reqwest::dns::Addrs =
                Box::new(ips.into_iter().map(|ip| SocketAddr::new(ip, 0)));
            Ok(addrs)
        })
    }
}

/// Shared upstream access for the cache engine, SNI proxy and platform APIs.
#[derive(Clone)]
pub struct Upstream {
    client: reqwest::Client,
    resolver: Arc<dyn HostResolver>,
    http_port: u16,
    instance_id: String,
}

pub const PROCESSED_BY_HEADER: &str = "x-gamecacher-processed-by";

impl Upstream {
    pub fn new(
        resolver: Arc<dyn HostResolver>,
        http_port: u16,
        instance_id: String,
    ) -> Result<Self> {
        let client = reqwest::Client::builder()
            .dns_resolver(Arc::new(ReqwestAdapter(resolver.clone())))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(60))
            .redirect(reqwest::redirect::Policy::limited(10))
            .user_agent(concat!("game-cacher/", env!("CARGO_PKG_VERSION")))
            .build()?;
        Ok(Self {
            client,
            resolver,
            http_port,
            instance_id,
        })
    }

    /// Client with upstream DNS + loop guard; follows redirects (never cached).
    pub fn client(&self) -> &reqwest::Client {
        &self.client
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// `http://<host>[:port]<path_and_query>` honouring the upstream port override.
    pub fn http_url(&self, host: &str, path_and_query: &str) -> String {
        let host = host.split(':').next().unwrap_or(host);
        if self.http_port == 80 {
            format!("http://{host}{path_and_query}")
        } else {
            format!("http://{host}:{}{path_and_query}", self.http_port)
        }
    }

    pub async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>> {
        self.resolver.resolve(host).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn loop_guard() {
        let own: IpAddr = "10.0.0.2".parse().unwrap();
        let g = LoopGuard::new(Arc::new(StaticResolver(own)), vec![own]);
        let err = g.resolve("cdn.example.com").await.unwrap_err();
        assert!(err.downcast_ref::<LoopDetected>().is_some());
        let g = LoopGuard::new(
            Arc::new(StaticResolver("1.2.3.4".parse().unwrap())),
            vec![own],
        );
        assert_eq!(g.resolve("x").await.unwrap().len(), 1);
    }

    #[test]
    fn urls() {
        let u = Upstream::new(
            Arc::new(StaticResolver([127, 0, 0, 1].into())),
            80,
            "i".into(),
        )
        .unwrap();
        assert_eq!(u.http_url("a.com:80", "/x?y"), "http://a.com/x?y");
        let u = Upstream::new(
            Arc::new(StaticResolver([127, 0, 0, 1].into())),
            8081,
            "i".into(),
        )
        .unwrap();
        assert_eq!(u.http_url("a.com", "/x"), "http://a.com:8081/x");
    }
}
