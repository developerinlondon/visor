use super::*;
use crate::net::switch::MacAddr;
use std::net::Ipv4Addr;

/// Helper: build a frame and parse it for forwarding.
fn build_and_parse(dst: MacAddr, src: MacAddr, ethertype: u16, payload: &[u8]) -> Vec<u8> {
    EthernetFrame::build(dst, src, ethertype, payload)
}

// ── EthernetFrame parsing ─────────────────────────────────────────

#[test]
fn test_parse_ethernet_frame_valid() {
    let dst = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let src = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
    let ethertype = [0x08, 0x00]; // IPv4
    let payload = b"hello world";

    let mut frame_bytes = Vec::new();
    frame_bytes.extend_from_slice(&dst);
    frame_bytes.extend_from_slice(&src);
    frame_bytes.extend_from_slice(&ethertype);
    frame_bytes.extend_from_slice(payload);

    let frame = EthernetFrame::parse(&frame_bytes).expect("should parse valid frame");
    assert_eq!(frame.dst_mac(), MacAddr::new(dst));
    assert_eq!(frame.src_mac(), MacAddr::new(src));
    assert_eq!(frame.ethertype(), 0x0800);
    assert_eq!(frame.payload(), payload);
}

#[test]
fn test_parse_ethernet_frame_too_short() {
    let short_frame = [0u8; 13]; // Minimum is 14 bytes
    let result = EthernetFrame::parse(&short_frame);
    assert!(result.is_err());
}

#[test]
fn test_parse_ethernet_frame_minimum_size() {
    let frame_bytes = [0u8; 14]; // Exactly minimum (no payload)
    let frame = EthernetFrame::parse(&frame_bytes).expect("should parse minimum frame");
    assert_eq!(frame.payload(), &[] as &[u8]);
}

#[test]
fn test_ethernet_frame_is_broadcast() {
    let broadcast_dst = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    let src = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let ethertype = [0x08, 0x06]; // ARP

    let mut frame_bytes = Vec::new();
    frame_bytes.extend_from_slice(&broadcast_dst);
    frame_bytes.extend_from_slice(&src);
    frame_bytes.extend_from_slice(&ethertype);
    frame_bytes.extend_from_slice(b"arp data");

    let frame = EthernetFrame::parse(&frame_bytes).unwrap();
    assert!(frame.is_broadcast());
}

#[test]
fn test_ethernet_frame_is_not_broadcast() {
    let unicast_dst = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let src = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
    let ethertype = [0x08, 0x00]; // IPv4

    let mut frame_bytes = Vec::new();
    frame_bytes.extend_from_slice(&unicast_dst);
    frame_bytes.extend_from_slice(&src);
    frame_bytes.extend_from_slice(&ethertype);
    frame_bytes.extend_from_slice(b"data");

    let frame = EthernetFrame::parse(&frame_bytes).unwrap();
    assert!(!frame.is_broadcast());
}

#[test]
fn test_ethernet_frame_is_arp() {
    let dst = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];
    let src = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let ethertype = [0x08, 0x06]; // ARP

    let mut frame_bytes = Vec::new();
    frame_bytes.extend_from_slice(&dst);
    frame_bytes.extend_from_slice(&src);
    frame_bytes.extend_from_slice(&ethertype);
    frame_bytes.extend_from_slice(b"arp payload");

    let frame = EthernetFrame::parse(&frame_bytes).unwrap();
    assert!(frame.is_arp());
    assert!(!frame.is_ipv4());
}

#[test]
fn test_ethernet_frame_is_ipv4() {
    let dst = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
    let src = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];
    let ethertype = [0x08, 0x00]; // IPv4

    let mut frame_bytes = Vec::new();
    frame_bytes.extend_from_slice(&dst);
    frame_bytes.extend_from_slice(&src);
    frame_bytes.extend_from_slice(&ethertype);
    frame_bytes.extend_from_slice(b"ip data");

    let frame = EthernetFrame::parse(&frame_bytes).unwrap();
    assert!(frame.is_ipv4());
    assert!(!frame.is_arp());
}

#[test]
fn test_ethernet_frame_build() {
    let dst = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let src = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    let payload = b"test payload";

    let frame_bytes = EthernetFrame::build(dst, src, ETHERTYPE_IPV4, payload);
    assert_eq!(frame_bytes.len(), 14 + payload.len());

    let parsed = EthernetFrame::parse(&frame_bytes).unwrap();
    assert_eq!(parsed.dst_mac(), dst);
    assert_eq!(parsed.src_mac(), src);
    assert_eq!(parsed.ethertype(), ETHERTYPE_IPV4);
    assert_eq!(parsed.payload(), payload);
}

// ── PacketSwitch forwarding ───────────────────────────────────────

#[tokio::test]
async fn test_packet_switch_register_port() {
    let mut pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let ip = Ipv4Addr::new(10, 0, 0, 2);
    let rx = pswitch.register_port("vm-1", mac, ip).expect("should register");

    assert_eq!(pswitch.port_count(), 1);
    assert!(pswitch.has_port(&mac));
    // rx channel should be open
    drop(rx);
}

#[tokio::test]
async fn test_packet_switch_register_duplicate_mac() {
    let mut pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let _rx1 = pswitch.register_port("vm-1", mac, Ipv4Addr::new(10, 0, 0, 2)).unwrap();
    let result = pswitch.register_port("vm-2", mac, Ipv4Addr::new(10, 0, 0, 3));
    assert!(result.is_err());
}

#[tokio::test]
async fn test_packet_switch_unregister_port() {
    let mut pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let _rx = pswitch.register_port("vm-1", mac, Ipv4Addr::new(10, 0, 0, 2)).unwrap();
    assert_eq!(pswitch.port_count(), 1);

    pswitch.unregister_port(&mac).unwrap();
    assert_eq!(pswitch.port_count(), 0);
    assert!(!pswitch.has_port(&mac));
}

#[tokio::test]
async fn test_packet_switch_unicast_forwarding() {
    let mut pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let mac1 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mac2 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    let _rx1 = pswitch.register_port("vm-1", mac1, Ipv4Addr::new(10, 0, 0, 2)).unwrap();
    let mut rx2 = pswitch.register_port("vm-2", mac2, Ipv4Addr::new(10, 0, 0, 3)).unwrap();

    // Build a frame from vm-1 → vm-2
    let frame_bytes = build_and_parse(mac2, mac1, ETHERTYPE_IPV4, b"hello from vm-1");
    let frame = EthernetFrame::parse(&frame_bytes).unwrap();

    let delivered = pswitch.forward_frame(&frame).expect("should forward");
    assert_eq!(delivered, 1);

    // vm-2 should receive the frame
    let received = rx2.try_recv().expect("vm-2 should receive frame");
    let parsed = EthernetFrame::parse(&received).unwrap();
    assert_eq!(parsed.dst_mac(), mac2);
    assert_eq!(parsed.src_mac(), mac1);
    assert_eq!(parsed.payload(), b"hello from vm-1");
}

#[tokio::test]
async fn test_packet_switch_broadcast_forwarding() {
    let mut pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let mac1 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mac2 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    let mac3 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x03]);

    let _rx1 = pswitch.register_port("vm-1", mac1, Ipv4Addr::new(10, 0, 0, 2)).unwrap();
    let mut rx2 = pswitch.register_port("vm-2", mac2, Ipv4Addr::new(10, 0, 0, 3)).unwrap();
    let mut rx3 = pswitch.register_port("vm-3", mac3, Ipv4Addr::new(10, 0, 0, 4)).unwrap();

    // Build a broadcast frame from vm-1
    let broadcast = MacAddr::new([0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    let frame_bytes = build_and_parse(broadcast, mac1, ETHERTYPE_ARP, b"arp request");
    let frame = EthernetFrame::parse(&frame_bytes).unwrap();

    // Broadcast should deliver to all ports except sender
    let delivered = pswitch.forward_frame(&frame).expect("should broadcast");
    assert_eq!(delivered, 2); // vm-2 and vm-3 (not vm-1)

    // vm-2 and vm-3 should receive
    let received2 = rx2.try_recv().expect("vm-2 should receive broadcast");
    let received3 = rx3.try_recv().expect("vm-3 should receive broadcast");

    let parsed2 = EthernetFrame::parse(&received2).unwrap();
    assert_eq!(parsed2.payload(), b"arp request");
    let parsed3 = EthernetFrame::parse(&received3).unwrap();
    assert_eq!(parsed3.payload(), b"arp request");
}

#[tokio::test]
async fn test_packet_switch_unknown_destination() {
    let mut pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let mac1 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let unknown_mac = MacAddr::new([0x02, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]);
    let _rx1 = pswitch.register_port("vm-1", mac1, Ipv4Addr::new(10, 0, 0, 2)).unwrap();

    // Frame to unknown MAC — should be dropped (returns 0)
    let frame_bytes = build_and_parse(unknown_mac, mac1, ETHERTYPE_IPV4, b"lost packet");
    let frame = EthernetFrame::parse(&frame_bytes).unwrap();
    let delivered = pswitch.forward_frame(&frame).expect("should not error");
    assert_eq!(delivered, 0);
}

#[tokio::test]
async fn test_packet_switch_no_loopback() {
    let mut pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let mac1 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mut rx1 = pswitch.register_port("vm-1", mac1, Ipv4Addr::new(10, 0, 0, 2)).unwrap();

    // Unicast to self should NOT deliver (no loopback)
    let frame_bytes = build_and_parse(mac1, mac1, ETHERTYPE_IPV4, b"self");
    let frame = EthernetFrame::parse(&frame_bytes).unwrap();
    let delivered = pswitch.forward_frame(&frame).expect("should not error");
    assert_eq!(delivered, 0);

    // Nothing in rx1
    assert!(rx1.try_recv().is_err());
}

#[tokio::test]
async fn test_packet_switch_forward_after_unregister() {
    let mut pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let mac1 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mac2 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    let _rx1 = pswitch.register_port("vm-1", mac1, Ipv4Addr::new(10, 0, 0, 2)).unwrap();
    let _rx2 = pswitch.register_port("vm-2", mac2, Ipv4Addr::new(10, 0, 0, 3)).unwrap();

    // Unregister vm-2
    pswitch.unregister_port(&mac2).unwrap();

    // Frame to vm-2 should not deliver (port removed)
    let frame_bytes = build_and_parse(mac2, mac1, ETHERTYPE_IPV4, b"to removed port");
    let frame = EthernetFrame::parse(&frame_bytes).unwrap();
    let delivered = pswitch.forward_frame(&frame).expect("should not error");
    assert_eq!(delivered, 0);
}

#[test]
fn test_packet_switch_metrics() {
    let pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let metrics = pswitch.metrics();
    assert_eq!(metrics.frames_forwarded, 0);
    assert_eq!(metrics.frames_dropped, 0);
    assert_eq!(metrics.frames_broadcast, 0);
    assert_eq!(metrics.bytes_forwarded, 0);
}

#[tokio::test]
async fn test_packet_switch_metrics_after_forward() {
    let mut pswitch = PacketSwitch::new("test-net", Ipv4Addr::new(10, 0, 0, 0), 24, Ipv4Addr::new(10, 0, 0, 1));

    let mac1 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mac2 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    let _rx1 = pswitch.register_port("vm-1", mac1, Ipv4Addr::new(10, 0, 0, 2)).unwrap();
    let _rx2 = pswitch.register_port("vm-2", mac2, Ipv4Addr::new(10, 0, 0, 3)).unwrap();

    let payload = b"test data";
    let frame_bytes = build_and_parse(mac2, mac1, ETHERTYPE_IPV4, payload);
    let frame = EthernetFrame::parse(&frame_bytes).unwrap();
    pswitch.forward_frame(&frame).unwrap();

    let metrics = pswitch.metrics();
    assert_eq!(metrics.frames_forwarded, 1);
    assert_eq!(metrics.frames_dropped, 0);
    assert_eq!(metrics.frames_broadcast, 0);
    assert_eq!(metrics.bytes_forwarded, frame_bytes.len() as u64);
}
