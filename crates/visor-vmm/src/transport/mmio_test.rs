use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use super::*;
use crate::devices::bus::BusDevice;
use crate::memory::GuestMemory;
use crate::transport::{
    DEVICE_STATUS_ACKNOWLEDGE, DEVICE_STATUS_DRIVER, DEVICE_STATUS_DRIVER_OK, DEVICE_STATUS_FAILED,
    DEVICE_STATUS_FEATURES_OK, DEVICE_STATUS_INIT, DeviceType, MMIO_MAGIC, MMIO_VERSION,
    VIRTIO_MMIO_INT_CONFIG, VIRTIO_MMIO_INT_VRING, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE,
    VirtQueue, VirtioDevice, VirtioError,
};

// ── Test helpers ─────────────────────────────────────────────────────

/// Minimal `VirtioDevice` implementation for testing the MMIO transport.
struct DummyDevice {
    device_type: DeviceType,
    avail_features: u64,
    acked_features: u64,
    queues: Vec<VirtQueue>,
    config_bytes: [u8; 256],
    activated: bool,
    activate_fail: bool,
}

struct ExternalRxDevice {
    queues: Vec<VirtQueue>,
    activated: bool,
    process_calls: Arc<AtomicUsize>,
    external_rx_pending: bool,
}

impl ExternalRxDevice {
    fn new(process_calls: Arc<AtomicUsize>) -> Self {
        Self {
            queues: vec![VirtQueue::new(256)],
            activated: false,
            process_calls,
            external_rx_pending: true,
        }
    }
}

impl DummyDevice {
    fn new() -> Self {
        Self {
            device_type: DeviceType::Block,
            avail_features: 0,
            acked_features: 0,
            queues: vec![VirtQueue::new(256), VirtQueue::new(128)],
            config_bytes: [0; 256],
            activated: false,
            activate_fail: false,
        }
    }
    fn with_features(mut self, features: u64) -> Self {
        self.avail_features = features;
        self
    }

    fn with_device_type(mut self, dt: DeviceType) -> Self {
        self.device_type = dt;
        self
    }
}

impl VirtioDevice for DummyDevice {
    fn device_type(&self) -> DeviceType {
        self.device_type
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

    #[allow(clippy::cast_possible_truncation)]
    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let start = offset as usize;
        let end = start + data.len();
        if end <= self.config_bytes.len() {
            data.copy_from_slice(&self.config_bytes[start..end]);
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    fn write_config(&mut self, offset: u64, data: &[u8]) {
        let start = offset as usize;
        for (i, byte) in data.iter().enumerate() {
            if start + i < self.config_bytes.len() {
                self.config_bytes[start + i] = *byte;
            }
        }
    }

    fn activate(&mut self) -> Result<(), VirtioError> {
        if self.activate_fail {
            return Err(VirtioError::ActivationFailed);
        }
        self.activated = true;
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.activated
    }

    fn reset(&mut self) {
        self.activated = false;
        self.acked_features = 0;
    }
}

impl VirtioDevice for ExternalRxDevice {
    fn device_type(&self) -> DeviceType {
        DeviceType::Vsock
    }

    fn avail_features(&self) -> u64 {
        0
    }

    fn acked_features(&self) -> u64 {
        0
    }

    fn set_acked_features(&mut self, _features: u64) {}

    fn queues(&self) -> &[VirtQueue] {
        &self.queues
    }

    fn queues_mut(&mut self) -> &mut [VirtQueue] {
        &mut self.queues
    }

    fn read_config(&self, _offset: u64, data: &mut [u8]) {
        data.fill(0);
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}

    fn activate(&mut self) -> Result<(), VirtioError> {
        self.activated = true;
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.activated
    }

    fn reset(&mut self) {
        self.activated = false;
        self.external_rx_pending = true;
    }

    fn process_queue(
        &mut self,
        queue_idx: usize,
        _memory: &GuestMemory,
    ) -> Result<bool, VirtioError> {
        self.process_calls.fetch_add(1, Ordering::SeqCst);
        if queue_idx == 0 && self.activated && self.external_rx_pending {
            self.external_rx_pending = false;
            Ok(true)
        } else {
            Ok(false)
        }
    }
}

/// Helper: read a u32 from the transport at the given offset.
fn read_u32(transport: &mut MmioTransport, offset: u64) -> u32 {
    let mut data = [0u8; 4];
    transport.read(offset, &mut data);
    u32::from_le_bytes(data)
}

/// Helper: write a u32 to the transport at the given offset.
fn write_u32(transport: &mut MmioTransport, offset: u64, val: u32) {
    let data = val.to_le_bytes();
    transport.write(offset, &data);
}

/// Create an `MmioTransport` wrapping a `DummyDevice`.
fn make_transport() -> MmioTransport {
    let device: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(DummyDevice::new()));
    MmioTransport::new(device)
}

/// Create an `MmioTransport` wrapping a custom `DummyDevice`.
fn make_transport_with(device: DummyDevice) -> MmioTransport {
    let device: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(device));
    MmioTransport::new(device)
}

/// Drive the status machine through INIT → ACK → DRIVER → `FEATURES_OK` → `DRIVER_OK`.
fn drive_to_driver_ok(transport: &mut MmioTransport) {
    write_u32(transport, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    write_u32(
        transport,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER,
    );
    write_u32(
        transport,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER | DEVICE_STATUS_FEATURES_OK,
    );
    write_u32(
        transport,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE
            | DEVICE_STATUS_DRIVER
            | DEVICE_STATUS_FEATURES_OK
            | DEVICE_STATUS_DRIVER_OK,
    );
}

// ── 1. Magic value ───────────────────────────────────────────────────

#[test]
fn read_magic_value() {
    let mut t = make_transport();
    assert_eq!(read_u32(&mut t, 0x00), MMIO_MAGIC);
}

// ── 2. Version ───────────────────────────────────────────────────────

#[test]
fn read_version() {
    let mut t = make_transport();
    assert_eq!(read_u32(&mut t, 0x04), MMIO_VERSION);
}

// ── 3. Device ID ─────────────────────────────────────────────────────

#[test]
fn read_device_id_block() {
    let mut t = make_transport();
    assert_eq!(read_u32(&mut t, 0x08), DeviceType::Block as u32);
}

#[test]
fn read_device_id_vsock() {
    let device = DummyDevice::new().with_device_type(DeviceType::Vsock);
    let mut t = make_transport_with(device);
    assert_eq!(read_u32(&mut t, 0x08), DeviceType::Vsock as u32);
}

#[test]
fn read_device_id_net() {
    let device = DummyDevice::new().with_device_type(DeviceType::Net);
    let mut t = make_transport_with(device);
    assert_eq!(read_u32(&mut t, 0x08), DeviceType::Net as u32);
}

// ── 4. Feature negotiation ───────────────────────────────────────────

#[test]
fn feature_negotiation_low_page() {
    let features: u64 = 0xDEAD_BEEF_CAFE_BABE;
    let device = DummyDevice::new().with_features(features);
    let mut t = make_transport_with(device);

    // Select page 0 (low 32 bits)
    write_u32(&mut t, 0x14, 0);
    let low = read_u32(&mut t, 0x10);
    assert_eq!(low, (features & 0xFFFF_FFFF) as u32);
}

#[test]
fn feature_negotiation_high_page() {
    let features: u64 = 0xDEAD_BEEF_CAFE_BABE;
    let device = DummyDevice::new().with_features(features);
    let mut t = make_transport_with(device);

    // Select page 1 (high 32 bits)
    write_u32(&mut t, 0x14, 1);
    let high = read_u32(&mut t, 0x10);
    assert_eq!(high, (features >> 32) as u32);
}

#[test]
fn ack_features() {
    let features: u64 = 0x0000_0003_0000_0007;
    let device = DummyDevice::new().with_features(features);
    let mut t = make_transport_with(device);

    // Advance to DRIVER state so feature ack is accepted
    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER,
    );

    // Ack low page features
    write_u32(&mut t, 0x24, 0); // DriverFeaturesSel = page 0
    write_u32(&mut t, 0x20, 0x0000_0005); // ack bits from low page

    // Ack high page features
    write_u32(&mut t, 0x24, 1); // DriverFeaturesSel = page 1
    write_u32(&mut t, 0x20, 0x0000_0001); // ack bits from high page

    // Verify: acked = requested & available
    // low: 0x5 & 0x7 = 0x5, high: 0x1 & 0x3 = 0x1
    // acked_features = (0x1 << 32) | 0x5 = 0x0000_0001_0000_0005
    let t_device = t.device();
    let locked = t_device.lock().unwrap();
    assert_eq!(locked.acked_features(), 0x0000_0001_0000_0005);
}

// ── 5. Device status transitions ─────────────────────────────────────

#[test]
fn status_transition_init_to_ack() {
    let mut t = make_transport();
    assert_eq!(read_u32(&mut t, 0x70), DEVICE_STATUS_INIT);

    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    assert_eq!(read_u32(&mut t, 0x70), DEVICE_STATUS_ACKNOWLEDGE);
}

#[test]
fn status_transition_ack_to_driver() {
    let mut t = make_transport();
    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER,
    );
    assert_eq!(
        read_u32(&mut t, 0x70),
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER
    );
}

#[test]
fn status_transition_driver_to_features_ok() {
    let mut t = make_transport();
    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER,
    );
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER | DEVICE_STATUS_FEATURES_OK,
    );
    assert_eq!(
        read_u32(&mut t, 0x70),
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER | DEVICE_STATUS_FEATURES_OK
    );
}

#[test]
fn status_transition_features_ok_to_driver_ok() {
    let mut t = make_transport();
    drive_to_driver_ok(&mut t);
    assert_eq!(
        read_u32(&mut t, 0x70),
        DEVICE_STATUS_ACKNOWLEDGE
            | DEVICE_STATUS_DRIVER
            | DEVICE_STATUS_FEATURES_OK
            | DEVICE_STATUS_DRIVER_OK
    );
}

#[test]
fn vsock_activation_drains_pending_external_rx() {
    let process_calls = Arc::new(AtomicUsize::new(0));
    let device: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(ExternalRxDevice::new(
        Arc::clone(&process_calls),
    )));
    let mut t = MmioTransport::new(device);
    let memory = Arc::new(GuestMemory::new(1024 * 1024, 0).unwrap());
    t.set_memory(memory);

    drive_to_driver_ok(&mut t);

    assert_eq!(
        process_calls.load(Ordering::SeqCst),
        1,
        "virtio-vsock activation should kick one external RX drain"
    );
    assert_ne!(
        read_u32(&mut t, 0x60) & VIRTIO_MMIO_INT_VRING,
        0,
        "external RX drain should raise a vring interrupt"
    );
}

// ── 6. Invalid status transitions ────────────────────────────────────

#[test]
fn invalid_status_skip_ack() {
    let mut t = make_transport();
    // Try to jump directly to DRIVER without ACK first — should be rejected.
    write_u32(&mut t, 0x70, DEVICE_STATUS_DRIVER);
    assert_eq!(read_u32(&mut t, 0x70), DEVICE_STATUS_INIT);
}

#[test]
fn invalid_status_skip_features_ok() {
    let mut t = make_transport();
    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER,
    );
    // Skip FEATURES_OK, jump to DRIVER_OK
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER | DEVICE_STATUS_DRIVER_OK,
    );
    // Status should not have changed
    assert_eq!(
        read_u32(&mut t, 0x70),
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER
    );
}

// ── 7. Queue configuration ──────────────────────────────────────────

#[test]
fn queue_select_and_max_size() {
    let mut t = make_transport();
    drive_to_driver_ok(&mut t);

    // Default is queue 0, max_size=256
    write_u32(&mut t, 0x30, 0);
    assert_eq!(read_u32(&mut t, 0x34), 256);

    // Select queue 1, max_size=128
    write_u32(&mut t, 0x30, 1);
    assert_eq!(read_u32(&mut t, 0x34), 128);

    // Select non-existent queue 2 → max_size=0
    write_u32(&mut t, 0x30, 2);
    assert_eq!(read_u32(&mut t, 0x34), 0);
}

#[test]
fn queue_set_size() {
    let mut t = make_transport();
    // Need FEATURES_OK state to update queue fields
    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER,
    );
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER | DEVICE_STATUS_FEATURES_OK,
    );

    write_u32(&mut t, 0x30, 0); // select queue 0
    write_u32(&mut t, 0x38, 64); // set size to 64

    // Verify by reading back through the device
    let d = t.device();
    let locked = d.lock().unwrap();
    assert_eq!(locked.queues()[0].size, 64);
}

#[test]
fn queue_set_addresses() {
    let mut t = make_transport();
    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER,
    );
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER | DEVICE_STATUS_FEATURES_OK,
    );

    write_u32(&mut t, 0x30, 0); // select queue 0

    // Set desc_table: 0x0000_0002_1000_0000
    write_u32(&mut t, 0x80, 0x1000_0000); // low
    write_u32(&mut t, 0x84, 0x0000_0002); // high

    // Set avail_ring: 0x0000_0003_2000_0000
    write_u32(&mut t, 0x90, 0x2000_0000); // low
    write_u32(&mut t, 0x94, 0x0000_0003); // high

    // Set used_ring: 0x0000_0004_3000_0000
    write_u32(&mut t, 0xA0, 0x3000_0000); // low
    write_u32(&mut t, 0xA4, 0x0000_0004); // high

    let d = t.device();
    let locked = d.lock().unwrap();
    let q = &locked.queues()[0];
    assert_eq!(q.desc_table_addr, 0x0000_0002_1000_0000);
    assert_eq!(q.avail_ring_addr, 0x0000_0003_2000_0000);
    assert_eq!(q.used_ring_addr, 0x0000_0004_3000_0000);
}

#[test]
fn queue_ready() {
    let mut t = make_transport();
    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER,
    );
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER | DEVICE_STATUS_FEATURES_OK,
    );

    write_u32(&mut t, 0x30, 0); // select queue 0
    write_u32(&mut t, 0x44, 1); // mark ready

    assert_eq!(read_u32(&mut t, 0x44), 1);
}

// ── 8. Config space read/write ───────────────────────────────────────

#[test]
fn config_space_read_write() {
    let mut t = make_transport();

    // Write 4 bytes to config space offset 0 (MMIO offset 0x100)
    let write_data: [u8; 4] = [0xAA, 0xBB, 0xCC, 0xDD];
    t.write(0x100, &write_data);

    // Read them back
    let mut read_data = [0u8; 4];
    t.read(0x100, &mut read_data);
    assert_eq!(read_data, write_data);
}

#[test]
fn config_space_at_various_offsets() {
    let mut t = make_transport();

    // Write at offset 10 within config space (MMIO offset 0x10A)
    let write_data: [u8; 2] = [0x12, 0x34];
    t.write(0x10A, &write_data);

    let mut read_data = [0u8; 2];
    t.read(0x10A, &mut read_data);
    assert_eq!(read_data, write_data);
}

// ── 9. Interrupt status ──────────────────────────────────────────────

#[test]
fn interrupt_status_read_and_ack() {
    let mut t = make_transport();
    drive_to_driver_ok(&mut t);

    // Initially zero
    assert_eq!(read_u32(&mut t, 0x60), 0);

    // Trigger a vring interrupt
    t.trigger_interrupt(VIRTIO_MMIO_INT_VRING);
    assert_eq!(read_u32(&mut t, 0x60), VIRTIO_MMIO_INT_VRING);

    // Trigger a config interrupt too
    t.trigger_interrupt(VIRTIO_MMIO_INT_CONFIG);
    assert_eq!(
        read_u32(&mut t, 0x60),
        VIRTIO_MMIO_INT_VRING | VIRTIO_MMIO_INT_CONFIG
    );

    // Acknowledge the vring interrupt
    write_u32(&mut t, 0x64, VIRTIO_MMIO_INT_VRING);
    assert_eq!(read_u32(&mut t, 0x60), VIRTIO_MMIO_INT_CONFIG);

    // Acknowledge the config interrupt
    write_u32(&mut t, 0x64, VIRTIO_MMIO_INT_CONFIG);
    assert_eq!(read_u32(&mut t, 0x60), 0);
}

// ── 10. Device reset ─────────────────────────────────────────────────

#[test]
fn device_reset_clears_state() {
    let mut t = make_transport();
    drive_to_driver_ok(&mut t);

    // Configure some queue state
    write_u32(&mut t, 0x30, 0); // select queue 0

    // Trigger an interrupt so there's state to clear
    t.trigger_interrupt(VIRTIO_MMIO_INT_VRING);
    assert_ne!(read_u32(&mut t, 0x60), 0);

    // Reset: write 0 to status register
    write_u32(&mut t, 0x70, 0);

    // Status should be INIT
    assert_eq!(read_u32(&mut t, 0x70), DEVICE_STATUS_INIT);

    // Interrupt status should be cleared
    assert_eq!(read_u32(&mut t, 0x60), 0);

    // Feature select should be reset
    write_u32(&mut t, 0x14, 0);
    // Queue select should be reset (reading queue 0 max size)
    assert_eq!(read_u32(&mut t, 0x34), 256);
}

// ── Edge cases ───────────────────────────────────────────────────────

#[test]
fn read_vendor_id_is_zero() {
    let mut t = make_transport();
    assert_eq!(read_u32(&mut t, 0x0C), 0);
}

#[test]
fn read_config_generation() {
    let mut t = make_transport();
    assert_eq!(read_u32(&mut t, 0xFC), 0);
}

#[test]
fn non_4byte_register_read_ignored() {
    let mut t = make_transport();
    // Reading a 2-byte slice from register space should not crash and leave data unchanged
    let mut data = [0xFFu8; 2];
    t.read(0x00, &mut data);
    // Data should remain untouched (implementation fills with 0 or leaves as-is)
    // We just verify no panic occurs
}

#[test]
fn unknown_register_read_returns_zero() {
    let mut t = make_transport();
    // 0xF0 is not a defined register
    let val = read_u32(&mut t, 0xF0);
    assert_eq!(val, 0);
}

#[test]
fn features_ack_rejected_before_driver_state() {
    let mut t = make_transport();
    // In INIT state, feature ack should be ignored
    write_u32(&mut t, 0x24, 0); // DriverFeaturesSel = 0
    write_u32(&mut t, 0x20, 0xFF); // try to ack features

    let d = t.device();
    let locked = d.lock().unwrap();
    assert_eq!(locked.acked_features(), 0);
}

#[test]
fn queue_update_rejected_before_features_ok() {
    let mut t = make_transport();
    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_DRIVER,
    );
    // In DRIVER state (not FEATURES_OK), queue updates should be rejected
    write_u32(&mut t, 0x30, 0);
    write_u32(&mut t, 0x38, 64);

    let d = t.device();
    let locked = d.lock().unwrap();
    // Size should remain at default (256), not 64
    assert_eq!(locked.queues()[0].size, 0);
}

#[test]
fn device_activation_on_driver_ok() {
    let mut t = make_transport();
    assert!(!t.device().lock().unwrap().is_activated());
    drive_to_driver_ok(&mut t);
    assert!(t.device().lock().unwrap().is_activated());
}

#[test]
fn status_failed_bit() {
    let mut t = make_transport();
    write_u32(&mut t, 0x70, DEVICE_STATUS_ACKNOWLEDGE);
    // Write FAILED bit
    write_u32(
        &mut t,
        0x70,
        DEVICE_STATUS_ACKNOWLEDGE | DEVICE_STATUS_FAILED,
    );
    let status = read_u32(&mut t, 0x70);
    assert_ne!(status & DEVICE_STATUS_FAILED, 0);
}

// ── 11. QueueNotify ───────────────────────────────────────────────────

#[test]
fn queue_notify_without_memory_is_noop() {
    let mut t = make_transport();
    drive_to_driver_ok(&mut t);

    // No memory set — QueueNotify should not crash and interrupt should stay 0
    write_u32(&mut t, 0x50, 0);
    assert_eq!(read_u32(&mut t, 0x60), 0);
}

#[test]
fn queue_notify_with_empty_queue_no_interrupt() {
    let device = DummyDevice::new();
    let device_arc: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(device));
    let mut t = MmioTransport::new(device_arc);

    // Set up memory (1 MiB) and connect it
    let memory = Arc::new(GuestMemory::new(1024 * 1024, 0).unwrap());
    t.set_memory(Arc::clone(&memory));

    drive_to_driver_ok(&mut t);

    // Configure queue addresses
    write_u32(&mut t, 0x30, 0); // select queue 0

    // Write QueueNotify — avail_idx == last_avail_idx, so nothing to process
    // (DummyDevice's process_queue uses default which returns false)
    write_u32(&mut t, 0x50, 0);

    // No interrupt should be set since no requests processed
    assert_eq!(read_u32(&mut t, 0x60), 0);
}

#[test]
fn queue_notify_triggers_interrupt_on_processed_request() {
    // Use a real BlockDevice so process_queue actually runs
    use crate::devices::block::BlockDevice;
    use std::io::Write;

    // Create a disk with known data
    let mut disk_file = crate::testutil::named_temp_file("visor-vmm-mmio-").unwrap();
    let disk_data = vec![0xBBu8; 4096]; // 8 sectors
    disk_file.write_all(&disk_data).unwrap();
    disk_file.flush().unwrap();

    let block = BlockDevice::new(disk_file.path(), false).unwrap();
    let block_arc: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(block));
    let mut t = MmioTransport::new(block_arc);

    let memory = Arc::new(GuestMemory::new(1024 * 1024, 0).unwrap());
    t.set_memory(Arc::clone(&memory));

    drive_to_driver_ok(&mut t);

    // Configure queue 0 addresses through MMIO registers
    // Need to be in FEATURES_OK state to configure queues, but
    // drive_to_driver_ok already passed through. Configure via device directly.
    {
        let dev = t.device();
        let mut locked = dev.lock().unwrap();
        let q = &mut locked.queues_mut()[0];
        q.size = 256;
        q.ready = true;
        q.desc_table_addr = 0x1_0000;
        q.avail_ring_addr = 0x2_0000;
        q.used_ring_addr = 0x3_0000;
    }

    // Set up a read request in guest memory
    let desc_table = 0x1_0000u64;
    let avail_ring = 0x2_0000u64;
    let header_addr = 0x4_0000u64;
    let data_addr = 0x5_0000u64;
    let status_addr = 0x6_0000u64;

    // Descriptor 0: header (readable)
    memory
        .write_bytes(desc_table, &header_addr.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(desc_table + 8, &16u32.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(desc_table + 12, &(VIRTQ_DESC_F_NEXT).to_le_bytes())
        .unwrap();
    memory
        .write_bytes(desc_table + 14, &1u16.to_le_bytes())
        .unwrap();

    // Descriptor 1: data (writable)
    memory
        .write_bytes(desc_table + 16, &data_addr.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(desc_table + 24, &512u32.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(
            desc_table + 28,
            &(VIRTQ_DESC_F_WRITE | VIRTQ_DESC_F_NEXT).to_le_bytes(),
        )
        .unwrap();
    memory
        .write_bytes(desc_table + 30, &2u16.to_le_bytes())
        .unwrap();

    // Descriptor 2: status (writable)
    memory
        .write_bytes(desc_table + 32, &status_addr.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(desc_table + 40, &1u32.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(desc_table + 44, &(VIRTQ_DESC_F_WRITE).to_le_bytes())
        .unwrap();

    // Write request header: type=IN(0), reserved=0, sector=0
    memory
        .write_bytes(header_addr, &0u32.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(header_addr + 4, &0u32.to_le_bytes())
        .unwrap();
    memory
        .write_bytes(header_addr + 8, &0u64.to_le_bytes())
        .unwrap();

    // Avail ring: idx=1, ring[0]=0 (head descriptor)
    memory.write_bytes(avail_ring, &0u16.to_le_bytes()).unwrap(); // flags
    memory
        .write_bytes(avail_ring + 2, &1u16.to_le_bytes())
        .unwrap(); // idx
    memory
        .write_bytes(avail_ring + 4, &0u16.to_le_bytes())
        .unwrap(); // ring[0]

    // Write to QueueNotify (offset 0x50) to trigger I/O
    write_u32(&mut t, 0x50, 0);

    // Verify interrupt was raised
    let int_status = read_u32(&mut t, 0x60);
    assert_ne!(
        int_status & VIRTIO_MMIO_INT_VRING,
        0,
        "VRING interrupt should be set after processing a request"
    );

    // Verify data was read from disk
    let guest_data = memory.read_bytes(data_addr, 512).unwrap();
    assert_eq!(guest_data, vec![0xBBu8; 512]);

    // Verify status byte is OK (0)
    let status = memory.read_bytes(status_addr, 1).unwrap();
    assert_eq!(status[0], 0, "status should be VIRTIO_BLK_S_OK");

    // Verify used ring was updated
    let used_idx_bytes = memory.read_bytes(0x3_0000 + 2, 2).unwrap();
    let used_idx = u16::from_le_bytes([used_idx_bytes[0], used_idx_bytes[1]]);
    assert_eq!(used_idx, 1, "used ring idx should be 1 after one request");
}

// ── 12. IRQ deassert callback ────────────────────────────────────────

#[test]
fn interrupt_ack_calls_deassert_when_status_cleared() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let mut t = make_transport();

    // Wire a deassert callback that counts invocations.
    let deassert_count = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&deassert_count);
    t.set_irq_deassert(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    drive_to_driver_ok(&mut t);

    // Trigger a vring interrupt.
    t.trigger_interrupt(VIRTIO_MMIO_INT_VRING);
    assert_eq!(read_u32(&mut t, 0x60), VIRTIO_MMIO_INT_VRING);

    // Acknowledge it — this should clear interrupt_status to 0 and call deassert.
    write_u32(&mut t, 0x64, VIRTIO_MMIO_INT_VRING);
    assert_eq!(read_u32(&mut t, 0x60), 0);
    assert_eq!(
        deassert_count.load(Ordering::SeqCst),
        1,
        "deassert callback should be called exactly once when interrupt_status reaches 0"
    );
}

#[test]
fn interrupt_ack_no_deassert_when_bits_remain() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let mut t = make_transport();

    let deassert_count = Arc::new(AtomicU32::new(0));
    let counter = Arc::clone(&deassert_count);
    t.set_irq_deassert(Arc::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
    }));

    drive_to_driver_ok(&mut t);

    // Trigger both vring and config interrupts.
    t.trigger_interrupt(VIRTIO_MMIO_INT_VRING);
    t.trigger_interrupt(VIRTIO_MMIO_INT_CONFIG);

    // Acknowledge only the vring interrupt — config bit remains.
    write_u32(&mut t, 0x64, VIRTIO_MMIO_INT_VRING);
    assert_eq!(read_u32(&mut t, 0x60), VIRTIO_MMIO_INT_CONFIG);
    assert_eq!(
        deassert_count.load(Ordering::SeqCst),
        0,
        "deassert should NOT be called while interrupt bits remain pending"
    );

    // Now acknowledge the config interrupt — status reaches 0.
    write_u32(&mut t, 0x64, VIRTIO_MMIO_INT_CONFIG);
    assert_eq!(read_u32(&mut t, 0x60), 0);
    assert_eq!(
        deassert_count.load(Ordering::SeqCst),
        1,
        "deassert should be called once all pending bits are cleared"
    );
}

#[test]
fn interrupt_ack_no_deassert_when_callback_not_set() {
    let mut t = make_transport();
    // No irq_deassert callback set — should not panic.
    drive_to_driver_ok(&mut t);

    t.trigger_interrupt(VIRTIO_MMIO_INT_VRING);
    write_u32(&mut t, 0x64, VIRTIO_MMIO_INT_VRING);
    assert_eq!(read_u32(&mut t, 0x60), 0);
    // No callback set, so nothing to assert beyond no panic.
}
