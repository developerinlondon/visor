//! Virtio block device backend (`virtio-blk`).
//!
//! Provides a host file as a guest block device (`/dev/vda`).
//! Handles device configuration, feature negotiation, and I/O processing
//! through virtqueue descriptor chains.
//!
//! # I/O Processing
//!
//! When the guest writes to `QueueNotify`, the MMIO transport calls
//! [`BlockDevice::process_queue`] which:
//! 1. Reads the avail ring to discover pending descriptor chains
//! 2. Walks each chain: header → data buffer(s) → status byte
//! 3. Performs the requested file I/O (read, write, flush, get-id)
//! 4. Writes results to the used ring for the guest to consume
//!
//! # Config space layout (virtio-blk spec)
//!
//! - Offset `0..7`: capacity in 512-byte sectors (`u64`, little-endian).

use std::cmp;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
#[cfg(target_os = "linux")]
use std::os::linux::fs::MetadataExt;
use std::path::Path;

use crate::memory::GuestMemory;
use crate::transport::{
    DeviceType, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioDevice, VirtioError,
    VirtqDesc,
};

// ── Feature flags ────────────────────────────────────────────────────

/// Virtio feature: device is read-only.
pub const VIRTIO_BLK_F_RO: u64 = 1 << 5;

/// Virtio feature: modern virtio (version 1).
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

// ── Constants ────────────────────────────────────────────────────────

/// Sector size in bytes.
const SECTOR_SIZE: u64 = 512;

/// Maximum virtqueue size for the block device.
const QUEUE_MAX_SIZE: u16 = 256;

/// Number of virtqueues (block device has 1 request queue).
const NUM_QUEUES: usize = 1;

/// Size of the device ID array.
const DEVICE_ID_LEN: usize = 20;

// ── Virtio-blk request types ───────────────────────────────────────

/// Read from device to guest (device-writable data buffer).
const VIRTIO_BLK_T_IN: u32 = 0;

/// Write from guest to device (device-readable data buffer).
const VIRTIO_BLK_T_OUT: u32 = 1;

/// Flush (fsync) the device backing store.
const VIRTIO_BLK_T_FLUSH: u32 = 4;

/// Get device identifier string.
const VIRTIO_BLK_T_GET_ID: u32 = 8;

// ── Virtio-blk status bytes ────────────────────────────────────────

/// Request completed successfully.
const VIRTIO_BLK_S_OK: u8 = 0;

/// Request failed due to I/O error.
const VIRTIO_BLK_S_IOERR: u8 = 1;

/// Unsupported request type.
const VIRTIO_BLK_S_UNSUPP: u8 = 2;

// ── Errors ───────────────────────────────────────────────────────────

/// Errors from block device operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BlockError {
    /// Failed to open disk image.
    #[error("failed to open disk image: {0}")]
    OpenFile(std::io::Error),
    /// Failed to get disk size.
    #[error("failed to get disk size: {0}")]
    GetSize(std::io::Error),
    /// I/O error during request processing.
    #[error("block I/O error: {0}")]
    Io(std::io::Error),
    /// Guest memory access error.
    #[error("guest memory error: {0}")]
    Memory(crate::memory::MemoryError),
    /// Invalid virtqueue descriptor.
    #[error("invalid descriptor: {0}")]
    InvalidDescriptor(String),
}

// ── BlockDevice ──────────────────────────────────────────────────────

/// Virtio block device backed by a host file.
///
/// Implements [`VirtioDevice`] for use with the MMIO transport.
/// The backing file is exposed to the guest as a block device.
#[derive(Debug)]
#[non_exhaustive]
pub struct BlockDevice {
    /// Backing disk file.
    disk_file: File,
    /// Number of 512-byte sectors.
    num_sectors: u64,
    /// Whether the disk is read-only.
    read_only: bool,
    /// Feature bits offered by the device.
    avail_features: u64,
    /// Feature bits acknowledged by the driver.
    acked_features: u64,
    /// Virtqueues (block has 1 request queue).
    queues: Vec<VirtQueue>,
    /// Whether the device has been activated by the driver.
    activated: bool,
    /// Device ID derived from file metadata (20 bytes, as per virtio spec).
    device_id: [u8; DEVICE_ID_LEN],
}

impl BlockDevice {
    /// Creates a new block device backed by the file at `disk_path`.
    ///
    /// Opens the file (read-only or read-write based on `read_only`),
    /// determines its size, and computes the sector count.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError::OpenFile`] if the file cannot be opened, or
    /// [`BlockError::GetSize`] if the file size cannot be determined.
    pub fn new(disk_path: &Path, read_only: bool) -> Result<Self, BlockError> {
        let mut disk_file = OpenOptions::new()
            .read(true)
            .write(!read_only)
            .open(disk_path)
            .map_err(BlockError::OpenFile)?;

        let file_size = disk_file
            .seek(SeekFrom::End(0))
            .map_err(BlockError::GetSize)?;

        let num_sectors = file_size / SECTOR_SIZE;

        let mut avail_features = VIRTIO_F_VERSION_1;
        if read_only {
            avail_features |= VIRTIO_BLK_F_RO;
        }

        let device_id = Self::build_device_id(&disk_file);

        let queues = (0..NUM_QUEUES)
            .map(|_| VirtQueue::new(QUEUE_MAX_SIZE))
            .collect();

        Ok(Self {
            disk_file,
            num_sectors,
            read_only,
            avail_features,
            acked_features: 0,
            queues,
            activated: false,
            device_id,
        })
    }

    /// Returns the number of 512-byte sectors on the disk.
    #[must_use]
    pub fn num_sectors(&self) -> u64 {
        self.num_sectors
    }

    /// Returns a reference to the backing disk file.
    #[must_use]
    pub fn disk_file(&self) -> &File {
        &self.disk_file
    }

    /// Returns whether the disk is read-only.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Returns the device ID (20 bytes derived from file metadata).
    #[must_use]
    pub fn device_id(&self) -> &[u8; DEVICE_ID_LEN] {
        &self.device_id
    }

    /// Builds a device ID from file metadata (`st_dev`, `st_rdev`, `st_ino`),
    /// following the kvmtool convention used by Firecracker.
    #[cfg(target_os = "linux")]
    fn build_device_id(disk_file: &File) -> [u8; DEVICE_ID_LEN] {
        let mut id = [0u8; DEVICE_ID_LEN];
        if let Ok(meta) = disk_file.metadata() {
            let id_str = format!("{}{}{}", meta.st_dev(), meta.st_rdev(), meta.st_ino());
            let bytes = id_str.as_bytes();
            let len = cmp::min(bytes.len(), DEVICE_ID_LEN);
            id[..len].copy_from_slice(&bytes[..len]);
        }
        id
    }

    /// Builds a device ID from file metadata (`dev`, `rdev`, `ino`),
    /// following the kvmtool convention used by Firecracker.
    #[cfg(not(target_os = "linux"))]
    fn build_device_id(disk_file: &File) -> [u8; DEVICE_ID_LEN] {
        use std::os::unix::fs::MetadataExt;
        let mut id = [0u8; DEVICE_ID_LEN];
        if let Ok(meta) = disk_file.metadata() {
            let id_str = format!("{}{}{}", meta.dev(), meta.rdev(), meta.ino());
            let bytes = id_str.as_bytes();
            let len = cmp::min(bytes.len(), DEVICE_ID_LEN);
            id[..len].copy_from_slice(&bytes[..len]);
        }
        id
    }
}

// ── I/O processing ────────────────────────────────────────────────────

impl BlockDevice {
    /// Processes all pending requests from the given virtqueue.
    ///
    /// Reads the avail ring, walks descriptor chains for each pending request,
    /// performs the file I/O, writes status and used ring entries.
    ///
    /// Returns `Ok(true)` if any requests were processed.
    ///
    /// # Errors
    ///
    /// Returns [`BlockError`] for fatal errors that affect the entire queue.
    /// Individual request failures write `IOERR` status and continue.
    pub fn process_queue(
        &mut self,
        memory: &GuestMemory,
        queue: &mut VirtQueue,
    ) -> Result<bool, BlockError> {
        if !queue.ready || queue.size == 0 {
            return Ok(false);
        }

        // Read the current avail ring idx (offset +2 in the avail ring).
        let avail_idx_bytes = memory
            .read_bytes(queue.avail_ring_addr + 2, 2)
            .map_err(BlockError::Memory)?;
        let avail_idx = u16::from_le_bytes([avail_idx_bytes[0], avail_idx_bytes[1]]);
        let mut processed = false;

        while queue.last_avail_idx != avail_idx {
            let avail_offset = 4 + u64::from(queue.last_avail_idx % queue.size) * 2;
            let desc_idx_bytes = memory
                .read_bytes(queue.avail_ring_addr + avail_offset, 2)
                .map_err(BlockError::Memory)?;
            let head_idx = u16::from_le_bytes([desc_idx_bytes[0], desc_idx_bytes[1]]);

            let (status, written) = self.process_one_request(memory, queue, head_idx);

            // Write used ring entry: 4 bytes at offset 4 + (last_used_idx % size) * 8
            let used_offset = 4 + u64::from(queue.last_used_idx % queue.size) * 8;
            let used_addr = queue.used_ring_addr + used_offset;
            let id_bytes = u32::from(head_idx).to_le_bytes();
            let len_bytes = written.to_le_bytes();
            // Used ring writes must succeed — if they fail, the guest will never
            // see the completed request and will hang waiting for it.
            memory
                .write_bytes(used_addr, &id_bytes)
                .map_err(BlockError::Memory)?;
            memory
                .write_bytes(used_addr + 4, &len_bytes)
                .map_err(BlockError::Memory)?;

            queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
            queue.last_used_idx = queue.last_used_idx.wrapping_add(1);
            processed = true;

            // Write status byte — failure means the guest can't read completion status.
            Self::write_status_for_chain(memory, queue, head_idx, status)?;
        }

        if processed {
            // Update used ring idx so the guest knows new entries are available.
            let used_idx_bytes = queue.last_used_idx.to_le_bytes();
            memory
                .write_bytes(queue.used_ring_addr + 2, &used_idx_bytes)
                .map_err(BlockError::Memory)?;
        }

        Ok(processed)
    }

    /// Reads a single descriptor from the descriptor table in guest memory.
    fn read_desc(
        memory: &GuestMemory,
        queue: &VirtQueue,
        idx: u16,
    ) -> Result<VirtqDesc, BlockError> {
        if idx >= queue.size {
            return Err(BlockError::InvalidDescriptor(format!(
                "descriptor index {idx} >= queue size {}",
                queue.size
            )));
        }
        let addr = queue.desc_table_addr + u64::from(idx) * 16;
        let bytes = memory.read_bytes(addr, 16).map_err(BlockError::Memory)?;
        Ok(VirtqDesc {
            addr: u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            len: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            flags: u16::from_le_bytes([bytes[12], bytes[13]]),
            next: u16::from_le_bytes([bytes[14], bytes[15]]),
        })
    }

    /// Processes a single request descriptor chain, returning (status, bytes written).
    ///
    /// On any error, returns `VIRTIO_BLK_S_IOERR` without panicking.
    fn process_one_request(
        &mut self,
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
    ) -> (u8, u32) {
        match self.process_one_request_inner(memory, queue, head_idx) {
            Ok((status, written)) => (status, written),
            Err(_) => (VIRTIO_BLK_S_IOERR, 0),
        }
    }

    /// Inner implementation that can return errors for a single request.
    fn process_one_request_inner(
        &mut self,
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
    ) -> Result<(u8, u32), BlockError> {
        // 1. Read header descriptor (first in chain).
        let header_desc = Self::read_desc(memory, queue, head_idx)?;
        if header_desc.flags & VIRTQ_DESC_F_WRITE != 0 {
            return Err(BlockError::InvalidDescriptor(
                "header descriptor has WRITE flag".into(),
            ));
        }
        if header_desc.len < 16 {
            return Err(BlockError::InvalidDescriptor(
                "header too small (need 16 bytes)".into(),
            ));
        }
        if header_desc.flags & VIRTQ_DESC_F_NEXT == 0 {
            return Err(BlockError::InvalidDescriptor(
                "header has no NEXT descriptor".into(),
            ));
        }

        // Parse request header: type(u32), reserved(u32), sector(u64).
        let hdr_bytes = memory
            .read_bytes(header_desc.addr, 16)
            .map_err(BlockError::Memory)?;
        let req_type = u32::from_le_bytes([hdr_bytes[0], hdr_bytes[1], hdr_bytes[2], hdr_bytes[3]]);
        let sector = u64::from_le_bytes([
            hdr_bytes[8],
            hdr_bytes[9],
            hdr_bytes[10],
            hdr_bytes[11],
            hdr_bytes[12],
            hdr_bytes[13],
            hdr_bytes[14],
            hdr_bytes[15],
        ]);

        // 2. Walk remaining descriptors to find data + status.
        //    The last descriptor in the chain is always the status byte.
        //    Everything between the header and the status is data.
        let mut chain = Vec::new();
        let mut current_idx = header_desc.next;
        let mut visited = 0u32;
        loop {
            let desc = Self::read_desc(memory, queue, current_idx)?;
            chain.push(desc);
            visited += 1;
            if visited > u32::from(queue.size) {
                return Err(BlockError::InvalidDescriptor(
                    "descriptor chain cycle detected".into(),
                ));
            }
            if desc.flags & VIRTQ_DESC_F_NEXT == 0 {
                break;
            }
            current_idx = desc.next;
        }

        if chain.is_empty() {
            return Err(BlockError::InvalidDescriptor(
                "no data or status descriptors".into(),
            ));
        }

        // Last descriptor is the status byte.
        let status_desc = chain[chain.len() - 1];
        if status_desc.flags & VIRTQ_DESC_F_WRITE == 0 {
            return Err(BlockError::InvalidDescriptor(
                "status descriptor missing WRITE flag".into(),
            ));
        }

        // Data descriptors are everything except the last.
        let data_descs = &chain[..chain.len() - 1];

        // 3. Perform I/O based on request type.
        let mut total_written: u32 = 0;
        let status = match req_type {
            VIRTIO_BLK_T_IN => self.handle_read(memory, data_descs, sector, &mut total_written)?,
            VIRTIO_BLK_T_OUT => self.handle_write(memory, data_descs, sector)?,
            VIRTIO_BLK_T_FLUSH => self.handle_flush()?,
            VIRTIO_BLK_T_GET_ID => self.handle_get_id(memory, data_descs, &mut total_written)?,
            _ => VIRTIO_BLK_S_UNSUPP,
        };

        // 4. Write status byte to the status descriptor.
        memory
            .write_bytes(status_desc.addr, &[status])
            .map_err(BlockError::Memory)?;
        // Status descriptor is device-writable: count the 1 byte.
        total_written += 1;

        Ok((status, total_written))
    }

    /// Handles a read request (`VIRTIO_BLK_T_IN`): disk → guest memory.
    fn handle_read(
        &mut self,
        memory: &GuestMemory,
        data_descs: &[VirtqDesc],
        sector: u64,
        total_written: &mut u32,
    ) -> Result<u8, BlockError> {
        let offset = sector * SECTOR_SIZE;
        self.disk_file
            .seek(SeekFrom::Start(offset))
            .map_err(BlockError::Io)?;

        for desc in data_descs {
            if desc.flags & VIRTQ_DESC_F_WRITE == 0 {
                return Ok(VIRTIO_BLK_S_IOERR);
            }
            let mut buf = vec![0u8; desc.len as usize];
            self.disk_file
                .read_exact(&mut buf)
                .map_err(BlockError::Io)?;
            memory
                .write_bytes(desc.addr, &buf)
                .map_err(BlockError::Memory)?;
            *total_written += desc.len;
        }
        Ok(VIRTIO_BLK_S_OK)
    }

    /// Handles a write request (`VIRTIO_BLK_T_OUT`): guest memory → disk.
    fn handle_write(
        &mut self,
        memory: &GuestMemory,
        data_descs: &[VirtqDesc],
        sector: u64,
    ) -> Result<u8, BlockError> {
        if self.read_only {
            return Ok(VIRTIO_BLK_S_IOERR);
        }
        let offset = sector * SECTOR_SIZE;
        self.disk_file
            .seek(SeekFrom::Start(offset))
            .map_err(BlockError::Io)?;

        for desc in data_descs {
            if desc.flags & VIRTQ_DESC_F_WRITE != 0 {
                return Ok(VIRTIO_BLK_S_IOERR);
            }
            let buf = memory
                .read_bytes(desc.addr, desc.len as usize)
                .map_err(BlockError::Memory)?;
            self.disk_file.write_all(&buf).map_err(BlockError::Io)?;
        }
        Ok(VIRTIO_BLK_S_OK)
    }

    /// Handles a flush request (`VIRTIO_BLK_T_FLUSH`): fsync the backing file.
    fn handle_flush(&mut self) -> Result<u8, BlockError> {
        self.disk_file.sync_all().map_err(BlockError::Io)?;
        Ok(VIRTIO_BLK_S_OK)
    }

    /// Handles a get-id request (`VIRTIO_BLK_T_GET_ID`): write device ID.
    fn handle_get_id(
        &self,
        memory: &GuestMemory,
        data_descs: &[VirtqDesc],
        total_written: &mut u32,
    ) -> Result<u8, BlockError> {
        for desc in data_descs {
            if desc.flags & VIRTQ_DESC_F_WRITE == 0 {
                return Ok(VIRTIO_BLK_S_IOERR);
            }
            let len = cmp::min(desc.len as usize, DEVICE_ID_LEN);
            memory
                .write_bytes(desc.addr, &self.device_id[..len])
                .map_err(BlockError::Memory)?;
            // len <= DEVICE_ID_LEN (20), so this conversion always succeeds.
            if let Ok(n) = u32::try_from(len) {
                *total_written += n;
            }
        }
        Ok(VIRTIO_BLK_S_OK)
    }

    /// Finds the status descriptor in a chain and writes the status byte.
    ///
    /// Used by [`process_queue`](Self::process_queue) to write status after
    /// the used ring entry is already written. This walks the chain again
    /// to find the last descriptor.
    fn write_status_for_chain(
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
        status: u8,
    ) -> Result<(), BlockError> {
        // Walk to the last descriptor in the chain.
        let mut idx = head_idx;
        let mut visited = 0u32;
        loop {
            let desc = Self::read_desc(memory, queue, idx)?;
            visited += 1;
            if visited > u32::from(queue.size) {
                return Err(BlockError::InvalidDescriptor(
                    "descriptor chain cycle in status write".into(),
                ));
            }
            if desc.flags & VIRTQ_DESC_F_NEXT == 0 {
                // This is the status descriptor.
                memory
                    .write_bytes(desc.addr, &[status])
                    .map_err(BlockError::Memory)?;
                return Ok(());
            }
            idx = desc.next;
        }
    }
}

impl VirtioDevice for BlockDevice {
    fn device_type(&self) -> DeviceType {
        DeviceType::Block
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

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let config_bytes = self.num_sectors.to_le_bytes();
        for (i, byte) in data.iter_mut().enumerate() {
            let Some(idx) = usize::try_from(offset).ok().and_then(|o| o.checked_add(i)) else {
                *byte = 0;
                continue;
            };
            if let Some(&val) = config_bytes.get(idx) {
                *byte = val;
            } else {
                *byte = 0;
            }
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // Config is read-only for block devices — no-op.
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
        let Some(queue) = self.queues.get_mut(queue_idx) else {
            return Ok(false);
        };
        // Clone queue state to avoid double-borrow of self.
        // We need &mut self for disk I/O and &mut queue for index updates.
        let mut queue_state = queue.clone();
        let result = self.process_queue(memory, &mut queue_state);
        // Write back the updated indices regardless of error.
        if let Some(q) = self.queues.get_mut(queue_idx) {
            q.last_avail_idx = queue_state.last_avail_idx;
            q.last_used_idx = queue_state.last_used_idx;
        }
        match result {
            Ok(processed) => Ok(processed),
            Err(e) => {
                tracing::error!("block device process_queue failed: {e}");
                Ok(false)
            }
        }
    }
}

#[cfg(all(test, target_os = "linux"))]
#[path = "block_test.rs"]
mod tests;
