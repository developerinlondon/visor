use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use tempfile::NamedTempFile;

use super::{
    BlockDevice, BlockError, VIRTIO_BLK_F_RO, VIRTIO_BLK_S_IOERR, VIRTIO_BLK_S_OK,
    VIRTIO_BLK_S_UNSUPP, VIRTIO_BLK_T_FLUSH, VIRTIO_BLK_T_GET_ID, VIRTIO_BLK_T_IN,
    VIRTIO_BLK_T_OUT, VIRTIO_F_VERSION_1,
};
use crate::memory::GuestMemory;
use crate::transport::{
    DeviceType, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioDevice,
};

/// Creates a temp file with `num_sectors * 512` bytes of zeros.
#[allow(clippy::cast_possible_truncation)]
fn make_disk(num_sectors: u64) -> NamedTempFile {
    let mut f = crate::testutil::named_temp_file("visor-vmm-block-").unwrap();
    let size = num_sectors * 512;
    f.write_all(&vec![0u8; size as usize]).unwrap();
    f.flush().unwrap();
    f
}

// ── Constructor tests ────────────────────────────────────────────────

#[test]
fn new_opens_file_and_computes_sectors() {
    let disk = make_disk(16);
    let dev = BlockDevice::new(disk.path(), true).unwrap();
    assert_eq!(dev.num_sectors(), 16);
}

#[test]
fn new_fails_with_nonexistent_file() {
    let result = BlockDevice::new(Path::new("/tmp/visor-no-such-file-abc123"), true);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, BlockError::OpenFile(_)));
}

// ── VirtioDevice trait tests ─────────────────────────────────────────

#[test]
fn device_type_returns_block() {
    let disk = make_disk(8);
    let dev = BlockDevice::new(disk.path(), true).unwrap();
    assert_eq!(dev.device_type(), DeviceType::Block);
}

#[test]
fn avail_features_includes_version_1() {
    let disk = make_disk(8);
    let dev = BlockDevice::new(disk.path(), true).unwrap();
    let features = dev.avail_features();
    assert_ne!(
        features & VIRTIO_F_VERSION_1,
        0,
        "VIRTIO_F_VERSION_1 must be set"
    );
}

#[test]
fn read_only_flag_sets_ro_feature() {
    let disk = make_disk(8);

    let readonly_dev = BlockDevice::new(disk.path(), true).unwrap();
    assert_ne!(
        readonly_dev.avail_features() & VIRTIO_BLK_F_RO,
        0,
        "RO feature must be set when read_only=true"
    );

    let readwrite_dev = BlockDevice::new(disk.path(), false).unwrap();
    assert_eq!(
        readwrite_dev.avail_features() & VIRTIO_BLK_F_RO,
        0,
        "RO feature must NOT be set when read_only=false"
    );
}

#[test]
fn read_config_returns_capacity_as_le_u64() {
    let disk = make_disk(42);
    let dev = BlockDevice::new(disk.path(), true).unwrap();

    let mut buf = [0u8; 8];
    dev.read_config(0, &mut buf);
    let capacity = u64::from_le_bytes(buf);
    assert_eq!(capacity, 42);
}

#[test]
fn read_config_beyond_capacity_returns_zeros() {
    let disk = make_disk(8);
    let dev = BlockDevice::new(disk.path(), true).unwrap();

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
    let disk = make_disk(42);
    let mut dev = BlockDevice::new(disk.path(), true).unwrap();

    // Attempt to overwrite capacity
    dev.write_config(0, &[0xFF; 8]);

    // Config should still return original capacity
    let mut buf = [0u8; 8];
    dev.read_config(0, &mut buf);
    let capacity = u64::from_le_bytes(buf);
    assert_eq!(capacity, 42, "write_config must be a no-op");
}

#[test]
fn queues_returns_one_queue_with_max_size_256() {
    let disk = make_disk(8);
    let dev = BlockDevice::new(disk.path(), true).unwrap();

    let queues = dev.queues();
    assert_eq!(queues.len(), 1, "block device has exactly 1 virtqueue");
    assert_eq!(queues[0].max_size, 256);
}

#[test]
fn activate_deactivate_cycle() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), true).unwrap();

    assert!(!dev.is_activated(), "device should start deactivated");
    dev.activate().unwrap();
    assert!(
        dev.is_activated(),
        "device should be activated after activate()"
    );
}

#[test]
fn reset_clears_activation_and_resets_queues() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), true).unwrap();

    // Activate and configure a queue
    dev.activate().unwrap();
    dev.queues_mut()[0].size = 128;
    dev.queues_mut()[0].ready = true;

    dev.reset();

    assert!(!dev.is_activated(), "reset should deactivate");
    assert_eq!(dev.queues()[0].size, 0, "reset should clear queue size");
    assert!(!dev.queues()[0].ready, "reset should clear queue ready");
}

#[test]
fn acked_features_roundtrip() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), true).unwrap();

    assert_eq!(dev.acked_features(), 0, "acked_features starts at 0");

    dev.set_acked_features(VIRTIO_F_VERSION_1);
    assert_eq!(dev.acked_features(), VIRTIO_F_VERSION_1);
}

#[test]
fn partial_config_read() {
    let disk = make_disk(256);
    let dev = BlockDevice::new(disk.path(), true).unwrap();

    // Read only first 4 bytes of capacity (256 sectors = 0x100)
    let mut buf = [0u8; 4];
    dev.read_config(0, &mut buf);
    // 256 in LE is [0x00, 0x01, 0x00, 0x00]
    assert_eq!(buf, [0x00, 0x01, 0x00, 0x00]);
}

// ── I/O processing test helpers ──────────────────────────────────────

/// Guest memory layout for tests:
/// - Descriptor table at 0x1000 (up to 256 entries × 16 bytes = 4096 bytes)
/// - Avail ring at 0x2000
/// - Used ring at 0x3000
/// - Header buffer at 0x4000 (16 bytes)
/// - Data buffer at 0x5000 (up to 8192 bytes)
/// - Status byte at 0x6000
const TEST_DESC_TABLE: u64 = 0x1000;
const TEST_AVAIL_RING: u64 = 0x2000;
const TEST_USED_RING: u64 = 0x3000;
const TEST_HEADER_ADDR: u64 = 0x4000;
const TEST_DATA_ADDR: u64 = 0x5000;
const TEST_STATUS_ADDR: u64 = 0x6000;

/// Creates test guest memory (1 MiB).
fn make_memory() -> GuestMemory {
    GuestMemory::new(1024 * 1024, 0).unwrap()
}

/// Sets up a `VirtQueue` with test addresses and marks it ready.
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

/// Writes a virtio-blk request header (type, reserved, sector) to guest memory.
fn write_header(memory: &GuestMemory, addr: u64, req_type: u32, sector: u64) {
    memory.write_bytes(addr, &req_type.to_le_bytes()).unwrap();
    memory.write_bytes(addr + 4, &0u32.to_le_bytes()).unwrap();
    memory.write_bytes(addr + 8, &sector.to_le_bytes()).unwrap();
}

/// Sets up the avail ring with one entry pointing to the given descriptor head.
fn write_avail_ring(memory: &GuestMemory, queue: &VirtQueue, head_idx: u16, avail_idx: u16) {
    // flags at offset 0 (unused, set to 0)
    memory
        .write_bytes(queue.avail_ring_addr, &0u16.to_le_bytes())
        .unwrap();
    // idx at offset 2
    memory
        .write_bytes(queue.avail_ring_addr + 2, &avail_idx.to_le_bytes())
        .unwrap();
    // ring entry at offset 4 + (avail_idx - 1) % size * 2
    let ring_offset = 4 + u64::from((avail_idx.wrapping_sub(1)) % queue.size) * 2;
    memory
        .write_bytes(queue.avail_ring_addr + ring_offset, &head_idx.to_le_bytes())
        .unwrap();
}

/// Reads the status byte from the status descriptor address in guest memory.
fn read_status(memory: &GuestMemory) -> u8 {
    memory.read_bytes(TEST_STATUS_ADDR, 1).unwrap()[0]
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

/// Sets up a standard 3-descriptor read request chain:
/// desc 0: header (readable), desc 1: data (writable), desc 2: status (writable)
fn setup_read_request(memory: &GuestMemory, queue: &VirtQueue, sector: u64, data_len: u32) {
    // Header descriptor (idx 0) → data (idx 1) → status (idx 2)
    write_desc(memory, queue, 0, TEST_HEADER_ADDR, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        memory,
        queue,
        1,
        TEST_DATA_ADDR,
        data_len,
        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
        2,
    );
    write_desc(memory, queue, 2, TEST_STATUS_ADDR, 1, VIRTQ_DESC_F_WRITE, 0);

    write_header(memory, TEST_HEADER_ADDR, VIRTIO_BLK_T_IN, sector);
    write_avail_ring(memory, queue, 0, 1);
}

/// Sets up a standard 3-descriptor write request chain:
/// desc 0: header (readable), desc 1: data (readable), desc 2: status (writable)
#[allow(clippy::cast_possible_truncation)]
fn setup_write_request(memory: &GuestMemory, queue: &VirtQueue, sector: u64, data: &[u8]) {
    write_desc(memory, queue, 0, TEST_HEADER_ADDR, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        memory,
        queue,
        1,
        TEST_DATA_ADDR,
        data.len() as u32,
        VIRTQ_DESC_F_NEXT,
        2,
    );
    write_desc(memory, queue, 2, TEST_STATUS_ADDR, 1, VIRTQ_DESC_F_WRITE, 0);

    write_header(memory, TEST_HEADER_ADDR, VIRTIO_BLK_T_OUT, sector);
    memory.write_bytes(TEST_DATA_ADDR, data).unwrap();
    write_avail_ring(memory, queue, 0, 1);
}

// ── process_queue tests ─────────────────────────────────────────────

#[test]
fn process_queue_empty_queue() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // avail_idx == last_avail_idx (both 0) → nothing to process
    memory
        .write_bytes(queue.avail_ring_addr + 2, &0u16.to_le_bytes())
        .unwrap();

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(!result, "should not process when queue is empty");
}

#[test]
fn process_queue_read_request() {
    // Create a disk with known data in sector 0
    let disk = make_disk(8);
    {
        let mut f = disk.as_file();
        f.seek(SeekFrom::Start(0)).unwrap();
        let data = vec![0xABu8; 512];
        f.write_all(&data).unwrap();
        f.flush().unwrap();
    }

    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // Set up a read request for sector 0, reading 512 bytes
    setup_read_request(&memory, &queue, 0, 512);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result, "should have processed one request");

    // Verify data was read from disk into guest memory
    let guest_data = memory.read_bytes(TEST_DATA_ADDR, 512).unwrap();
    assert_eq!(
        guest_data,
        vec![0xABu8; 512],
        "read data should match disk content"
    );

    // Verify status byte is OK
    assert_eq!(read_status(&memory), VIRTIO_BLK_S_OK);

    // Verify used ring was updated
    assert_eq!(read_used_idx(&memory, &queue), 1);
    let (id, len) = read_used_elem(&memory, &queue, 0);
    assert_eq!(id, 0, "used elem id should be head descriptor index");
    // len = data (512) + status (1)
    assert_eq!(
        len, 513,
        "used elem len should be total bytes written to device-writable descriptors"
    );

    // Verify queue indices advanced
    assert_eq!(queue.last_avail_idx, 1);
    assert_eq!(queue.last_used_idx, 1);
}

#[test]
fn process_queue_write_request() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // Set up a write request: write 512 bytes of 0xCD to sector 1
    let write_data = vec![0xCDu8; 512];
    setup_write_request(&memory, &queue, 1, &write_data);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result, "should have processed one request");

    // Verify status byte is OK
    assert_eq!(read_status(&memory), VIRTIO_BLK_S_OK);

    // Verify data was written to disk at sector 1 (offset 512)
    let mut f = disk.as_file();
    f.seek(SeekFrom::Start(512)).unwrap();
    let mut disk_data = vec![0u8; 512];
    f.read_exact(&mut disk_data).unwrap();
    assert_eq!(disk_data, write_data, "disk should contain written data");

    // Verify used ring
    assert_eq!(read_used_idx(&memory, &queue), 1);
    let (id, len) = read_used_elem(&memory, &queue, 0);
    assert_eq!(id, 0);
    // Only the status byte is device-writable for a write request
    assert_eq!(len, 1);
}

#[test]
fn process_queue_flush_request() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // Flush request: header → status (no data descriptors)
    write_desc(
        &memory,
        &queue,
        0,
        TEST_HEADER_ADDR,
        16,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        &memory,
        &queue,
        1,
        TEST_STATUS_ADDR,
        1,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    write_header(&memory, TEST_HEADER_ADDR, VIRTIO_BLK_T_FLUSH, 0);
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result);
    assert_eq!(read_status(&memory), VIRTIO_BLK_S_OK);
}

#[test]
fn process_queue_get_id_request() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // GET_ID request: header → data (writable) → status (writable)
    setup_read_request(&memory, &queue, 0, 20); // reuse read setup
    // Override header type to GET_ID
    write_header(&memory, TEST_HEADER_ADDR, VIRTIO_BLK_T_GET_ID, 0);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result);
    assert_eq!(read_status(&memory), VIRTIO_BLK_S_OK);

    // Verify device ID was written to data buffer
    let id_data = memory.read_bytes(TEST_DATA_ADDR, 20).unwrap();
    assert_eq!(&id_data, dev.device_id().as_slice());
}

#[test]
fn process_queue_unsupported_request_type() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // Use an unknown request type (99)
    write_desc(
        &memory,
        &queue,
        0,
        TEST_HEADER_ADDR,
        16,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        &memory,
        &queue,
        1,
        TEST_STATUS_ADDR,
        1,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    write_header(&memory, TEST_HEADER_ADDR, 99, 0);
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result, "should still process the request");
    assert_eq!(read_status(&memory), VIRTIO_BLK_S_UNSUPP);
}

#[test]
fn process_queue_write_to_readonly_returns_ioerr() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), true).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // Write request to a read-only device
    let write_data = vec![0xAAu8; 512];
    setup_write_request(&memory, &queue, 0, &write_data);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result);
    assert_eq!(read_status(&memory), VIRTIO_BLK_S_IOERR);
}

#[test]
fn process_queue_invalid_header_write_flag() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // Header descriptor has WRITE flag (invalid)
    write_desc(
        &memory,
        &queue,
        0,
        TEST_HEADER_ADDR,
        16,
        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        &memory,
        &queue,
        1,
        TEST_STATUS_ADDR,
        1,
        VIRTQ_DESC_F_WRITE,
        0,
    );

    write_header(&memory, TEST_HEADER_ADDR, VIRTIO_BLK_T_IN, 0);
    write_avail_ring(&memory, &queue, 0, 1);

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result, "request should be processed (with error)");
    // Should get IOERR status
    assert_eq!(read_status(&memory), VIRTIO_BLK_S_IOERR);
}

#[test]
fn process_queue_not_ready_returns_false() {
    let disk = make_disk(8);
    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();
    queue.ready = false;

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(!result, "should not process when queue is not ready");
}

#[test]
fn process_queue_multiple_requests() {
    let disk = make_disk(8);
    {
        // Write known data to sectors 0 and 1
        let mut f = disk.as_file();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&vec![0x11u8; 512]).unwrap();
        f.write_all(&vec![0x22u8; 512]).unwrap();
        f.flush().unwrap();
    }

    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // Request 1: read sector 0 (descs 0,1,2)
    let data1_addr: u64 = 0x5000;
    let status1_addr: u64 = 0x6000;
    let header1_addr: u64 = 0x4000;
    write_desc(&memory, &queue, 0, header1_addr, 16, VIRTQ_DESC_F_NEXT, 1);
    write_desc(
        &memory,
        &queue,
        1,
        data1_addr,
        512,
        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
        2,
    );
    write_desc(&memory, &queue, 2, status1_addr, 1, VIRTQ_DESC_F_WRITE, 0);
    write_header(&memory, header1_addr, VIRTIO_BLK_T_IN, 0);

    // Request 2: read sector 1 (descs 3,4,5)
    let header2_addr: u64 = 0x7000;
    let data2_addr: u64 = 0x8000;
    let status2_addr: u64 = 0x9000;
    write_desc(&memory, &queue, 3, header2_addr, 16, VIRTQ_DESC_F_NEXT, 4);
    write_desc(
        &memory,
        &queue,
        4,
        data2_addr,
        512,
        VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT,
        5,
    );
    write_desc(&memory, &queue, 5, status2_addr, 1, VIRTQ_DESC_F_WRITE, 0);
    write_header(&memory, header2_addr, VIRTIO_BLK_T_IN, 1);

    // Set avail ring with 2 entries
    memory
        .write_bytes(queue.avail_ring_addr + 2, &2u16.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(queue.avail_ring_addr + 4, &0u16.to_le_bytes())
        .unwrap(); // entry 0 → desc 0
    memory
        .write_bytes(queue.avail_ring_addr + 6, &3u16.to_le_bytes())
        .unwrap(); // entry 1 → desc 3

    let result = dev.process_queue(&memory, &mut queue).unwrap();
    assert!(result, "should process both requests");

    // Verify both reads
    let data1 = memory.read_bytes(data1_addr, 512).unwrap();
    assert_eq!(data1, vec![0x11u8; 512], "request 1 data");
    let data2 = memory.read_bytes(data2_addr, 512).unwrap();
    assert_eq!(data2, vec![0x22u8; 512], "request 2 data");

    // Both should have OK status
    assert_eq!(
        memory.read_bytes(status1_addr, 1).unwrap()[0],
        VIRTIO_BLK_S_OK
    );
    assert_eq!(
        memory.read_bytes(status2_addr, 1).unwrap()[0],
        VIRTIO_BLK_S_OK
    );

    // Used ring should have 2 entries
    assert_eq!(read_used_idx(&memory, &queue), 2);
    assert_eq!(queue.last_avail_idx, 2);
    assert_eq!(queue.last_used_idx, 2);
}

#[test]
fn process_queue_via_virtio_device_trait() {
    // Test the VirtioDevice::process_queue path (the trait method)
    let disk = make_disk(8);
    {
        let mut f = disk.as_file();
        f.seek(SeekFrom::Start(0)).unwrap();
        f.write_all(&vec![0xEFu8; 512]).unwrap();
        f.flush().unwrap();
    }

    let mut dev = BlockDevice::new(disk.path(), false).unwrap();
    let memory = make_memory();

    // Configure the device's own queue
    dev.queues_mut()[0].size = 256;
    dev.queues_mut()[0].ready = true;
    dev.queues_mut()[0].desc_table_addr = TEST_DESC_TABLE;
    dev.queues_mut()[0].avail_ring_addr = TEST_AVAIL_RING;
    dev.queues_mut()[0].used_ring_addr = TEST_USED_RING;

    setup_read_request(&memory, &dev.queues()[0].clone(), 0, 512);

    let result = VirtioDevice::process_queue(&mut dev, 0, &memory).unwrap();
    assert!(result, "VirtioDevice::process_queue should succeed");

    let guest_data = memory.read_bytes(TEST_DATA_ADDR, 512).unwrap();
    assert_eq!(guest_data, vec![0xEFu8; 512]);
    assert_eq!(read_status(&memory), VIRTIO_BLK_S_OK);
}
