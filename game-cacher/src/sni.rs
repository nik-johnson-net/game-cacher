//! TLS passthrough: peek the ClientHello SNI, resolve via upstream DNS, splice TCP.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::metrics::Metrics;
use crate::platforms::Platforms;
use crate::upstream::{LoopDetected, Upstream};

const MAX_HELLO: usize = 16 * 1024;
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Serves passthrough connections from `listener` to `<sni>:<upstream_port>` (443 in production).
/// Only names owned by a platform are relayed, so the listener can't be used as an
/// open TCP relay into the LAN (IP literals never match a platform domain).
pub fn spawn(
    listener: TcpListener,
    upstream: Upstream,
    upstream_port: u16,
    platforms: Arc<Platforms>,
    metrics: Arc<Metrics>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let (stream, peer) = match listener.accept().await {
                Ok(v) => v,
                Err(e) => {
                    log::warn!("sni accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            };
            let upstream = upstream.clone();
            let metrics = metrics.clone();
            let platforms = platforms.clone();
            tokio::spawn(async move {
                if let Err(e) = handle(stream, &upstream, upstream_port, &platforms, &metrics).await
                {
                    log::debug!("sni connection from {peer}: {e:#}");
                }
            });
        }
    })
}

async fn handle(
    mut client: TcpStream,
    upstream: &Upstream,
    port: u16,
    platforms: &Platforms,
    metrics: &Metrics,
) -> Result<()> {
    let (buf, host) = match tokio::time::timeout(HELLO_TIMEOUT, read_hello(&mut client)).await {
        Ok(Ok(v)) => v,
        Ok(Err(e)) => {
            metrics.sni_connection("error");
            return Err(e);
        }
        Err(_) => {
            metrics.sni_connection("error");
            return Err(anyhow!("timed out waiting for ClientHello"));
        }
    };
    if platforms.by_host(&host).is_none() {
        metrics.sni_connection("rejected");
        return Err(anyhow!("{host} is not a platform domain"));
    }
    let ips = match upstream.resolve(&host).await {
        Ok(ips) if !ips.is_empty() => ips,
        Ok(_) => {
            metrics.sni_connection("error");
            return Err(anyhow!("no addresses for {host}"));
        }
        Err(e) => {
            let is_loop = e.downcast_ref::<LoopDetected>().is_some();
            metrics.sni_connection(if is_loop { "loop" } else { "error" });
            return Err(e.context(format!("resolving {host}")));
        }
    };
    let addr = SocketAddr::new(ips[0], port);
    let connected = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr)).await;
    let mut server = match connected {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => {
            metrics.sni_connection("error");
            return Err(e).context(format!("connecting to {host} ({addr})"));
        }
        Err(_) => {
            metrics.sni_connection("error");
            return Err(anyhow!("connect to {host} ({addr}) timed out"));
        }
    };
    if let Err(e) = server.write_all(&buf).await {
        metrics.sni_connection("error");
        return Err(e.into());
    }
    metrics.sni_connection("ok");
    metrics.sni_active_delta(1);
    let res = tokio::io::copy_bidirectional(&mut client, &mut server).await;
    metrics.sni_active_delta(-1);
    res.map(|_| ()).map_err(Into::into)
}

/// Reads until a full ClientHello is buffered; returns the raw bytes and the SNI host.
async fn read_hello(client: &mut TcpStream) -> Result<(Vec<u8>, String)> {
    let mut buf = Vec::with_capacity(2048);
    let mut chunk = [0u8; 2048];
    loop {
        match client_hello_sni(&buf) {
            Sniff::Found(host) => return Ok((buf, host)),
            Sniff::Invalid => return Err(anyhow!("no usable SNI in ClientHello")),
            Sniff::Incomplete => {}
        }
        if buf.len() >= MAX_HELLO {
            return Err(anyhow!("ClientHello exceeds {MAX_HELLO} bytes"));
        }
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            return Err(anyhow!("connection closed before ClientHello completed"));
        }
        buf.extend_from_slice(&chunk[..n]);
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Sniff {
    Found(String),
    /// More bytes are needed to decide.
    Incomplete,
    /// Not a ClientHello, malformed, or no server_name.
    Invalid,
}

/// Bounds-checked cursor; every accessor returns `None` instead of panicking.
struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        if self.0.len() < n {
            return None;
        }
        let (head, tail) = self.0.split_at(n);
        self.0 = tail;
        Some(head)
    }
    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|b| b[0])
    }
    fn u16(&mut self) -> Option<usize> {
        self.take(2)
            .map(|b| u16::from_be_bytes([b[0], b[1]]) as usize)
    }
    fn vec8(&mut self) -> Option<&'a [u8]> {
        let n = self.u8()? as usize;
        self.take(n)
    }
    fn vec16(&mut self) -> Option<&'a [u8]> {
        let n = self.u16()?;
        self.take(n)
    }
}

/// Parses the server name out of the (possibly partial) bytes of a TLS connection start.
pub fn client_hello_sni(data: &[u8]) -> Sniff {
    let handshake = match collect_handshake(data) {
        Collected::Complete(h) => h,
        Collected::Incomplete => return Sniff::Incomplete,
        Collected::Invalid => return Sniff::Invalid,
    };
    match parse_client_hello(&handshake) {
        Some(host) => Sniff::Found(host),
        None => Sniff::Invalid,
    }
}

enum Collected {
    Complete(Vec<u8>),
    Incomplete,
    Invalid,
}

/// Reassembles handshake-record fragments until the first handshake message is complete.
fn collect_handshake(data: &[u8]) -> Collected {
    let mut r = Reader(data);
    let mut hs: Vec<u8> = Vec::new();
    loop {
        if hs.len() >= 4 {
            if hs[0] != 1 {
                return Collected::Invalid;
            }
            let len = ((hs[1] as usize) << 16) | ((hs[2] as usize) << 8) | hs[3] as usize;
            if len + 4 > MAX_HELLO {
                return Collected::Invalid;
            }
            if hs.len() >= len + 4 {
                hs.truncate(len + 4);
                return Collected::Complete(hs);
            }
        }
        // Validate as much of the record header as we have before waiting for more.
        if r.0.is_empty() {
            return Collected::Incomplete;
        }
        if r.0[0] != 0x16 || (r.0.len() >= 2 && r.0[1] != 3) {
            return Collected::Invalid;
        }
        let Some(header) = r.take(5) else {
            return Collected::Incomplete;
        };
        let rec_len = u16::from_be_bytes([header[3], header[4]]) as usize;
        if rec_len == 0 || rec_len > 16384 {
            return Collected::Invalid;
        }
        // Accept a partial final record so a long hello can be decided early.
        let avail = r.0.len().min(rec_len);
        hs.extend_from_slice(r.take(avail).unwrap_or_default());
        // A short record leaves `r` empty, so the next iteration either finds the
        // message complete or reports Incomplete.
    }
}

fn parse_client_hello(msg: &[u8]) -> Option<String> {
    let mut r = Reader(msg.get(4..)?);
    r.take(2 + 32)?; // legacy_version + random
    r.vec8()?; // session id
    r.vec16()?; // cipher suites
    r.vec8()?; // compression methods
    let mut exts = Reader(r.vec16()?);
    while !exts.0.is_empty() {
        let ty = exts.u16()?;
        let body = exts.vec16()?;
        if ty == 0 {
            return parse_server_name(body);
        }
    }
    None
}

fn parse_server_name(body: &[u8]) -> Option<String> {
    let mut list = Reader(Reader(body).vec16()?);
    while !list.0.is_empty() {
        let name_type = list.u8()?;
        let name = list.vec16()?;
        if name_type == 0 {
            return valid_host(name);
        }
    }
    None
}

fn valid_host(name: &[u8]) -> Option<String> {
    let s = std::str::from_utf8(name).ok()?;
    let s = s.trim_end_matches('.');
    let ok = !s.is_empty()
        && s.len() <= 253
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_'));
    ok.then(|| s.to_ascii_lowercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Captures the first flight a rustls client writes for `host`.
    pub(crate) fn client_hello(host: &str) -> Vec<u8> {
        use rustls::pki_types::ServerName;
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(rustls::RootCertStore::empty())
            .with_no_client_auth();
        let name = ServerName::try_from(host.to_string()).unwrap();
        let mut conn = rustls::ClientConnection::new(Arc::new(config), name).unwrap();
        let mut out = Vec::new();
        conn.write_tls(&mut out).unwrap();
        out
    }

    #[test]
    fn parses_real_client_hello() {
        let hello = client_hello("CDN.Example.com");
        assert_eq!(
            client_hello_sni(&hello),
            Sniff::Found("cdn.example.com".into())
        );
    }

    #[test]
    fn trailing_bytes_ignored() {
        let mut hello = client_hello("a.example.com");
        hello.extend_from_slice(b"extra application bytes");
        assert_eq!(
            client_hello_sni(&hello),
            Sniff::Found("a.example.com".into())
        );
    }

    #[test]
    fn truncated_is_incomplete_never_panics() {
        let hello = client_hello("a.example.com");
        for n in 0..hello.len() {
            assert_eq!(client_hello_sni(&hello[..n]), Sniff::Incomplete, "len {n}");
        }
    }

    #[test]
    fn fragmented_across_records() {
        let hello = client_hello("frag.example.com");
        let payload = &hello[5..];
        let (a, b) = payload.split_at(40);
        let mut data = Vec::new();
        for part in [a, b] {
            data.extend_from_slice(&[0x16, 3, 1]);
            data.extend_from_slice(&(part.len() as u16).to_be_bytes());
            data.extend_from_slice(part);
        }
        assert_eq!(
            client_hello_sni(&data),
            Sniff::Found("frag.example.com".into())
        );
    }

    #[test]
    fn no_sni_extension() {
        // ClientHello with an empty extensions block.
        let mut body = vec![3, 3];
        body.extend_from_slice(&[0u8; 32]);
        body.push(0); // session id
        body.extend_from_slice(&[0, 2, 0x13, 0x01]);
        body.extend_from_slice(&[1, 0]);
        body.extend_from_slice(&[0, 0]);
        let mut hs = vec![1, 0, 0, body.len() as u8];
        hs.extend_from_slice(&body);
        let mut rec = vec![0x16, 3, 1, 0, hs.len() as u8];
        rec.extend_from_slice(&hs);
        assert_eq!(client_hello_sni(&rec), Sniff::Invalid);
    }

    #[test]
    fn garbage_is_invalid() {
        assert_eq!(client_hello_sni(b"GET / HTTP/1.1\r\n\r\n"), Sniff::Invalid);
        assert_eq!(client_hello_sni(&[0x16, 3, 1, 0xff, 0xff]), Sniff::Invalid);
        assert_eq!(client_hello_sni(&[0x16, 3, 1, 0, 0]), Sniff::Invalid);
        // Handshake type other than ClientHello.
        assert_eq!(
            client_hello_sni(&[0x16, 3, 1, 0, 4, 2, 0, 0, 0]),
            Sniff::Invalid
        );
    }

    #[test]
    fn corrupted_hello_never_panics() {
        let hello = client_hello("a.example.com");
        for i in 0..hello.len() {
            let mut bad = hello.clone();
            bad[i] ^= 0xff;
            let _ = client_hello_sni(&bad);
        }
    }

    #[test]
    fn hostile_server_name_rejected() {
        assert_eq!(valid_host(b"a b"), None);
        assert_eq!(valid_host(b""), None);
        assert_eq!(valid_host(&[0xff, 0xfe]), None);
        assert_eq!(valid_host(b"x.com."), Some("x.com".into()));
    }
}
