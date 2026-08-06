//! Virtio filesystem device (`virtio-fs`).
//!
//! Provides read-only host directory passthrough to the guest via the FUSE
//! protocol over virtio. The guest mounts the shared filesystem using the
//! tag specified in the device config space.
//!
//! # P1 scope (minimal read-only subset)
//!
//! - `FUSE_INIT`: session initialization
//! - `FUSE_LOOKUP`: resolve a filename to an inode
//! - `FUSE_GETATTR`: stat a file/directory
//! - `FUSE_OPEN` / `FUSE_OPENDIR`: open a file or directory handle
//! - `FUSE_READ`: read file contents
//! - `FUSE_READDIR`: list directory entries
//! - `FUSE_RELEASE` / `FUSE_RELEASEDIR`: close handles
//! - `FUSE_FORGET`: drop inode reference (no-op, no reply)
//!
//! # Queues
//!
//! - Queue 0 (`hiprio`): high-priority requests (unused in P1).
//! - Queue 1 (`request`): normal FUSE requests.
//!
//! # Config space layout (virtio-fs spec)
//!
//! - Offset `0..35`: filesystem tag (36 bytes, null-padded UTF-8 string).
//! - Offset `36..39`: `num_request_queues` (`u32`, little-endian).

use std::collections::HashMap;
use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
#[cfg(target_os = "linux")]
use std::os::linux::fs::MetadataExt as LinuxMetadataExt;
#[cfg(not(target_os = "linux"))]
use std::os::unix::fs::MetadataExt as UnixMetadataExt;
use std::path::{Path, PathBuf};

use crate::memory::GuestMemory;
use crate::transport::{
    DeviceType, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioDevice, VirtioError,
    VirtqDesc,
};

// ── Feature flags ────────────────────────────────────────────────────

/// Virtio feature: modern virtio (version 1).
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

// ── Constants ────────────────────────────────────────────────────────

/// Maximum virtqueue size for the fs device.
const QUEUE_MAX_SIZE: u16 = 256;

/// Number of virtqueues (hiprio + 1 request queue).
const NUM_QUEUES: usize = 2;

/// Request queue index.
const REQUEST_QUEUE: usize = 1;

/// Filesystem tag maximum length (per virtio-fs spec).
const TAG_LEN: usize = 36;

/// Config space size: 36-byte tag + 4-byte `num_request_queues`.
const CONFIG_SIZE: usize = TAG_LEN + 4;

/// Root inode number (FUSE convention).
const FUSE_ROOT_ID: u64 = 1;

// ── FUSE opcodes (subset) ────────────────────────────────────────────

const FUSE_LOOKUP: u32 = 1;
const FUSE_FORGET: u32 = 2;
const FUSE_GETATTR: u32 = 3;
const FUSE_OPEN: u32 = 14;
const FUSE_READ: u32 = 15;
const FUSE_RELEASE: u32 = 18;
const FUSE_INIT: u32 = 26;
const FUSE_OPENDIR: u32 = 27;
const FUSE_READDIR: u32 = 28;
const FUSE_RELEASEDIR: u32 = 29;

// ── FUSE header sizes ────────────────────────────────────────────────

const FUSE_IN_HEADER_SIZE: usize = 40;
const FUSE_OUT_HEADER_SIZE: usize = 16;

// ── FUSE error codes ─────────────────────────────────────────────────

const ENOENT: i32 = -2;
const ENOSYS: i32 = -38;
const ENOTDIR: i32 = -20;
const EBADF: i32 = -9;
const EISDIR: i32 = -21;

// ── Errors ───────────────────────────────────────────────────────────

/// Errors from filesystem device operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum FsError {
    /// Failed to access the shared directory.
    #[error("failed to access shared directory: {0}")]
    SharedDir(std::io::Error),
    /// Failed to read host file.
    #[error("host file I/O error: {0}")]
    Io(std::io::Error),
    /// Guest memory access error.
    #[error("guest memory error: {0}")]
    Memory(crate::memory::MemoryError),
    /// Invalid virtqueue descriptor.
    #[error("invalid descriptor: {0}")]
    InvalidDescriptor(String),
    /// Invalid FUSE request.
    #[error("invalid FUSE request: {0}")]
    InvalidRequest(String),
}

// ── Metadata helpers ─────────────────────────────────────────────────

/// Returns the file size from metadata (cross-platform).
#[cfg(target_os = "linux")]
fn meta_size(meta: &fs::Metadata) -> u64 {
    meta.st_size()
}

/// Returns the file size from metadata (cross-platform).
#[cfg(not(target_os = "linux"))]
fn meta_size(meta: &fs::Metadata) -> u64 {
    meta.size()
}

/// Returns the file mode from metadata (cross-platform).
#[cfg(target_os = "linux")]
fn meta_mode(meta: &fs::Metadata) -> u32 {
    meta.st_mode()
}

/// Returns the file mode from metadata (cross-platform).
#[cfg(not(target_os = "linux"))]
fn meta_mode(meta: &fs::Metadata) -> u32 {
    meta.mode()
}

// ── Inode table ──────────────────────────────────────────────────────

/// Metadata about a file or directory tracked by inode number.
#[derive(Debug, Clone)]
struct InodeEntry {
    path: PathBuf,
    is_dir: bool,
    size: u64,
    mode: u32,
    ino: u64,
}

// ── FsDevice ─────────────────────────────────────────────────────────

/// Virtio filesystem device for host directory passthrough.
///
/// Implements [`VirtioDevice`] for use with the MMIO transport. Exposes
/// a host directory to the guest as a read-only FUSE filesystem.
#[derive(Debug)]
#[non_exhaustive]
pub struct FsDevice {
    /// Root directory on the host to share with the guest.
    shared_dir: PathBuf,
    /// Filesystem tag visible to the guest (max 36 bytes).
    tag: [u8; TAG_LEN],
    /// Feature bits offered by the device.
    avail_features: u64,
    /// Feature bits acknowledged by the driver.
    acked_features: u64,
    /// Virtqueues (hiprio + request).
    queues: Vec<VirtQueue>,
    /// Whether the device has been activated by the driver.
    activated: bool,
    /// Inode table mapping inode numbers to host paths.
    inodes: HashMap<u64, InodeEntry>,
    /// Next inode number to assign.
    next_ino: u64,
    /// Next file handle to assign.
    next_fh: u64,
    /// Open file handles mapping fh → inode.
    open_handles: HashMap<u64, u64>,
}

impl FsDevice {
    /// Creates a new filesystem device sharing the given host directory.
    ///
    /// The `tag` is the mount tag the guest uses to identify this filesystem
    /// (e.g., `"myfs"`). It is truncated to 36 bytes if longer.
    ///
    /// # Errors
    ///
    /// Returns [`FsError::SharedDir`] if the directory metadata cannot be read.
    pub fn new(shared_dir: &Path, tag: &str) -> Result<Self, FsError> {
        let meta = fs::metadata(shared_dir).map_err(FsError::SharedDir)?;

        let mut tag_bytes = [0u8; TAG_LEN];
        let tag_src = tag.as_bytes();
        let len = tag_src.len().min(TAG_LEN);
        tag_bytes[..len].copy_from_slice(&tag_src[..len]);

        let queues = (0..NUM_QUEUES)
            .map(|_| VirtQueue::new(QUEUE_MAX_SIZE))
            .collect();

        let mut inodes = HashMap::new();
        let root_entry = InodeEntry {
            path: shared_dir.to_path_buf(),
            is_dir: meta.is_dir(),
            size: meta_size(&meta),
            mode: meta_mode(&meta),
            ino: FUSE_ROOT_ID,
        };
        inodes.insert(FUSE_ROOT_ID, root_entry);

        Ok(Self {
            shared_dir: shared_dir.to_path_buf(),
            tag: tag_bytes,
            avail_features: VIRTIO_F_VERSION_1,
            acked_features: 0,
            queues,
            activated: false,
            inodes,
            next_ino: 2,
            next_fh: 1,
            open_handles: HashMap::new(),
        })
    }

    /// Returns the shared directory path.
    #[must_use]
    pub fn shared_dir(&self) -> &Path {
        &self.shared_dir
    }

    /// Returns the filesystem tag as a byte slice.
    #[must_use]
    pub fn tag(&self) -> &[u8; TAG_LEN] {
        &self.tag
    }

    /// Returns the filesystem tag as a string (trimmed of null bytes).
    #[must_use]
    pub fn tag_str(&self) -> &str {
        let end = self.tag.iter().position(|&b| b == 0).unwrap_or(TAG_LEN);
        std::str::from_utf8(&self.tag[..end]).unwrap_or("")
    }

    /// Looks up or creates an inode for the given path.
    fn get_or_create_inode(&mut self, path: &Path) -> Option<u64> {
        for (ino, entry) in &self.inodes {
            if entry.path == path {
                return Some(*ino);
            }
        }

        let meta = fs::metadata(path).ok()?;
        let ino = self.next_ino;
        self.next_ino += 1;
        self.inodes.insert(
            ino,
            InodeEntry {
                path: path.to_path_buf(),
                is_dir: meta.is_dir(),
                size: meta_size(&meta),
                mode: meta_mode(&meta),
                ino,
            },
        );
        Some(ino)
    }

    /// Allocates a new file handle for an inode.
    fn alloc_handle(&mut self, ino: u64) -> u64 {
        let fh = self.next_fh;
        self.next_fh += 1;
        self.open_handles.insert(fh, ino);
        fh
    }

    /// Processes all pending requests from the request queue.
    ///
    /// # Errors
    ///
    /// Returns [`FsError`] for fatal errors that affect the entire queue.
    pub fn process_request_queue(
        &mut self,
        memory: &GuestMemory,
        queue: &mut VirtQueue,
    ) -> Result<bool, FsError> {
        if !queue.ready || queue.size == 0 {
            return Ok(false);
        }

        let avail_idx_bytes = memory
            .read_bytes(queue.avail_ring_addr + 2, 2)
            .map_err(FsError::Memory)?;
        let avail_idx = u16::from_le_bytes([avail_idx_bytes[0], avail_idx_bytes[1]]);

        let mut processed = false;

        while queue.last_avail_idx != avail_idx {
            let avail_offset = 4 + u64::from(queue.last_avail_idx % queue.size) * 2;
            let desc_idx_bytes = memory
                .read_bytes(queue.avail_ring_addr + avail_offset, 2)
                .map_err(FsError::Memory)?;
            let head_idx = u16::from_le_bytes([desc_idx_bytes[0], desc_idx_bytes[1]]);

            let written = self.process_fuse_request(memory, queue, head_idx);

            let used_offset = 4 + u64::from(queue.last_used_idx % queue.size) * 8;
            let used_addr = queue.used_ring_addr + used_offset;
            memory
                .write_bytes(used_addr, &u32::from(head_idx).to_le_bytes())
                .map_err(FsError::Memory)?;
            memory
                .write_bytes(used_addr + 4, &written.to_le_bytes())
                .map_err(FsError::Memory)?;

            queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
            queue.last_used_idx = queue.last_used_idx.wrapping_add(1);
            processed = true;
        }

        if processed {
            let used_idx_bytes = queue.last_used_idx.to_le_bytes();
            memory
                .write_bytes(queue.used_ring_addr + 2, &used_idx_bytes)
                .map_err(FsError::Memory)?;
        }

        Ok(processed)
    }

    /// Reads a single descriptor from the descriptor table.
    fn read_desc(memory: &GuestMemory, queue: &VirtQueue, idx: u16) -> Result<VirtqDesc, FsError> {
        if idx >= queue.size {
            return Err(FsError::InvalidDescriptor(format!(
                "descriptor index {idx} >= queue size {}",
                queue.size
            )));
        }
        let addr = queue.desc_table_addr + u64::from(idx) * 16;
        let bytes = memory.read_bytes(addr, 16).map_err(FsError::Memory)?;
        Ok(VirtqDesc {
            addr: u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            len: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            flags: u16::from_le_bytes([bytes[12], bytes[13]]),
            next: u16::from_le_bytes([bytes[14], bytes[15]]),
        })
    }

    /// Collects all descriptors in a chain into readable/writable groups.
    fn collect_chain(
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
    ) -> Result<(Vec<VirtqDesc>, Vec<VirtqDesc>), FsError> {
        let mut readable = Vec::new();
        let mut writable = Vec::new();
        let mut current_idx = head_idx;
        let mut visited = 0u32;

        loop {
            let desc = Self::read_desc(memory, queue, current_idx)?;
            visited += 1;
            if visited > u32::from(queue.size) {
                return Err(FsError::InvalidDescriptor(
                    "descriptor chain cycle detected".into(),
                ));
            }

            if desc.flags & VIRTQ_DESC_F_WRITE != 0 {
                writable.push(desc);
            } else {
                readable.push(desc);
            }

            if desc.flags & VIRTQ_DESC_F_NEXT == 0 {
                break;
            }
            current_idx = desc.next;
        }

        Ok((readable, writable))
    }

    /// Reads all bytes from readable descriptors into a flat buffer.
    fn read_request_data(memory: &GuestMemory, descs: &[VirtqDesc]) -> Result<Vec<u8>, FsError> {
        let mut data = Vec::new();
        for desc in descs {
            let bytes = memory
                .read_bytes(desc.addr, desc.len as usize)
                .map_err(FsError::Memory)?;
            data.extend_from_slice(&bytes);
        }
        Ok(data)
    }

    /// Writes response data to writable descriptors, returning bytes written.
    fn write_response_data(
        memory: &GuestMemory,
        descs: &[VirtqDesc],
        data: &[u8],
    ) -> Result<u32, FsError> {
        let mut offset = 0usize;
        let mut written: u32 = 0;

        for desc in descs {
            if offset >= data.len() {
                break;
            }
            let chunk_len = (desc.len as usize).min(data.len() - offset);
            memory
                .write_bytes(desc.addr, &data[offset..offset + chunk_len])
                .map_err(FsError::Memory)?;
            offset += chunk_len;
            if let Ok(n) = u32::try_from(chunk_len) {
                written += n;
            }
        }

        Ok(written)
    }

    /// Processes a single FUSE request and returns the number of bytes written.
    fn process_fuse_request(
        &mut self,
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
    ) -> u32 {
        let Ok((readable, writable)) = Self::collect_chain(memory, queue, head_idx) else {
            return 0;
        };

        let Ok(request_data) = Self::read_request_data(memory, &readable) else {
            return 0;
        };

        if request_data.len() < FUSE_IN_HEADER_SIZE {
            let response = Self::make_error_response(0, ENOSYS);
            return Self::write_response_data(memory, &writable, &response).unwrap_or(0);
        }

        let opcode = u32::from_le_bytes([
            request_data[4],
            request_data[5],
            request_data[6],
            request_data[7],
        ]);
        let unique = u64::from_le_bytes([
            request_data[8],
            request_data[9],
            request_data[10],
            request_data[11],
            request_data[12],
            request_data[13],
            request_data[14],
            request_data[15],
        ]);
        let nodeid = u64::from_le_bytes([
            request_data[16],
            request_data[17],
            request_data[18],
            request_data[19],
            request_data[20],
            request_data[21],
            request_data[22],
            request_data[23],
        ]);

        let response = match opcode {
            FUSE_INIT => Self::handle_init(unique),
            FUSE_LOOKUP => self.handle_lookup(unique, nodeid, &request_data[FUSE_IN_HEADER_SIZE..]),
            FUSE_GETATTR => self.handle_getattr(unique, nodeid),
            FUSE_OPEN => self.handle_open(unique, nodeid),
            FUSE_OPENDIR => self.handle_opendir(unique, nodeid),
            FUSE_READ => self.handle_read(unique, &request_data[FUSE_IN_HEADER_SIZE..]),
            FUSE_READDIR => self.handle_readdir(unique, &request_data[FUSE_IN_HEADER_SIZE..]),
            FUSE_RELEASE | FUSE_RELEASEDIR => {
                self.handle_release(unique, &request_data[FUSE_IN_HEADER_SIZE..])
            }
            FUSE_FORGET => return 0, // FORGET has no reply
            _ => Self::make_error_response(unique, ENOSYS),
        };

        Self::write_response_data(memory, &writable, &response).unwrap_or(0)
    }

    /// Builds a FUSE error response (just the out header with an error code).
    fn make_error_response(unique: u64, error: i32) -> Vec<u8> {
        let mut resp = vec![0u8; FUSE_OUT_HEADER_SIZE];
        resp[0..4].copy_from_slice(&16u32.to_le_bytes());
        resp[4..8].copy_from_slice(&error.to_le_bytes());
        resp[8..16].copy_from_slice(&unique.to_le_bytes());
        resp
    }

    /// Builds a FUSE success response with a payload.
    fn make_response(unique: u64, payload: &[u8]) -> Vec<u8> {
        let total_len = FUSE_OUT_HEADER_SIZE + payload.len();
        let mut resp = vec![0u8; total_len];
        if let Ok(len) = u32::try_from(total_len) {
            resp[0..4].copy_from_slice(&len.to_le_bytes());
        }
        // error = 0 (success)
        resp[8..16].copy_from_slice(&unique.to_le_bytes());
        resp[FUSE_OUT_HEADER_SIZE..].copy_from_slice(payload);
        resp
    }

    /// Handles `FUSE_INIT`: returns protocol version and capabilities.
    fn handle_init(unique: u64) -> Vec<u8> {
        // fuse_init_out: major(4) + minor(4) + max_readahead(4) + flags(4)
        // + max_background(2) + congestion_threshold(2) + max_write(4)
        // + time_gran(4) + max_pages(2) + map_alignment(2) + flags2(4) + unused(28)
        // = 64 bytes total (FUSE 7.31+), but we only fill the essentials
        let mut payload = vec![0u8; 64];
        // FUSE major version = 7
        payload[0..4].copy_from_slice(&7u32.to_le_bytes());
        // FUSE minor version = 31
        payload[4..8].copy_from_slice(&31u32.to_le_bytes());
        // max_readahead
        payload[8..12].copy_from_slice(&(128 * 1024u32).to_le_bytes());
        // max_write
        payload[20..24].copy_from_slice(&(128 * 1024u32).to_le_bytes());
        Self::make_response(unique, &payload)
    }

    /// Handles `FUSE_LOOKUP`: resolve a name in a directory to an inode.
    fn handle_lookup(&mut self, unique: u64, parent_ino: u64, data: &[u8]) -> Vec<u8> {
        let Some(parent) = self.inodes.get(&parent_ino).cloned() else {
            return Self::make_error_response(unique, ENOENT);
        };
        if !parent.is_dir {
            return Self::make_error_response(unique, ENOTDIR);
        }

        let name_end = data.iter().position(|&b| b == 0).unwrap_or(data.len());
        let Ok(name) = std::str::from_utf8(&data[..name_end]) else {
            return Self::make_error_response(unique, ENOENT);
        };

        let child_path = parent.path.join(name);
        let Some(child_ino) = self.get_or_create_inode(&child_path) else {
            return Self::make_error_response(unique, ENOENT);
        };

        let Some(child) = self.inodes.get(&child_ino) else {
            return Self::make_error_response(unique, ENOENT);
        };

        // fuse_entry_out: nodeid(8) + generation(8) + entry_valid(8) + attr_valid(8)
        // + entry_valid_nsec(4) + attr_valid_nsec(4) + fuse_attr(88) = 128 bytes
        let payload = Self::make_entry_out(child);
        Self::make_response(unique, &payload)
    }

    /// Handles `FUSE_GETATTR`: return file attributes for an inode.
    fn handle_getattr(&self, unique: u64, nodeid: u64) -> Vec<u8> {
        let Some(entry) = self.inodes.get(&nodeid) else {
            return Self::make_error_response(unique, ENOENT);
        };

        // fuse_attr_out: attr_valid(8) + attr_valid_nsec(4) + dummy(4) + fuse_attr(88)
        let mut payload = vec![0u8; 104];
        // attr_valid = 1 second
        payload[0..8].copy_from_slice(&1u64.to_le_bytes());
        Self::write_fuse_attr(&mut payload[16..], entry);
        Self::make_response(unique, &payload)
    }

    /// Handles `FUSE_OPEN`: open a file handle (read-only).
    fn handle_open(&mut self, unique: u64, nodeid: u64) -> Vec<u8> {
        let Some(entry) = self.inodes.get(&nodeid) else {
            return Self::make_error_response(unique, ENOENT);
        };
        if entry.is_dir {
            return Self::make_error_response(unique, EISDIR);
        }

        let fh = self.alloc_handle(nodeid);
        // fuse_open_out: fh(8) + open_flags(4) + padding(4) = 16 bytes
        let mut payload = vec![0u8; 16];
        payload[0..8].copy_from_slice(&fh.to_le_bytes());
        Self::make_response(unique, &payload)
    }

    /// Handles `FUSE_OPENDIR`: open a directory handle.
    fn handle_opendir(&mut self, unique: u64, nodeid: u64) -> Vec<u8> {
        let Some(entry) = self.inodes.get(&nodeid) else {
            return Self::make_error_response(unique, ENOENT);
        };
        if !entry.is_dir {
            return Self::make_error_response(unique, ENOTDIR);
        }

        let fh = self.alloc_handle(nodeid);
        let mut payload = vec![0u8; 16];
        payload[0..8].copy_from_slice(&fh.to_le_bytes());
        Self::make_response(unique, &payload)
    }

    /// Handles `FUSE_READ`: read file data.
    fn handle_read(&mut self, unique: u64, data: &[u8]) -> Vec<u8> {
        if data.len() < 40 {
            return Self::make_error_response(unique, ENOSYS);
        }

        let fh = u64::from_le_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]);
        let offset = u64::from_le_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);
        let size = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);

        let Some(&ino) = self.open_handles.get(&fh) else {
            return Self::make_error_response(unique, EBADF);
        };
        let Some(entry) = self.inodes.get(&ino) else {
            return Self::make_error_response(unique, ENOENT);
        };

        let Ok(mut file) = File::open(&entry.path) else {
            return Self::make_error_response(unique, ENOENT);
        };

        if file.seek(SeekFrom::Start(offset)).is_err() {
            return Self::make_error_response(unique, EBADF);
        }

        let read_size = (size as usize).min(128 * 1024);
        let mut buf = vec![0u8; read_size];
        let Ok(bytes_read) = file.read(&mut buf) else {
            return Self::make_error_response(unique, EBADF);
        };

        Self::make_response(unique, &buf[..bytes_read])
    }

    /// Handles `FUSE_READDIR`: list directory entries.
    fn handle_readdir(&mut self, unique: u64, data: &[u8]) -> Vec<u8> {
        if data.len() < 40 {
            return Self::make_error_response(unique, ENOSYS);
        }

        let fh = u64::from_le_bytes([
            data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
        ]);
        let offset = u64::from_le_bytes([
            data[8], data[9], data[10], data[11], data[12], data[13], data[14], data[15],
        ]);
        let size = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);

        let Some(&ino) = self.open_handles.get(&fh) else {
            return Self::make_error_response(unique, EBADF);
        };
        let Some(entry) = self.inodes.get(&ino).cloned() else {
            return Self::make_error_response(unique, ENOENT);
        };
        if !entry.is_dir {
            return Self::make_error_response(unique, ENOTDIR);
        }

        let Ok(read_dir) = fs::read_dir(&entry.path) else {
            return Self::make_error_response(unique, ENOENT);
        };

        let entries: Vec<_> = read_dir.filter_map(std::result::Result::ok).collect();
        let max_size = size as usize;
        let mut dirent_buf = Vec::new();
        let mut entry_offset = 0u64;

        for dir_entry in &entries {
            entry_offset += 1;
            if entry_offset <= offset {
                continue;
            }

            let name_bytes = dir_entry.file_name();
            let name = name_bytes.as_encoded_bytes();
            let child_path = entry.path.join(dir_entry.file_name());
            let child_ino = self.get_or_create_inode(&child_path).unwrap_or(0);

            let file_type = if dir_entry.path().is_dir() {
                u32::from(libc::DT_DIR)
            } else {
                u32::from(libc::DT_REG)
            };

            // fuse_dirent: ino(8) + off(8) + namelen(4) + type(4) + name(padded to 8)
            let namelen = name.len();
            let padded_name_len = (namelen + 7) & !7;
            let dirent_size = 24 + padded_name_len;

            if dirent_buf.len() + dirent_size > max_size {
                break;
            }

            let mut dirent = vec![0u8; dirent_size];
            dirent[0..8].copy_from_slice(&child_ino.to_le_bytes());
            dirent[8..16].copy_from_slice(&entry_offset.to_le_bytes());
            if let Ok(nl) = u32::try_from(namelen) {
                dirent[16..20].copy_from_slice(&nl.to_le_bytes());
            }
            dirent[20..24].copy_from_slice(&file_type.to_le_bytes());
            dirent[24..24 + namelen].copy_from_slice(name);

            dirent_buf.extend_from_slice(&dirent);
        }

        Self::make_response(unique, &dirent_buf)
    }

    /// Handles `FUSE_RELEASE` / `FUSE_RELEASEDIR`: close a file handle.
    fn handle_release(&mut self, unique: u64, data: &[u8]) -> Vec<u8> {
        if data.len() >= 8 {
            let fh = u64::from_le_bytes([
                data[0], data[1], data[2], data[3], data[4], data[5], data[6], data[7],
            ]);
            self.open_handles.remove(&fh);
        }
        Self::make_response(unique, &[])
    }

    /// Writes FUSE attr struct (88 bytes) for an inode entry.
    fn write_fuse_attr(buf: &mut [u8], entry: &InodeEntry) {
        if buf.len() < 88 {
            return;
        }
        // ino (8)
        buf[0..8].copy_from_slice(&entry.ino.to_le_bytes());
        // size (8)
        buf[8..16].copy_from_slice(&entry.size.to_le_bytes());
        // blocks (8) — approximate
        let blocks = entry.size.div_ceil(512);
        buf[16..24].copy_from_slice(&blocks.to_le_bytes());
        // mode (4) at offset 40
        buf[40..44].copy_from_slice(&entry.mode.to_le_bytes());
        // nlink (4) at offset 44
        let nlink: u32 = if entry.is_dir { 2 } else { 1 };
        buf[44..48].copy_from_slice(&nlink.to_le_bytes());
    }

    /// Builds `fuse_entry_out` payload (128 bytes) for a looked-up entry.
    fn make_entry_out(entry: &InodeEntry) -> Vec<u8> {
        let mut payload = vec![0u8; 128];
        // nodeid (8)
        payload[0..8].copy_from_slice(&entry.ino.to_le_bytes());
        // generation (8) — zero
        // entry_valid (8) = 1 second
        payload[16..24].copy_from_slice(&1u64.to_le_bytes());
        // attr_valid (8) = 1 second
        payload[24..32].copy_from_slice(&1u64.to_le_bytes());
        // fuse_attr starts at offset 40
        Self::write_fuse_attr(&mut payload[40..], entry);
        payload
    }
}

impl VirtioDevice for FsDevice {
    fn device_type(&self) -> DeviceType {
        DeviceType::Fs
    }

    fn avail_features(&self) -> u64 {
        self.avail_features
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, features: u64) {
        self.acked_features = features;
    }

    fn queues(&self) -> &[VirtQueue] {
        &self.queues
    }

    fn queues_mut(&mut self) -> &mut [VirtQueue] {
        &mut self.queues
    }

    /// Reads virtio-fs config space.
    ///
    /// Layout:
    /// - `[0..36)`: filesystem tag (null-padded)
    /// - `[36..40)`: `num_request_queues` (LE u32, always 1 for P1)
    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let mut config = [0u8; CONFIG_SIZE];
        config[..TAG_LEN].copy_from_slice(&self.tag);
        // num_request_queues = 1
        config[TAG_LEN..CONFIG_SIZE].copy_from_slice(&1u32.to_le_bytes());

        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config.get(offset.wrapping_add(i)).copied().unwrap_or(0);
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // Config is read-only for fs devices — no-op.
    }

    fn activate(&mut self) -> Result<(), VirtioError> {
        self.activated = true;
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.activated
    }

    fn reset(&mut self) {
        self.activated = false;
        for queue in &mut self.queues {
            queue.reset();
        }
    }

    fn process_queue(
        &mut self,
        queue_idx: usize,
        memory: &GuestMemory,
    ) -> Result<bool, VirtioError> {
        if queue_idx != REQUEST_QUEUE {
            return Ok(false);
        }
        let Some(queue) = self.queues.get_mut(queue_idx) else {
            return Ok(false);
        };
        let mut queue_state = queue.clone();
        let result = self.process_request_queue(memory, &mut queue_state);
        if let Some(q) = self.queues.get_mut(queue_idx) {
            q.last_avail_idx = queue_state.last_avail_idx;
            q.last_used_idx = queue_state.last_used_idx;
        }
        match result {
            Ok(processed) => Ok(processed),
            Err(_) => Ok(false),
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "fs_test.rs"]
mod tests;
