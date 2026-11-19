use std::net::{IpAddr, Ipv4Addr};
use std::sync::Arc;
use std::time::Duration;

use game_cacher::metrics::Metrics;
use game_cacher::sni;
use game_cacher::upstream::{LoopGuard, StaticResolver, Upstream};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

fn client_hello(host: &str) -> Vec<u8> {
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

const LOCALHOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

#[tokio::test]
async fn splices_to_upstream_and_echoes() {
    let fake = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = fake.local_addr().unwrap().port();
    let (tx, rx) = tokio::sync::oneshot::channel();
    tokio::spawn(async move {
        let (mut s, _) = fake.accept().await.unwrap();
        let mut got = Vec::new();
        let mut buf = [0u8; 4096];
        let mut tx = Some(tx);
        loop {
            let n = s.read(&mut buf).await.unwrap();
            if n == 0 {
                break;
            }
            got.extend_from_slice(&buf[..n]);
            s.write_all(&buf[..n]).await.unwrap();
            if got.ends_with(b"payload-bytes")
                && let Some(tx) = tx.take()
            {
                tx.send(got.clone()).unwrap();
            }
        }
    });

    let metrics = Metrics::new();
    let upstream = Upstream::new(Arc::new(StaticResolver(LOCALHOST)), 80, "test".into()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    sni::spawn(listener, upstream, port, metrics.clone());

    let hello = client_hello("cdn.example.com");
    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&hello).await.unwrap();
    c.write_all(b"payload-bytes").await.unwrap();

    let got = tokio::time::timeout(Duration::from_secs(5), rx)
        .await
        .unwrap()
        .unwrap();
    let mut expected = hello.clone();
    expected.extend_from_slice(b"payload-bytes");
    assert_eq!(got, expected);

    let mut echo = vec![0u8; expected.len()];
    tokio::time::timeout(Duration::from_secs(5), c.read_exact(&mut echo))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(echo, expected);
    let text = metrics.encode();
    assert!(
        text.contains(r#"gamecacher_sni_connections_total{result="ok"} 1"#),
        "{text}"
    );
}

#[tokio::test]
async fn loop_is_detected_and_closed() {
    let metrics = Metrics::new();
    let guard = LoopGuard::new(Arc::new(StaticResolver(LOCALHOST)), vec![LOCALHOST]);
    let upstream = Upstream::new(Arc::new(guard), 80, "test".into()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    sni::spawn(listener, upstream, 9, metrics.clone());

    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(&client_hello("cdn.example.com")).await.unwrap();
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), c.read(&mut buf))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0);
    let text = metrics.encode();
    assert!(
        text.contains(r#"gamecacher_sni_connections_total{result="loop"}"#),
        "{text}"
    );
}

#[tokio::test]
async fn garbage_is_closed_as_error() {
    let metrics = Metrics::new();
    let upstream = Upstream::new(Arc::new(StaticResolver(LOCALHOST)), 80, "test".into()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    sni::spawn(listener, upstream, 9, metrics.clone());

    let mut c = TcpStream::connect(addr).await.unwrap();
    c.write_all(b"GET / HTTP/1.1\r\n\r\n").await.unwrap();
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), c.read(&mut buf))
        .await
        .unwrap()
        .unwrap_or(0);
    assert_eq!(n, 0);
    assert!(
        metrics
            .encode()
            .contains(r#"gamecacher_sni_connections_total{result="error"}"#)
    );
}
