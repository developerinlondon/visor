//! Virtio memory balloon device (`virtio-balloon`).
//!
//! Allows the host to reclaim unused guest memory and return it later. The guest
//! driver inflates the balloon (giving pages to the host) or deflates it (taking
//! pages back) in response to host requests via the device configuration.
//!
//! # Queues
//!
//! - Queue 0 (`inflate`): Guest sends page frame numbers (PFNs) to balloon.
//! - Queue 1 (`deflate`): Guest sends PFNs to release from balloon.
//!
//! # Config Space
//!
//! - Offset `0..3`: `num_pages` — number of 4 KiB pages the host wants in the balloon.
//! - Offset `4..7`: `actual` — number of pages the guest has actually inflated.

use std::sync::atomic::{AtomicU32, Ordering};

use crate::memory::GuestMemory;
use crate::transport::{DeviceType, VirtQueue, VirtioDevice, VirtioError};

/// Virtio feature: device supports statistics via virtqueue.
pub const VIRTIO_BALLOON_F_STATS_VQ: u64 = 1 << 1;

/// Virtio feature: modern virtio (version 1).
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// Page size in bytes (4 KiB, as per virtio-balloon spec).
const BALLOON_PAGE_SIZE: usize = 4096;

/// Maximum virtqueue size for balloon queues.
const QUEUE_MAX_SIZE: u16 = 128;

/// Number of virtqueues (inflate + deflate).
const NUM_QUEUES: usize = 2;

/// Inflate queue index.
const INFLATE_QUEUE: usize = 0;

/// Deflate queue index.
const DEFLATE_QUEUE: usize = 1;

// ── Errors ───────────────────────────────────────────────────────────

/// Errors from balloon device operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BalloonError {
    /// Guest memory access error during PFN processing.
    #[error("balloon memory access error: {0}")]
    Memory(#[from] crate::memory::MemoryError),
}

// ── BalloonDevice ────────────────────────────────────────────────────

/// Virtio memory balloon device.
///
/// The host sets [`target_pages`](Self::set_target_pages) to request the guest
/// inflate or deflate the balloon. The guest driver reads the target from the
/// config space and submits PFNs through the inflate/deflate queues.
///
/// Reclaimed pages are tracked by the `inflated_pages` counter. The host can
/// use [`madvise(MADV_DONTNEED)`](libc::MADV_DONTNEED) on the corresponding
/// host addresses to release physical memory back to the OS.
#[derive(Debug)]
#[non_exhaustive]
pub struct BalloonDevice {
    /// Feature bits offered by the device.
    avail_features: u64,
    /// Feature bits acknowledged by the driver.
    acked_features: u64,
    /// Virtqueues (inflate + deflate).
    queues: Vec<VirtQueue>,
    /// Whether the device has been activated by the driver.
    activated: bool,
    /// Target number of 4 KiB pages the host wants in the balloon.
    target_pages: AtomicU32,
    /// Actual number of 4 KiB pages currently in the balloon.
    actual_pages: AtomicU32,
    /// Total pages inflated over the device lifetime (monotonic counter).
    total_inflated: u64,
    /// Total pages deflated over the device lifetime (monotonic counter).
    total_deflated: u64,
}

impl BalloonDevice {
    /// Creates a new balloon device with the given initial target (in 4 KiB pages).
    #[must_use]
    pub fn new(initial_target_pages: u32) -> Self {
        let avail_features = VIRTIO_F_VERSION_1;

        let queues = (0..NUM_QUEUES)
            .map(|_| VirtQueue::new(QUEUE_MAX_SIZE))
            .collect();

        Self {
            avail_features,
            acked_features: 0,
            queues,
            activated: false,
            target_pages: AtomicU32::new(initial_target_pages),
            actual_pages: AtomicU32::new(0),
            total_inflated: 0,
            total_deflated: 0,
        }
    }

    /// Sets the target number of 4 KiB pages the host wants in the balloon.
    ///
    /// The guest driver will observe this via config space and inflate/deflate
    /// accordingly on its next config read.
    pub fn set_target_pages(&self, pages: u32) {
        self.target_pages.store(pages, Ordering::Relaxed);
    }

    /// Returns the target number of 4 KiB pages.
    #[must_use]
    pub fn target_pages(&self) -> u32 {
        self.target_pages.load(Ordering::Relaxed)
    }

    /// Returns the actual number of pages currently in the balloon (as reported by guest).
    #[must_use]
    pub fn actual_pages(&self) -> u32 {
        self.actual_pages.load(Ordering::Relaxed)
    }

    /// Returns the total bytes reclaimed by the balloon (`actual_pages * 4096`).
    #[must_use]
    pub fn reclaimed_bytes(&self) -> u64 {
        u64::from(self.actual_pages()) * BALLOON_PAGE_SIZE as u64
    }

    /// Returns the total number of pages inflated over the device lifetime.
    #[must_use]
    pub fn total_inflated(&self) -> u64 {
        self.total_inflated
    }

    /// Returns the total number of pages deflated over the device lifetime.
    #[must_use]
    pub fn total_deflated(&self) -> u64 {
        self.total_deflated
    }

    /// Processes the inflate queue: guest is giving pages to the balloon.
    ///
    /// Each descriptor contains an array of 4-byte PFNs (page frame numbers).
    /// For each PFN, we could `madvise(MADV_DONTNEED)` on the host address
    /// to release physical memory. For now, we just count them.
    fn process_inflate(&mut self, memory: &GuestMemory) -> Result<bool, BalloonError> {
        let queue = &mut self.queues[INFLATE_QUEUE];
        if !queue.ready || queue.size == 0 {
            return Ok(false);
        }

        let mut processed = false;
        let mut pages_inflated: u32 = 0;

        while let Some((desc_idx, pfn_count)) = next_avail_desc(queue, memory) {
            // Each descriptor holds an array of u32 PFNs
            let desc = read_desc(memory, queue.desc_table_addr, desc_idx)?;
            let pfns = pfn_count.min(desc.len as usize / 4);

            for i in 0..pfns {
                let pfn_addr = desc.addr + (i as u64 * 4);
                let pfn_bytes = memory.read_bytes(pfn_addr, 4)?;
                let pfn =
                    u32::from_le_bytes([pfn_bytes[0], pfn_bytes[1], pfn_bytes[2], pfn_bytes[3]]);

                // Calculate host address for this guest page and advise kernel
                let guest_page_addr = u64::from(pfn) * BALLOON_PAGE_SIZE as u64;
                if let Some(host_ptr) = memory.guest_to_host(guest_page_addr) {
                    // SAFETY: host_ptr points to a valid mmap region owned by GuestMemory.
                    // MADV_DONTNEED tells the kernel the pages are not needed and can be
                    // reclaimed. Future accesses will get zero-filled pages (demand-paged).
                    #[allow(unsafe_code)]
                    unsafe {
                        libc::madvise(host_ptr.cast(), BALLOON_PAGE_SIZE, libc::MADV_DONTNEED);
                    }
                }

                pages_inflated += 1;
            }

            // Add used entry
            add_used(queue, memory, desc_idx, 0)?;
            processed = true;
        }

        if pages_inflated > 0 {
            let new_actual = self.actual_pages().saturating_add(pages_inflated);
            self.actual_pages.store(new_actual, Ordering::Relaxed);
            self.total_inflated += u64::from(pages_inflated);
        }

        Ok(processed)
    }

    /// Processes the deflate queue: guest is taking pages back from the balloon.
    fn process_deflate(&mut self, memory: &GuestMemory) -> Result<bool, BalloonError> {
        let queue = &mut self.queues[DEFLATE_QUEUE];
        if !queue.ready || queue.size == 0 {
            return Ok(false);
        }

        let mut processed = false;
        let mut pages_deflated: u32 = 0;

        while let Some((desc_idx, pfn_count)) = next_avail_desc(queue, memory) {
            let desc = read_desc(memory, queue.desc_table_addr, desc_idx)?;
            let pfns = pfn_count.min(desc.len as usize / 4);
            pages_deflated += u32::try_from(pfns).unwrap_or(u32::MAX);

            // Walk descriptor chain (deflate doesn't need per-PFN processing — pages
            // will be demand-paged back by the guest on next access).
            let _ = desc;

            add_used(queue, memory, desc_idx, 0)?;
            processed = true;
        }

        if pages_deflated > 0 {
            let new_actual = self.actual_pages().saturating_sub(pages_deflated);
            self.actual_pages.store(new_actual, Ordering::Relaxed);
            self.total_deflated += u64::from(pages_deflated);
        }

        Ok(processed)
    }
}

impl VirtioDevice for BalloonDevice {
    fn device_type(&self) -> DeviceType {
        DeviceType::Balloon
    }

    fn avail_features(&self) -> u64 {
        self.avail_features
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, features: u64) {
        self.acked_features = features & self.avail_features;
    }

    fn queues(&self) -> &[VirtQueue] {
        &self.queues
    }

    fn queues_mut(&mut self) -> &mut [VirtQueue] {
        &mut self.queues
    }

    /// Reads balloon config space.
    ///
    /// Layout:
    /// - `[0..4)`: `num_pages` (target) — LE u32
    /// - `[4..8)`: `actual` — LE u32
    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let config = [
            self.target_pages().to_le_bytes(),
            self.actual_pages().to_le_bytes(),
        ]
        .concat();

        let offset = usize::try_from(offset).unwrap_or(usize::MAX);
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = config.get(offset + i).copied().unwrap_or(0);
        }
    }

    /// Writes to balloon config space (guest updates `actual` field).
    fn write_config(&mut self, offset: u64, data: &[u8]) {
        // The guest writes the `actual` field at offset 4.
        if offset == 4 && data.len() >= 4 {
            let actual = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            self.actual_pages.store(actual, Ordering::Relaxed);
        }
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
        self.acked_features = 0;
        for queue in &mut self.queues {
            queue.reset();
        }
    }

    fn process_queue(
        &mut self,
        queue_idx: usize,
        memory: &GuestMemory,
    ) -> Result<bool, VirtioError> {
        match queue_idx {
            INFLATE_QUEUE => self
                .process_inflate(memory)
                .map_err(|_| VirtioError::ActivationFailed),
            DEFLATE_QUEUE => self
                .process_deflate(memory)
                .map_err(|_| VirtioError::ActivationFailed),
            _ => Ok(false),
        }
    }
}

// ── Virtqueue helper functions ─────────────────────────────────────────

/// Descriptor layout from memory.
#[derive(Debug, Clone, Copy)]
struct DescInfo {
    addr: u64,
    len: u32,
}

/// Reads a virtqueue descriptor from guest memory.
fn read_desc(
    memory: &GuestMemory,
    desc_table_addr: u64,
    idx: u16,
) -> Result<DescInfo, BalloonError> {
    let desc_addr = desc_table_addr + u64::from(idx) * 16;
    // Read only addr (8 bytes) and len (4 bytes) — we don't need flags/next
    // for balloon PFN arrays (single-descriptor chains).
    let bytes = memory.read_bytes(desc_addr, 12)?;

    Ok(DescInfo {
        addr: u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]),
        len: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
    })
}

/// Returns the next available descriptor index and estimated PFN count from the avail ring.
fn next_avail_desc(queue: &mut VirtQueue, memory: &GuestMemory) -> Option<(u16, usize)> {
    // Read avail_idx from the avail ring (offset 2 from avail_ring_addr).
    let avail_idx_bytes = memory.read_bytes(queue.avail_ring_addr + 2, 2).ok()?;
    let avail_idx = u16::from_le_bytes([avail_idx_bytes[0], avail_idx_bytes[1]]);

    if queue.last_avail_idx == avail_idx {
        return None;
    }

    // Read the descriptor index from the avail ring entry.
    let ring_entry_offset = 4 + u64::from(queue.last_avail_idx % queue.size) * 2;
    let entry_bytes = memory
        .read_bytes(queue.avail_ring_addr + ring_entry_offset, 2)
        .ok()?;
    let desc_idx = u16::from_le_bytes([entry_bytes[0], entry_bytes[1]]);

    // Read the descriptor to estimate PFN count.
    let desc = read_desc(memory, queue.desc_table_addr, desc_idx).ok()?;
    let pfn_count = desc.len as usize / 4;

    queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);

    Some((desc_idx, pfn_count))
}

/// Adds an entry to the used ring.
fn add_used(
    queue: &mut VirtQueue,
    memory: &GuestMemory,
    desc_idx: u16,
    len: u32,
) -> Result<(), BalloonError> {
    let used_idx_offset = queue.used_ring_addr + 2;
    let used_idx_bytes = memory.read_bytes(used_idx_offset, 2)?;
    let used_idx = u16::from_le_bytes([used_idx_bytes[0], used_idx_bytes[1]]);

    // Write used element: id (u32) + len (u32) = 8 bytes
    let ring_offset = queue.used_ring_addr + 4 + u64::from(used_idx % queue.size) * 8;
    let mut elem = [0u8; 8];
    elem[..4].copy_from_slice(&u32::from(desc_idx).to_le_bytes());
    elem[4..].copy_from_slice(&len.to_le_bytes());
    memory.write_bytes(ring_offset, &elem)?;

    // Increment used_idx
    let new_used_idx = used_idx.wrapping_add(1);
    memory.write_bytes(used_idx_offset, &new_used_idx.to_le_bytes())?;

    queue.last_used_idx = new_used_idx;

    Ok(())
}

#[cfg(test)]
#[path = "balloon_test.rs"]
mod tests;
