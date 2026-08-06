use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use super::{NetDevice, PacketIo, VIRTIO_F_VERSION_1, VIRTIO_NET_F_MAC, VNET_HDR_SIZE};
use crate::memory::GuestMemory;
use crate::transport::{
    DeviceType, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioDevice,
};

// ── Constructor tests ────────────────────────────────────────────────

#[test]
fn new_creates_device_with_given_mac() {
    let mac = [0x02, 0x56, 0x49, 0x53, 0x00, 0x01];
    let dev = NetDevice::new(mac);
    assert_eq!(dev.mac_addr(), &mac);
}

#[test]
fn generate_mac_returns_locally_administered() {
    let mac = NetDevice::generate_mac();
    // Bit 1 of the first byte must be set (locally administered).
    assert_ne!(mac[0] & 0x02, 0, "locally-administered bit must be set");
    // Bit 0 of the first byte must be clear (unicast).
    assert_eq!(mac[0] & 0x01, 0, "multicast bit must be clear");
}

#[test]
fn vnet_header_size_matches_modern_virtio_net_header_v1() {
    assert_eq!(
        VNET_HDR_SIZE, 12,
        "virtio version 1 uses the flattened 12-byte virtio_net_hdr_v1 header"
    );
}

// ── VirtioDevice trait tests ─────────────────────────────────────────

#[test]
fn device_type_returns_net() {
    let dev = NetDevice::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    assert_eq!(dev.device_type(), DeviceType::Net);
}

#[test]
fn avail_features_includes_mac_and_version_1() {
    let dev = NetDevice::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let features = dev.avail_features();
    assert_ne!(
        features & VIRTIO_NET_F_MAC,
        0,
        "VIRTIO_NET_F_MAC must be set"
    );
    assert_ne!(
        features & VIRTIO_F_VERSION_1,
        0,
        "VIRTIO_F_VERSION_1 must be set"
    );
}

#[test]
fn acked_features_starts_at_zero() {
    let dev = NetDevice::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    assert_eq!(dev.acked_features(), 0, "acked_features starts at 0");
}

#[test]
fn acked_features_roundtrip() {
    let mut dev = NetDevice::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    dev.set_acked_features(VIRTIO_F_VERSION_1);
    assert_eq!(dev.acked_features(), VIRTIO_F_VERSION_1);
}

#[test]
fn queues_returns_two_queues() {
    let dev = NetDevice::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let queues = dev.queues();
    assert_eq!(queues.len(), 2, "net device has exactly 2 virtqueues");
}

#[test]
fn queue_max_sizes_are_256() {
    let dev = NetDevice::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let queues = dev.queues();
    assert_eq!(queues[0].max_size, 256, "rx queue max_size");
    assert_eq!(queues[1].max_size, 256, "tx queue max_size");
}

#[test]
fn queues_mut_allows_configuration() {
    let mut dev = NetDevice::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    dev.queues_mut()[0].ready = true;
    dev.queues_mut()[0].size = 128;
    dev.queues_mut()[1].desc_table_addr = 0x1000;

    assert!(dev.queues()[0].ready);
    assert_eq!(dev.queues()[0].size, 128);
    assert_eq!(dev.queues()[1].desc_table_addr, 0x1000);
}

#[test]
fn read_config_returns_mac_bytes() {
    let mac = [0x02, 0x56, 0x49, 0x53, 0x00, 0x01];
    let dev = NetDevice::new(mac);

    let mut buf = [0u8; 6];
    dev.read_config(0, &mut buf);
    assert_eq!(buf, mac);
}

#[test]
fn read_config_partial_from_offset() {
    let mac = [0x02, 0x56, 0x49, 0x53, 0x00, 0x01];
    let dev = NetDevice::new(mac);

    let mut buf = [0u8; 3];
    dev.read_config(3, &mut buf);
    assert_eq!(buf, [0x53, 0x00, 0x01]);
}

#[test]
fn read_config_beyond_mac_returns_zeros() {
    let dev = NetDevice::new([0x02, 0x56, 0x49, 0x53, 0x00, 0x01]);

    let mut buf = [0xFFu8; 4];
    dev.read_config(6, &mut buf);
    assert_eq!(
        buf,
        [0, 0, 0, 0],
        "reads beyond config space should return zeros"
    );
}

#[test]
fn read_config_spanning_boundary_pads_with_zeros() {
    let mac = [0x02, 0x56, 0x49, 0x53, 0x00, 0x01];
    let dev = NetDevice::new(mac);

    // Read 4 bytes starting at offset 4 — only 2 valid MAC bytes, then zeros
    let mut buf = [0xFFu8; 4];
    dev.read_config(4, &mut buf);
    assert_eq!(buf, [0x00, 0x01, 0x00, 0x00]);
}

#[test]
fn write_config_is_noop() {
    let mac = [0x02, 0x56, 0x49, 0x53, 0x00, 0x01];
    let mut dev = NetDevice::new(mac);

    dev.write_config(0, &[0xFF; 6]);

    let mut buf = [0u8; 6];
    dev.read_config(0, &mut buf);
    assert_eq!(buf, mac, "write_config must be a no-op");
}

#[test]
fn activate_succeeds_and_is_activated_true() {
    let mut dev = NetDevice::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    assert!(!dev.is_activated(), "device should start deactivated");
    dev.activate().unwrap();
    assert!(
        dev.is_activated(),
        "device should be activated after activate()"
    );
}

#[test]
fn reset_clears_activation_and_resets_queues() {
    let mut dev = NetDevice::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);

    dev.activate().unwrap();
    dev.queues_mut()[0].size = 128;
    dev.queues_mut()[0].ready = true;
    dev.queues_mut()[1].size = 64;
    dev.queues_mut()[1].ready = true;

    dev.reset();

    assert!(!dev.is_activated(), "reset should deactivate");
    assert_eq!(dev.queues()[0].size, 0, "reset should clear rx queue size");
    assert!(!dev.queues()[0].ready, "reset should clear rx queue ready");
    assert_eq!(dev.queues()[1].size, 0, "reset should clear tx queue size");
    assert!(!dev.queues()[1].ready, "reset should clear tx queue ready");
}

// ── MockPacketIo ────────────────────────────────────────────────────

/// Shared state for verifying packets sent by the TX path.
#[derive(Debug, Default, Clone)]
struct MockState {
    sent: Arc<Mutex<Vec<Vec<u8>>>>,
    recv_queue: Arc<Mutex<VecDeque<Vec<u8>>>>,
}

/// Mock packet I/O backend for testing process_queue.
struct MockPacketIo {
    state: MockState,
}

impl MockPacketIo {
    fn new() -> (Self, MockState) {
        let state = MockState::default();
        (
            Self {
                state: state.clone(),
            },
            state,
        )
    }

    fn with_recv(packets: Vec<Vec<u8>>) -> (Self, MockState) {
        let state = MockState {
            sent: Arc::new(Mutex::new(Vec::new())),
            recv_queue: Arc::new(Mutex::new(VecDeque::from(packets))),
        };
        (
            Self {
                state: state.clone(),
            },
            state,
        )
    }
}

impl PacketIo for MockPacketIo {
    fn send(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        let len = buf.len();
        self.state.sent.lock().unwrap().push(buf.to_vec());
        Ok(len)
    }

    fn try_recv(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        match self.state.recv_queue.lock().unwrap().pop_front() {
            Some(pkt) => {
                let len = std::cmp::min(pkt.len(), buf.len());
                buf[..len].copy_from_slice(&pkt[..len]);
                Ok(len)
            }
            None => Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "no packets available",
            )),
        }
    }
}

// ── Test helpers ────────────────────────────────────────────────────

/// Guest memory layout for net I/O tests:
/// - Descriptor table at 0x1000
/// - Avail ring at 0x2000
/// - Used ring at 0x3000
/// - Buffer 0 at 0x4000 (header/first desc)
/// - Buffer 1 at 0x5000 (second desc data)
const TEST_DESC_TABLE: u64 = 0x1000;
const TEST_AVAIL_RING: u64 = 0x2000;
const TEST_USED_RING: u64 = 0x3000;
const TEST_BUF0_ADDR: u64 = 0x4000;
const TEST_BUF1_ADDR: u64 = 0x5000;

/// Creates test guest memory (1 MiB at address 0).
fn make_memory() -> GuestMemory {
    GuestMemory::new(1024 * 1024, 0).unwrap()
}

/// Sets up a VirtQueue with test addresses and marks it ready.
fn make_test_queue() -> VirtQueue {
    let mut q = VirtQueue::new(256);
    q.size = 256;
    q.ready = true;
    q.desc_table_addr = TEST_DESC_TABLE;
    q.avail_ring_addr = TEST_AVAIL_RING;
    q.used_ring_addr = TEST_USED_RING;
    q.last_avail_idx = 0;
    q.last_used_idx = 0;
    q
}

/// Creates a NetDevice with a MockPacketIo backend.
fn make_net_device_with_mock(mock: MockPacketIo) -> NetDevice {
    NetDevice::with_packet_io([0x02, 0x56, 0x49, 0x53, 0x00, 0x01], Box::new(mock))
}

/// Writes a descriptor to the descriptor table in guest memory.
fn write_desc(
    memory: &GuestMemory,
    queue: &VirtQueue,
    idx: u16,
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
) {
    let offset = queue.desc_table_addr + u64::from(idx) * 16;
    memory.write_bytes(offset, &addr.to_le_bytes()).unwrap();
    memory.write_bytes(offset + 8, &len.to_le_bytes()).unwrap();
    memory
        .write_bytes(offset + 12, &flags.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(offset + 14, &next.to_le_bytes())
        .unwrap();
}

/// Sets up the avail ring with one entry pointing to the given descriptor head.
fn write_avail_ring(memory: &GuestMemory, queue: &VirtQueue, head_idx: u16, avail_idx: u16) {
    // flags at offset 0
    memory
        .write_bytes(queue.avail_ring_addr, &0u16.to_le_bytes())
        .unwrap();
    // idx at offset 2
    memory
        .write_bytes(queue.avail_ring_addr + 2, &avail_idx.to_le_bytes())
        .unwrap();
    // ring entry at offset 4 + (avail_idx - 1) % size * 2
    let ring_offset = 4 + u64::from(avail_idx.wrapping_sub(1) % queue.size) * 2;
    memory
        .write_bytes(queue.avail_ring_addr + ring_offset, &head_idx.to_le_bytes())
        .unwrap();
}

/// Reads the used ring idx (at offset +2).
fn read_used_idx(memory: &GuestMemory, queue: &VirtQueue) -> u16 {
    let bytes = memory.read_bytes(queue.used_ring_addr + 2, 2).unwrap();
    u16::from_le_bytes([bytes[0], bytes[1]])
}

/// Reads a used ring entry (id, len) at the given index.
fn read_used_elem(memory: &GuestMemory, queue: &VirtQueue, idx: u16) -> (u32, u32) {
    let offset = queue.used_ring_addr + 4 + u64::from(idx % queue.size) * 8;
    let id_bytes = memory.read_bytes(offset, 4).unwrap();
    let len_bytes = memory.read_bytes(offset + 4, 4).unwrap();
    (
        u32::from_le_bytes([id_bytes[0], id_bytes[1], id_bytes[2], id_bytes[3]]),
        u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]),
    )
}

// ── with_packet_io constructor tests ────────────────────────────────

#[test]
fn with_packet_io_creates_device_with_backend() {
    let (mock, _state) = MockPacketIo::new();
    let dev = make_net_device_with_mock(mock);
    assert_eq!(dev.mac_addr(), &[0x02, 0x56, 0x49, 0x53, 0x00, 0x01]);
    assert_eq!(dev.device_type(), DeviceType::Net);
}

// ── TX process_queue tests ──────────────────────────────────────────

#[test]
fn tx_process_queue_sends_frame_stripping_vnet_header() {
    let (mock, state) = MockPacketIo::new();
    let mut dev = make_net_device_with_mock(mock);
    let memory = make_memory();
    let queue = make_test_queue();

    // Set up device queues (queue index 1 = TX).
    dev.queues_mut()[1] = queue.clone();

    // Build a TX descriptor chain: single descriptor with vnet header + frame.
    let vnet_hdr = [0u8; VNET_HDR_SIZE]; // 12 bytes, all zeros
    let frame = b"\xff\xff\xff\xff\xff\xff\x02\x56\x49\x53\x00\x01\x08\x00hello";
    let mut payload = Vec::new();
    payload.extend_from_slice(&vnet_hdr);
    payload.extend_from_slice(frame);
    memory.write_bytes(TEST_BUF0_ADDR, &payload).unwrap();

    // Single descriptor: readable, no NEXT.
    write_desc(
        &memory,
        &queue,
        0,
        TEST_BUF0_ADDR,
        payload.len() as u32,
        0, // readable, no flags
        0,
    );
    write_avail_ring(&memory, &queue, 0, 1);

    // Process TX queue (index 1).
    let result = dev.process_queue(1, &memory).unwrap();
    assert!(result, "TX should have processed a descriptor");

    // Verify the mock received the frame (header stripped).
    let sent = state.sent.lock().unwrap();
    assert_eq!(sent.len(), 1, "one frame should have been sent");
    assert_eq!(sent[0], frame, "sent frame should match (header stripped)");
    drop(sent);

    // Verify used ring updated.
    let used_idx = read_used_idx(&memory, &dev.queues()[1]);
    assert_eq!(used_idx, 1);
    let (id, len) = read_used_elem(&memory, &dev.queues()[1], 0);
    assert_eq!(id, 0, "used entry should reference descriptor head 0");
    assert_eq!(len, 0, "TX used ring len is always 0");
}

#[test]
fn tx_process_queue_chained_descriptors() {
    let (mock, state) = MockPacketIo::new();
    let mut dev = make_net_device_with_mock(mock);
    let memory = make_memory();
    let queue = make_test_queue();
    dev.queues_mut()[1] = queue.clone();

    // Desc 0: vnet header (12 bytes) in first desc, chained to desc 1.
    let vnet_hdr = [0u8; VNET_HDR_SIZE];
    memory.write_bytes(TEST_BUF0_ADDR, &vnet_hdr).unwrap();
    write_desc(
        &memory,
        &queue,
        0,
        TEST_BUF0_ADDR,
        VNET_HDR_SIZE as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );

    // Desc 1: ethernet frame.
    let frame = b"\xff\xff\xff\xff\xff\xff\x02\x56\x49\x53\x00\x01\x08\x00world";
    memory.write_bytes(TEST_BUF1_ADDR, frame).unwrap();
    write_desc(
        &memory,
        &queue,
        1,
        TEST_BUF1_ADDR,
        frame.len() as u32,
        0, // no NEXT, no WRITE
        0,
    );

    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(1, &memory).unwrap();
    assert!(result);

    let sent = state.sent.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0], frame.as_slice());
}

#[test]
fn tx_process_queue_empty_queue_returns_false() {
    let (mock, _state) = MockPacketIo::new();
    let mut dev = make_net_device_with_mock(mock);
    let memory = make_memory();
    let queue = make_test_queue();
    dev.queues_mut()[1] = queue;

    // avail_idx = 0, last_avail_idx = 0 → nothing to process.
    memory
        .write_bytes(TEST_AVAIL_RING + 2, &0u16.to_le_bytes())
        .unwrap();

    let result = dev.process_queue(1, &memory).unwrap();
    assert!(!result, "empty queue should return false");
}

#[test]
fn tx_header_only_no_frame_sends_nothing() {
    let (mock, state) = MockPacketIo::new();
    let mut dev = make_net_device_with_mock(mock);
    let memory = make_memory();
    let queue = make_test_queue();
    dev.queues_mut()[1] = queue.clone();

    // Single descriptor with only the 12-byte vnet header (no frame data).
    let vnet_hdr = [0u8; VNET_HDR_SIZE];
    memory.write_bytes(TEST_BUF0_ADDR, &vnet_hdr).unwrap();
    write_desc(
        &memory,
        &queue,
        0,
        TEST_BUF0_ADDR,
        VNET_HDR_SIZE as u32,
        0,
        0,
    );
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(1, &memory).unwrap();
    assert!(result, "should still consume the descriptor");

    // No frame data → nothing sent.
    let sent = state.sent.lock().unwrap();
    assert!(
        sent.is_empty(),
        "no frame data means nothing should be sent"
    );
}

// ── RX process_queue tests ──────────────────────────────────────────

#[test]
fn rx_process_queue_fills_descriptor_with_vnet_header_and_frame() {
    let frame = vec![
        0xFFu8, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x02, 0x56, 0x49, 0x53, 0x00, 0x01, 0x08, 0x00, 0x41,
        0x42,
    ];
    let (mock, _state) = MockPacketIo::with_recv(vec![frame.clone()]);

    let mut dev = make_net_device_with_mock(mock);
    let memory = make_memory();
    let queue = make_test_queue();
    dev.queues_mut()[0] = queue.clone();

    // Single RX descriptor: writable, big enough for header + frame.
    let buf_size = (VNET_HDR_SIZE + frame.len()) as u32;
    write_desc(
        &memory,
        &queue,
        0,
        TEST_BUF0_ADDR,
        buf_size,
        VIRTQ_DESC_F_WRITE,
        0,
    );
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(0, &memory).unwrap();
    assert!(result, "RX should have processed a descriptor");

    // Read back from guest memory: first 12 bytes = vnet header (zeros).
    let written = memory
        .read_bytes(TEST_BUF0_ADDR, VNET_HDR_SIZE + frame.len())
        .unwrap();
    assert_eq!(
        &written[..VNET_HDR_SIZE],
        &[0u8; VNET_HDR_SIZE],
        "vnet header should be all zeros"
    );
    assert_eq!(
        &written[VNET_HDR_SIZE..],
        &frame,
        "frame should follow the vnet header"
    );

    // Verify used ring.
    let used_idx = read_used_idx(&memory, &dev.queues()[0]);
    assert_eq!(used_idx, 1);
    let (id, len) = read_used_elem(&memory, &dev.queues()[0], 0);
    assert_eq!(id, 0);
    assert_eq!(
        len,
        (VNET_HDR_SIZE + frame.len()) as u32,
        "used len = header + frame"
    );
}

#[test]
fn rx_process_queue_chained_rx_descriptors() {
    let frame = vec![0xAA; 100]; // 100-byte frame
    let (mock, _state) = MockPacketIo::with_recv(vec![frame.clone()]);

    let mut dev = make_net_device_with_mock(mock);
    let memory = make_memory();
    let queue = make_test_queue();
    dev.queues_mut()[0] = queue.clone();

    // Desc 0: header buffer (12 bytes), chained to desc 1.
    write_desc(
        &memory,
        &queue,
        0,
        TEST_BUF0_ADDR,
        VNET_HDR_SIZE as u32,
        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
        1,
    );
    // Desc 1: data buffer (256 bytes).
    write_desc(
        &memory,
        &queue,
        1,
        TEST_BUF1_ADDR,
        256,
        VIRTQ_DESC_F_WRITE,
        0,
    );
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(0, &memory).unwrap();
    assert!(result);

    // Verify header in desc 0.
    let hdr = memory.read_bytes(TEST_BUF0_ADDR, VNET_HDR_SIZE).unwrap();
    assert_eq!(hdr, vec![0u8; VNET_HDR_SIZE]);

    // Verify frame in desc 1.
    let data = memory.read_bytes(TEST_BUF1_ADDR, frame.len()).unwrap();
    assert_eq!(data, frame);

    // Used ring len should be header + frame.
    let (_, len) = read_used_elem(&memory, &dev.queues()[0], 0);
    assert_eq!(len, (VNET_HDR_SIZE + frame.len()) as u32);
}

#[test]
fn rx_process_queue_no_packets_returns_false() {
    let (mock, _state) = MockPacketIo::new(); // empty recv queue → WouldBlock
    let mut dev = make_net_device_with_mock(mock);
    let memory = make_memory();
    let queue = make_test_queue();
    dev.queues_mut()[0] = queue.clone();

    // Provide an RX descriptor.
    write_desc(
        &memory,
        &queue,
        0,
        TEST_BUF0_ADDR,
        1600,
        VIRTQ_DESC_F_WRITE,
        0,
    );
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(0, &memory).unwrap();
    assert!(!result, "no packets available → should return false");
}

#[test]
fn rx_process_queue_multiple_packets() {
    let frame1 = vec![0x11; 64];
    let frame2 = vec![0x22; 128];
    let (mock, _state) = MockPacketIo::with_recv(vec![frame1.clone(), frame2.clone()]);

    let mut dev = make_net_device_with_mock(mock);
    let memory = make_memory();
    let queue = make_test_queue();
    dev.queues_mut()[0] = queue.clone();

    // Two RX descriptors.
    write_desc(
        &memory,
        &queue,
        0,
        TEST_BUF0_ADDR,
        1600,
        VIRTQ_DESC_F_WRITE,
        0,
    );
    write_desc(
        &memory,
        &queue,
        1,
        TEST_BUF1_ADDR,
        1600,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    // Avail ring with 2 entries.
    memory
        .write_bytes(queue.avail_ring_addr, &0u16.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(queue.avail_ring_addr + 2, &2u16.to_le_bytes())
        .unwrap();
    // Entry 0 → desc 0, entry 1 → desc 1.
    memory
        .write_bytes(queue.avail_ring_addr + 4, &0u16.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(queue.avail_ring_addr + 6, &1u16.to_le_bytes())
        .unwrap();

    let result = dev.process_queue(0, &memory).unwrap();
    assert!(result);

    // Verify both packets were written.
    let used_idx = read_used_idx(&memory, &dev.queues()[0]);
    assert_eq!(used_idx, 2, "both packets should be in the used ring");

    // Check first packet.
    let data0 = memory
        .read_bytes(TEST_BUF0_ADDR + VNET_HDR_SIZE as u64, frame1.len())
        .unwrap();
    assert_eq!(data0, frame1);

    // Check second packet.
    let data1 = memory
        .read_bytes(TEST_BUF1_ADDR + VNET_HDR_SIZE as u64, frame2.len())
        .unwrap();
    assert_eq!(data1, frame2);
}

// ── process_queue without packet_io ─────────────────────────────────

#[test]
fn process_queue_without_packet_io_returns_false() {
    let mut dev = NetDevice::new([0x02, 0x56, 0x49, 0x53, 0x00, 0x01]);
    let memory = make_memory();
    let queue = make_test_queue();
    dev.queues_mut()[0] = queue;
    dev.queues_mut()[1] = make_test_queue();

    // Should return false for both RX and TX when no packet_io is set.
    assert!(!dev.process_queue(0, &memory).unwrap());
    assert!(!dev.process_queue(1, &memory).unwrap());
}

// ── Invalid queue index ─────────────────────────────────────────────

#[test]
fn process_queue_invalid_index_returns_false() {
    let (mock, _state) = MockPacketIo::new();
    let mut dev = make_net_device_with_mock(mock);
    let memory = make_memory();

    // Index 2 is out of range for net device.
    assert!(!dev.process_queue(2, &memory).unwrap());
    assert!(!dev.process_queue(99, &memory).unwrap());
}
