use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use game_cacher::dns::DnsServer;
use game_cacher::metrics::Metrics;
use game_cacher::platforms::{self, Platforms};
use game_cacher::settings::{PlatformSettings, SettingsStore};
use game_cacher::storage::MemoryStorage;
use hickory_proto::op::{Message, MessageType, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, Record, RecordType, rdata};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

fn query(name: &str, ty: RecordType) -> Vec<u8> {
    let mut m = Message::new();
    m.set_id(77)
        .set_message_type(MessageType::Query)
        .set_recursion_desired(true)
        .add_query(Query::query(Name::from_str(name).unwrap(), ty));
    m.to_vec().unwrap()
}

async fn udp_ask(server: SocketAddr, name: &str, ty: RecordType) -> Message {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    sock.send_to(&query(name, ty), server).await.unwrap();
    let mut buf = vec![0u8; 4096];
    let (n, _) = tokio::time::timeout(Duration::from_secs(5), sock.recv_from(&mut buf))
        .await
        .expect("dns response timed out")
        .unwrap();
    Message::from_vec(&buf[..n]).unwrap()
}

/// Fake upstream DNS answering every query with 9.9.9.9.
async fn fake_upstream() -> SocketAddr {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let addr = sock.local_addr().unwrap();
    tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let Ok((n, peer)) = sock.recv_from(&mut buf).await else {
                return;
            };
            let req = Message::from_vec(&buf[..n]).unwrap();
            let mut resp = Message::new();
            resp.set_id(req.id())
                .set_message_type(MessageType::Response);
            resp.add_queries(req.queries().iter().cloned());
            resp.add_answer(Record::from_rdata(
                req.queries()[0].name().clone(),
                30,
                RData::A(rdata::A(Ipv4Addr::new(9, 9, 9, 9))),
            ));
            sock.send_to(&resp.to_vec().unwrap(), peer).await.unwrap();
        }
    });
    addr
}

async fn start(forwarders: Vec<SocketAddr>) -> (SocketAddr, Arc<SettingsStore>, Arc<Metrics>) {
    let settings = Arc::new(
        SettingsStore::load(Arc::new(MemoryStorage::new()))
            .await
            .unwrap(),
    );
    let metrics = Metrics::new();
    let server = DnsServer::new(
        vec!["192.168.1.10".parse().unwrap(), "fd00::10".parse().unwrap()],
        forwarders,
        Arc::new(Platforms::new(platforms::all())),
        settings.clone(),
        metrics.clone(),
    );
    let addr = server.bind("127.0.0.1:0".parse().unwrap()).await.unwrap();
    (addr, settings, metrics)
}

#[tokio::test]
async fn udp_override_aaaa_forward_and_refused() {
    let upstream = fake_upstream().await;
    let (addr, settings, metrics) = start(vec![upstream]).await;

    let r = udp_ask(addr, "lancache.steamcontent.com.", RecordType::A).await;
    assert_eq!(
        r.answers()[0].data(),
        &RData::A(rdata::A("192.168.1.10".parse().unwrap()))
    );
    assert!(r.authoritative());

    let r = udp_ask(addr, "lancache.steamcontent.com.", RecordType::AAAA).await;
    assert_eq!(
        r.answers()[0].data(),
        &RData::AAAA(rdata::AAAA("fd00::10".parse().unwrap()))
    );

    settings
        .update(|s| {
            s.platforms
                .insert("steam".into(), PlatformSettings { enabled: false });
        })
        .await
        .unwrap();
    let r = udp_ask(addr, "lancache.steamcontent.com.", RecordType::A).await;
    assert_eq!(
        r.answers()[0].data(),
        &RData::A(rdata::A(Ipv4Addr::new(9, 9, 9, 9)))
    );
    let text = metrics.encode();
    assert!(text.contains(r#"result="override""#), "{text}");
    assert!(text.contains(r#"result="forward""#), "{text}");

    let (addr, _, _) = start(vec![]).await;
    let r = udp_ask(addr, "example.com.", RecordType::A).await;
    assert_eq!(r.response_code(), ResponseCode::Refused);
}

#[tokio::test]
async fn tcp_override_multiple_queries() {
    let (addr, _, _) = start(vec![]).await;
    let mut stream = TcpStream::connect(addr).await.unwrap();
    for _ in 0..2 {
        let q = query("lancache.steamcontent.com.", RecordType::A);
        stream
            .write_all(&(q.len() as u16).to_be_bytes())
            .await
            .unwrap();
        stream.write_all(&q).await.unwrap();
        let mut len = [0u8; 2];
        stream.read_exact(&mut len).await.unwrap();
        let mut resp = vec![0u8; u16::from_be_bytes(len) as usize];
        stream.read_exact(&mut resp).await.unwrap();
        let r = Message::from_vec(&resp).unwrap();
        assert_eq!(
            r.answers()[0].data(),
            &RData::A(rdata::A("192.168.1.10".parse().unwrap()))
        );
    }
}
