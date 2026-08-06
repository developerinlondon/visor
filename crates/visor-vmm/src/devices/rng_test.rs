use std::io::Write;
use std::path::Path;

use tempfile::NamedTempFile;

use super::{RngDevice, RngError, VIRTIO_F_VERSION_1};
use crate::memory::GuestMemory;
use crate::transport::{
    DeviceType, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioDevice,
};

// ── Constructor tests ────────────────────────────────────────────────

#[test]
fn new_opens_dev_urandom() {
    let dev = RngDevice::new();
    assert!(dev.is_ok(), "/dev/urandom should be available on Linux");
}

#[test]
fn with_source_opens_custom_file() {
    let mut f = crate::testutil::named_temp_file("visor-vmm-rng-").unwrap();
    f.write_all(&[0xAA; 256]).unwrap();
    f.flush().unwrap();
    let dev = RngDevice::with_source(f.path());
    assert!(dev.is_ok());
}

#[test]
fn with_source_fails_on_nonexistent_file() {
    let result = RngDevice::with_source(Path::new("/tmp/visor-no-such-entropy-abc"));
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), RngError::OpenSource(_)));
}

// ── VirtioDevice trait tests ─────────────────────────────────────────

#[test]
fn device_type_returns_rng() {
    let dev = RngDevice::new().unwrap();
    assert_eq!(dev.device_type(), DeviceType::Rng);
}

#[test]
fn avail_features_includes_version_1() {
    let dev = RngDevice::new().unwrap();
    assert_ne!(
        dev.avail_features() & VIRTIO_F_VERSION_1,
        0,
        "VIRTIO_F_VERSION_1 must be set"
    );
}

#[test]
fn acked_features_starts_at_zero() {
    let dev = RngDevice::new().unwrap();
    assert_eq!(dev.acked_features(), 0);
}

#[test]
fn acked_features_roundtrip() {
    let mut dev = RngDevice::new().unwrap();
    dev.set_acked_features(VIRTIO_F_VERSION_1);
    assert_eq!(dev.acked_features(), VIRTIO_F_VERSION_1);
}

#[test]
fn queues_returns_one_queue() {
    let dev = RngDevice::new().unwrap();
    assert_eq!(dev.queues().len(), 1, "rng device has exactly 1 virtqueue");
    assert_eq!(dev.queues()[0].max_size, 256);
}

#[test]
fn read_config_returns_zeros() {
    let dev = RngDevice::new().unwrap();
    let mut buf = [0xFFu8; 8];
    dev.read_config(0, &mut buf);
    assert_eq!(buf, [0; 8], "rng has no config space");
}

#[test]
fn write_config_is_noop() {
    let mut dev = RngDevice::new().unwrap();
    dev.write_config(0, &[0xFF; 8]);
    let mut buf = [0xFFu8; 8];
    dev.read_config(0, &mut buf);
    assert_eq!(buf, [0; 8]);
}

#[test]
fn activate_deactivate_cycle() {
    let mut dev = RngDevice::new().unwrap();
    assert!(!dev.is_activated());
    dev.activate().unwrap();
    assert!(dev.is_activated());
}

#[test]
fn reset_clears_activation_and_queues() {
    let mut dev = RngDevice::new().unwrap();
    dev.activate().unwrap();
    dev.queues_mut()[0].size = 128;
    dev.queues_mut()[0].ready = true;

    dev.reset();

    assert!(!dev.is_activated());
    assert_eq!(dev.queues()[0].size, 0);
    assert!(!dev.queues()[0].ready);
}

// ── I/O processing test helpers ──────────────────────────────────────

const TEST_DESC_TABLE: u64 = 0x1000;
const TEST_AVAIL_RING: u64 = 0x2000;
const TEST_USED_RING: u64 = 0x3000;
const TEST_DATA_ADDR: u64 = 0x5000;

fn make_memory() -> GuestMemory {
    GuestMemory::new(1024 * 1024, 0).unwrap()
}

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

fn write_avail_ring(memory: &GuestMemory, queue: &VirtQueue, head_idx: u16, avail_idx: u16) {
    memory
        .write_bytes(queue.avail_ring_addr, &0u16.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(queue.avail_ring_addr + 2, &avail_idx.to_le_bytes())
        .unwrap();
    let ring_offset = 4 + u64::from((avail_idx.wrapping_sub(1)) % queue.size) * 2;
    memory
        .write_bytes(queue.avail_ring_addr + ring_offset, &head_idx.to_le_bytes())
        .unwrap();
}

fn read_used_idx(memory: &GuestMemory, queue: &VirtQueue) -> u16 {
    let bytes = memory.read_bytes(queue.used_ring_addr + 2, 2).unwrap();
    u16::from_le_bytes([bytes[0], bytes[1]])
}

fn read_used_elem(memory: &GuestMemory, queue: &VirtQueue, idx: u16) -> (u32, u32) {
    let offset = queue.used_ring_addr + 4 + u64::from(idx % queue.size) * 8;
    let id_bytes = memory.read_bytes(offset, 4).unwrap();
    let len_bytes = memory.read_bytes(offset + 4, 4).unwrap();
    (
        u32::from_le_bytes([id_bytes[0], id_bytes[1], id_bytes[2], id_bytes[3]]),
        u32::from_le_bytes([len_bytes[0], len_bytes[1], len_bytes[2], len_bytes[3]]),
    )
}

/// Creates an RNG device backed by a tempfile filled with known bytes.
fn make_rng_with_known_entropy(pattern: u8, size: usize) -> (RngDevice, NamedTempFile) {
    let mut f = crate::testutil::named_temp_file("visor-vmm-rng-").unwrap();
    f.write_all(&vec![pattern; size]).unwrap();
    f.flush().unwrap();
    let dev = RngDevice::with_source(f.path()).unwrap();
    (dev, f)
}

// ── process_queue tests ─────────────────────────────────────────────

#[test]
fn process_queue_empty_queue() {
    let mut dev = RngDevice::new().unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    memory
        .write_bytes(queue.avail_ring_addr + 2, &0u16.to_le_bytes())
        .unwrap();

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(!result, "should not process when queue is empty");
}

#[test]
fn process_queue_not_ready_returns_false() {
    let mut dev = RngDevice::new().unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();
    queue.ready = false;

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(!result, "should not process when queue is not ready");
}

#[test]
fn process_queue_fills_single_buffer() {
    let (mut dev, _tmp) = make_rng_with_known_entropy(0xAB, 1024);
    let memory = make_memory();
    let mut queue = make_test_queue();

    write_desc(
        &memory,
        &queue,
        0,
        TEST_DATA_ADDR,
        64,
        VIRTQ_DESC_F_WRITE,
        0,
    );
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result, "should have processed one request");

    let data = memory.read_bytes(TEST_DATA_ADDR, 64).unwrap();
    assert_eq!(
        data,
        vec![0xABu8; 64],
        "buffer should be filled with entropy"
    );

    assert_eq!(read_used_idx(&memory, &queue), 1);
    let (id, len) = read_used_elem(&memory, &queue, 0);
    assert_eq!(id, 0);
    assert_eq!(len, 64);
}

#[test]
fn process_queue_fills_chained_buffers() {
    let (mut dev, _tmp) = make_rng_with_known_entropy(0xCD, 2048);
    let memory = make_memory();
    let mut queue = make_test_queue();

    let data2_addr: u64 = 0x6000;
    write_desc(
        &memory,
        &queue,
        0,
        TEST_DATA_ADDR,
        128,
        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(&memory, &queue, 1, data2_addr, 256, VIRTQ_DESC_F_WRITE, 0);
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result);

    let data1 = memory.read_bytes(TEST_DATA_ADDR, 128).unwrap();
    assert_eq!(data1, vec![0xCDu8; 128]);

    let data2 = memory.read_bytes(data2_addr, 256).unwrap();
    assert_eq!(data2, vec![0xCDu8; 256]);

    let (id, len) = read_used_elem(&memory, &queue, 0);
    assert_eq!(id, 0);
    assert_eq!(len, 384); // 128 + 256
}

#[test]
fn process_queue_skips_non_writable_descriptors() {
    let (mut dev, _tmp) = make_rng_with_known_entropy(0xEE, 1024);
    let memory = make_memory();
    let mut queue = make_test_queue();

    // desc 0: readable (no WRITE flag) → should be skipped
    // desc 1: writable → should be filled
    write_desc(&memory, &queue, 0, TEST_DATA_ADDR, 64, VIRTQ_DESC_F_NEXT, 1);
    let writable_addr: u64 = 0x6000;
    write_desc(&memory, &queue, 1, writable_addr, 32, VIRTQ_DESC_F_WRITE, 0);
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result);

    let (_, len) = read_used_elem(&memory, &queue, 0);
    assert_eq!(len, 32, "only writable descriptor bytes should be counted");
}

#[test]
fn process_queue_via_virtio_device_trait() {
    let (mut dev, _tmp) = make_rng_with_known_entropy(0x77, 1024);
    let memory = make_memory();

    dev.queues_mut()[0].size = 256;
    dev.queues_mut()[0].ready = true;
    dev.queues_mut()[0].desc_table_addr = TEST_DESC_TABLE;
    dev.queues_mut()[0].avail_ring_addr = TEST_AVAIL_RING;
    dev.queues_mut()[0].used_ring_addr = TEST_USED_RING;

    write_desc(
        &memory,
        &dev.queues()[0],
        0,
        TEST_DATA_ADDR,
        64,
        VIRTQ_DESC_F_WRITE,
        0,
    );
    write_avail_ring(&memory, &dev.queues()[0], 0, 1);

    let result = VirtioDevice::process_queue(&mut dev, 0, &memory).unwrap();
    assert!(result, "VirtioDevice::process_queue should succeed");

    let data = memory.read_bytes(TEST_DATA_ADDR, 64).unwrap();
    assert_eq!(data, vec![0x77u8; 64]);
}

#[test]
fn process_queue_invalid_queue_idx_returns_false() {
    let mut dev = RngDevice::new().unwrap();
    let memory = make_memory();
    let result = VirtioDevice::process_queue(&mut dev, 99, &memory).unwrap();
    assert!(!result);
}

// ── Error display test ───────────────────────────────────────────────

#[test]
fn rng_error_display_is_readable() {
    let err = RngError::OpenSource(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
    let msg = format!("{err}");
    assert!(
        msg.contains("entropy source"),
        "should mention entropy source: {msg}"
    );
}
