use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::*;
use crate::memory::GuestMemory;
use crate::transport::{
    DeviceType, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioDevice,
};

// ── Constructor tests ────────────────────────────────────────────────

fn make_shared_dir() -> TempDir {
    let dir = crate::testutil::tempdir("visor-vmm-fs-").unwrap();
    fs::write(dir.path().join("hello.txt"), "Hello, world!\n").unwrap();
    fs::create_dir(dir.path().join("subdir")).unwrap();
    fs::write(
        dir.path().join("subdir").join("nested.txt"),
        "nested content",
    )
    .unwrap();
    dir
}

#[test]
fn new_creates_device_with_tag() {
    let dir = make_shared_dir();
    let dev = FsDevice::new(dir.path(), "myfs").unwrap();
    assert_eq!(dev.tag_str(), "myfs");
}

#[test]
fn new_truncates_long_tag() {
    let dir = make_shared_dir();
    let long_tag = "a".repeat(50);
    let dev = FsDevice::new(dir.path(), &long_tag).unwrap();
    assert_eq!(dev.tag_str().len(), TAG_LEN);
}

#[test]
fn new_fails_on_nonexistent_dir() {
    let result = FsDevice::new(Path::new("/tmp/visor-no-such-dir-xyz"), "test");
    assert!(result.is_err());
    assert!(matches!(result.unwrap_err(), FsError::SharedDir(_)));
}

#[test]
fn shared_dir_returns_path() {
    let dir = make_shared_dir();
    let dev = FsDevice::new(dir.path(), "tag").unwrap();
    assert_eq!(dev.shared_dir(), dir.path());
}

// ── VirtioDevice trait tests ─────────────────────────────────────────

#[test]
fn device_type_returns_fs() {
    let dir = make_shared_dir();
    let dev = FsDevice::new(dir.path(), "fs").unwrap();
    assert_eq!(dev.device_type(), DeviceType::Fs);
}

#[test]
fn avail_features_includes_version_1() {
    let dir = make_shared_dir();
    let dev = FsDevice::new(dir.path(), "fs").unwrap();
    assert_ne!(
        dev.avail_features() & VIRTIO_F_VERSION_1,
        0,
        "VIRTIO_F_VERSION_1 must be set"
    );
}

#[test]
fn acked_features_starts_at_zero() {
    let dir = make_shared_dir();
    let dev = FsDevice::new(dir.path(), "fs").unwrap();
    assert_eq!(dev.acked_features(), 0);
}

#[test]
fn acked_features_roundtrip() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    dev.set_acked_features(VIRTIO_F_VERSION_1);
    assert_eq!(dev.acked_features(), VIRTIO_F_VERSION_1);
}

#[test]
fn queues_returns_two_queues() {
    let dir = make_shared_dir();
    let dev = FsDevice::new(dir.path(), "fs").unwrap();
    assert_eq!(
        dev.queues().len(),
        2,
        "fs device has hiprio + request queues"
    );
}

#[test]
fn read_config_returns_tag_and_num_queues() {
    let dir = make_shared_dir();
    let dev = FsDevice::new(dir.path(), "visor").unwrap();

    let mut buf = [0u8; 40];
    dev.read_config(0, &mut buf);

    assert_eq!(&buf[..5], b"visor");
    assert_eq!(buf[5..TAG_LEN], vec![0u8; TAG_LEN - 5]);

    let num_queues = u32::from_le_bytes([buf[36], buf[37], buf[38], buf[39]]);
    assert_eq!(num_queues, 1);
}

#[test]
fn read_config_beyond_end_returns_zeros() {
    let dir = make_shared_dir();
    let dev = FsDevice::new(dir.path(), "fs").unwrap();

    let mut buf = [0xFFu8; 4];
    dev.read_config(40, &mut buf);
    assert_eq!(buf, [0, 0, 0, 0]);
}

#[test]
fn write_config_is_noop() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "test").unwrap();
    dev.write_config(0, &[0xFF; 36]);

    let mut buf = [0u8; 4];
    dev.read_config(0, &mut buf);
    assert_eq!(&buf[..4], b"test");
}

#[test]
fn activate_deactivate_cycle() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    assert!(!dev.is_activated());
    dev.activate().unwrap();
    assert!(dev.is_activated());
}

#[test]
fn reset_clears_activation_and_queues() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    dev.activate().unwrap();
    dev.queues_mut()[0].size = 128;
    dev.queues_mut()[0].ready = true;

    dev.reset();

    assert!(!dev.is_activated());
    assert_eq!(dev.queues()[0].size, 0);
    assert!(!dev.queues()[0].ready);
}

// ── FUSE request test helpers ────────────────────────────────────────

const TEST_DESC_TABLE: u64 = 0x1000;
const TEST_AVAIL_RING: u64 = 0x2000;
const TEST_USED_RING: u64 = 0x3000;
const TEST_REQUEST_ADDR: u64 = 0x4000;
const TEST_RESPONSE_ADDR: u64 = 0x8000;

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

#[allow(clippy::cast_possible_truncation)]
fn build_fuse_in_header(opcode: u32, unique: u64, nodeid: u64) -> [u8; FUSE_IN_HEADER_SIZE] {
    let mut header = [0u8; FUSE_IN_HEADER_SIZE];
    // len (4) — will be set to total request size
    header[0..4].copy_from_slice(&(FUSE_IN_HEADER_SIZE as u32).to_le_bytes());
    // opcode (4)
    header[4..8].copy_from_slice(&opcode.to_le_bytes());
    // unique (8)
    header[8..16].copy_from_slice(&unique.to_le_bytes());
    // nodeid (8)
    header[16..24].copy_from_slice(&nodeid.to_le_bytes());
    header
}

#[allow(clippy::cast_possible_truncation)]
fn setup_fuse_request(
    memory: &GuestMemory,
    queue: &VirtQueue,
    request_data: &[u8],
    response_size: u32,
) {
    memory.write_bytes(TEST_REQUEST_ADDR, request_data).unwrap();

    write_desc(
        memory,
        queue,
        0,
        TEST_REQUEST_ADDR,
        request_data.len() as u32,
        VIRTQ_DESC_F_NEXT,
        1,
    );
    write_desc(
        memory,
        queue,
        1,
        TEST_RESPONSE_ADDR,
        response_size,
        VIRTQ_DESC_F_WRITE,
        0,
    );
    write_avail_ring(memory, queue, 0, 1);
}

fn read_fuse_out_header(memory: &GuestMemory) -> (u32, i32, u64) {
    let header = memory
        .read_bytes(TEST_RESPONSE_ADDR, FUSE_OUT_HEADER_SIZE)
        .unwrap();
    let len = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let error = i32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    let unique = u64::from_le_bytes([
        header[8], header[9], header[10], header[11], header[12], header[13], header[14],
        header[15],
    ]);
    (len, error, unique)
}

// ── FUSE_INIT test ───────────────────────────────────────────────────

#[test]
fn fuse_init_returns_version() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    let header = build_fuse_in_header(FUSE_INIT, 1, 0);
    setup_fuse_request(&memory, &queue, &header, 1024);

    let result = dev.process_request_queue(&memory, &mut queue).unwrap();
    assert!(result);

    let (_, error, unique) = read_fuse_out_header(&memory);
    assert_eq!(error, 0, "INIT should succeed");
    assert_eq!(unique, 1);

    let payload = memory
        .read_bytes(TEST_RESPONSE_ADDR + FUSE_OUT_HEADER_SIZE as u64, 8)
        .unwrap();
    let major = u32::from_le_bytes([payload[0], payload[1], payload[2], payload[3]]);
    let minor = u32::from_le_bytes([payload[4], payload[5], payload[6], payload[7]]);
    assert_eq!(major, 7);
    assert_eq!(minor, 31);
}

// ── FUSE_GETATTR test ────────────────────────────────────────────────

#[test]
fn fuse_getattr_root() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    let header = build_fuse_in_header(FUSE_GETATTR, 2, FUSE_ROOT_ID);
    setup_fuse_request(&memory, &queue, &header, 1024);

    let result = dev.process_request_queue(&memory, &mut queue).unwrap();
    assert!(result);

    let (_, error, unique) = read_fuse_out_header(&memory);
    assert_eq!(error, 0, "GETATTR on root should succeed");
    assert_eq!(unique, 2);
}

#[test]
fn fuse_getattr_nonexistent_inode() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    let header = build_fuse_in_header(FUSE_GETATTR, 3, 999);
    setup_fuse_request(&memory, &queue, &header, 1024);

    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (_, error, _) = read_fuse_out_header(&memory);
    assert_eq!(error, ENOENT);
}

// ── FUSE_LOOKUP test ─────────────────────────────────────────────────

#[test]
fn fuse_lookup_existing_file() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    let header = build_fuse_in_header(FUSE_LOOKUP, 4, FUSE_ROOT_ID);
    let name = b"hello.txt\0";
    let mut request = header.to_vec();
    request.extend_from_slice(name);

    setup_fuse_request(&memory, &queue, &request, 1024);

    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (_, error, unique) = read_fuse_out_header(&memory);
    assert_eq!(error, 0, "LOOKUP should succeed for existing file");
    assert_eq!(unique, 4);

    let entry_data = memory
        .read_bytes(TEST_RESPONSE_ADDR + FUSE_OUT_HEADER_SIZE as u64, 8)
        .unwrap();
    let nodeid = u64::from_le_bytes([
        entry_data[0],
        entry_data[1],
        entry_data[2],
        entry_data[3],
        entry_data[4],
        entry_data[5],
        entry_data[6],
        entry_data[7],
    ]);
    assert_ne!(nodeid, 0, "should return a valid nodeid");
}

#[test]
fn fuse_lookup_nonexistent_file() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    let header = build_fuse_in_header(FUSE_LOOKUP, 5, FUSE_ROOT_ID);
    let name = b"nope.txt\0";
    let mut request = header.to_vec();
    request.extend_from_slice(name);

    setup_fuse_request(&memory, &queue, &request, 1024);

    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (_, error, _) = read_fuse_out_header(&memory);
    assert_eq!(error, ENOENT, "LOOKUP should fail for nonexistent file");
}

// ── FUSE_OPEN + FUSE_READ test ──────────────────────────────────────

#[test]
fn fuse_open_and_read_file() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();

    // Step 1: LOOKUP hello.txt
    let mut queue = make_test_queue();
    let header = build_fuse_in_header(FUSE_LOOKUP, 10, FUSE_ROOT_ID);
    let mut request = header.to_vec();
    request.extend_from_slice(b"hello.txt\0");
    setup_fuse_request(&memory, &queue, &request, 1024);
    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (_, error, _) = read_fuse_out_header(&memory);
    assert_eq!(error, 0);

    let entry_data = memory
        .read_bytes(TEST_RESPONSE_ADDR + FUSE_OUT_HEADER_SIZE as u64, 8)
        .unwrap();
    let file_nodeid = u64::from_le_bytes([
        entry_data[0],
        entry_data[1],
        entry_data[2],
        entry_data[3],
        entry_data[4],
        entry_data[5],
        entry_data[6],
        entry_data[7],
    ]);

    // Step 2: OPEN file
    let mut queue = make_test_queue();
    let header = build_fuse_in_header(FUSE_OPEN, 11, file_nodeid);
    setup_fuse_request(&memory, &queue, &header, 1024);
    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (_, error, _) = read_fuse_out_header(&memory);
    assert_eq!(error, 0, "OPEN should succeed");

    let open_out = memory
        .read_bytes(TEST_RESPONSE_ADDR + FUSE_OUT_HEADER_SIZE as u64, 8)
        .unwrap();
    let fh = u64::from_le_bytes([
        open_out[0],
        open_out[1],
        open_out[2],
        open_out[3],
        open_out[4],
        open_out[5],
        open_out[6],
        open_out[7],
    ]);
    assert_ne!(fh, 0, "should return a valid file handle");

    // Step 3: READ from file
    let mut queue = make_test_queue();
    let header = build_fuse_in_header(FUSE_READ, 12, file_nodeid);
    // fuse_read_in: fh(8) + offset(8) + size(4) + read_flags(4) + lock_owner(8) + flags(4) + padding(4)
    let mut read_in = vec![0u8; 40];
    read_in[0..8].copy_from_slice(&fh.to_le_bytes());
    read_in[8..16].copy_from_slice(&0u64.to_le_bytes()); // offset 0
    read_in[16..20].copy_from_slice(&1024u32.to_le_bytes()); // size
    let mut request = header.to_vec();
    request.extend_from_slice(&read_in);
    setup_fuse_request(&memory, &queue, &request, 2048);
    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (resp_len, error, _) = read_fuse_out_header(&memory);
    assert_eq!(error, 0, "READ should succeed");

    let payload_len = resp_len as usize - FUSE_OUT_HEADER_SIZE;
    let payload = memory
        .read_bytes(
            TEST_RESPONSE_ADDR + FUSE_OUT_HEADER_SIZE as u64,
            payload_len,
        )
        .unwrap();
    assert_eq!(
        String::from_utf8_lossy(&payload),
        "Hello, world!\n",
        "should read file contents"
    );
}

// ── FUSE_OPENDIR + FUSE_READDIR test ────────────────────────────────

#[test]
fn fuse_opendir_and_readdir() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();

    // Step 1: OPENDIR root
    let mut queue = make_test_queue();
    let header = build_fuse_in_header(FUSE_OPENDIR, 20, FUSE_ROOT_ID);
    setup_fuse_request(&memory, &queue, &header, 1024);
    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (_, error, _) = read_fuse_out_header(&memory);
    assert_eq!(error, 0, "OPENDIR on root should succeed");

    let open_out = memory
        .read_bytes(TEST_RESPONSE_ADDR + FUSE_OUT_HEADER_SIZE as u64, 8)
        .unwrap();
    let fh = u64::from_le_bytes([
        open_out[0],
        open_out[1],
        open_out[2],
        open_out[3],
        open_out[4],
        open_out[5],
        open_out[6],
        open_out[7],
    ]);

    // Step 2: READDIR
    let mut queue = make_test_queue();
    let header = build_fuse_in_header(FUSE_READDIR, 21, FUSE_ROOT_ID);
    let mut readdir_in = vec![0u8; 40];
    readdir_in[0..8].copy_from_slice(&fh.to_le_bytes());
    readdir_in[8..16].copy_from_slice(&0u64.to_le_bytes()); // offset 0
    readdir_in[16..20].copy_from_slice(&4096u32.to_le_bytes()); // size
    let mut request = header.to_vec();
    request.extend_from_slice(&readdir_in);
    setup_fuse_request(&memory, &queue, &request, 8192);
    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (resp_len, error, _) = read_fuse_out_header(&memory);
    assert_eq!(error, 0, "READDIR should succeed");
    assert!(
        resp_len as usize > FUSE_OUT_HEADER_SIZE,
        "READDIR should return some entries"
    );
}

// ── FUSE_RELEASE test ────────────────────────────────────────────────

#[test]
fn fuse_release_succeeds() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    // Build RELEASE request with fh=42
    let header = build_fuse_in_header(FUSE_RELEASE, 30, FUSE_ROOT_ID);
    let mut release_in = vec![0u8; 24];
    release_in[0..8].copy_from_slice(&42u64.to_le_bytes());
    let mut request = header.to_vec();
    request.extend_from_slice(&release_in);

    setup_fuse_request(&memory, &queue, &request, 1024);
    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (_, error, _) = read_fuse_out_header(&memory);
    assert_eq!(error, 0, "RELEASE should always succeed");
}

// ── Unsupported opcode test ──────────────────────────────────────────

#[test]
fn unsupported_opcode_returns_enosys() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();
    let mut queue = make_test_queue();

    let header = build_fuse_in_header(999, 40, FUSE_ROOT_ID);
    setup_fuse_request(&memory, &queue, &header, 1024);
    dev.process_request_queue(&memory, &mut queue).unwrap();

    let (_, error, _) = read_fuse_out_header(&memory);
    assert_eq!(error, ENOSYS, "unsupported opcodes should return ENOSYS");
}

// ── Process queue via trait test ─────────────────────────────────────

#[test]
fn process_queue_via_virtio_device_trait() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();

    dev.queues_mut()[1].size = 256;
    dev.queues_mut()[1].ready = true;
    dev.queues_mut()[1].desc_table_addr = TEST_DESC_TABLE;
    dev.queues_mut()[1].avail_ring_addr = TEST_AVAIL_RING;
    dev.queues_mut()[1].used_ring_addr = TEST_USED_RING;

    let header = build_fuse_in_header(FUSE_INIT, 50, 0);
    setup_fuse_request(&memory, &dev.queues()[1], &header, 1024);

    let result = VirtioDevice::process_queue(&mut dev, 1, &memory).unwrap();
    assert!(result, "VirtioDevice::process_queue should succeed");
}

#[test]
fn process_queue_hiprio_returns_false() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();
    let result = VirtioDevice::process_queue(&mut dev, 0, &memory).unwrap();
    assert!(!result, "hiprio queue is unused in P1");
}

#[test]
fn process_queue_invalid_idx_returns_false() {
    let dir = make_shared_dir();
    let mut dev = FsDevice::new(dir.path(), "fs").unwrap();
    let memory = make_memory();
    let result = VirtioDevice::process_queue(&mut dev, 99, &memory).unwrap();
    assert!(!result);
}

// ── Error display test ───────────────────────────────────────────────

#[test]
fn fs_error_display_is_readable() {
    let err = FsError::SharedDir(std::io::Error::new(std::io::ErrorKind::NotFound, "gone"));
    let msg = format!("{err}");
    assert!(
        msg.contains("shared directory"),
        "should mention shared directory: {msg}"
    );
}
