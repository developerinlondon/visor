//! Virtio transport layer for bridging the MMIO bus to virtio device backends.
//!
//! This module defines the [`VirtioDevice`] trait that all virtio backends
//! (block, vsock, net) must implement, and the [`MmioTransport`](mmio::MmioTransport)
//! that exposes them as [`BusDevice`](crate::devices::bus::BusDevice)
//! instances on the MMIO bus.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────┐    MMIO read/write    ┌───────────────┐    VirtioDevice    ┌──────────┐
//! │  Guest   │──────────────────────>│ MmioTransport │───────────────────>│  Block / │
//! │  vCPU    │                       │ (BusDevice)   │                    │  Vsock / │
//! └──────────┘                       └───────────────┘                    │  Net     │
//!                                                                        └──────────┘
//! ```

pub mod mmio;
pub mod pci;
pub mod pci_bus;

/// Virtio MMIO magic value (`virt` in little-endian).
pub const MMIO_MAGIC: u32 = 0x7472_6976;

/// Virtio MMIO version (2 = modern/non-legacy).
pub const MMIO_VERSION: u32 = 2;

/// Vendor ID (0 = unspecified, as per virtio spec for MMIO).
pub const VENDOR_ID: u32 = 0;

// ── Interrupt flags ──────────────────────────────────────────────────

/// Interrupt flag: used vring buffers available.
pub const VIRTIO_MMIO_INT_VRING: u32 = 0x01;

/// Interrupt flag: device configuration changed.
pub const VIRTIO_MMIO_INT_CONFIG: u32 = 0x02;

// ── Device status bits ───────────────────────────────────────────────

/// Initial status after reset.
pub const DEVICE_STATUS_INIT: u32 = 0;

/// Guest OS has found the device and recognized it as a valid virtio device.
pub const DEVICE_STATUS_ACKNOWLEDGE: u32 = 1;

/// Guest OS knows how to drive the device.
pub const DEVICE_STATUS_DRIVER: u32 = 2;

/// Driver has acknowledged all the features it understands.
pub const DEVICE_STATUS_FEATURES_OK: u32 = 8;

/// Driver is set up and ready to drive the device.
pub const DEVICE_STATUS_DRIVER_OK: u32 = 4;

/// Something went wrong with the device.
pub const DEVICE_STATUS_FAILED: u32 = 128;

// ── Device types ─────────────────────────────────────────────────────

/// Virtio device type identifiers per the virtio specification.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum DeviceType {
    /// Network interface (virtio-net).
    Net = 1,
    /// Block device (virtio-blk).
    Block = 2,
    /// Console device.
    Console = 3,
    /// Entropy source (virtio-rng).
    Rng = 4,
    /// Memory balloon.
    Balloon = 5,
    /// Socket device (virtio-vsock).
    Vsock = 19,
    /// Filesystem device (virtio-fs).
    Fs = 26,
}

// ── Errors ───────────────────────────────────────────────────────────

/// Errors from virtio device operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VirtioError {
    /// Device activation failed.
    #[error("virtio device activation failed")]
    ActivationFailed,
}

// ── Virtqueue descriptor flags ───────────────────────────────────────

/// Descriptor has a `next` field linking to another descriptor.
pub const VIRTQ_DESC_F_NEXT: u16 = 0x1;

/// Descriptor buffer is device-writable (vs device-readable).
pub const VIRTQ_DESC_F_WRITE: u16 = 0x2;

// ── Virtqueue memory layout structs ─────────────────────────────────

/// Virtqueue descriptor (16 bytes, matches virtio spec 2.7.5).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqDesc {
    /// Guest physical address of the buffer.
    pub addr: u64,
    /// Buffer length in bytes.
    pub len: u32,
    /// Descriptor flags (`VIRTQ_DESC_F_NEXT`, `VIRTQ_DESC_F_WRITE`).
    pub flags: u16,
    /// Next descriptor index (valid when `VIRTQ_DESC_F_NEXT` is set).
    pub next: u16,
}

/// Used ring element (8 bytes, matches virtio spec 2.7.8).
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct VirtqUsedElem {
    /// Descriptor chain head index.
    pub id: u32,
    /// Total bytes written to device-writable descriptors.
    pub len: u32,
}

// ── VirtQueue ────────────────────────────────────────────────────────

/// Virtqueue state for the MMIO transport.
///
/// Holds the configuration written by the guest driver (sizes, addresses)
/// and runtime state (avail/used indices) for I/O processing.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VirtQueue {
    /// Maximum queue size supported by the device.
    pub max_size: u16,
    /// Queue size configured by the driver (must be ≤ `max_size`).
    pub size: u16,
    /// Whether the queue has been marked ready by the driver.
    pub ready: bool,
    /// Guest-physical address of the descriptor table.
    pub desc_table_addr: u64,
    /// Guest-physical address of the available ring.
    pub avail_ring_addr: u64,
    /// Guest-physical address of the used ring.
    pub used_ring_addr: u64,
    /// Tracks which avail ring entry we have consumed up to (wrapping counter).
    pub last_avail_idx: u16,
    /// Tracks the next used ring entry to write (wrapping counter).
    pub last_used_idx: u16,
}

impl VirtQueue {
    /// Creates a new virtqueue with the given maximum size.
    ///
    /// All addresses and indices are initialized to zero and the queue starts not-ready.
    #[must_use]
    pub fn new(max_size: u16) -> Self {
        Self {
            max_size,
            size: 0,
            ready: false,
            desc_table_addr: 0,
            avail_ring_addr: 0,
            used_ring_addr: 0,
            last_avail_idx: 0,
            last_used_idx: 0,
        }
    }

    /// Resets the queue to its initial state, preserving `max_size`.
    pub fn reset(&mut self) {
        self.size = 0;
        self.ready = false;
        self.desc_table_addr = 0;
        self.avail_ring_addr = 0;
        self.used_ring_addr = 0;
        self.last_avail_idx = 0;
        self.last_used_idx = 0;
    }
}

// ── VirtioDevice trait ───────────────────────────────────────────────

/// Trait that all virtio device backends must implement.
///
/// The MMIO transport calls these methods to negotiate features, configure
/// queues, and activate/reset the device on behalf of the guest driver.
pub trait VirtioDevice: Send {
    /// Returns the virtio device type identifier.
    fn device_type(&self) -> DeviceType;

    /// Returns the full set of feature bits the device offers.
    fn avail_features(&self) -> u64;

    /// Returns the feature bits acknowledged by the driver.
    fn acked_features(&self) -> u64;

    /// Sets the feature bits acknowledged by the driver.
    ///
    /// The transport ensures only bits from [`avail_features`](Self::avail_features)
    /// can be acknowledged.
    fn set_acked_features(&mut self, features: u64);

    /// Returns the device's virtqueues.
    fn queues(&self) -> &[VirtQueue];

    /// Returns a mutable reference to the device's virtqueues.
    fn queues_mut(&mut self) -> &mut [VirtQueue];

    /// Reads device-specific configuration at `offset` into `data`.
    fn read_config(&self, offset: u64, data: &mut [u8]);

    /// Writes device-specific configuration at `offset` from `data`.
    fn write_config(&mut self, offset: u64, data: &[u8]);

    /// Activates the device after the driver has completed initialization.
    ///
    /// # Errors
    ///
    /// Returns [`VirtioError`] if the device cannot be activated.
    fn activate(&mut self) -> Result<(), VirtioError>;

    /// Returns whether the device has been activated.
    fn is_activated(&self) -> bool;

    /// Resets the device to its initial state.
    fn reset(&mut self);

    /// Processes pending I/O for the given queue index.
    ///
    /// Called by the MMIO transport when the guest writes to `QueueNotify`.
    /// Returns `Ok(true)` if any requests were processed (an interrupt
    /// should be injected), `Ok(false)` if the queue was empty.
    ///
    /// The default implementation does nothing and returns `Ok(false)`.
    ///
    /// # Errors
    ///
    /// Returns [`VirtioError`] if a fatal device error occurs.
    fn process_queue(
        &mut self,
        _queue_idx: usize,
        _memory: &crate::memory::GuestMemory,
    ) -> Result<bool, VirtioError> {
        Ok(false)
    }
}
