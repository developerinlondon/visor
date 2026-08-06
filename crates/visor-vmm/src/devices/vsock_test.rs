use std::num::Wrapping;

use super::{
    CONN_TX_BUF_SIZE, ConnMapKey, ConnState, HOST_CID, RXQ, TXQ, VIRTIO_F_VERSION_1,
    VSOCK_FLAGS_SHUTDOWN_RCV, VSOCK_FLAGS_SHUTDOWN_SEND, VSOCK_PKT_HDR_SIZE, VSOCK_TYPE_STREAM,
    VsockConnection, VsockDevice, VsockError, VsockOp, VsockPacket,
};
use crate::memory::GuestMemory;
use crate::transport::{DeviceType, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtioDevice};

// ── Constructor tests ────────────────────────────────────────────────

#[test]
fn new_sets_guest_cid() {
    let dev = VsockDevice::new(3);
    assert_eq!(dev.guest_cid(), 3);
}

#[test]
fn new_with_various_cids() {
    for &cid in &[0, 1, 2, 3, 1000, u64::MAX] {
        let dev = VsockDevice::new(cid);
        assert_eq!(dev.guest_cid(), cid);
    }
}

// ── VirtioDevice trait tests ─────────────────────────────────────────

#[test]
fn device_type_returns_vsock() {
    let dev = VsockDevice::new(3);
    assert_eq!(dev.device_type(), DeviceType::Vsock);
}

#[test]
fn avail_features_includes_version_1() {
    let dev = VsockDevice::new(3);
    let features = dev.avail_features();
    assert_ne!(
        features & VIRTIO_F_VERSION_1,
        0,
        "VIRTIO_F_VERSION_1 must be set"
    );
}

#[test]
fn acked_features_starts_at_zero() {
    let dev = VsockDevice::new(3);
    assert_eq!(dev.acked_features(), 0, "acked_features starts at 0");
}

#[test]
fn acked_features_roundtrip() {
    let mut dev = VsockDevice::new(3);
    assert_eq!(dev.acked_features(), 0, "acked_features starts at 0");

    dev.set_acked_features(VIRTIO_F_VERSION_1);
    assert_eq!(dev.acked_features(), VIRTIO_F_VERSION_1);
}

#[test]
fn queues_returns_three_queues() {
    let dev = VsockDevice::new(3);
    let queues = dev.queues();
    assert_eq!(queues.len(), 3, "vsock device has exactly 3 virtqueues");
}

#[test]
fn queue_max_sizes_are_256() {
    let dev = VsockDevice::new(3);
    let queues = dev.queues();
    for (i, q) in queues.iter().enumerate() {
        assert_eq!(q.max_size, 256, "queue {i} max_size should be 256");
    }
}

#[test]
fn queues_mut_allows_modification() {
    let mut dev = VsockDevice::new(3);
    dev.queues_mut()[0].ready = true;
    dev.queues_mut()[0].size = 128;
    dev.queues_mut()[1].desc_table_addr = 0x1000;
    dev.queues_mut()[2].avail_ring_addr = 0x2000;

    assert!(dev.queues()[0].ready);
    assert_eq!(dev.queues()[0].size, 128);
    assert_eq!(dev.queues()[1].desc_table_addr, 0x1000);
    assert_eq!(dev.queues()[2].avail_ring_addr, 0x2000);
}

// ── Config space tests ───────────────────────────────────────────────

#[test]
fn read_config_returns_cid_as_le_u64() {
    let dev = VsockDevice::new(42);
    let mut buf = [0u8; 8];
    dev.read_config(0, &mut buf);
    let cid = u64::from_le_bytes(buf);
    assert_eq!(cid, 42);
}

#[test]
fn read_config_partial_read() {
    let dev = VsockDevice::new(256);
    // Read only first 4 bytes of CID (256 = 0x100)
    let mut buf = [0u8; 4];
    dev.read_config(0, &mut buf);
    // 256 in LE is [0x00, 0x01, 0x00, 0x00]
    assert_eq!(buf, [0x00, 0x01, 0x00, 0x00]);
}

#[test]
fn read_config_middle_of_cid() {
    let dev = VsockDevice::new(0x0102_0304_0506_0708);
    // Read 4 bytes starting at offset 2
    let mut buf = [0u8; 4];
    dev.read_config(2, &mut buf);
    // CID LE bytes: [0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01]
    // Offset 2..6: [0x06, 0x05, 0x04, 0x03]
    assert_eq!(buf, [0x06, 0x05, 0x04, 0x03]);
}

#[test]
fn read_config_out_of_bounds_returns_zeros() {
    let dev = VsockDevice::new(3);
    let mut buf = [0xFFu8; 4];
    dev.read_config(8, &mut buf);
    assert_eq!(
        buf,
        [0, 0, 0, 0],
        "reads beyond config space should return zeros"
    );
}

#[test]
fn write_config_is_noop() {
    let mut dev = VsockDevice::new(42);

    // Attempt to overwrite CID
    dev.write_config(0, &[0xFF; 8]);

    // Config should still return original CID
    let mut buf = [0u8; 8];
    dev.read_config(0, &mut buf);
    let cid = u64::from_le_bytes(buf);
    assert_eq!(cid, 42, "write_config must be a no-op");
}

// ── Activate / reset tests ───────────────────────────────────────────

#[test]
fn activate_succeeds() {
    let mut dev = VsockDevice::new(3);
    assert!(!dev.is_activated(), "device should start deactivated");
    dev.activate().unwrap();
    assert!(
        dev.is_activated(),
        "device should be activated after activate()"
    );
}

#[test]
fn reset_clears_activation_and_queues() {
    let mut dev = VsockDevice::new(3);

    // Activate and configure queues
    dev.activate().unwrap();
    dev.queues_mut()[0].size = 128;
    dev.queues_mut()[0].ready = true;
    dev.queues_mut()[1].size = 64;
    dev.queues_mut()[2].ready = true;

    dev.reset();

    assert!(!dev.is_activated(), "reset should deactivate");
    for (i, q) in dev.queues().iter().enumerate() {
        assert_eq!(q.size, 0, "reset should clear queue {i} size");
        assert!(!q.ready, "reset should clear queue {i} ready");
    }
}

// ── VsockPacket tests ────────────────────────────────────────────────

#[test]
fn vsock_packet_header_size_is_44() {
    assert_eq!(VSOCK_PKT_HDR_SIZE, 44);
}

#[test]
fn vsock_packet_round_trip() {
    let pkt = VsockPacket {
        src_cid: 2,
        dst_cid: 3,
        src_port: 1234,
        dst_port: 5678,
        len: 100,
        pkt_type: 1,
        op: VsockOp::Rw as u16,
        flags: 0,
        buf_alloc: 4096,
        fwd_cnt: 50,
    };
    let bytes = pkt.to_bytes();
    let pkt2 = VsockPacket::from_bytes(&bytes).unwrap();
    assert_eq!(pkt, pkt2);
}

#[test]
fn vsock_packet_from_short_buffer_returns_none() {
    let buf = [0u8; VSOCK_PKT_HDR_SIZE - 1];
    assert!(
        VsockPacket::from_bytes(&buf).is_none(),
        "buffer shorter than header should return None"
    );
    assert!(
        VsockPacket::from_bytes(&[]).is_none(),
        "empty buffer should return None"
    );
}

#[test]
fn vsock_packet_all_field_values() {
    let pkt = VsockPacket {
        src_cid: u64::MAX,
        dst_cid: u64::MAX - 1,
        src_port: u32::MAX,
        dst_port: u32::MAX - 1,
        len: u32::MAX,
        pkt_type: u16::MAX,
        op: u16::MAX,
        flags: u32::MAX,
        buf_alloc: u32::MAX,
        fwd_cnt: u32::MAX,
    };
    let bytes = pkt.to_bytes();
    assert_eq!(bytes.len(), VSOCK_PKT_HDR_SIZE);
    let pkt2 = VsockPacket::from_bytes(&bytes).unwrap();
    assert_eq!(pkt.src_cid, pkt2.src_cid);
    assert_eq!(pkt.dst_cid, pkt2.dst_cid);
    assert_eq!(pkt.src_port, pkt2.src_port);
    assert_eq!(pkt.dst_port, pkt2.dst_port);
    assert_eq!(pkt.len, pkt2.len);
    assert_eq!(pkt.pkt_type, pkt2.pkt_type);
    assert_eq!(pkt.op, pkt2.op);
    assert_eq!(pkt.flags, pkt2.flags);
    assert_eq!(pkt.buf_alloc, pkt2.buf_alloc);
    assert_eq!(pkt.fwd_cnt, pkt2.fwd_cnt);
}

#[test]
fn vsock_op_values() {
    assert_eq!(VsockOp::Request as u16, 1);
    assert_eq!(VsockOp::Response as u16, 2);
    assert_eq!(VsockOp::Rst as u16, 3);
    assert_eq!(VsockOp::Shutdown as u16, 4);
    assert_eq!(VsockOp::Rw as u16, 5);
    assert_eq!(VsockOp::CreditUpdate as u16, 6);
    assert_eq!(VsockOp::CreditRequest as u16, 7);
}

// ── TxBuf tests ─────────────────────────────────────────────────────

#[test]
fn txbuf_push_and_flush() {
    use super::TxBuf;

    let mut txbuf = TxBuf::new();
    assert!(txbuf.is_empty());
    assert_eq!(txbuf.len(), 0);

    txbuf.push(&[1, 2, 3, 4]).unwrap();
    assert_eq!(txbuf.len(), 4);
    assert!(!txbuf.is_empty());

    txbuf.push(&[5, 6, 7, 8]).unwrap();
    assert_eq!(txbuf.len(), 8);

    let mut sink = Vec::new();
    let flushed = txbuf.flush_to(&mut sink).unwrap();
    assert_eq!(flushed, 8);
    assert_eq!(sink, [1, 2, 3, 4, 5, 6, 7, 8]);
    assert!(txbuf.is_empty());
}

#[test]
fn txbuf_full_returns_error() {
    use super::TxBuf;

    let mut txbuf = TxBuf::new();
    let big = vec![0u8; CONN_TX_BUF_SIZE as usize];
    txbuf.push(&big).unwrap();
    assert_eq!(txbuf.len(), CONN_TX_BUF_SIZE as usize);

    // One more byte should fail.
    assert!(matches!(txbuf.push(&[1]), Err(VsockError::TxBufFull)));
}

#[test]
fn txbuf_wrap_around() {
    use super::TxBuf;

    let mut txbuf = TxBuf::new();
    // Fill most of the buffer, flush, then push again to wrap.
    let almost = vec![0xAA; CONN_TX_BUF_SIZE as usize - 4];
    txbuf.push(&almost).unwrap();

    let mut sink = Vec::new();
    txbuf.flush_to(&mut sink).unwrap();
    assert!(txbuf.is_empty());

    // Now head is near the end. Push 8 bytes to wrap.
    txbuf.push(&[1, 2, 3, 4, 5, 6, 7, 8]).unwrap();
    assert_eq!(txbuf.len(), 8);

    sink.clear();
    txbuf.flush_to(&mut sink).unwrap();
    assert_eq!(sink, [1, 2, 3, 4, 5, 6, 7, 8]);
}

#[test]
fn txbuf_lazy_allocation() {
    use super::TxBuf;

    let txbuf = TxBuf::new();
    // No memory allocated until first push.
    assert_eq!(std::mem::size_of_val(&txbuf), std::mem::size_of::<TxBuf>());
}

// ── PendingRxSet tests ──────────────────────────────────────────────

#[test]
fn pending_rx_set_operations() {
    use super::{PendingRx, PendingRxSet};

    let mut set = PendingRxSet::empty();
    assert!(set.is_empty());
    assert!(!set.contains(PendingRx::Rst));

    set.insert(PendingRx::Rst);
    assert!(!set.is_empty());
    assert!(set.contains(PendingRx::Rst));
    assert!(!set.contains(PendingRx::Rw));

    // Remove returns true if present.
    assert!(set.remove(PendingRx::Rst));
    assert!(set.is_empty());

    // Remove returns false if absent.
    assert!(!set.remove(PendingRx::Rst));
}

#[test]
fn pending_rx_set_from_single() {
    use super::{PendingRx, PendingRxSet};

    let set = PendingRxSet::from(PendingRx::Response);
    assert!(set.contains(PendingRx::Response));
    assert!(!set.contains(PendingRx::Request));
}

// ── VsockConnection tests ───────────────────────────────────────────

const LOCAL_CID: u64 = HOST_CID;
const PEER_CID: u64 = 3;
const LOCAL_PORT: u32 = 1002;
const PEER_PORT: u32 = 1003;
const PEER_BUF_ALLOC: u32 = 64 * 1024;

/// Helper: create an established connection (peer-initiated).
fn established_conn() -> VsockConnection {
    let mut conn =
        VsockConnection::new_peer_init(LOCAL_CID, PEER_CID, LOCAL_PORT, PEER_PORT, PEER_BUF_ALLOC);
    // Drain the pending RESPONSE to transition to Established.
    let (pkt, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::Response as u16);
    assert_eq!(conn.state(), ConnState::Established);
    conn
}

/// Helper: build a TX packet header from peer.
fn tx_pkt(op: VsockOp, flags: u32) -> VsockPacket {
    VsockPacket {
        src_cid: PEER_CID,
        dst_cid: LOCAL_CID,
        src_port: PEER_PORT,
        dst_port: LOCAL_PORT,
        len: 0,
        pkt_type: VSOCK_TYPE_STREAM,
        op: op as u16,
        flags,
        buf_alloc: PEER_BUF_ALLOC,
        fwd_cnt: 0,
    }
}

// ── Handshake tests ─────────────────────────────────────────────────

#[test]
fn peer_initiated_handshake() {
    let mut conn =
        VsockConnection::new_peer_init(LOCAL_CID, PEER_CID, LOCAL_PORT, PEER_PORT, PEER_BUF_ALLOC);
    assert_eq!(conn.state(), ConnState::PeerInit);
    assert!(conn.has_pending_rx());

    let (pkt, payload) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::Response as u16);
    assert_eq!(pkt.src_cid, LOCAL_CID);
    assert_eq!(pkt.dst_cid, PEER_CID);
    assert_eq!(pkt.src_port, LOCAL_PORT);
    assert_eq!(pkt.dst_port, PEER_PORT);
    assert_eq!(pkt.pkt_type, VSOCK_TYPE_STREAM);
    assert_eq!(pkt.buf_alloc, CONN_TX_BUF_SIZE);
    assert!(payload.is_empty());
    assert_eq!(conn.state(), ConnState::Established);
}

#[test]
fn host_initiated_handshake() {
    let mut conn = VsockConnection::new_local_init(LOCAL_CID, PEER_CID, LOCAL_PORT, PEER_PORT);
    assert_eq!(conn.state(), ConnState::LocalInit);
    assert!(conn.has_pending_rx());

    // Should yield a REQUEST and arm the timeout.
    assert!(!conn.will_expire());
    let (pkt, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::Request as u16);
    assert!(conn.will_expire());
    assert!(!conn.has_expired());

    // Guest sends RESPONSE.
    let resp = tx_pkt(VsockOp::Response, 0);
    conn.send_pkt(&resp, &[]).unwrap();
    assert_eq!(conn.state(), ConnState::Established);
    assert!(!conn.will_expire());
}

#[test]
fn host_initiated_request_timeout() {
    let mut conn = VsockConnection::new_local_init(LOCAL_CID, PEER_CID, LOCAL_PORT, PEER_PORT);
    let _ = conn.recv_pkt(4096).unwrap();
    assert!(conn.will_expire());
    assert!(!conn.has_expired());

    // Sleep past the timeout.
    std::thread::sleep(std::time::Duration::from_millis(
        super::CONN_REQUEST_TIMEOUT_MS + 50,
    ));
    assert!(conn.has_expired());
}

// ── Data transfer tests ─────────────────────────────────────────────

#[test]
fn host_to_guest_data() {
    let mut conn = established_conn();
    let data = &[1, 2, 3, 4];
    conn.push_host_data(data);
    assert!(conn.has_pending_rx());

    let (pkt, payload) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::Rw as u16);
    assert_eq!(pkt.len as usize, data.len());
    assert_eq!(payload, data);

    // No more data.
    assert!(matches!(conn.recv_pkt(4096), Err(VsockError::NoData)));
}

#[test]
fn host_to_guest_data_respects_max_data_len() {
    let mut conn = established_conn();
    conn.push_host_data(&[1, 2, 3, 4, 5, 6, 7, 8]);

    // Only 4 bytes allowed per packet.
    let (pkt, payload) = conn.recv_pkt(4).unwrap();
    assert_eq!(pkt.op, VsockOp::Rw as u16);
    assert_eq!(payload, [1, 2, 3, 4]);

    // Remaining data needs another recv.
    conn.pending_rx.insert(super::PendingRx::Rw);
    let (pkt2, payload2) = conn.recv_pkt(4).unwrap();
    assert_eq!(pkt2.op, VsockOp::Rw as u16);
    assert_eq!(payload2, [5, 6, 7, 8]);
}

#[test]
fn guest_to_host_data() {
    let mut conn = established_conn();
    let data = &[10, 20, 30, 40];
    let pkt = tx_pkt(VsockOp::Rw, 0);
    conn.send_pkt(&pkt, data).unwrap();

    assert!(!conn.tx_buf().is_empty());
    assert_eq!(conn.tx_buf().len(), 4);

    // Flush to a buffer.
    let mut sink = Vec::new();
    let flushed = conn.flush_tx_buf(&mut sink).unwrap();
    assert_eq!(flushed, 4);
    assert_eq!(sink, data);
    assert!(conn.tx_buf().is_empty());
}

#[test]
fn guest_rw_empty_payload_is_dropped() {
    let mut conn = established_conn();
    let pkt = tx_pkt(VsockOp::Rw, 0);
    conn.send_pkt(&pkt, &[]).unwrap();
    assert!(conn.tx_buf().is_empty());
}

// ── Shutdown tests ──────────────────────────────────────────────────

#[test]
fn peer_shutdown_both_flags_yields_rst() {
    let mut conn = established_conn();
    let pkt = tx_pkt(
        VsockOp::Shutdown,
        VSOCK_FLAGS_SHUTDOWN_RCV | VSOCK_FLAGS_SHUTDOWN_SEND,
    );
    conn.send_pkt(&pkt, &[]).unwrap();
    assert_eq!(conn.state(), ConnState::PeerClosed(true, true));

    // Should immediately yield RST.
    assert!(conn.has_pending_rx());
    let (rst, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(rst.op, VsockOp::Rst as u16);
}

#[test]
fn peer_shutdown_recv_only() {
    let mut conn = established_conn();
    let pkt = tx_pkt(VsockOp::Shutdown, VSOCK_FLAGS_SHUTDOWN_RCV);
    conn.send_pkt(&pkt, &[]).unwrap();
    assert_eq!(conn.state(), ConnState::PeerClosed(true, false));

    // Can still send data (guest hasn't closed send).
    let rw = tx_pkt(VsockOp::Rw, 0);
    conn.send_pkt(&rw, &[1, 2, 3]).unwrap();
    assert_eq!(conn.tx_buf().len(), 3);
}

#[test]
fn peer_shutdown_incremental_flags() {
    let mut conn = established_conn();

    // First: shutdown recv only.
    let pkt1 = tx_pkt(VsockOp::Shutdown, VSOCK_FLAGS_SHUTDOWN_RCV);
    conn.send_pkt(&pkt1, &[]).unwrap();
    assert_eq!(conn.state(), ConnState::PeerClosed(true, false));

    // Second: shutdown send only (flags are OR'd).
    let pkt2 = tx_pkt(VsockOp::Shutdown, VSOCK_FLAGS_SHUTDOWN_SEND);
    conn.send_pkt(&pkt2, &[]).unwrap();
    assert_eq!(conn.state(), ConnState::PeerClosed(true, true));

    // Both flags set + TX empty → RST.
    let (rst, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(rst.op, VsockOp::Rst as u16);
}

#[test]
fn host_close_sends_shutdown() {
    let mut conn = established_conn();
    conn.notify_host_closed();
    assert!(conn.has_pending_rx());

    let (pkt, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::Shutdown as u16);
    assert_ne!(pkt.flags & VSOCK_FLAGS_SHUTDOWN_RCV, 0);
    assert_ne!(pkt.flags & VSOCK_FLAGS_SHUTDOWN_SEND, 0);
    assert_eq!(conn.state(), ConnState::LocalClosed);
    assert!(conn.will_expire());
}

// ── Credit flow control tests ───────────────────────────────────────

#[test]
fn peer_avail_credit_basic() {
    let conn =
        VsockConnection::new_peer_init(LOCAL_CID, PEER_CID, LOCAL_PORT, PEER_PORT, PEER_BUF_ALLOC);
    // Fresh connection: full credit available.
    assert_eq!(conn.peer_avail_credit(), PEER_BUF_ALLOC);
}

#[test]
fn credit_request_when_peer_has_zero_credit() {
    let mut conn = established_conn();

    // Exhaust peer credit: set rx_cnt = peer_buf_alloc.
    conn.rx_cnt = Wrapping(PEER_BUF_ALLOC);
    assert_eq!(conn.peer_avail_credit(), 0);

    // Push some host data and try to deliver.
    conn.push_host_data(&[1, 2, 3, 4]);
    let (pkt, _) = conn.recv_pkt(4096).unwrap();
    // Should send CREDIT_REQUEST instead of data.
    assert_eq!(pkt.op, VsockOp::CreditRequest as u16);
}

#[test]
fn credit_request_from_peer_yields_credit_update() {
    let mut conn = established_conn();
    let pkt = tx_pkt(VsockOp::CreditRequest, 0);
    conn.send_pkt(&pkt, &[]).unwrap();

    assert!(conn.has_pending_rx());
    let (resp, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(resp.op, VsockOp::CreditUpdate as u16);
    assert_eq!(resp.buf_alloc, CONN_TX_BUF_SIZE);
}

#[test]
fn credit_wrapping_at_u32_boundary() {
    let mut conn = established_conn();

    // Set counters near u32::MAX to test wrapping.
    conn.peer_buf_alloc = 1000;
    conn.rx_cnt = Wrapping(u32::MAX - 500);
    conn.peer_fwd_cnt = Wrapping(u32::MAX - 500);

    // Credit should be full (1000).
    assert_eq!(conn.peer_avail_credit(), 1000);

    // Simulate sending 800 bytes: rx_cnt wraps around u32::MAX.
    conn.rx_cnt += Wrapping(800);
    assert_eq!(conn.peer_avail_credit(), 200);

    // Simulate peer forwarding 800 bytes: peer_fwd_cnt wraps too.
    conn.peer_fwd_cnt += Wrapping(800);
    assert_eq!(conn.peer_avail_credit(), 1000);
}

#[test]
fn proactive_credit_update_after_tx_drain() {
    let mut conn = established_conn();

    // Simulate stale credit: last update was long ago.
    conn.last_fwd_cnt_to_peer = Wrapping(0);
    let initial_fwd_cnt = CONN_TX_BUF_SIZE - super::CONN_CREDIT_UPDATE_THRESHOLD - 6;
    conn.fwd_cnt = Wrapping(initial_fwd_cnt);

    // First RW: just below threshold — no credit update yet.
    let pkt = tx_pkt(VsockOp::Rw, 0);
    conn.send_pkt(&pkt, &[1, 2, 3, 4]).unwrap();
    assert!(!conn.has_pending_rx());

    // Second RW: pushes past threshold — credit update pending.
    conn.send_pkt(&pkt, &[5, 6, 7, 8]).unwrap();

    // Need to flush to advance fwd_cnt before we can get the credit update.
    let mut sink = Vec::new();
    conn.flush_tx_buf(&mut sink).unwrap();

    assert!(conn.has_pending_rx());
    let (cu, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(cu.op, VsockOp::CreditUpdate as u16);
}

// ── Kill and RST tests ──────────────────────────────────────────────

#[test]
fn kill_yields_rst() {
    let mut conn = established_conn();
    conn.kill();
    assert_eq!(conn.state(), ConnState::Killed);
    assert!(conn.has_pending_rx());

    let (pkt, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::Rst as u16);

    // After RST, no more pending RX.
    assert!(matches!(conn.recv_pkt(4096), Err(VsockError::NoData)));
}

#[test]
fn data_in_invalid_state_yields_rst() {
    let mut conn = established_conn();
    conn.state = ConnState::LocalClosed;
    conn.push_host_data(&[1, 2]);

    let (pkt, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::Rst as u16);
}

#[test]
fn rst_has_highest_priority() {
    let mut conn = established_conn();
    // Schedule both data and RST.
    conn.push_host_data(&[1, 2, 3]);
    conn.kill();

    // RST should come first.
    let (pkt, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::Rst as u16);
}

// ── Invalid packet handling tests ───────────────────────────────────

#[test]
fn response_in_wrong_state_is_dropped() {
    let mut conn = established_conn();
    let pkt = tx_pkt(VsockOp::Response, 0);
    // RESPONSE in Established state should be silently dropped.
    conn.send_pkt(&pkt, &[]).unwrap();
    assert_eq!(conn.state(), ConnState::Established);
}

#[test]
fn rw_in_peer_closed_no_send_is_dropped() {
    let mut conn = established_conn();

    // Shut down guest's send direction.
    let shutdown = tx_pkt(VsockOp::Shutdown, VSOCK_FLAGS_SHUTDOWN_SEND);
    conn.send_pkt(&shutdown, &[]).unwrap();
    assert_eq!(conn.state(), ConnState::PeerClosed(false, true));

    // RW from guest should be dropped (guest promised no more send).
    let rw = tx_pkt(VsockOp::Rw, 0);
    conn.send_pkt(&rw, &[1, 2]).unwrap();
    assert!(conn.tx_buf().is_empty());
}

#[test]
fn no_data_when_no_pending_rx() {
    let mut conn = established_conn();
    assert!(!conn.has_pending_rx());
    assert!(matches!(conn.recv_pkt(4096), Err(VsockError::NoData)));
}

// ── Virtqueue test infrastructure ────────────────────────────────────

/// Memory layout for a test virtqueue.
///
/// Places descriptor table, avail ring, and used ring at fixed offsets
/// within guest memory so tests can write descriptor chains and verify
/// used ring results.
struct VirtqTestHelper {
    memory: GuestMemory,
    desc_table_addr: u64,
    avail_ring_addr: u64,
    used_ring_addr: u64,
    queue_size: u16,
    /// Next free descriptor index for building chains.
    next_desc: u16,
    /// Next avail ring entry to write.
    next_avail: u16,
}

impl VirtqTestHelper {
    /// Creates a test helper with a 1 MiB memory region.
    ///
    /// Layout (queue_size=16):
    ///   desc table:  0x10000  (16 * 16 = 256 bytes)
    ///   avail ring:  0x10100  (4 + 16*2 + 2 = 38 bytes, padded)
    ///   used ring:   0x10200  (4 + 16*8 + 2 = 134 bytes, padded)
    ///   data area:   0x20000+ (for descriptor buffers)
    fn new() -> Self {
        let memory = GuestMemory::new(1024 * 1024, 0).unwrap();
        let queue_size: u16 = 16;
        Self {
            memory,
            desc_table_addr: 0x10000,
            avail_ring_addr: 0x10100,
            used_ring_addr: 0x10200,
            queue_size,
            next_desc: 0,
            next_avail: 0,
        }
    }

    /// Configures a VsockDevice queue with this helper's addresses.
    fn configure_queue(&self, dev: &mut VsockDevice, queue_idx: usize) {
        let queues = dev.queues_mut();
        queues[queue_idx].size = self.queue_size;
        queues[queue_idx].ready = true;
        queues[queue_idx].desc_table_addr = self.desc_table_addr;
        queues[queue_idx].avail_ring_addr = self.avail_ring_addr;
        queues[queue_idx].used_ring_addr = self.used_ring_addr;
    }

    /// Writes a descriptor to the descriptor table.
    fn write_desc(&self, idx: u16, addr: u64, len: u32, flags: u16, next: u16) {
        let offset = self.desc_table_addr + u64::from(idx) * 16;
        self.memory
            .write_bytes(offset, &addr.to_le_bytes())
            .unwrap();
        self.memory
            .write_bytes(offset + 8, &len.to_le_bytes())
            .unwrap();
        self.memory
            .write_bytes(offset + 12, &flags.to_le_bytes())
            .unwrap();
        self.memory
            .write_bytes(offset + 14, &next.to_le_bytes())
            .unwrap();
    }

    /// Writes a TX descriptor chain (header + optional payload) and adds to avail ring.
    /// Returns the head descriptor index.
    fn push_tx_chain(&mut self, pkt: &VsockPacket, payload: &[u8]) -> u16 {
        let head = self.next_desc;
        let hdr_buf_addr: u64 = 0x20000 + u64::from(head) * 0x1000;

        // Write header bytes to guest memory.
        self.memory
            .write_bytes(hdr_buf_addr, &pkt.to_bytes())
            .unwrap();

        if payload.is_empty() {
            // Single descriptor: header only, no NEXT flag.
            self.write_desc(head, hdr_buf_addr, VSOCK_PKT_HDR_SIZE as u32, 0, 0);
            self.next_desc += 1;
        } else {
            let data_idx = head + 1;
            let data_buf_addr = hdr_buf_addr + 0x800;
            self.memory.write_bytes(data_buf_addr, payload).unwrap();

            // Header descriptor with NEXT → data descriptor.
            self.write_desc(
                head,
                hdr_buf_addr,
                VSOCK_PKT_HDR_SIZE as u32,
                VIRTQ_DESC_F_NEXT,
                data_idx,
            );
            self.write_desc(data_idx, data_buf_addr, payload.len() as u32, 0, 0);
            self.next_desc += 2;
        }

        // Add to avail ring.
        let avail_entry_offset = self.avail_ring_addr + 4 + u64::from(self.next_avail) * 2;
        self.memory
            .write_bytes(avail_entry_offset, &head.to_le_bytes())
            .unwrap();
        self.next_avail += 1;

        // Update avail idx.
        self.memory
            .write_bytes(self.avail_ring_addr + 2, &self.next_avail.to_le_bytes())
            .unwrap();

        head
    }

    /// Pushes an empty RX descriptor chain (device-writable header + data).
    /// Returns the head descriptor index.
    fn push_rx_chain(&mut self, data_buf_len: u32) -> u16 {
        let head = self.next_desc;
        let hdr_buf_addr: u64 = 0x20000 + u64::from(head) * 0x1000;
        let data_idx = head + 1;
        let data_buf_addr = hdr_buf_addr + 0x800;

        // Header descriptor (device-writable) with NEXT → data descriptor.
        self.write_desc(
            head,
            hdr_buf_addr,
            VSOCK_PKT_HDR_SIZE as u32,
            VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
            data_idx,
        );
        self.write_desc(data_idx, data_buf_addr, data_buf_len, VIRTQ_DESC_F_WRITE, 0);
        self.next_desc += 2;

        // Add to avail ring.
        let avail_entry_offset = self.avail_ring_addr + 4 + u64::from(self.next_avail) * 2;
        self.memory
            .write_bytes(avail_entry_offset, &head.to_le_bytes())
            .unwrap();
        self.next_avail += 1;
        self.memory
            .write_bytes(self.avail_ring_addr + 2, &self.next_avail.to_le_bytes())
            .unwrap();

        head
    }

    /// Pushes a single-descriptor RX chain where payload space is inline
    /// after the 44-byte header in the same buffer.
    fn push_rx_chain_single(&mut self, inline_payload_len: u32) -> u16 {
        let head = self.next_desc;
        let hdr_buf_addr: u64 = 0x20000 + u64::from(head) * 0x1000;
        self.write_desc(
            head,
            hdr_buf_addr,
            VSOCK_PKT_HDR_SIZE as u32 + inline_payload_len,
            VIRTQ_DESC_F_WRITE,
            0,
        );
        self.next_desc += 1;

        let avail_entry_offset = self.avail_ring_addr + 4 + u64::from(self.next_avail) * 2;
        self.memory
            .write_bytes(avail_entry_offset, &head.to_le_bytes())
            .unwrap();
        self.next_avail += 1;
        self.memory
            .write_bytes(self.avail_ring_addr + 2, &self.next_avail.to_le_bytes())
            .unwrap();

        head
    }

    /// Reads a used ring entry (id, len) at the given index.
    fn read_used_entry(&self, idx: u16) -> (u32, u32) {
        let offset = self.used_ring_addr + 4 + u64::from(idx) * 8;
        let id_bytes = self.memory.read_bytes(offset, 4).unwrap();
        let len_bytes = self.memory.read_bytes(offset + 4, 4).unwrap();
        (
            u32::from_le_bytes([id_bytes[0], id_bytes[1], id_bytes[2], id_bytes[3]]),
            u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]),
        )
    }

    /// Reads the used ring index.
    fn read_used_idx(&self) -> u16 {
        let bytes = self.memory.read_bytes(self.used_ring_addr + 2, 2).unwrap();
        u16::from_le_bytes([bytes[0], bytes[1]])
    }

    /// Reads a VsockPacket from the given guest address.
    fn read_pkt_at(&self, addr: u64) -> VsockPacket {
        let bytes = self.memory.read_bytes(addr, VSOCK_PKT_HDR_SIZE).unwrap();
        VsockPacket::from_bytes(&bytes).unwrap()
    }

    /// Returns the guest address where descriptor `idx` points.
    fn desc_buf_addr(&self, idx: u16) -> u64 {
        0x20000 + u64::from(idx) * 0x1000
    }

    // ── Dual-queue support ─────────────────────────────────────────

    /// RX queue addresses (separate region in the same memory).
    const RX_DESC_TABLE: u64 = 0x30000;
    const RX_AVAIL_RING: u64 = 0x30100;
    const RX_USED_RING: u64 = 0x30200;
    const RX_DATA_BASE: u64 = 0x40000;

    /// Configures the RX queue at alternate addresses in the same memory.
    fn configure_rx_queue(&self, dev: &mut VsockDevice) {
        let queues = dev.queues_mut();
        queues[RXQ].size = self.queue_size;
        queues[RXQ].ready = true;
        queues[RXQ].desc_table_addr = Self::RX_DESC_TABLE;
        queues[RXQ].avail_ring_addr = Self::RX_AVAIL_RING;
        queues[RXQ].used_ring_addr = Self::RX_USED_RING;
    }

    /// Pushes an RX descriptor chain at the alternate RX addresses.
    /// Uses a separate descriptor index counter to avoid collisions with TX.
    fn push_rx_chain_alt(&mut self, data_buf_len: u32, rx_desc_idx: u16) -> u16 {
        let head = rx_desc_idx;
        let hdr_buf_addr: u64 = Self::RX_DATA_BASE + u64::from(head) * 0x1000;
        let data_idx = head + 1;
        let data_buf_addr = hdr_buf_addr + 0x800;

        // Write descriptors to the RX descriptor table.
        let rx_desc = Self::RX_DESC_TABLE;
        let desc_offset = |idx: u16| rx_desc + u64::from(idx) * 16;
        self.memory
            .write_bytes(desc_offset(head), &hdr_buf_addr.to_le_bytes())
            .unwrap();
        self.memory
            .write_bytes(
                desc_offset(head) + 8,
                &(VSOCK_PKT_HDR_SIZE as u32).to_le_bytes(),
            )
            .unwrap();
        self.memory
            .write_bytes(
                desc_offset(head) + 12,
                &(VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT).to_le_bytes(),
            )
            .unwrap();
        self.memory
            .write_bytes(desc_offset(head) + 14, &data_idx.to_le_bytes())
            .unwrap();

        self.memory
            .write_bytes(desc_offset(data_idx), &data_buf_addr.to_le_bytes())
            .unwrap();
        self.memory
            .write_bytes(desc_offset(data_idx) + 8, &data_buf_len.to_le_bytes())
            .unwrap();
        self.memory
            .write_bytes(
                desc_offset(data_idx) + 12,
                &VIRTQ_DESC_F_WRITE.to_le_bytes(),
            )
            .unwrap();
        self.memory
            .write_bytes(desc_offset(data_idx) + 14, &0u16.to_le_bytes())
            .unwrap();

        // Add to RX avail ring.
        let avail_entry = Self::RX_AVAIL_RING + 4 + u64::from(rx_desc_idx / 2) * 2;
        self.memory
            .write_bytes(avail_entry, &head.to_le_bytes())
            .unwrap();
        let avail_count = rx_desc_idx / 2 + 1;
        self.memory
            .write_bytes(Self::RX_AVAIL_RING + 2, &avail_count.to_le_bytes())
            .unwrap();

        head
    }

    /// Reads the RX used ring index.
    fn read_rx_used_idx(&self) -> u16 {
        let bytes = self.memory.read_bytes(Self::RX_USED_RING + 2, 2).unwrap();
        u16::from_le_bytes([bytes[0], bytes[1]])
    }

    /// Reads a VsockPacket from the RX data area.
    fn read_rx_pkt(&self, rx_desc_idx: u16) -> VsockPacket {
        let addr = Self::RX_DATA_BASE + u64::from(rx_desc_idx) * 0x1000;
        let bytes = self.memory.read_bytes(addr, VSOCK_PKT_HDR_SIZE).unwrap();
        VsockPacket::from_bytes(&bytes).unwrap()
    }
}

/// Builds a TX packet from the guest (src_cid=3, dst_cid=HOST_CID).
fn make_guest_pkt(op: VsockOp, guest_port: u32, host_port: u32) -> VsockPacket {
    VsockPacket {
        src_cid: 3,
        dst_cid: HOST_CID,
        src_port: guest_port,
        dst_port: host_port,
        len: 0,
        pkt_type: VSOCK_TYPE_STREAM,
        op: op as u16,
        flags: 0,
        buf_alloc: 65536,
        fwd_cnt: 0,
    }
}

// ── VsockOp conversion tests ────────────────────────────────────────

#[test]
fn vsock_op_from_u16_valid() {
    assert_eq!(VsockOp::from_u16(1), Some(VsockOp::Request));
    assert_eq!(VsockOp::from_u16(5), Some(VsockOp::Rw));
    assert_eq!(VsockOp::from_u16(7), Some(VsockOp::CreditRequest));
}

#[test]
fn vsock_op_from_u16_invalid() {
    assert_eq!(VsockOp::from_u16(0), None);
    assert_eq!(VsockOp::from_u16(8), None);
    assert_eq!(VsockOp::from_u16(255), None);
}

// ── TX queue processing tests ───────────────────────────────────────

#[test]
fn tx_queue_empty_returns_false() {
    let mut dev = VsockDevice::new(3);
    let helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, TXQ);
    // Queue is configured but avail ring is empty → no work.
    let result = dev.process_queue(TXQ, &helper.memory).unwrap();
    assert!(!result);
}

#[test]
fn tx_queue_unready_returns_false() {
    let mut dev = VsockDevice::new(3);
    // Don't configure the queue (not ready).
    let memory = GuestMemory::new(1024 * 1024, 0).unwrap();
    let result = dev.process_queue(TXQ, &memory).unwrap();
    assert!(!result);
}

#[test]
fn tx_invalid_queue_index_returns_false() {
    let mut dev = VsockDevice::new(3);
    let memory = GuestMemory::new(1024 * 1024, 0).unwrap();
    let result = dev.process_queue(99, &memory).unwrap();
    assert!(!result);
}

#[test]
fn tx_request_creates_connection() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, TXQ);

    let pkt = make_guest_pkt(VsockOp::Request, 1234, 5678);
    helper.push_tx_chain(&pkt, &[]);

    let result = dev.process_queue(TXQ, &helper.memory).unwrap();
    assert!(result);

    // Connection should exist.
    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    assert!(dev.connections().contains_key(&key));
    let conn = &dev.connections()[&key];
    assert_eq!(conn.state(), ConnState::PeerInit);
}

#[test]
fn tx_rst_removes_connection() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, TXQ);

    // First create a connection via REQUEST.
    let req = make_guest_pkt(VsockOp::Request, 1234, 5678);
    helper.push_tx_chain(&req, &[]);
    dev.process_queue(TXQ, &helper.memory).unwrap();

    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    assert!(dev.connections().contains_key(&key));

    // Now send RST.
    let rst = make_guest_pkt(VsockOp::Rst, 1234, 5678);
    helper.push_tx_chain(&rst, &[]);
    dev.process_queue(TXQ, &helper.memory).unwrap();

    assert!(!dev.connections().contains_key(&key));
}

#[test]
fn tx_rw_buffers_data_in_connection() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, TXQ);

    // Create connection.
    let req = make_guest_pkt(VsockOp::Request, 1234, 5678);
    helper.push_tx_chain(&req, &[]);
    dev.process_queue(TXQ, &helper.memory).unwrap();

    // The connection is PeerInit — transition to Established by simulating RESPONSE.
    // recv_pkt on PeerInit returns RESPONSE, which transitions to Established.
    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    {
        let conn = dev.connections.get_mut(&key).unwrap();
        // Consume the pending RESPONSE to move to Established.
        let (pkt, _) = conn.recv_pkt(4096).unwrap();
        assert_eq!(pkt.op, VsockOp::Response as u16);
        assert_eq!(conn.state(), ConnState::Established);
    }

    // Now send RW with data payload.
    let mut rw = make_guest_pkt(VsockOp::Rw, 1234, 5678);
    rw.len = 5;
    helper.push_tx_chain(&rw, &[10, 20, 30, 40, 50]);
    dev.process_queue(TXQ, &helper.memory).unwrap();

    let conn = &dev.connections()[&key];
    assert!(!conn.tx_buf().is_empty());
}

#[test]
fn tx_shutdown_transitions_state() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, TXQ);

    // Create and establish connection.
    let req = make_guest_pkt(VsockOp::Request, 1234, 5678);
    helper.push_tx_chain(&req, &[]);
    dev.process_queue(TXQ, &helper.memory).unwrap();
    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    {
        let conn = dev.connections.get_mut(&key).unwrap();
        let _ = conn.recv_pkt(4096).unwrap();
    }

    // Send SHUTDOWN with both flags.
    let mut shutdown = make_guest_pkt(VsockOp::Shutdown, 1234, 5678);
    shutdown.flags = VSOCK_FLAGS_SHUTDOWN_RCV | VSOCK_FLAGS_SHUTDOWN_SEND;
    helper.push_tx_chain(&shutdown, &[]);
    dev.process_queue(TXQ, &helper.memory).unwrap();

    let conn = &dev.connections()[&key];
    assert_eq!(conn.state(), ConnState::PeerClosed(true, true));
}

#[test]
fn tx_malformed_header_silently_dropped() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, TXQ);

    // Write a descriptor with only 10 bytes (less than 44-byte header).
    let head = helper.next_desc;
    let buf_addr: u64 = 0x20000;
    helper.memory.write_bytes(buf_addr, &[0u8; 10]).unwrap();
    helper.write_desc(head, buf_addr, 10, 0, 0);
    helper.next_desc += 1;

    // Add to avail ring.
    let avail_offset = helper.avail_ring_addr + 4;
    helper
        .memory
        .write_bytes(avail_offset, &head.to_le_bytes())
        .unwrap();
    helper.next_avail += 1;
    helper
        .memory
        .write_bytes(helper.avail_ring_addr + 2, &helper.next_avail.to_le_bytes())
        .unwrap();

    // Should process (return true = consumed avail entry) but not create any connection.
    let result = dev.process_queue(TXQ, &helper.memory).unwrap();
    assert!(result);
    assert!(dev.connections().is_empty());
}

#[test]
fn tx_used_ring_updated_after_processing() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, TXQ);

    let pkt = make_guest_pkt(VsockOp::Request, 1234, 5678);
    let head = helper.push_tx_chain(&pkt, &[]);
    dev.process_queue(TXQ, &helper.memory).unwrap();

    // Used ring should have one entry.
    assert_eq!(helper.read_used_idx(), 1);
    let (id, len) = helper.read_used_entry(0);
    assert_eq!(id, u32::from(head));
    assert_eq!(len, 0); // TX entries report 0 bytes written.
}

#[test]
fn tx_multiple_packets_processed_in_order() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, TXQ);

    let req1 = make_guest_pkt(VsockOp::Request, 1001, 80);
    let req2 = make_guest_pkt(VsockOp::Request, 1002, 443);
    let head1 = helper.push_tx_chain(&req1, &[]);
    let head2 = helper.push_tx_chain(&req2, &[]);

    dev.process_queue(TXQ, &helper.memory).unwrap();

    // Both connections should exist.
    assert_eq!(dev.connections().len(), 2);
    assert_eq!(helper.read_used_idx(), 2);

    let (id1, _) = helper.read_used_entry(0);
    let (id2, _) = helper.read_used_entry(1);
    assert_eq!(id1, u32::from(head1));
    assert_eq!(id2, u32::from(head2));
}

// ── RX queue processing tests ───────────────────────────────────────

#[test]
fn rx_queue_empty_returns_false() {
    let mut dev = VsockDevice::new(3);
    let helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, RXQ);
    let result = dev.process_queue(RXQ, &helper.memory).unwrap();
    assert!(!result);
}

#[test]
fn rx_no_pending_data_returns_false() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, RXQ);

    // Provide an RX buffer but no connections have pending data.
    helper.push_rx_chain(4096);
    let result = dev.process_queue(RXQ, &helper.memory).unwrap();
    assert!(!result);
}

#[test]
fn rx_delivers_response_after_request() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();

    // Configure TX queue at default addresses.
    helper.configure_queue(&mut dev, TXQ);

    // Configure RX queue at alternate addresses in the SAME memory.
    helper.configure_rx_queue(&mut dev);
    helper.push_rx_chain_alt(4096, 0);

    // Guest sends REQUEST on TX queue.
    let req = make_guest_pkt(VsockOp::Request, 1234, 5678);
    helper.push_tx_chain(&req, &[]);

    // Process TX → should also fill RX with RESPONSE.
    let result = dev.process_queue(TXQ, &helper.memory).unwrap();
    assert!(result);

    // RX used ring should have the RESPONSE.
    assert_eq!(helper.read_rx_used_idx(), 1);

    // Read the response packet.
    let rx_pkt = helper.read_rx_pkt(0);
    assert_eq!(rx_pkt.op, VsockOp::Response as u16);
    assert_eq!(rx_pkt.src_cid, HOST_CID);
    assert_eq!(rx_pkt.dst_cid, 3);
    assert_eq!(rx_pkt.src_port, 5678);
    assert_eq!(rx_pkt.dst_port, 1234);
}

#[test]
fn rx_delivers_host_data() {
    let mut dev = VsockDevice::new(3);

    // Create and establish a connection directly.
    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    let mut conn = VsockConnection::new_peer_init(HOST_CID, 3, 5678, 1234, 65536);
    // Drain the RESPONSE to move to Established.
    let _ = conn.recv_pkt(4096).unwrap();
    // Push host data.
    conn.push_host_data(&[0xAA, 0xBB, 0xCC]);
    dev.connections.insert(key, conn);

    // Set up RX queue.
    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, RXQ);
    helper.push_rx_chain(4096);

    let result = dev.process_queue(RXQ, &helper.memory).unwrap();
    assert!(result);

    // Check the response packet.
    let rx_pkt = helper.read_pkt_at(helper.desc_buf_addr(0));
    assert_eq!(rx_pkt.op, VsockOp::Rw as u16);
    assert_eq!(rx_pkt.len, 3);

    // Read payload from data descriptor.
    let data_addr = helper.desc_buf_addr(0) + 0x800;
    let payload = helper.memory.read_bytes(data_addr, 3).unwrap();
    assert_eq!(payload, vec![0xAA, 0xBB, 0xCC]);
}

#[test]
fn rx_delivers_host_data_with_single_descriptor_chain() {
    let mut dev = VsockDevice::new(3);

    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    let mut conn = VsockConnection::new_peer_init(HOST_CID, 3, 5678, 1234, 65536);
    let _ = conn.recv_pkt(4096).unwrap(); // drain RESPONSE
    conn.push_host_data(&[0xDE, 0xAD, 0xBE, 0xEF]);
    dev.connections.insert(key, conn);

    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, RXQ);
    let head = helper.push_rx_chain_single(64);

    let result = dev.process_queue(RXQ, &helper.memory).unwrap();
    assert!(result);

    let hdr_addr = helper.desc_buf_addr(head);
    let rx_pkt = helper.read_pkt_at(hdr_addr);
    assert_eq!(rx_pkt.op, VsockOp::Rw as u16);
    assert_eq!(rx_pkt.len, 4);

    let payload = helper
        .memory
        .read_bytes(hdr_addr + VSOCK_PKT_HDR_SIZE as u64, 4)
        .unwrap();
    assert_eq!(payload, vec![0xDE, 0xAD, 0xBE, 0xEF]);
}

#[test]
fn rx_rst_removes_connection() {
    let mut dev = VsockDevice::new(3);

    // Create a killed connection.
    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    let mut conn = VsockConnection::new_peer_init(HOST_CID, 3, 5678, 1234, 65536);
    conn.kill();
    dev.connections.insert(key, conn);

    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, RXQ);
    helper.push_rx_chain(4096);

    let result = dev.process_queue(RXQ, &helper.memory).unwrap();
    assert!(result);

    // RST should have been sent and connection removed.
    let rx_pkt = helper.read_pkt_at(helper.desc_buf_addr(0));
    assert_eq!(rx_pkt.op, VsockOp::Rst as u16);
    assert!(!dev.connections().contains_key(&key));
}

#[test]
fn rx_used_ring_reports_correct_length() {
    let mut dev = VsockDevice::new(3);

    // Connection with pending RESPONSE (44 bytes header, no payload).
    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    let conn = VsockConnection::new_peer_init(HOST_CID, 3, 5678, 1234, 65536);
    dev.connections.insert(key, conn);

    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, RXQ);
    let head = helper.push_rx_chain(4096);

    dev.process_queue(RXQ, &helper.memory).unwrap();

    let (id, len) = helper.read_used_entry(0);
    assert_eq!(id, u32::from(head));
    // RESPONSE is header-only: 44 bytes.
    assert_eq!(len, VSOCK_PKT_HDR_SIZE as u32);
}

#[test]
fn rx_data_used_ring_includes_payload_length() {
    let mut dev = VsockDevice::new(3);

    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    let mut conn = VsockConnection::new_peer_init(HOST_CID, 3, 5678, 1234, 65536);
    let _ = conn.recv_pkt(4096).unwrap(); // drain RESPONSE
    conn.push_host_data(&[1, 2, 3, 4, 5]);
    dev.connections.insert(key, conn);

    let mut helper = VirtqTestHelper::new();
    helper.configure_queue(&mut dev, RXQ);
    let head = helper.push_rx_chain(4096);

    dev.process_queue(RXQ, &helper.memory).unwrap();

    let (id, len) = helper.read_used_entry(0);
    assert_eq!(id, u32::from(head));
    // Header (44) + payload (5) = 49 bytes.
    assert_eq!(len, VSOCK_PKT_HDR_SIZE as u32 + 5);
}

// ── Combined TX+RX tests ────────────────────────────────────────────

#[test]
fn request_response_round_trip() {
    let mut dev = VsockDevice::new(3);
    let mut helper = VirtqTestHelper::new();

    // Configure both queues in the same memory.
    helper.configure_queue(&mut dev, TXQ);
    helper.configure_rx_queue(&mut dev);

    // Guest sends REQUEST and provides RX buffer.
    let req = make_guest_pkt(VsockOp::Request, 1234, 5678);
    helper.push_tx_chain(&req, &[]);
    helper.push_rx_chain_alt(4096, 0);

    // Single process_queue(TXQ) should:
    // 1. Parse REQUEST and create connection
    // 2. Fill RX with RESPONSE (connection transitions to Established)
    let result = dev.process_queue(TXQ, &helper.memory).unwrap();
    assert!(result);

    // Verify connection is Established (RESPONSE was consumed by RX).
    let key = ConnMapKey {
        local_port: 5678,
        peer_port: 1234,
    };
    let conn = &dev.connections()[&key];
    assert_eq!(conn.state(), ConnState::Established);

    // Verify TX used ring.
    assert_eq!(helper.read_used_idx(), 1);

    // Verify RX has RESPONSE.
    assert_eq!(helper.read_rx_used_idx(), 1);
}

#[test]
fn reset_clears_connections_after_activation() {
    let mut dev = VsockDevice::new(3);
    let key = ConnMapKey {
        local_port: 1,
        peer_port: 2,
    };
    let conn = VsockConnection::new_peer_init(HOST_CID, 3, 1, 2, 65536);
    dev.connections.insert(key, conn);
    dev.activate().unwrap();
    assert!(!dev.connections().is_empty());

    dev.reset();
    assert!(dev.connections().is_empty());
}

#[test]
fn reset_preserves_connections_before_activation() {
    let mut dev = VsockDevice::new(3);
    let key = ConnMapKey {
        local_port: 1,
        peer_port: 2,
    };
    let conn = VsockConnection::new_local_init(HOST_CID, 3, 1, 2);
    dev.connections.insert(key, conn);
    assert!(!dev.connections().is_empty());
    assert!(!dev.is_activated());

    dev.reset();

    assert!(
        dev.connections().contains_key(&key),
        "pre-activation reset should not drop pending host connection"
    );
}

#[test]
fn event_queue_returns_false() {
    let mut dev = VsockDevice::new(3);
    let memory = GuestMemory::new(1024 * 1024, 0).unwrap();
    let result = dev.process_queue(2, &memory).unwrap();
    assert!(!result);
}

// ── Credit flow re-insertion tests ──────────────────────────────────

#[test]
fn credit_request_re_inserts_rw_pending() {
    let mut conn = established_conn();

    // Exhaust peer credit so CreditRequest is sent instead of data.
    conn.rx_cnt = Wrapping(PEER_BUF_ALLOC);
    assert_eq!(conn.peer_avail_credit(), 0);

    conn.push_host_data(&[1, 2, 3, 4]);
    let (pkt, payload) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::CreditRequest as u16);
    assert!(payload.is_empty());

    // PendingRx::Rw must still be set so data delivery is retried
    // after the guest provides credit.
    assert!(
        conn.has_pending_rx(),
        "Rw must remain pending after CreditRequest"
    );
}

#[test]
fn send_pkt_re_inserts_rw_when_credit_arrives() {
    let mut conn = established_conn();

    // Exhaust peer credit.
    conn.rx_cnt = Wrapping(PEER_BUF_ALLOC);
    conn.peer_fwd_cnt = Wrapping(0);
    assert_eq!(conn.peer_avail_credit(), 0);

    // Push host data — it sits in rx_buf but cannot be delivered.
    conn.push_host_data(&[0xAA, 0xBB]);
    let (pkt, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::CreditRequest as u16);

    // Simulate guest sending CreditUpdate with fwd_cnt that frees space.
    let credit_pkt = VsockPacket {
        src_cid: PEER_CID,
        dst_cid: LOCAL_CID,
        src_port: PEER_PORT,
        dst_port: LOCAL_PORT,
        len: 0,
        pkt_type: VSOCK_TYPE_STREAM,
        op: VsockOp::CreditUpdate as u16,
        flags: 0,
        buf_alloc: PEER_BUF_ALLOC,
        fwd_cnt: PEER_BUF_ALLOC, // guest forwarded everything
    };
    conn.send_pkt(&credit_pkt, &[]).unwrap();

    // Credit is now available and rx_buf has data → Rw should be pending.
    assert!(conn.peer_avail_credit() > 0);
    assert!(
        conn.has_pending_rx(),
        "Rw must be re-inserted when credit arrives"
    );

    // Now the data should actually be delivered.
    let (data_pkt, payload) = conn.recv_pkt(4096).unwrap();
    assert_eq!(data_pkt.op, VsockOp::Rw as u16);
    assert_eq!(payload, &[0xAA, 0xBB]);
}

#[test]
fn host_initiated_conn_data_delivered_after_response() {
    // End-to-end: host initiates connection, pushes data, guest responds,
    // and data must be deliverable.
    let mut conn = VsockConnection::new_local_init(LOCAL_CID, PEER_CID, LOCAL_PORT, PEER_PORT);

    // Push host data before handshake completes.
    conn.push_host_data(&[10, 20, 30]);

    // Drain the REQUEST packet.
    let (req, _) = conn.recv_pkt(4096).unwrap();
    assert_eq!(req.op, VsockOp::Request as u16);

    // Data delivery deferred — connection still in LocalInit.
    // (PendingRx::Rw is re-inserted by recv_data_pkt for LocalInit state.)
    assert!(conn.has_pending_rx());

    // Guest sends RESPONSE with buf_alloc > 0.
    let resp = VsockPacket {
        src_cid: PEER_CID,
        dst_cid: LOCAL_CID,
        src_port: PEER_PORT,
        dst_port: LOCAL_PORT,
        len: 0,
        pkt_type: VSOCK_TYPE_STREAM,
        op: VsockOp::Response as u16,
        flags: 0,
        buf_alloc: PEER_BUF_ALLOC,
        fwd_cnt: 0,
    };
    conn.send_pkt(&resp, &[]).unwrap();
    assert_eq!(conn.state(), ConnState::Established);

    // send_pkt should have re-inserted Rw because credit > 0 and rx_buf non-empty.
    assert!(
        conn.has_pending_rx(),
        "Rw must be pending after RESPONSE with credit"
    );

    // Data should now be deliverable.
    let (data_pkt, payload) = conn.recv_pkt(4096).unwrap();
    assert_eq!(data_pkt.op, VsockOp::Rw as u16);
    assert_eq!(payload, &[10, 20, 30]);
}

#[test]
fn host_initiated_conn_requests_credit_after_zero_credit_response() {
    let mut conn = VsockConnection::new_local_init(LOCAL_CID, PEER_CID, LOCAL_PORT, PEER_PORT);

    conn.push_host_data(&[10, 20, 30]);
    let _ = conn.recv_pkt(4096).unwrap();

    let resp = VsockPacket {
        src_cid: PEER_CID,
        dst_cid: LOCAL_CID,
        src_port: PEER_PORT,
        dst_port: LOCAL_PORT,
        len: 0,
        pkt_type: VSOCK_TYPE_STREAM,
        op: VsockOp::Response as u16,
        flags: 0,
        buf_alloc: 0,
        fwd_cnt: 0,
    };
    conn.send_pkt(&resp, &[]).unwrap();

    let (credit_req, payload) = conn.recv_pkt(4096).unwrap();
    assert_eq!(credit_req.op, VsockOp::CreditRequest as u16);
    assert!(payload.is_empty());
    assert!(
        conn.has_pending_rx(),
        "Rw must remain pending after requesting credit"
    );
}

#[test]
fn rw_data_waits_for_guest_payload_buffer_capacity() {
    let mut conn = established_conn();

    conn.push_host_data(&[0xAA, 0xBB, 0xCC]);

    assert!(
        matches!(conn.recv_pkt(0), Err(VsockError::NoData)),
        "host data must not be emitted as a zero-length RW packet"
    );
    assert!(
        conn.has_pending_rx(),
        "Rw must remain pending until the guest provides payload capacity"
    );

    let (pkt, payload) = conn.recv_pkt(4096).unwrap();
    assert_eq!(pkt.op, VsockOp::Rw as u16);
    assert_eq!(payload, &[0xAA, 0xBB, 0xCC]);
}
