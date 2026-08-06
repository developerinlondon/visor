//! Virtio MMIO transport implementation.
//!
//! [`MmioTransport`] implements [`BusDevice`] and translates guest MMIO
//! reads/writes into operations on the wrapped [`VirtioDevice`].
//!
//! The register layout follows the virtio 1.x specification (version 2,
//! modern/non-legacy). All registers are 4 bytes wide and little-endian.
//!
//! # Register map (read)
//!
//! | Offset | Name             | Value                              |
//! | ------ | ---------------- | ---------------------------------- |
//! | 0x00   | MagicValue       | `0x74726976` ("virt")               |
//! | 0x04   | Version          | 2                                  |
//! | 0x08   | DeviceID         | device type from backend           |
//! | 0x0C   | VendorID         | 0                                  |
//! | 0x10   | DeviceFeatures   | features page selected by 0x14     |
//! | 0x34   | QueueNumMax      | max size of selected queue         |
//! | 0x44   | QueueReady       | 1 if selected queue is ready       |
//! | 0x60   | InterruptStatus  | pending interrupt flags            |
//! | 0x70   | Status           | device status bits                 |
//! | 0xFC   | ConfigGeneration | config space change counter        |
//! | 0x100+ | Config space     | device-specific configuration      |

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use crate::platform::event::InterruptEvent;

use crate::devices::bus::BusDevice;
use crate::memory::GuestMemory;

use super::{
    DEVICE_STATUS_ACKNOWLEDGE, DEVICE_STATUS_DRIVER, DEVICE_STATUS_DRIVER_OK, DEVICE_STATUS_FAILED,
    DEVICE_STATUS_FEATURES_OK, DEVICE_STATUS_INIT, DeviceType, MMIO_MAGIC, MMIO_VERSION, VENDOR_ID,
    VIRTIO_MMIO_INT_VRING, VirtioDevice,
};

/// Virtio MMIO transport — bridges the MMIO bus to a virtio device backend.
///
/// Wraps an `Arc<Mutex<dyn VirtioDevice>>` and implements [`BusDevice`] so it
/// can be registered on the MMIO bus. The guest driver interacts with the
/// device entirely through MMIO register reads and writes.
#[non_exhaustive]
pub struct MmioTransport {
    /// The wrapped virtio device.
    device: Arc<Mutex<dyn VirtioDevice>>,

    /// Which 32-bit page of device features to expose (0=low, 1=high).
    features_select: u32,

    /// Which 32-bit page of driver-acknowledged features to receive.
    acked_features_select: u32,

    /// Index of the currently selected virtqueue.
    queue_select: u32,

    /// Current device status (bitmask of `DEVICE_STATUS_*` constants).
    device_status: u32,

    /// Monotonically increasing config-space generation counter.
    config_generation: u32,

    /// Pending interrupt flags (atomic for cross-thread signaling).
    interrupt_status: Arc<AtomicU32>,

    /// Guest memory reference for I/O processing during `QueueNotify`.
    memory: Option<Arc<GuestMemory>>,

    /// Interrupt event for injecting IRQs to the guest via irqfd.
    irq_evt: Option<Arc<dyn InterruptEvent>>,

    /// Callback invoked when `InterruptACK` clears all pending bits.
    ///
    /// On macOS HVF, level-triggered GIC SPIs must be explicitly deasserted
    /// when the guest acknowledges the interrupt. On Linux/KVM, irqfd handles
    /// this internally, so the callback is `None`.
    irq_deassert: Option<Arc<dyn Fn() + Send + Sync>>,
}

impl MmioTransport {
    /// Creates a new MMIO transport wrapping the given virtio device.
    #[must_use]
    pub fn new(device: Arc<Mutex<dyn VirtioDevice>>) -> Self {
        Self {
            device,
            features_select: 0,
            acked_features_select: 0,
            queue_select: 0,
            device_status: DEVICE_STATUS_INIT,
            config_generation: 0,
            interrupt_status: Arc::new(AtomicU32::new(0)),
            memory: None,
            irq_evt: None,
            irq_deassert: None,
        }
    }

    /// Returns a clone of the inner device `Arc` for external access.
    #[must_use]
    pub fn device(&self) -> Arc<Mutex<dyn VirtioDevice>> {
        Arc::clone(&self.device)
    }

    /// Returns a clone of the interrupt status `Arc` for external signaling.
    #[must_use]
    pub fn interrupt_status(&self) -> Arc<AtomicU32> {
        Arc::clone(&self.interrupt_status)
    }

    /// Sets interrupt flag bits (OR into the current status).
    ///
    /// This is used by device backends or tests to signal the guest.
    pub fn trigger_interrupt(&self, flags: u32) {
        self.interrupt_status.fetch_or(flags, Ordering::SeqCst);
    }

    /// Sets the guest memory reference for I/O processing.
    pub fn set_memory(&mut self, memory: Arc<GuestMemory>) {
        self.memory = Some(memory);
    }

    /// Sets the IRQ event for signaling the guest after I/O completion.
    pub fn set_irq_evt(&mut self, evt: Arc<dyn InterruptEvent>) {
        self.irq_evt = Some(evt);
    }

    /// Sets the callback invoked when `InterruptACK` clears all pending bits.
    ///
    /// On macOS HVF, this should call `gic_set_spi(intid, false)` to deassert
    /// the level-triggered SPI line. On Linux/KVM, this is not needed.
    pub fn set_irq_deassert(&mut self, callback: Arc<dyn Fn() + Send + Sync>) {
        self.irq_deassert = Some(callback);
    }
    /// Processes pending device events outside the `QueueNotify` path.
    ///
    /// Used for RX-side polling (e.g., network): the run loop calls this
    /// when the backend signals that packets are available, without the
    /// guest having written to `QueueNotify`.
    ///
    /// Returns `true` if any requests were processed and an interrupt was
    /// signaled to the guest.
    #[must_use]
    pub fn process_external_queue(&self, queue_idx: usize) -> bool {
        let Some(ref memory) = self.memory else {
            return false;
        };
        let Ok(mut locked) = self.device.lock() else {
            return false;
        };
        if !locked.is_activated() {
            return false;
        }
        if locked.process_queue(queue_idx, memory).unwrap_or(false) {
            self.interrupt_status
                .fetch_or(VIRTIO_MMIO_INT_VRING, Ordering::SeqCst);
            if let Some(ref irq_evt) = self.irq_evt {
                let _ = irq_evt.trigger();
            }
            tracing::debug!(queue_idx, "process_external_queue: data delivered to guest");
            true
        } else {
            false
        }
    }

    /// Returns `true` if the device status has all bits in `required` set
    /// and none of the bits in `forbidden` set.
    fn check_status(&self, required: u32, forbidden: u32) -> bool {
        self.device_status & (required | forbidden) == required
    }

    /// Applies `f` to the currently selected queue, if it exists.
    /// Returns `default` if the queue index is out of range.
    fn with_queue<U, F>(&self, default: U, f: F) -> U
    where
        F: FnOnce(&super::VirtQueue) -> U,
    {
        let Ok(locked) = self.device.lock() else {
            return default;
        };
        match locked.queues().get(self.queue_select as usize) {
            Some(queue) => f(queue),
            None => default,
        }
    }

    /// Applies `f` to the currently selected queue (mutable), if it exists
    /// and the device is in a state that allows queue configuration
    /// (`FEATURES_OK` set, `DRIVER_OK` and `FAILED` not set).
    fn update_queue_field<F: FnOnce(&mut super::VirtQueue)>(&mut self, f: F) {
        if !self.check_status(
            DEVICE_STATUS_FEATURES_OK,
            DEVICE_STATUS_DRIVER_OK | DEVICE_STATUS_FAILED,
        ) {
            return;
        }
        let Ok(mut locked) = self.device.lock() else {
            return;
        };
        if let Some(queue) = locked.queues_mut().get_mut(self.queue_select as usize) {
            f(queue);
        }
    }

    /// Implements the virtio device status state machine.
    ///
    /// Valid transitions (each step adds bits, never clears):
    /// - `INIT(0) → ACKNOWLEDGE(1)`
    /// - `ACKNOWLEDGE(1) → ACKNOWLEDGE|DRIVER(3)`
    /// - `ACKNOWLEDGE|DRIVER(3) → ACKNOWLEDGE|DRIVER|FEATURES_OK(11)`
    /// - `ACKNOWLEDGE|DRIVER|FEATURES_OK(11) → ...|DRIVER_OK(15)`
    /// - Any state with FAILED bit → sets FAILED
    /// - Writing 0 → full reset
    fn set_device_status(&mut self, status: u32) {
        let device_kind = self.device.lock().ok().map(|d| d.device_type());
        // Identify the device for diagnostic logging.
        let device_type = device_kind.map_or(0, |d| d as u32);
        let mut kick_vsock_rx = false;

        // Writing 0 triggers a full device reset.
        if status == 0 {
            tracing::debug!(device_type, "virtio-mmio: device reset");
            self.reset();
            return;
        }

        // FAILED bit can always be set.
        if (status & DEVICE_STATUS_FAILED) != 0 {
            tracing::debug!(device_type, status, "virtio-mmio: FAILED bit set");
            self.device_status |= DEVICE_STATUS_FAILED;
            return;
        }

        // Check valid transitions based on changed bits.
        let changed = !self.device_status & status;
        tracing::debug!(
            device_type,
            current = self.device_status,
            requested = status,
            changed,
            "virtio-mmio: status transition attempt"
        );
        match changed {
            DEVICE_STATUS_ACKNOWLEDGE if self.device_status == DEVICE_STATUS_INIT => {
                self.device_status = status;
            }
            DEVICE_STATUS_DRIVER if self.device_status == DEVICE_STATUS_ACKNOWLEDGE => {
                self.device_status = status;
            }
            DEVICE_STATUS_FEATURES_OK
                if self.device_status == (DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER) =>
            {
                self.device_status = status;
            }
            DEVICE_STATUS_DRIVER_OK
                if self.device_status
                    == (DEVICE_STATUS_ACKNOWLEDGE
                        | DEVICE_STATUS_DRIVER
                        | DEVICE_STATUS_FEATURES_OK) =>
            {
                self.device_status = status;
                // Activate the device when reaching DRIVER_OK.
                if let Ok(mut locked) = self.device.lock() {
                    tracing::debug!(device_type, "virtio-mmio: activating device");
                    if !locked.is_activated() {
                        if locked.activate().is_err() {
                            tracing::debug!(device_type, "virtio-mmio: activation FAILED");
                            self.device_status |= DEVICE_STATUS_FAILED;
                        } else if matches!(device_kind, Some(DeviceType::Vsock)) {
                            // Host-side vsock data can arrive before the guest
                            // completes virtio-vsock initialization. Drain the RX
                            // queue once on activation so buffered requests are
                            // delivered as soon as descriptors are ready.
                            kick_vsock_rx = true;
                        }
                    }
                }
            }
            _ => {
                tracing::debug!(
                    device_type,
                    current = self.device_status,
                    requested = status,
                    changed,
                    "virtio-mmio: INVALID status transition, ignored"
                );
            }
        }
        if kick_vsock_rx {
            let _ = self.process_external_queue(0);
        }
    }

    /// Resets the transport and the underlying device to initial state.
    fn reset(&mut self) {
        self.features_select = 0;
        self.acked_features_select = 0;
        self.queue_select = 0;
        self.device_status = DEVICE_STATUS_INIT;
        self.interrupt_status.store(0, Ordering::SeqCst);

        if let Ok(mut locked) = self.device.lock() {
            locked.reset();
            // Reset all queues to initial state.
            for queue in locked.queues_mut() {
                queue.reset();
            }
        }
        // config_generation is kept monotonically increasing (not reset).
    }

    /// Reads a register value for MMIO offsets 0x00..0xFF.
    fn read_register(&self, offset: u64) -> u32 {
        let value = match offset {
            0x00 => MMIO_MAGIC,
            0x04 => MMIO_VERSION,
            0x08 => {
                let Ok(locked) = self.device.lock() else {
                    return 0;
                };
                locked.device_type() as u32
            }
            0x0C => VENDOR_ID,
            0x10 => {
                let Ok(locked) = self.device.lock() else {
                    return 0;
                };
                let features = locked.avail_features();
                match self.features_select {
                    0 => (features & 0xFFFF_FFFF) as u32,
                    1 => (features >> 32) as u32,
                    _ => 0,
                }
            }
            0x34 => self.with_queue(0, |q| u32::from(q.max_size)),
            0x44 => self.with_queue(0, |q| u32::from(q.ready)),
            0x60 => self.interrupt_status.load(Ordering::SeqCst),
            0x70 => self.device_status,
            0xFC => self.config_generation,
            _ => 0,
        };
        // Log reads of key registers at trace level for deep debugging.
        if matches!(offset, 0x00 | 0x04 | 0x08 | 0x70) {
            tracing::trace!(
                offset,
                value,
                status = self.device_status,
                "virtio-mmio: read_register"
            );
        }
        value
    }

    /// Handles a write to MMIO register space (0x00..0xFF).
    fn write_register(&mut self, offset: u64, value: u32) {
        // Raw register writes are only useful for deep bring-up work. Keep
        // them at trace so debug logs stay focused on state transitions.
        if matches!(offset, 0x30 | 0x38 | 0x44 | 0x70 | 0x80..=0xA4) {
            let dt = self
                .device
                .lock()
                .ok()
                .map_or(0, |d| d.device_type() as u32);
            tracing::trace!(
                device_type = dt,
                offset = format_args!("0x{offset:02x}"),
                value,
                queue_select = self.queue_select,
                status = self.device_status,
                "virtio-mmio: write_register"
            );
        }
        match offset {
            0x14 => self.features_select = value,
            0x20 if self.check_status(DEVICE_STATUS_DRIVER, DEVICE_STATUS_FEATURES_OK) => {
                // Driver features — only accept in DRIVER state, before FEATURES_OK.
                let Ok(mut locked) = self.device.lock() else {
                    return;
                };
                let page_features = match self.acked_features_select {
                    0 => u64::from(value),
                    1 => u64::from(value) << 32,
                    _ => return,
                };
                // Only ack features that the device actually offers.
                let available = locked.avail_features();
                let already_acked = locked.acked_features();
                let new_acked = already_acked | (page_features & available);
                locked.set_acked_features(new_acked);
            }
            0x24 => self.acked_features_select = value,
            0x30 => self.queue_select = value,
            0x38 => {
                let v = value;
                self.update_queue_field(|q| q.size = (v & 0xFFFF) as u16);
            }
            0x44 => self.update_queue_field(|q| q.ready = value == 1),
            0x50 => {
                // QueueNotify — process I/O for the notified queue.
                if let Some(ref memory) = self.memory {
                    if let Ok(mut locked) = self.device.lock() {
                        let queue_idx = value as usize;
                        let result = locked.process_queue(queue_idx, memory);
                        if result.unwrap_or(false) {
                            self.interrupt_status
                                .fetch_or(VIRTIO_MMIO_INT_VRING, Ordering::SeqCst);
                            if let Some(ref irq_evt) = self.irq_evt {
                                let _ = irq_evt.trigger();
                            }
                        }
                    }
                }
            }
            0x64 if self.check_status(DEVICE_STATUS_DRIVER_OK, 0) => {
                // InterruptACK — clear acknowledged bits.
                let prev = self.interrupt_status.fetch_and(!value, Ordering::SeqCst);
                // If all pending bits are now clear, deassert the level-triggered
                // interrupt line. On macOS HVF this calls gic_set_spi(intid, false).
                if prev & !value == 0 {
                    if let Some(ref deassert) = self.irq_deassert {
                        deassert();
                    }
                }
            }
            0x70 => self.set_device_status(value),
            0x80 => self.update_queue_field(|q| {
                q.desc_table_addr = (q.desc_table_addr & !0xFFFF_FFFF) | u64::from(value);
            }),
            0x84 => self.update_queue_field(|q| {
                q.desc_table_addr = (q.desc_table_addr & 0xFFFF_FFFF) | (u64::from(value) << 32);
            }),
            0x90 => self.update_queue_field(|q| {
                q.avail_ring_addr = (q.avail_ring_addr & !0xFFFF_FFFF) | u64::from(value);
            }),
            0x94 => self.update_queue_field(|q| {
                q.avail_ring_addr = (q.avail_ring_addr & 0xFFFF_FFFF) | (u64::from(value) << 32);
            }),
            0xA0 => self.update_queue_field(|q| {
                q.used_ring_addr = (q.used_ring_addr & !0xFFFF_FFFF) | u64::from(value);
            }),
            0xA4 => self.update_queue_field(|q| {
                q.used_ring_addr = (q.used_ring_addr & 0xFFFF_FFFF) | (u64::from(value) << 32);
            }),
            _ => {
                // Unknown register — ignore.
            }
        }
    }
}

impl BusDevice for MmioTransport {
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        match offset {
            0x00..=0xFF if data.len() == 4 => {
                let value = self.read_register(offset);
                data.copy_from_slice(&value.to_le_bytes());
            }
            0x100..=0xFFF => {
                let Ok(locked) = self.device.lock() else {
                    return;
                };
                locked.read_config(offset - 0x100, data);
            }
            _ => {}
        }
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        match offset {
            0x00..=0xFF if data.len() == 4 => {
                let value = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                self.write_register(offset, value);
            }
            0x100..=0xFFF => {
                let Ok(mut locked) = self.device.lock() else {
                    return;
                };
                locked.write_config(offset - 0x100, data);
            }
            _ => {
                // Invalid write — ignore.
            }
        }
    }
}

impl std::fmt::Debug for MmioTransport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MmioTransport")
            .field("device", &self.device)
            .field("device_status", &self.device_status)
            .field("queue_select", &self.queue_select)
            .field(
                "irq_evt",
                &self.irq_evt.as_ref().map(|_| "<InterruptEvent>"),
            )
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for dyn VirtioDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VirtioDevice")
            .field("device_type", &self.device_type())
            .field("activated", &self.is_activated())
            .finish()
    }
}

#[cfg(test)]
#[path = "mmio_test.rs"]
mod tests;
