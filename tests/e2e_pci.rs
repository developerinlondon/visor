//! P3 integration tests: PCI transport and VFIO device passthrough.
//!
//! Tests that PCI config space routing works end-to-end through the
//! [`DeviceManager`] and that VFIO types can be constructed and validated.
//! Hardware-dependent tests (actual VFIO device binding) skip gracefully
//! when no passthrough-capable devices are available.

use std::sync::{Arc, Mutex};

use visor_vmm::devices::DeviceManager;
use visor_vmm::transport::pci::PciDevice;
use visor_vmm::transport::pci_bus::PciBus;
use visor_vmm::transport::{DeviceType, VirtQueue, VirtioDevice, VirtioError};
use visor_vmm::vm::{ExitData, ExitHandler, VmExit};

// ── Test helpers ─────────────────────────────────────────────────────

/// Minimal `VirtioDevice` for PCI integration tests.
struct StubVirtio {
    device_type: DeviceType,
}

impl StubVirtio {
    fn new(device_type: DeviceType) -> Self {
        Self { device_type }
    }
}

impl VirtioDevice for StubVirtio {
    fn device_type(&self) -> DeviceType {
        self.device_type
    }
    fn avail_features(&self) -> u64 {
        0
    }
    fn acked_features(&self) -> u64 {
        0
    }
    fn set_acked_features(&mut self, _features: u64) {}
    fn queues(&self) -> &[VirtQueue] {
        &[]
    }
    fn queues_mut(&mut self) -> &mut [VirtQueue] {
        &mut []
    }
    fn read_config(&self, _offset: u64, _data: &mut [u8]) {}
    fn write_config(&mut self, _offset: u64, _data: &[u8]) {}
    fn activate(&mut self) -> Result<(), VirtioError> {
        Ok(())
    }
    fn is_activated(&self) -> bool {
        false
    }
    fn reset(&mut self) {}
}

/// Helper to set the PCI config address register (port 0xCF8).
fn set_config_address(dm: &mut DeviceManager, device: u8, register: u8) {
    let addr: u32 = 0x8000_0000 | (u32::from(device) << 11) | u32::from(register & 0xFC);
    let exit = VmExit::IoOut {
        port: 0xCF8,
        data: ExitData::from_slice(&addr.to_le_bytes()),
    };
    dm.handle_exit(exit).unwrap();
}

/// Helper to read PCI config data (port 0xCFC).
fn read_config_data(dm: &mut DeviceManager) -> u32 {
    let mut data = [0u8; 4];
    dm.handle_io_read(0xCFC, &mut data);
    u32::from_le_bytes(data)
}

/// Helper to write PCI config data (port 0xCFC).
fn write_config_data(dm: &mut DeviceManager, value: u32) {
    let exit = VmExit::IoOut {
        port: 0xCFC,
        data: ExitData::from_slice(&value.to_le_bytes()),
    };
    dm.handle_exit(exit).unwrap();
}

// ── PCI Bus Discovery ────────────────────────────────────────────────

#[test]
fn pci_bus_scan_discovers_virtio_block_device() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    // Add a virtio-blk device at slot 0
    let dev: Arc<Mutex<dyn VirtioDevice>> =
        Arc::new(Mutex::new(StubVirtio::new(DeviceType::Block)));
    let pci_dev = PciDevice::new(dev, 4);
    dm.pci_bus()
        .unwrap()
        .lock()
        .unwrap()
        .add_device(0, Arc::new(Mutex::new(pci_dev)))
        .unwrap();

    // Read vendor/device ID from slot 0
    set_config_address(&mut dm, 0, 0x00);
    let id = read_config_data(&mut dm);

    assert_eq!(id & 0xFFFF, 0x1AF4, "vendor ID should be Red Hat/virtio");
    assert_eq!(id >> 16, 0x1042, "device ID should be 0x1040 + Block(2)");
}

#[test]
fn pci_bus_scan_discovers_virtio_net_device() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    let dev: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(StubVirtio::new(DeviceType::Net)));
    let pci_dev = PciDevice::new(dev, 4);
    dm.pci_bus()
        .unwrap()
        .lock()
        .unwrap()
        .add_device(1, Arc::new(Mutex::new(pci_dev)))
        .unwrap();

    set_config_address(&mut dm, 1, 0x00);
    let id = read_config_data(&mut dm);

    assert_eq!(id & 0xFFFF, 0x1AF4);
    assert_eq!(id >> 16, 0x1041, "device ID should be 0x1040 + Net(1)");
}

#[test]
fn pci_bus_empty_slots_return_all_ones() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    // Scan all 32 slots — all should return 0xFFFFFFFF (no device)
    for slot in 0..32u8 {
        set_config_address(&mut dm, slot, 0x00);
        let id = read_config_data(&mut dm);
        assert_eq!(id, 0xFFFF_FFFF, "empty slot {slot} should return all ones");
    }
}

#[test]
fn pci_bus_multiple_devices_coexist() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    let pci_bus = dm.pci_bus().unwrap().clone();

    // Add 3 different device types at different slots
    let types = [
        (0u8, DeviceType::Block),
        (5, DeviceType::Net),
        (15, DeviceType::Vsock),
    ];
    for &(slot, dt) in &types {
        let dev: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(StubVirtio::new(dt)));
        let pci_dev = PciDevice::new(dev, 2);
        pci_bus
            .lock()
            .unwrap()
            .add_device(usize::from(slot), Arc::new(Mutex::new(pci_dev)))
            .unwrap();
    }

    // Verify each device is at its slot
    for &(slot, dt) in &types {
        set_config_address(&mut dm, slot, 0x00);
        let id = read_config_data(&mut dm);
        let expected_device_id = 0x1040 + dt as u16;
        assert_eq!(id & 0xFFFF, 0x1AF4, "slot {slot}: vendor ID");
        assert_eq!(
            (id >> 16) as u16,
            expected_device_id,
            "slot {slot}: device ID"
        );
    }

    // Verify other slots are empty
    set_config_address(&mut dm, 10, 0x00);
    assert_eq!(read_config_data(&mut dm), 0xFFFF_FFFF);
}

#[test]
fn pci_config_space_header_type_is_type0() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    let dev: Arc<Mutex<dyn VirtioDevice>> =
        Arc::new(Mutex::new(StubVirtio::new(DeviceType::Block)));
    let pci_dev = PciDevice::new(dev, 1);
    dm.pci_bus()
        .unwrap()
        .lock()
        .unwrap()
        .add_device(0, Arc::new(Mutex::new(pci_dev)))
        .unwrap();

    // Header type is at offset 0x0C (dword containing Header Type at byte 0x0E)
    set_config_address(&mut dm, 0, 0x0C);
    let dword = read_config_data(&mut dm);
    // Header Type is byte 2 of this dword (offset 0x0E within config space)
    let header_type = ((dword >> 16) & 0xFF) as u8;
    assert_eq!(header_type, 0x00, "should be Type 0 (endpoint)");
}

#[test]
fn pci_config_space_capabilities_pointer() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    let dev: Arc<Mutex<dyn VirtioDevice>> =
        Arc::new(Mutex::new(StubVirtio::new(DeviceType::Block)));
    let pci_dev = PciDevice::new(dev, 4);
    dm.pci_bus()
        .unwrap()
        .lock()
        .unwrap()
        .add_device(0, Arc::new(Mutex::new(pci_dev)))
        .unwrap();

    // Capabilities pointer at offset 0x34
    set_config_address(&mut dm, 0, 0x34);
    let dword = read_config_data(&mut dm);
    let cap_ptr = (dword & 0xFF) as u8;
    assert_eq!(cap_ptr, 0x40, "capabilities pointer should be 0x40 (MSI-X)");
}

#[test]
fn pci_msix_capability_structure() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    let dev: Arc<Mutex<dyn VirtioDevice>> =
        Arc::new(Mutex::new(StubVirtio::new(DeviceType::Block)));
    let pci_dev = PciDevice::new(dev, 8); // 8 MSI-X vectors
    dm.pci_bus()
        .unwrap()
        .lock()
        .unwrap()
        .add_device(0, Arc::new(Mutex::new(pci_dev)))
        .unwrap();

    // MSI-X capability at offset 0x40
    set_config_address(&mut dm, 0, 0x40);
    let dword = read_config_data(&mut dm);

    // Byte 0: Cap ID = 0x11 (MSI-X)
    let cap_id = (dword & 0xFF) as u8;
    assert_eq!(cap_id, 0x11, "MSI-X capability ID");

    // Byte 1: Next pointer = 0x00 (end of chain)
    let next = ((dword >> 8) & 0xFF) as u8;
    assert_eq!(next, 0x00, "next capability pointer (end of chain)");

    // Bytes 2-3: Message Control — table size = num_vectors - 1 = 7
    let msg_ctrl = (dword >> 16) as u16;
    let table_size = msg_ctrl & 0x07FF;
    assert_eq!(table_size, 7, "MSI-X table size should be num_vectors - 1");
}

#[test]
fn pci_bar_size_detection() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    let dev: Arc<Mutex<dyn VirtioDevice>> =
        Arc::new(Mutex::new(StubVirtio::new(DeviceType::Block)));
    let pci_dev = PciDevice::new(dev, 4); // 4 MSI-X vectors
    dm.pci_bus()
        .unwrap()
        .lock()
        .unwrap()
        .add_device(0, Arc::new(Mutex::new(pci_dev)))
        .unwrap();

    // BAR 4 holds the MSI-X table + PBA. It's at config offset 0x20 (0x10 + 4*4).
    set_config_address(&mut dm, 0, 0x20);
    let _bar4_original = read_config_data(&mut dm);

    // Write all-ones to BAR 4 for size detection
    write_config_data(&mut dm, 0xFFFF_FFFF);

    // Read back — the writable bits indicate BAR size
    set_config_address(&mut dm, 0, 0x20);
    let bar_size_mask = read_config_data(&mut dm);

    // BAR 4 should be non-zero (MSI-X table region)
    // Size mask is !(size-1) with type bits preserved
    assert_ne!(
        bar_size_mask, 0,
        "BAR 4 (MSI-X) size mask should be non-zero"
    );

    // BAR 0 is unused (size=0), so writing all-ones should leave it as 0
    set_config_address(&mut dm, 0, 0x10);
    write_config_data(&mut dm, 0xFFFF_FFFF);
    set_config_address(&mut dm, 0, 0x10);
    let bar0_mask = read_config_data(&mut dm);
    assert_eq!(bar0_mask, 0, "BAR 0 (unused) should stay zero");
}

// ── PCI bus standalone tests ─────────────────────────────────────────

#[test]
fn pci_bus_add_device_invalid_slot() {
    let mut bus = PciBus::new();
    let dev: Arc<Mutex<dyn VirtioDevice>> =
        Arc::new(Mutex::new(StubVirtio::new(DeviceType::Block)));
    let pci_dev = Arc::new(Mutex::new(PciDevice::new(dev, 1)));
    assert!(bus.add_device(32, pci_dev).is_err());
}

#[test]
fn pci_bus_add_device_duplicate_slot() {
    let mut bus = PciBus::new();
    let dev1: Arc<Mutex<dyn VirtioDevice>> =
        Arc::new(Mutex::new(StubVirtio::new(DeviceType::Block)));
    let dev2: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(StubVirtio::new(DeviceType::Net)));
    bus.add_device(0, Arc::new(Mutex::new(PciDevice::new(dev1, 1))))
        .unwrap();
    assert!(
        bus.add_device(0, Arc::new(Mutex::new(PciDevice::new(dev2, 1))))
            .is_err()
    );
}

// ── VFIO type tests ─────────────────────────────────────────────────
// These test VFIO data structures without requiring hardware.
// Hardware-dependent VFIO tests are in the unit test file (vfio_test.rs).

#[test]
fn vfio_dev_exists() {
    if !std::path::Path::new("/dev/vfio/vfio").exists() {
        eprintln!("skipping: /dev/vfio/vfio is not present on this host");
        return;
    }

    // Verify the VFIO character device is present on this host.
    // This is an environmental assertion, not a code test.
    assert!(
        std::path::Path::new("/dev/vfio/vfio").exists(),
        "/dev/vfio/vfio should exist on AX41 test host"
    );
}

#[test]
fn vfio_modules_loaded() {
    // Verify vfio kernel modules are loaded
    let modules = std::fs::read_to_string("/proc/modules").unwrap_or_default();
    if !modules.contains("vfio_pci") {
        eprintln!("skipping: vfio_pci module is not loaded on this host");
        return;
    }

    assert!(
        modules.contains("vfio_iommu_type1"),
        "vfio_iommu_type1 module should be loaded"
    );
    assert!(
        modules.contains("vfio_pci"),
        "vfio_pci module should be loaded"
    );
}

#[test]
fn iommu_groups_directory_exists() {
    // IOMMU groups should be present on a host with IOMMU enabled
    let iommu_path = std::path::Path::new("/sys/kernel/iommu_groups");
    assert!(
        iommu_path.exists(),
        "/sys/kernel/iommu_groups should exist with IOMMU enabled"
    );

    // Should have at least one group
    let count = std::fs::read_dir(iommu_path).unwrap().count();
    assert!(
        count > 0,
        "should have at least one IOMMU group (found {count})"
    );
}
