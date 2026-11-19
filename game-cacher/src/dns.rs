//! DNS server: answers A/AAAA for enabled platforms' domains with the cache IPs,
//! forwards everything else to `--dns-forward` (or REFUSED when unset).

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use hickory_proto::op::{Message, MessageType, OpCode, ResponseCode};
use hickory_proto::rr::{RData, Record, RecordType, rdata};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};

use crate::metrics::Metrics;
use crate::platforms::{Platforms, host_matches};
use crate::settings::SettingsStore;

const OVERRIDE_TTL: u32 = 600;
const FORWARD_TIMEOUT: Duration = Duration::from_secs(3);
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(10);

pub struct DnsServer {
    cache_ips: Vec<IpAddr>,
    forwarders: Vec<SocketAddr>,
    platforms: Arc<Platforms>,
    settings: Arc<SettingsStore>,
    metrics: Arc<Metrics>,
}

impl DnsServer {
    pub fn new(
        cache_ips: Vec<IpAddr>,
        forwarders: Vec<SocketAddr>,
        platforms: Arc<Platforms>,
        settings: Arc<SettingsStore>,
        metrics: Arc<Metrics>,
    ) -> Arc<Self> {
        Arc::new(Self {
            cache_ips,
            forwarders,
            platforms,
            settings,
            metrics,
        })
    }

    /// Binds UDP and TCP on `addr` (port 0 → same ephemeral port for both) and
    /// serves in background tasks. Returns the bound address.
    pub async fn bind(self: &Arc<Self>, addr: SocketAddr) -> Result<SocketAddr> {
        let udp = UdpSocket::bind(addr)
            .await
            .with_context(|| format!("binding DNS UDP {addr}"))?;
        let bound = udp.local_addr()?;
        let tcp_addr = SocketAddr::new(bound.ip(), bound.port());
        let tcp = TcpListener::bind(tcp_addr)
            .await
            .with_context(|| format!("binding DNS TCP {tcp_addr}"))?;
        tokio::spawn(self.clone().serve_udp(Arc::new(udp)));
        tokio::spawn(self.clone().serve_tcp(tcp));
        Ok(bound)
    }

    async fn serve_udp(self: Arc<Self>, socket: Arc<UdpSocket>) {
        let mut buf = vec![0u8; 4096];
        loop {
            let (n, peer) = match socket.recv_from(&mut buf).await {
                Ok(v) => v,
                Err(e) => {
                    log::debug!("dns udp recv: {e}");
                    continue;
                }
            };
            let packet = buf[..n].to_vec();
            let (this, socket) = (self.clone(), socket.clone());
            tokio::spawn(async move {
                if let Some(resp) = this.process(&packet, false).await
                    && let Err(e) = socket.send_to(&resp, peer).await
                {
                    log::debug!("dns udp send to {peer}: {e}");
                }
            });
        }
    }

    async fn serve_tcp(self: Arc<Self>, listener: TcpListener) {
        loop {
            match listener.accept().await {
                Ok((stream, peer)) => {
                    let this = self.clone();
                    tokio::spawn(async move {
                        if let Err(e) = this.tcp_conn(stream).await {
                            log::debug!("dns tcp {peer}: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    log::warn!("dns tcp accept: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }
        }
    }

    async fn tcp_conn(&self, mut stream: TcpStream) -> Result<()> {
        loop {
            let mut len = [0u8; 2];
            match tokio::time::timeout(TCP_IDLE_TIMEOUT, stream.read_exact(&mut len)).await {
                Ok(Ok(_)) => {}
                // Peer closed or idle timeout: normal end of connection.
                _ => return Ok(()),
            }
            let len = u16::from_be_bytes(len) as usize;
            let mut packet = vec![0u8; len];
            tokio::time::timeout(TCP_IDLE_TIMEOUT, stream.read_exact(&mut packet))
                .await
                .map_err(|_| anyhow!("timed out reading query"))??;
            let Some(resp) = self.process(&packet, true).await else {
                return Ok(());
            };
            let resp_len = u16::try_from(resp.len()).context("response too large")?;
            stream.write_all(&resp_len.to_be_bytes()).await?;
            stream.write_all(&resp).await?;
        }
    }

    /// Handles one raw query. `None` means the packet was malformed and is dropped.
    async fn process(&self, raw: &[u8], tcp: bool) -> Option<Vec<u8>> {
        let req = match Message::from_vec(raw) {
            Ok(m) if m.message_type() == MessageType::Query => m,
            _ => {
                self.metrics.dns_query("error");
                return None;
            }
        };
        if let Some(resp) = self.override_response(&req).await {
            self.metrics.dns_query("override");
            return resp.to_vec().ok();
        }
        if self.forwarders.is_empty() {
            self.metrics.dns_query("refused");
            return error_response(&req, ResponseCode::Refused);
        }
        match self.forward(raw, tcp).await {
            Some(resp) => {
                self.metrics.dns_query("forward");
                Some(resp)
            }
            None => {
                self.metrics.dns_query("error");
                error_response(&req, ResponseCode::ServFail)
            }
        }
    }

    /// The authoritative answer if the query is for an enabled platform's domain.
    async fn override_response(&self, req: &Message) -> Option<Message> {
        if req.op_code() != OpCode::Query || req.queries().len() != 1 {
            return None;
        }
        let query = &req.queries()[0];
        let name = query.name().to_ascii().to_ascii_lowercase();
        let name = name.trim_end_matches('.');
        let settings = self.settings.get().await;
        let overridden = self.platforms.all().iter().any(|p| {
            settings.platform_enabled(p.id()) && p.domains().iter().any(|d| host_matches(d, name))
        });
        if !overridden {
            return None;
        }
        let mut resp = Message::new();
        resp.set_id(req.id())
            .set_message_type(MessageType::Response)
            .set_op_code(OpCode::Query)
            .set_authoritative(true)
            .set_recursion_desired(req.recursion_desired())
            .set_response_code(ResponseCode::NoError)
            .add_query(query.clone());
        for ip in &self.cache_ips {
            let rdata = match (ip, query.query_type()) {
                (IpAddr::V4(v4), RecordType::A) => RData::A(rdata::A(*v4)),
                (IpAddr::V6(v6), RecordType::AAAA) => RData::AAAA(rdata::AAAA(*v6)),
                _ => continue,
            };
            resp.add_answer(Record::from_rdata(
                query.name().clone(),
                OVERRIDE_TTL,
                rdata,
            ));
        }
        Some(resp)
    }

    /// Tries forwarders in order; relays the first response verbatim.
    async fn forward(&self, raw: &[u8], tcp: bool) -> Option<Vec<u8>> {
        for server in &self.forwarders {
            let attempt = if tcp {
                forward_tcp(*server, raw).await
            } else {
                forward_udp(*server, raw).await
            };
            match attempt {
                Ok(resp) => return Some(resp),
                Err(e) => log::warn!("dns forwarder {server} failed: {e:#}"),
            }
        }
        None
    }
}

fn error_response(req: &Message, code: ResponseCode) -> Option<Vec<u8>> {
    let mut resp = Message::error_msg(req.id(), req.op_code(), code);
    resp.set_recursion_desired(req.recursion_desired());
    resp.add_queries(req.queries().iter().cloned());
    resp.to_vec().ok()
}

async fn forward_udp(server: SocketAddr, raw: &[u8]) -> Result<Vec<u8>> {
    let local: SocketAddr = if server.is_ipv4() {
        ([0, 0, 0, 0], 0).into()
    } else {
        ([0u16; 8], 0).into()
    };
    let socket = UdpSocket::bind(local).await?;
    socket.connect(server).await?;
    socket.send(raw).await?;
    let id = raw.get(..2).unwrap_or_default();
    let exchange = async {
        let mut buf = vec![0u8; 65535];
        loop {
            let n = socket.recv(&mut buf).await?;
            // Ignore stray packets that do not answer our query.
            if n >= 2 && &buf[..2] == id {
                buf.truncate(n);
                return Ok::<_, std::io::Error>(buf);
            }
        }
    };
    let resp = tokio::time::timeout(FORWARD_TIMEOUT, exchange)
        .await
        .map_err(|_| anyhow!("timed out"))??;
    Ok(resp)
}

async fn forward_tcp(server: SocketAddr, raw: &[u8]) -> Result<Vec<u8>> {
    let exchange = async {
        let mut stream = TcpStream::connect(server).await?;
        let len = u16::try_from(raw.len()).context("query too large")?;
        stream.write_all(&len.to_be_bytes()).await?;
        stream.write_all(raw).await?;
        let mut len = [0u8; 2];
        stream.read_exact(&mut len).await?;
        let mut resp = vec![0u8; u16::from_be_bytes(len) as usize];
        stream.read_exact(&mut resp).await?;
        Ok::<_, anyhow::Error>(resp)
    };
    tokio::time::timeout(FORWARD_TIMEOUT, exchange)
        .await
        .map_err(|_| anyhow!("timed out"))?
}

#[cfg(test)]
mod tests {
    use hickory_proto::op::Query;
    use hickory_proto::rr::Name;
    use std::str::FromStr;

    use super::*;
    use crate::platforms;
    use crate::storage::MemoryStorage;

    async fn server(forwarders: Vec<SocketAddr>) -> Arc<DnsServer> {
        let settings = SettingsStore::load(Arc::new(MemoryStorage::new()))
            .await
            .unwrap();
        DnsServer::new(
            vec!["10.0.0.5".parse().unwrap(), "fd00::5".parse().unwrap()],
            forwarders,
            Arc::new(Platforms::new(platforms::all())),
            Arc::new(settings),
            Metrics::new(),
        )
    }

    fn query(name: &str, ty: RecordType) -> Vec<u8> {
        let mut m = Message::new();
        m.set_id(0x1234)
            .set_message_type(MessageType::Query)
            .set_recursion_desired(true)
            .add_query(Query::query(Name::from_str(name).unwrap(), ty));
        m.to_vec().unwrap()
    }

    async fn ask(s: &DnsServer, name: &str, ty: RecordType) -> Message {
        let raw = s.process(&query(name, ty), false).await.unwrap();
        Message::from_vec(&raw).unwrap()
    }

    #[tokio::test]
    async fn override_a_and_aaaa() {
        let s = server(vec![]).await;
        let r = ask(&s, "Lancache.SteamContent.com.", RecordType::A).await;
        assert_eq!(r.id(), 0x1234);
        assert!(r.authoritative());
        assert_eq!(r.response_code(), ResponseCode::NoError);
        assert_eq!(r.answers().len(), 1);
        assert_eq!(r.answers()[0].ttl(), 600);
        assert_eq!(
            r.answers()[0].data(),
            &RData::A(rdata::A("10.0.0.5".parse().unwrap()))
        );
        let r = ask(&s, "lancache.steamcontent.com.", RecordType::AAAA).await;
        assert_eq!(
            r.answers()[0].data(),
            &RData::AAAA(rdata::AAAA("fd00::5".parse().unwrap()))
        );
        // Other qtypes: NOERROR, empty.
        let r = ask(&s, "lancache.steamcontent.com.", RecordType::TXT).await;
        assert_eq!(r.response_code(), ResponseCode::NoError);
        assert!(r.answers().is_empty());
    }

    #[tokio::test]
    async fn wildcard_domains() {
        let s = server(vec![]).await;
        let r = ask(&s, "us.cdn.blizzard.com.", RecordType::A).await;
        assert_eq!(r.answers().len(), 1);
        // Apex of a wildcard-only pattern is covered by its own exact entry here;
        // a lookalike is not overridden.
        let r = ask(&s, "evilcdn.blizzard.com.", RecordType::A).await;
        assert_eq!(r.response_code(), ResponseCode::Refused);
    }

    #[tokio::test]
    async fn no_ip_of_family_gives_empty_noerror() {
        let s = DnsServer::new(
            vec!["10.0.0.5".parse().unwrap()],
            vec![],
            Arc::new(Platforms::new(platforms::all())),
            Arc::new(
                SettingsStore::load(Arc::new(MemoryStorage::new()))
                    .await
                    .unwrap(),
            ),
            Metrics::new(),
        );
        let r = ask(&s, "lancache.steamcontent.com.", RecordType::AAAA).await;
        assert_eq!(r.response_code(), ResponseCode::NoError);
        assert!(r.answers().is_empty());
    }

    #[tokio::test]
    async fn unknown_domain_refused_without_forwarders() {
        let s = server(vec![]).await;
        let r = ask(&s, "example.com.", RecordType::A).await;
        assert_eq!(r.response_code(), ResponseCode::Refused);
        assert!(s.metrics.encode().contains(r#"result="refused""#));
    }

    #[tokio::test]
    async fn disabled_platform_not_overridden() {
        let s = server(vec![]).await;
        s.settings
            .update(|st| {
                st.platforms.insert(
                    "steam".into(),
                    crate::settings::PlatformSettings { enabled: false },
                );
            })
            .await
            .unwrap();
        let r = ask(&s, "lancache.steamcontent.com.", RecordType::A).await;
        assert_eq!(r.response_code(), ResponseCode::Refused);
        // Other platforms stay overridden.
        let r = ask(&s, "cdn.blizzard.com.", RecordType::A).await;
        assert_eq!(r.answers().len(), 1);
    }

    #[tokio::test]
    async fn dead_forwarder_gives_servfail() {
        // A bound-then-dropped port: nothing answers, TCP refused.
        let tmp = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = tmp.local_addr().unwrap();
        drop(tmp);
        let s = server(vec![dead]).await;
        let raw = query("example.com.", RecordType::A);
        let resp = s.process(&raw, true).await.unwrap();
        let r = Message::from_vec(&resp).unwrap();
        assert_eq!(r.response_code(), ResponseCode::ServFail);
        assert!(s.metrics.encode().contains(r#"result="error""#));
    }

    #[tokio::test]
    async fn malformed_is_dropped() {
        let s = server(vec![]).await;
        assert!(s.process(&[1, 2, 3], false).await.is_none());
        assert!(s.process(&[], true).await.is_none());
        // A response packet is not a query.
        let mut m = Message::new();
        m.set_message_type(MessageType::Response);
        assert!(s.process(&m.to_vec().unwrap(), false).await.is_none());
    }
}
