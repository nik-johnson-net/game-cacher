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

/// Hickory resolver pointed only at the configured upstream DNS servers
/// (UDP and TCP); no system config and no hosts file.
pub struct DnsResolver {
    resolver: hickory_resolver::TokioResolver,
}

impl DnsResolver {
    pub fn new(servers: &[SocketAddr]) -> Result<Self> {
        use hickory_resolver::config::{NameServerConfig, ResolveHosts, ResolverConfig};
        use hickory_resolver::name_server::TokioConnectionProvider;
        use hickory_resolver::proto::xfer::Protocol;

        if servers.is_empty() {
            anyhow::bail!("no upstream DNS servers configured");
        }
        let mut config = ResolverConfig::new();
        for s in servers {
            config.add_name_server(NameServerConfig::new(*s, Protocol::Udp));
            config.add_name_server(NameServerConfig::new(*s, Protocol::Tcp));
        }
        let mut builder = hickory_resolver::Resolver::builder_with_config(
            config,
            TokioConnectionProvider::default(),
        );
        let opts = builder.options_mut();
        opts.use_hosts_file = ResolveHosts::Never;
        opts.attempts = 2;
        opts.timeout = Duration::from_secs(5);
        Ok(Self {
            resolver: builder.build(),
        })
    }
}

#[async_trait]
impl HostResolver for DnsResolver {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>> {
        if let Ok(ip) = host.parse::<IpAddr>() {
            return Ok(vec![ip]);
        }
        let lookup = self
            .resolver
            .lookup_ip(host)
            .await
            .map_err(|e| anyhow::anyhow!("resolving {host} via upstream DNS: {e}"))?;
        let ips: Vec<IpAddr> = lookup.iter().collect();
        if ips.is_empty() {
            anyhow::bail!("no addresses for {host}");
        }
        Ok(ips)
    }
}

/// Wraps a resolver and drops private, loopback, link-local and other non-public
/// addresses, so the proxy and SNI listener can't be steered into the LAN by a
/// name that resolves to an internal address. CDNs are always public.
pub struct PublicOnly(pub Arc<dyn HostResolver>);

#[async_trait]
impl HostResolver for PublicOnly {
    async fn resolve(&self, host: &str) -> Result<Vec<IpAddr>> {
        let addrs: Vec<IpAddr> = self
            .0
            .resolve(host)
            .await?
            .into_iter()
            .filter(|a| is_public(*a))
            .collect();
        if addrs.is_empty() {
            anyhow::bail!("{host} resolves only to non-public addresses");
        }
        Ok(addrs)
    }
}

/// Globally routable unicast address (rough equivalent of the unstable `IpAddr::is_global`).
pub fn is_public(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            !(v4.is_private()
                || v4.is_loopback()
                || v4.is_link_local()
                || v4.is_unspecified()
                || v4.is_broadcast()
                || v4.is_multicast()
                || v4.is_documentation()
                || o[0] == 0
                || (o[0] == 100 && (o[1] & 0xc0) == 64) // 100.64/10 CGNAT
                || (o[0] == 198 && (o[1] & 0xfe) == 18) // 198.18/15 benchmarking
                || o[0] >= 240)
        }
        IpAddr::V6(v6) => {
            if let Some(v4) = v6.to_ipv4_mapped() {
                return is_public(IpAddr::V4(v4));
            }
            let seg = v6.segments();
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                || (seg[0] & 0xfe00) == 0xfc00 // unique local
                || (seg[0] & 0xffc0) == 0xfe80 // link local
                || (seg[0] == 0x2001 && seg[1] == 0x0db8)) // documentation
        }
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
            let ips = inner.resolve(name.as_str()).await.map_err(
                |e| -> Box<dyn std::error::Error + Send + Sync> {
                    // Keep LoopDetected downcastable through reqwest's error chain.
                    match e.downcast::<LoopDetected>() {
                        Ok(l) => Box::new(l),
                        Err(e) => e.into(),
                    }
                },
            )?;
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

    #[tokio::test]
    async fn dns_resolver_ip_literal_and_config() {
        assert!(DnsResolver::new(&[]).is_err());
        // Nothing listens here, so a query would fail: literals must not query.
        let r = DnsResolver::new(&["127.0.0.1:9".parse().unwrap()]).unwrap();
        assert_eq!(
            r.resolve("203.0.113.7").await.unwrap(),
            vec!["203.0.113.7".parse::<IpAddr>().unwrap()]
        );
        assert_eq!(
            r.resolve("::1").await.unwrap(),
            vec!["::1".parse::<IpAddr>().unwrap()]
        );
    }

    #[test]
    fn public_addresses() {
        for ip in ["1.1.1.1", "151.101.1.1", "2606:4700::1111"] {
            assert!(is_public(ip.parse().unwrap()), "{ip}");
        }
        for ip in [
            "10.0.0.1",
            "172.16.5.4",
            "192.168.1.1",
            "127.0.0.1",
            "169.254.1.1",
            "0.0.0.0",
            "100.64.0.1",
            "255.255.255.255",
            "::1",
            "fd00::1",
            "fe80::1",
            "::ffff:192.168.1.1",
        ] {
            assert!(!is_public(ip.parse().unwrap()), "{ip}");
        }
    }

    #[tokio::test]
    async fn public_only() {
        let r = PublicOnly(Arc::new(StaticResolver("10.0.0.1".parse().unwrap())));
        assert!(r.resolve("internal.example.com").await.is_err());
        let r = PublicOnly(Arc::new(StaticResolver("1.1.1.1".parse().unwrap())));
        assert_eq!(r.resolve("x").await.unwrap().len(), 1);
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
