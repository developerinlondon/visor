//! Lock-free SPSC ring buffer in shared memory for inter-VM networking.
//!
//! Uses monotonically increasing byte indices and Acquire/Release atomics.
//! Packets are stored as: [u32 length][payload][padding to 8-byte align].

#![allow(unsafe_code)]

use std::sync::atomic::{AtomicU64, Ordering};

/// Ring buffer error types.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RingError {
    #[error("ring buffer capacity {size} too small (minimum {min})")]
    TooSmall { size: usize, min: usize },
    #[error("packet size {size} exceeds maximum {max}")]
    PacketTooLarge { size: usize, max: usize },
    #[error("mmap failed: {0}")]
    Mmap(#[from] crate::shared_memory::SharedMemoryError),
}

/// Header in shared memory. Cache-line aligned to prevent false sharing.
#[repr(C, align(64))]
pub struct RingHeader {
    write_idx: AtomicU64,
    _pad0: [u8; 56],
    read_idx: AtomicU64,
    _pad1: [u8; 56],
    capacity: u64,
    _pad2: [u8; 56],
}

/// Size of the ring header in bytes.
pub const HEADER_SIZE: usize = std::mem::size_of::<RingHeader>();
/// Minimum ring capacity (64 KB).
pub const MIN_CAPACITY: usize = 64 * 1024;
/// Maximum single packet size (jumbo frame).
pub const MAX_PACKET_SIZE: usize = 9014;
/// Default ring capacity (2 MB).
pub const DEFAULT_CAPACITY: usize = 2 * 1024 * 1024;
/// Entry header size (u32 length field).
const ENTRY_HEADER: usize = 4;
/// Alignment for entries.
const ENTRY_ALIGN: usize = 8;
/// Skip marker length (`0xFFFF_FFFF` indicates wraparound).
const SKIP_MARKER: u32 = 0xFFFF_FFFF;

/// Producer side — writes packets into the ring.
pub struct RingProducer {
    header: *mut RingHeader,
    data: *mut u8,
    capacity: usize,
}

/// Consumer side — reads packets from the ring.
pub struct RingConsumer {
    header: *const RingHeader,
    data: *const u8,
    capacity: usize,
}

// SAFETY: The raw pointers point to mmap'd shared memory that
// outlives both sides. Only one producer and one consumer exist.
unsafe impl Send for RingProducer {}
unsafe impl Send for RingConsumer {}

impl RingProducer {
    /// Create from raw mmap pointer. ptr must point to `HEADER_SIZE` + capacity bytes.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - `ptr` is valid and points to at least `HEADER_SIZE` + capacity bytes
    /// - The memory is mmap'd with `MAP_SHARED`
    /// - Only one `RingProducer` is created from this pointer
    /// - The memory outlives the `RingProducer`
    ///
    /// # Errors
    ///
    /// Returns `RingError::TooSmall` if capacity is less than `MIN_CAPACITY`.
    pub unsafe fn from_raw(ptr: *mut u8, capacity: usize) -> Result<Self, RingError> {
        if capacity < MIN_CAPACITY {
            return Err(RingError::TooSmall {
                size: capacity,
                min: MIN_CAPACITY,
            });
        }

        // SAFETY: Caller guarantees ptr is valid and points to HEADER_SIZE + capacity bytes.
        #[allow(clippy::cast_ptr_alignment)]
        let header = ptr.cast::<RingHeader>();
        unsafe {
            (*header).capacity = capacity as u64;
            (*header).write_idx.store(0, Ordering::Release);
            (*header).read_idx.store(0, Ordering::Release);
        }

        let data = unsafe { ptr.add(HEADER_SIZE) };

        Ok(Self {
            header,
            data,
            capacity,
        })
    }

    /// Try to send a packet. Returns false if ring is full.
    #[must_use]
    pub fn try_send(&self, packet: &[u8]) -> bool {
        if packet.len() > MAX_PACKET_SIZE {
            return false;
        }

        // SAFETY: header is valid and initialized in from_raw.
        let header = unsafe { &*self.header };

        let entry_size = align_up(ENTRY_HEADER + packet.len(), ENTRY_ALIGN);
        let write_idx = header.write_idx.load(Ordering::Acquire);
        let read_idx = header.read_idx.load(Ordering::Acquire);

        #[allow(clippy::cast_possible_truncation)]
        let write_pos = (write_idx % self.capacity as u64) as usize;
        #[allow(clippy::cast_possible_truncation)]
        let used = (write_idx - read_idx) as usize;
        let available = self.capacity - used;

        // Check if entry fits at current position
        if write_pos + entry_size <= self.capacity {
            if available >= entry_size {
                // Write the packet
                self.write_entry(write_idx, packet);
                header
                    .write_idx
                    .store(write_idx + entry_size as u64, Ordering::Release);
                return true;
            }
        } else {
            // Would wrap around. Check if we can write skip marker + entry at start.
            let gap = self.capacity - write_pos;
            let total_needed = gap + entry_size;

            if available >= total_needed {
                // Write skip marker to fill to end
                self.write_skip_marker(write_idx);
                let new_write_idx = write_idx + (self.capacity - write_pos) as u64;

                // Write packet at start
                self.write_entry(new_write_idx, packet);
                header
                    .write_idx
                    .store(new_write_idx + entry_size as u64, Ordering::Release);
                return true;
            }
        }

        false
    }

    /// Available space in bytes.
    #[must_use]
    pub fn available_space(&self) -> usize {
        // SAFETY: header is valid and initialized in from_raw.
        let header = unsafe { &*self.header };
        let write_idx = header.write_idx.load(Ordering::Acquire);
        let read_idx = header.read_idx.load(Ordering::Acquire);

        #[allow(clippy::cast_possible_truncation)]
        let used = (write_idx - read_idx) as usize;
        self.capacity - used
    }

    fn write_entry(&self, idx: u64, packet: &[u8]) {
        #[allow(clippy::cast_possible_truncation)]
        let pos = (idx % self.capacity as u64) as usize;
        #[allow(clippy::cast_possible_truncation)]
        let len = packet.len() as u32;

        // SAFETY: pos is always < capacity, and we've checked space in try_send.
        unsafe {
            #[allow(clippy::cast_ptr_alignment)]
            let len_ptr = self.data.add(pos).cast::<u32>();
            *len_ptr = len;
            let payload_ptr = self.data.add(pos + ENTRY_HEADER);
            std::ptr::copy_nonoverlapping(packet.as_ptr(), payload_ptr, packet.len());
        }
    }

    fn write_skip_marker(&self, idx: u64) {
        #[allow(clippy::cast_possible_truncation)]
        let pos = (idx % self.capacity as u64) as usize;

        // SAFETY: pos is always < capacity.
        unsafe {
            #[allow(clippy::cast_ptr_alignment)]
            let len_ptr = self.data.add(pos).cast::<u32>();
            *len_ptr = SKIP_MARKER;
        }
    }
}

impl RingConsumer {
    /// Create from raw mmap pointer. ptr must point to `HEADER_SIZE` + capacity bytes.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - `ptr` is valid and points to at least `HEADER_SIZE` + capacity bytes
    /// - The memory is mmap'd with `MAP_SHARED`
    /// - Only one `RingConsumer` is created from this pointer
    /// - The memory outlives the `RingConsumer`
    ///
    /// # Errors
    ///
    /// Returns `RingError::TooSmall` if capacity is less than `MIN_CAPACITY`.
    pub unsafe fn from_raw(ptr: *mut u8, capacity: usize) -> Result<Self, RingError> {
        if capacity < MIN_CAPACITY {
            return Err(RingError::TooSmall {
                size: capacity,
                min: MIN_CAPACITY,
            });
        }

        #[allow(clippy::cast_ptr_alignment)]
        let header = ptr.cast::<RingHeader>();
        let data = unsafe { ptr.add(HEADER_SIZE) };

        Ok(Self {
            header,
            data,
            capacity,
        })
    }

    /// Try to receive a packet. Returns the number of bytes read into buf, or None if empty.
    pub fn try_recv(&self, buf: &mut [u8]) -> Option<usize> {
        // SAFETY: header is valid and initialized in from_raw.
        let header = unsafe { &*self.header };

        let read_idx = header.read_idx.load(Ordering::Acquire);
        let write_idx = header.write_idx.load(Ordering::Acquire);

        if read_idx >= write_idx {
            return None;
        }

        #[allow(clippy::cast_possible_truncation)]
        let read_pos = (read_idx % self.capacity as u64) as usize;

        // SAFETY: read_pos is always < capacity.
        let len = unsafe {
            #[allow(clippy::cast_ptr_alignment)]
            let len_ptr = self.data.add(read_pos).cast::<u32>();
            *len_ptr
        };

        if len == SKIP_MARKER {
            // Skip to next aligned position at start
            let _skip_size = align_up(ENTRY_HEADER, ENTRY_ALIGN);
            let new_read_idx = read_idx + (self.capacity - read_pos) as u64;
            header.read_idx.store(new_read_idx, Ordering::Release);
            // Recursively try to read from the start
            return self.try_recv(buf);
        }

        let len = len as usize;
        if len > buf.len() {
            return None;
        }

        // SAFETY: read_pos + ENTRY_HEADER is always < capacity + ENTRY_HEADER.
        // We've verified len fits in buf.
        unsafe {
            let payload_ptr = self.data.add(read_pos + ENTRY_HEADER);
            std::ptr::copy_nonoverlapping(payload_ptr, buf.as_mut_ptr(), len);
        }

        let entry_size = align_up(ENTRY_HEADER + len, ENTRY_ALIGN);
        header
            .read_idx
            .store(read_idx + entry_size as u64, Ordering::Release);

        Some(len)
    }

    /// Check if there is data available to read.
    #[must_use]
    pub fn has_data(&self) -> bool {
        // SAFETY: header is valid and initialized in from_raw.
        let header = unsafe { &*self.header };
        let read_idx = header.read_idx.load(Ordering::Acquire);
        let write_idx = header.write_idx.load(Ordering::Acquire);

        read_idx < write_idx
    }
}

/// Create a producer-consumer pair from a `SharedMemoryRegion`.
///
/// # Errors
///
/// Returns `RingError::TooSmall` if the shared memory region is too small.
pub fn create_pair(
    shm: &crate::shared_memory::SharedMemoryRegion,
) -> Result<(RingProducer, RingConsumer), RingError> {
    let capacity = shm.size() - HEADER_SIZE;

    // SAFETY: shm.as_ptr() is valid and points to shm.size() bytes.
    // The memory is mmap'd with MAP_SHARED.
    unsafe {
        let producer = RingProducer::from_raw(shm.as_ptr(), capacity)?;
        let consumer = RingConsumer::from_raw(shm.as_ptr(), capacity)?;
        Ok((producer, consumer))
    }
}

/// Align a value up to the nearest multiple of align.
fn align_up(value: usize, align: usize) -> usize {
    (value + align - 1) & !(align - 1)
}

#[cfg(test)]
#[path = "shared_ring_test.rs"]
mod tests;
