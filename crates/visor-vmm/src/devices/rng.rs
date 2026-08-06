//! Virtio entropy source device (`virtio-rng`).
//!
//! Provides random bytes to the guest OS via a single virtqueue. The guest
//! driver places empty buffers on the queue; the device fills them with
//! entropy from the host's `/dev/urandom` (via `getrandom(2)` syscall).
//!
//! This is the simplest virtio device — no config space, no feature bits
//! beyond `VIRTIO_F_VERSION_1`, and a single read-only queue.
//!
//! # Queue layout
//!
//! - Queue 0 (`receiveq`): guest provides device-writable buffers; the device
//!   fills them with random bytes and returns them via the used ring.

use std::fs::File;
use std::io::Read;
use std::path::Path;

use crate::memory::GuestMemory;
use crate::transport::{
    DeviceType, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioDevice, VirtioError,
    VirtqDesc,
};

// ── Feature flags ────────────────────────────────────────────────────

/// Virtio feature: modern virtio (version 1).
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

// ── Constants ────────────────────────────────────────────────────────

/// Maximum virtqueue size for the RNG device.
const QUEUE_MAX_SIZE: u16 = 256;

/// Number of virtqueues (rng device has 1 receive queue).
const NUM_QUEUES: usize = 1;

/// Maximum bytes to generate per single request to avoid blocking.
const MAX_BYTES_PER_REQUEST: usize = 65536;

// ── Errors ───────────────────────────────────────────────────────────

/// Errors from RNG device operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RngError {
    /// Failed to open the entropy source.
    #[error("failed to open entropy source: {0}")]
    OpenSource(std::io::Error),
    /// Failed to read random bytes from the entropy source.
    #[error("failed to read random bytes: {0}")]
    ReadEntropy(std::io::Error),
    /// Guest memory access error.
    #[error("guest memory error: {0}")]
    Memory(crate::memory::MemoryError),
    /// Invalid virtqueue descriptor.
    #[error("invalid descriptor: {0}")]
    InvalidDescriptor(String),
}

// ── RngDevice ────────────────────────────────────────────────────────

/// Virtio entropy source device backed by a host entropy file.
///
/// Implements [`VirtioDevice`] for use with the MMIO transport.
/// Reads random bytes from `/dev/urandom` (or a custom source) and
/// delivers them to guest buffers via the virtqueue.
#[derive(Debug)]
#[non_exhaustive]
pub struct RngDevice {
    /// Host entropy source file (typically `/dev/urandom`).
    entropy_source: File,
    /// Feature bits offered by the device.
    avail_features: u64,
    /// Feature bits acknowledged by the driver.
    acked_features: u64,
    /// Virtqueues (rng has 1 receive queue).
    queues: Vec<VirtQueue>,
    /// Whether the device has been activated by the driver.
    activated: bool,
}

impl RngDevice {
    /// Creates a new RNG device using `/dev/urandom` as the entropy source.
    ///
    /// # Errors
    ///
    /// Returns [`RngError::OpenSource`] if `/dev/urandom` cannot be opened.
    pub fn new() -> Result<Self, RngError> {
        Self::with_source(Path::new("/dev/urandom"))
    }

    /// Creates a new RNG device backed by a custom entropy source file.
    ///
    /// # Errors
    ///
    /// Returns [`RngError::OpenSource`] if the file cannot be opened.
    pub fn with_source(path: &Path) -> Result<Self, RngError> {
        let entropy_source = File::open(path).map_err(RngError::OpenSource)?;

        let queues = (0..NUM_QUEUES)
            .map(|_| VirtQueue::new(QUEUE_MAX_SIZE))
            .collect();

        Ok(Self {
            entropy_source,
            avail_features: VIRTIO_F_VERSION_1,
            acked_features: 0,
            queues,
            activated: false,
        })
    }

    /// Processes all pending requests from the given virtqueue.
    ///
    /// Reads the avail ring, walks descriptor chains, fills device-writable
    /// buffers with random bytes, and writes results to the used ring.
    ///
    /// Returns `Ok(true)` if any requests were processed.
    ///
    /// # Errors
    ///
    /// Returns [`RngError`] for fatal errors that affect the entire queue.
    pub fn process_queue(
        &mut self,
        memory: &GuestMemory,
        queue: &mut VirtQueue,
    ) -> Result<bool, RngError> {
        if !queue.ready || queue.size == 0 {
            return Ok(false);
        }

        // Read the current avail ring idx.
        let avail_idx_bytes = memory
            .read_bytes(queue.avail_ring_addr + 2, 2)
            .map_err(RngError::Memory)?;
        let avail_idx = u16::from_le_bytes([avail_idx_bytes[0], avail_idx_bytes[1]]);

        let mut processed = false;

        while queue.last_avail_idx != avail_idx {
            let avail_offset = 4 + u64::from(queue.last_avail_idx % queue.size) * 2;
            let desc_idx_bytes = memory
                .read_bytes(queue.avail_ring_addr + avail_offset, 2)
                .map_err(RngError::Memory)?;
            let head_idx = u16::from_le_bytes([desc_idx_bytes[0], desc_idx_bytes[1]]);

            let written = self.fill_chain(memory, queue, head_idx)?;

            // Write used ring entry.
            let used_offset = 4 + u64::from(queue.last_used_idx % queue.size) * 8;
            let used_addr = queue.used_ring_addr + used_offset;
            let id_bytes = u32::from(head_idx).to_le_bytes();
            let len_bytes = written.to_le_bytes();
            memory
                .write_bytes(used_addr, &id_bytes)
                .map_err(RngError::Memory)?;
            memory
                .write_bytes(used_addr + 4, &len_bytes)
                .map_err(RngError::Memory)?;

            queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
            queue.last_used_idx = queue.last_used_idx.wrapping_add(1);
            processed = true;
        }

        if processed {
            // Update used ring idx.
            let used_idx_bytes = queue.last_used_idx.to_le_bytes();
            memory
                .write_bytes(queue.used_ring_addr + 2, &used_idx_bytes)
                .map_err(RngError::Memory)?;
        }

        Ok(processed)
    }

    /// Reads a single descriptor from the descriptor table in guest memory.
    fn read_desc(memory: &GuestMemory, queue: &VirtQueue, idx: u16) -> Result<VirtqDesc, RngError> {
        if idx >= queue.size {
            return Err(RngError::InvalidDescriptor(format!(
                "descriptor index {idx} >= queue size {}",
                queue.size
            )));
        }
        let addr = queue.desc_table_addr + u64::from(idx) * 16;
        let bytes = memory.read_bytes(addr, 16).map_err(RngError::Memory)?;
        Ok(VirtqDesc {
            addr: u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            len: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            flags: u16::from_le_bytes([bytes[12], bytes[13]]),
            next: u16::from_le_bytes([bytes[14], bytes[15]]),
        })
    }

    /// Fills all device-writable descriptors in a chain with random bytes.
    ///
    /// Returns the total number of bytes written.
    fn fill_chain(
        &mut self,
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
    ) -> Result<u32, RngError> {
        let mut total_written: u32 = 0;
        let mut current_idx = head_idx;
        let mut visited = 0u32;

        loop {
            let desc = Self::read_desc(memory, queue, current_idx)?;
            visited += 1;
            if visited > u32::from(queue.size) {
                return Err(RngError::InvalidDescriptor(
                    "descriptor chain cycle detected".into(),
                ));
            }

            // Only write to device-writable descriptors.
            if desc.flags & VIRTQ_DESC_F_WRITE != 0 {
                let len = (desc.len as usize).min(MAX_BYTES_PER_REQUEST);
                let mut buf = vec![0u8; len];
                self.entropy_source
                    .read_exact(&mut buf)
                    .map_err(RngError::ReadEntropy)?;
                memory
                    .write_bytes(desc.addr, &buf)
                    .map_err(RngError::Memory)?;
                if let Ok(n) = u32::try_from(len) {
                    total_written += n;
                }
            }

            if desc.flags & VIRTQ_DESC_F_NEXT == 0 {
                break;
            }
            current_idx = desc.next;
        }

        Ok(total_written)
    }
}

impl VirtioDevice for RngDevice {
    fn device_type(&self) -> DeviceType {
        DeviceType::Rng
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

    /// Reads device-specific configuration.
    ///
    /// The virtio-rng device has no config space — all reads return zero.
    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        data.fill(0);
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // No config space for rng devices — no-op.
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
        let mut queue_state = queue.clone();
        let result = self.process_queue(memory, &mut queue_state);
        // Write back the updated indices regardless of error.
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
#[path = "rng_test.rs"]
mod tests;
