use std::sync::{Arc, Mutex};

use super::*;
use crate::transport::{DeviceType, VirtQueue, VirtioDevice, VirtioError};

// ── Test helpers ─────────────────────────────────────────────────────

/// Minimal `VirtioDevice` implementation for testing the PCI transport.
struct DummyDevice {
    device_type: DeviceType,
    avail_features: u64,
    acked_features: u64,
    queues: Vec<VirtQueue>,
    activated: bool,
}

impl DummyDevice {
    fn new() -> Self {
        Self {
            device_type: DeviceType::Block,
            avail_features: 0,
            acked_features: 0,
            queues: vec![VirtQueue::new(256)],
            activated: false,
        }
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
        self.acked_features = 0;
    }
}

fn make_device() -> PciDevice {
    let dummy: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(DummyDevice::new()));
    PciDevice::new(dummy, 4)
}

fn make_device_with_type(dt: DeviceType) -> PciDevice {
    let dummy = DummyDevice::new().with_device_type(dt);
    let device: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(dummy));
    PciDevice::new(device, 4)
}

// ── 1. Vendor / Device ID ───────────────────────────────────────────

#[test]
fn test_config_read_vendor_device_id() {
    let dev = make_device();
    let mut data = [0u8; 4];
    dev.read_config(0x00, &mut data);
    let val = u32::from_le_bytes(data);

    // Low 16 bits: Vendor ID = 0x1AF4
    assert_eq!(val & 0xFFFF, u32::from(VIRTIO_PCI_VENDOR_ID));
    // High 16 bits: Device ID = 0x1040 + Block(2) = 0x1042
    assert_eq!(
        (val >> 16) & 0xFFFF,
        u32::from(VIRTIO_PCI_DEVICE_ID_BASE + DeviceType::Block as u16)
    );
}

#[test]
fn test_config_device_id_varies_by_type() {
    let dev = make_device_with_type(DeviceType::Net);
    let mut data = [0u8; 2];
    dev.read_config(0x02, &mut data);
    let device_id = u16::from_le_bytes(data);
    assert_eq!(
        device_id,
        VIRTIO_PCI_DEVICE_ID_BASE + DeviceType::Net as u16
    );
}

// ── 2. Header Type ──────────────────────────────────────────────────

#[test]
fn test_config_read_header_type() {
    let dev = make_device();
    let mut data = [0u8; 1];
    dev.read_config(0x0E, &mut data);
    assert_eq!(data[0], 0x00, "Header type should be 0x00 (Type 0)");
}

// ── 3. Command register ─────────────────────────────────────────────

#[test]
fn test_config_write_command() {
    let mut dev = make_device();

    // Write I/O + Memory + Bus Master enable bits
    let cmd: u16 = 0x0007;
    dev.write_config(0x04, &cmd.to_le_bytes());

    // Read back
    let mut data = [0u8; 2];
    dev.read_config(0x04, &mut data);
    assert_eq!(u16::from_le_bytes(data), 0x0007);
}

// ── 4. BAR size detection ───────────────────────────────────────────

#[test]
fn test_bar_size_detection() {
    let mut dev = make_device();
    let bar4_offset: u8 = 0x20; // BAR 4 = 0x10 + 4*4

    // Write all-ones to BAR 4
    dev.write_config(bar4_offset, &0xFFFF_FFFFu32.to_le_bytes());

    // Read back — size mask reveals region size (4 KiB)
    let mut data = [0u8; 4];
    dev.read_config(bar4_offset, &mut data);
    let val = u32::from_le_bytes(data);

    // Memory BAR, 4096 bytes → mask = 0xFFFFF000
    assert_eq!(val & 0xFFFF_F000, 0xFFFF_F000);
    assert_eq!(val & 0x01, 0, "bit 0 should be 0 (memory BAR)");
}

// ── 5. BAR base address ─────────────────────────────────────────────

#[test]
fn test_bar_set_base_address() {
    let mut dev = make_device();
    let bar4_offset: u8 = 0x20;

    let base: u32 = 0xFE00_0000;
    dev.write_config(bar4_offset, &base.to_le_bytes());

    let mut data = [0u8; 4];
    dev.read_config(bar4_offset, &mut data);
    let val = u32::from_le_bytes(data);
    assert_eq!(val & 0xFFFF_F000, 0xFE00_0000);
}

// ── 6. MSI-X capability chain ───────────────────────────────────────

#[test]
fn test_msix_capability_in_chain() {
    let dev = make_device();

    // Capabilities pointer at 0x34 should point to 0x40
    let mut cap_ptr = [0u8; 1];
    dev.read_config(0x34, &mut cap_ptr);
    assert_eq!(cap_ptr[0], 0x40);

    // Status register bit 4 (capabilities list) should be set
    let mut status = [0u8; 2];
    dev.read_config(0x06, &mut status);
    assert_ne!(status[0] & 0x10, 0, "capabilities list bit must be set");

    // Capability ID at 0x40 should be 0x11 (MSI-X)
    let mut cap_id = [0u8; 1];
    dev.read_config(0x40, &mut cap_id);
    assert_eq!(cap_id[0], 0x11, "MSI-X capability ID");

    // Next pointer should be 0x00 (end of chain)
    let mut next = [0u8; 1];
    dev.read_config(0x41, &mut next);
    assert_eq!(next[0], 0x00);
}

// ── 7. MSI-X table write ────────────────────────────────────────────

#[test]
fn test_msix_table_write() {
    let mut dev = make_device();

    // Write to MSI-X table entry 0 via BAR 4
    let addr_lo: u32 = 0xFEE0_0000;
    let addr_hi: u32 = 0;
    let msg_data: u32 = 0x42;
    let vector_ctrl: u32 = 0; // unmasked

    dev.write_bar(4, 0, &addr_lo.to_le_bytes()).unwrap();
    dev.write_bar(4, 4, &addr_hi.to_le_bytes()).unwrap();
    dev.write_bar(4, 8, &msg_data.to_le_bytes()).unwrap();
    dev.write_bar(4, 12, &vector_ctrl.to_le_bytes()).unwrap();

    // Read back and verify
    let mut data = [0u8; 4];

    dev.read_bar(4, 0, &mut data).unwrap();
    assert_eq!(u32::from_le_bytes(data), addr_lo);

    dev.read_bar(4, 4, &mut data).unwrap();
    assert_eq!(u32::from_le_bytes(data), addr_hi);

    dev.read_bar(4, 8, &mut data).unwrap();
    assert_eq!(u32::from_le_bytes(data), msg_data);

    dev.read_bar(4, 12, &mut data).unwrap();
    assert_eq!(u32::from_le_bytes(data), vector_ctrl);
}

// ── 8. MSI-X enable ─────────────────────────────────────────────────

#[test]
fn test_msix_enable() {
    let mut dev = make_device();
    assert!(!dev.msix_enabled());

    // MSI-X message control at cap_offset + 2 (0x42-0x43)
    // Bit 15 = enable → bit 7 of byte 0x43
    dev.write_config(0x42, &[0x00, 0x80]);

    assert!(dev.msix_enabled());
}
