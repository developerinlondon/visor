use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use hickory_server::proto::op::{Message, MessageType, OpCode, Query};
use hickory_server::proto::rr::{Name, RecordType};
use hickory_server::proto::serialize::binary::{BinDecodable, BinEncodable};
use tokio::net::UdpSocket;
use tokio::sync::RwLock;

use super::*;
use crate::net::dns::{DnsRegistry, DnsResolverConfig};

// ── Helpers ──────────────────────────────────────────────────────────

/// Build a DNS A-record query message for the given name.
fn build_query(name: &str) -> Vec<u8> {
    let mut msg = Message::new();
    msg.set_id(rand_id());
    msg.set_message_type(MessageType::Query);
    msg.set_op_code(OpCode::Query);
    msg.set_recursion_desired(true);

    let mut query = Query::new();
    query.set_name(Name::from_ascii(name).unwrap());
    query.set_query_type(RecordType::A);
    msg.add_query(query);

    msg.to_bytes().unwrap()
}

/// Send a DNS query to `addr` and return the parsed response.
async fn dns_query(addr: SocketAddr, name: &str) -> Message {
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let buf = build_query(name);
    sock.send_to(&buf, addr).await.unwrap();

    let mut recv = vec![0u8; 4096];
    let (len, _) =
        tokio::time::timeout(std::time::Duration::from_secs(5), sock.recv_from(&mut recv))
            .await
            .unwrap()
            .unwrap();
    Message::from_bytes(&recv[..len]).unwrap()
}

/// Simple random-ish message ID (good enough for tests).
fn rand_id() -> u16 {
    (std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .subsec_nanos()
        & 0xFFFF) as u16
}

/// Create a test config bound to 127.0.0.1 on port 0 (OS-assigned).
fn test_config() -> DnsResolverConfig {
    DnsResolverConfig::new(Ipv4Addr::LOCALHOST).with_port(0)
}

// ── Tests ────────────────────────────────────────────────────────────

#[tokio::test]
async fn dns_server_starts_and_stops() {
    let registry = Arc::new(RwLock::new(DnsRegistry::new()));
    let config = test_config();

    let server = DnsServer::start(&config, registry).await.unwrap();
    // Server is listening — addr() should return a valid address.
    let addr = server.addr();
    assert_ne!(addr.port(), 0);

    server.stop();
    // No panic — graceful shutdown.
}

#[tokio::test]
async fn dns_server_resolves_registered_name() {
    let registry = Arc::new(RwLock::new(DnsRegistry::new()));
    registry
        .write()
        .await
        .register("myvm", Ipv4Addr::new(10, 0, 0, 5));

    let config = test_config();
    let server = DnsServer::start(&config, registry).await.unwrap();
    let addr = server.addr();

    let resp = dns_query(addr, "myvm.visor.").await;
    let answers: Vec<_> = resp.answers().to_vec();
    assert!(!answers.is_empty(), "expected at least one answer");

    let ip = answers[0].data().as_a().expect("expected A record").0;
    assert_eq!(ip, Ipv4Addr::new(10, 0, 0, 5));

    server.stop();
}

#[tokio::test]
async fn dns_server_returns_nxdomain_for_unknown() {
    let registry = Arc::new(RwLock::new(DnsRegistry::new()));
    let config = test_config();
    let server = DnsServer::start(&config, registry).await.unwrap();
    let addr = server.addr();

    let resp = dns_query(addr, "nonexistent.visor.").await;
    assert_eq!(
        resp.response_code(),
        hickory_server::proto::op::ResponseCode::NXDomain
    );

    server.stop();
}

#[tokio::test]
async fn dns_server_case_insensitive_lookup() {
    let registry = Arc::new(RwLock::new(DnsRegistry::new()));
    registry
        .write()
        .await
        .register("MyVM", Ipv4Addr::new(10, 0, 0, 6));

    let config = test_config();
    let server = DnsServer::start(&config, registry).await.unwrap();
    let addr = server.addr();

    let resp = dns_query(addr, "myvm.visor.").await;
    let answers: Vec<_> = resp.answers().to_vec();
    assert!(!answers.is_empty(), "expected at least one answer");

    let ip = answers[0].data().as_a().expect("expected A record").0;
    assert_eq!(ip, Ipv4Addr::new(10, 0, 0, 6));

    server.stop();
}

#[tokio::test]
async fn dns_server_resolves_after_registration_update() {
    let registry = Arc::new(RwLock::new(DnsRegistry::new()));
    registry
        .write()
        .await
        .register("vm1", Ipv4Addr::new(10, 0, 0, 2));

    let config = test_config();
    let server = DnsServer::start(&config, registry.clone()).await.unwrap();
    let addr = server.addr();

    // First query — should return 10.0.0.2.
    let resp = dns_query(addr, "vm1.visor.").await;
    let ip = resp.answers()[0]
        .data()
        .as_a()
        .expect("expected A record")
        .0;
    assert_eq!(ip, Ipv4Addr::new(10, 0, 0, 2));

    // Update the registration.
    registry
        .write()
        .await
        .register("vm1", Ipv4Addr::new(10, 0, 0, 3));

    // Second query — should return the updated IP.
    let resp = dns_query(addr, "vm1.visor.").await;
    let ip = resp.answers()[0]
        .data()
        .as_a()
        .expect("expected A record")
        .0;
    assert_eq!(ip, Ipv4Addr::new(10, 0, 0, 3));

    server.stop();
}
