use std::sync::{Arc, Mutex};

use super::*;
use crate::devices::bus::BusDevice;
use crate::transport::pci::PciDevice;
use crate::transport::{DeviceType, VirtQueue, VirtioDevice, VirtioError};

// ── Test helpers ─────────────────────────────────────────────────────

/// Minimal `VirtioDevice` implementation for PCI bus tests.
struct DummyDevice {
    device_type: DeviceType,
    acked_features: u64,
    queues: Vec<VirtQueue>,
    activated: bool,
}

impl DummyDevice {
    fn new() -> Self {
        Self {
            device_type: DeviceType::Block,
            acked_features: 0,
            queues: vec![VirtQueue::new(256)],
            activated: false,
        }
    }
}

impl VirtioDevice for DummyDevice {
    fn device_type(&self) -> DeviceType {
        self.device_type
    }

    fn avail_features(&self) -> u64 {
        0
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

fn make_pci_device() -> Arc<Mutex<PciDevice>> {
    let dummy: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(DummyDevice::new()));
    Arc::new(Mutex::new(PciDevice::new(dummy, 4)))
}

/// Write a PCI config address (enable bit | device# | register offset).
fn write_config_addr(bus: &mut PciBus, slot: u32, offset: u8) {
    let addr: u32 = 0x8000_0000 | (slot << 11) | u32::from(offset & 0xFC);
    bus.write(0, &addr.to_le_bytes());
}

/// Read 4 bytes from config data port (offset 4).
fn read_config_data(bus: &mut PciBus) -> u32 {
    let mut data = [0u8; 4];
    bus.read(4, &mut data);
    u32::from_le_bytes(data)
}

// ── 1. Add device to slot ───────────────────────────────────────────

#[test]
fn test_add_device_to_slot() {
    let mut bus = PciBus::new();
    let dev = make_pci_device();
    bus.add_device(0, dev).unwrap();

    assert!(bus.device(0).is_some());
    assert!(bus.device(1).is_none());
}

// ── 2. Config I/O routing ───────────────────────────────────────────

#[test]
fn test_config_io_routing() {
    let mut bus = PciBus::new();
    let dev = make_pci_device();
    bus.add_device(0, dev).unwrap();

    // Write config address: enable=1, device=0, offset=0x00
    write_config_addr(&mut bus, 0, 0x00);
    let val = read_config_data(&mut bus);

    // Low 16 bits = vendor ID 0x1AF4
    assert_eq!(val & 0xFFFF, 0x1AF4);
}

// ── 3. Empty slot returns all ones ──────────────────────────────────

#[test]
fn test_empty_slot_returns_all_ones() {
    let mut bus = PciBus::new();

    // Read from empty slot 5
    write_config_addr(&mut bus, 5, 0x00);
    let val = read_config_data(&mut bus);

    assert_eq!(val, 0xFFFF_FFFF);
}

// ── 4. Bus supports 32 slots ────────────────────────────────────────

#[test]
fn test_32_slots() {
    let mut bus = PciBus::new();

    for slot in 0..32 {
        let dev = make_pci_device();
        bus.add_device(slot, dev).unwrap();
    }

    // Verify all slots occupied
    for slot in 0..32 {
        assert!(bus.device(slot).is_some(), "slot {slot} should have device");
    }

    // Slot 32 is out of range
    let dev = make_pci_device();
    assert!(bus.add_device(32, dev).is_err());
}
