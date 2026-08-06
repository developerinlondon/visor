use super::*;
use crate::vm::ExitData;
use std::io::Write;
use std::sync::{Arc, Mutex};

#[test]
fn device_manager_io_out_continues() {
    let mut dm = DeviceManager::new();
    let exit = VmExit::IoOut {
        port: 0x3F8,
        data: ExitData::from_slice(b"A"),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Continue);
}

#[test]
fn device_manager_io_in_continues() {
    let mut dm = DeviceManager::new();
    let exit = VmExit::IoIn {
        port: 0x3F8,
        size: 1,
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Continue);
}

#[test]
fn device_manager_mmio_write_continues() {
    let mut dm = DeviceManager::new();
    let exit = VmExit::MmioWrite {
        addr: 0xFEE0_0000,
        data: ExitData::from_slice(&[0x42]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Continue);
}

#[test]
fn device_manager_mmio_read_continues() {
    let mut dm = DeviceManager::new();
    let exit = VmExit::MmioRead {
        addr: 0xFEE0_0000,
        size: 4,
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Continue);
}

#[test]
fn device_manager_halt_continues() {
    let mut dm = DeviceManager::new();
    let action = dm.handle_exit(VmExit::Halt).unwrap();
    assert_eq!(action, ExitAction::Continue);
}

#[test]
fn device_manager_shutdown_stops() {
    let mut dm = DeviceManager::new();
    let action = dm.handle_exit(VmExit::Shutdown).unwrap();
    assert_eq!(action, ExitAction::Stop);
}

#[test]
fn device_manager_reboot_stops() {
    let mut dm = DeviceManager::new();
    let action = dm.handle_exit(VmExit::Reboot).unwrap();
    assert_eq!(action, ExitAction::Stop);
}

#[test]
fn device_manager_io_out_dispatches_to_pio_bus() {
    use super::serial::SerialDevice;

    // Create a serial device backed by an in-memory buffer.
    let output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = TestSink(Arc::clone(&output));
    let interrupt = Arc::new(crate::platform::event::MockInterruptEvent::new());
    let serial = SerialDevice::new(Box::new(sink), interrupt);
    let serial = Arc::new(Mutex::new(serial));

    let mut dm = DeviceManager::new();
    dm.pio_bus
        .register(
            u64::from(super::serial::COM1_PORT_BASE),
            super::serial::COM1_PORT_COUNT,
            serial,
        )
        .unwrap();

    // Write 'X' to COM1 data register via IoOut exit.
    let exit = VmExit::IoOut {
        port: super::serial::COM1_PORT_BASE,
        data: ExitData::from_slice(b"X"),
    };
    dm.handle_exit(exit).unwrap();

    let captured = output.lock().unwrap();
    assert_eq!(&*captured, b"X");
}

/// A `Write` implementation that appends to a shared `Vec<u8>`.
#[derive(Clone)]
struct TestSink(Arc<Mutex<Vec<u8>>>);

impl Write for TestSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[test]
fn device_manager_io_read_dispatches_to_pio_bus() {
    use super::serial::SerialDevice;

    // Create a serial device backed by an in-memory buffer.
    let output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = TestSink(Arc::clone(&output));
    let interrupt = Arc::new(crate::platform::event::MockInterruptEvent::new());
    let serial = SerialDevice::new(Box::new(sink), interrupt);
    let serial = Arc::new(Mutex::new(serial));

    let mut dm = DeviceManager::new();
    dm.pio_bus
        .register(
            u64::from(super::serial::COM1_PORT_BASE),
            super::serial::COM1_PORT_COUNT,
            serial,
        )
        .unwrap();

    // Read the Line Status Register (offset 5 from COM1 base).
    // An idle serial port should report TX empty (bit 5) + TX holding empty (bit 6).
    let mut data = [0u8; 1];
    dm.handle_io_read(super::serial::COM1_PORT_BASE + 5, &mut data);
    // bit 5 (0x20) = THR empty, bit 6 (0x40) = TX shift register empty
    assert_ne!(data[0] & 0x60, 0, "LSR should indicate TX ready");
}

#[test]
fn device_manager_mmio_read_dispatches_to_mmio_bus() {
    use super::bus::BusDevice;

    /// A trivial device that returns 0xAB on read.
    struct FixedDevice;
    impl BusDevice for FixedDevice {
        fn read(&mut self, _offset: u64, data: &mut [u8]) {
            for b in data.iter_mut() {
                *b = 0xAB;
            }
        }
        fn write(&mut self, _offset: u64, _data: &[u8]) {}
    }

    let device = Arc::new(Mutex::new(FixedDevice));
    let mut dm = DeviceManager::new();
    dm.mmio_bus.register(0x1000_0000, 0x100, device).unwrap();

    let mut data = [0u8; 4];
    dm.handle_mmio_read(0x1000_0010, &mut data);
    assert_eq!(data, [0xAB, 0xAB, 0xAB, 0xAB]);
}

#[test]
fn device_manager_io_read_unregistered_fills_0xff() {
    let mut dm = DeviceManager::new();
    let mut data = [0u8; 1];
    dm.handle_io_read(0x3F8, &mut data);
    // No device registered → default 0xFF (missing device behavior)
    assert_eq!(data[0], 0xFF);
}

#[test]
fn device_manager_mmio_read_unregistered_fills_0xff() {
    let mut dm = DeviceManager::new();
    let mut data = [0u8; 4];
    dm.handle_mmio_read(0xFEE0_0000, &mut data);
    assert_eq!(data, [0xFF, 0xFF, 0xFF, 0xFF]);
}

// ── Reboot I/O port detection tests ──────────────────────────────────

#[test]
fn device_manager_kbd_reset_port_0x64_stops() {
    let mut dm = DeviceManager::new();
    // Keyboard controller reset: port 0x64, data byte 0xFE
    let exit = VmExit::IoOut {
        port: 0x64,
        data: ExitData::from_slice(&[0xFE]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Stop);
}

#[test]
fn device_manager_kbd_port_0x64_non_reset_continues() {
    let mut dm = DeviceManager::new();
    // Port 0x64 with non-reset data should NOT stop
    let exit = VmExit::IoOut {
        port: 0x64,
        data: ExitData::from_slice(&[0xD1]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Continue);
}

#[test]
fn device_manager_pci_reset_port_0xcf9_stops() {
    let mut dm = DeviceManager::new();
    // PCI reset: port 0xCF9, bit 2 set (0x04 = hard reset, 0x06 = full reset)
    let exit = VmExit::IoOut {
        port: 0xCF9,
        data: ExitData::from_slice(&[0x06]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Stop);
}

#[test]
fn device_manager_pci_reset_port_0xcf9_bit2_only_stops() {
    let mut dm = DeviceManager::new();
    // Just bit 2 set
    let exit = VmExit::IoOut {
        port: 0xCF9,
        data: ExitData::from_slice(&[0x04]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Stop);
}

#[test]
fn device_manager_pci_port_0xcf9_no_reset_bit_continues() {
    let mut dm = DeviceManager::new();
    // Port 0xCF9 without bit 2 should NOT stop (e.g., 0x01 = soft reset enable only)
    let exit = VmExit::IoOut {
        port: 0xCF9,
        data: ExitData::from_slice(&[0x01]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Continue);
}

#[test]
fn device_manager_fast_reset_port_0x92_stops() {
    let mut dm = DeviceManager::new();
    // Fast A20/reset: port 0x92, bit 0 set
    let exit = VmExit::IoOut {
        port: 0x92,
        data: ExitData::from_slice(&[0x01]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Stop);
}

#[test]
fn device_manager_fast_reset_port_0x92_bit0_with_others_stops() {
    let mut dm = DeviceManager::new();
    // Bit 0 set among other bits — still a reset
    let exit = VmExit::IoOut {
        port: 0x92,
        data: ExitData::from_slice(&[0x03]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Stop);
}

#[test]
fn device_manager_port_0x92_no_reset_bit_continues() {
    let mut dm = DeviceManager::new();
    // Port 0x92 without bit 0 should NOT stop (e.g., A20 gate only = 0x02)
    let exit = VmExit::IoOut {
        port: 0x92,
        data: ExitData::from_slice(&[0x02]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Continue);
}

#[test]
fn device_manager_reboot_port_empty_data_continues() {
    let mut dm = DeviceManager::new();
    // Edge case: empty data on a reboot port should not crash, should continue
    let exit = VmExit::IoOut {
        port: 0x64,
        data: ExitData::from_slice(&[]),
    };
    let action = dm.handle_exit(exit).unwrap();
    assert_eq!(action, ExitAction::Continue);
}

// ── PCI bus wiring tests ─────────────────────────────────────────────

#[test]
fn device_manager_enable_pci_succeeds() {
    let mut dm = DeviceManager::new();
    assert!(dm.pci_bus().is_none());
    dm.enable_pci().unwrap();
    assert!(dm.pci_bus().is_some());
}

#[test]
fn device_manager_pci_config_address_read() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    // Write a config address to port 0xCF8
    let addr_bytes = 0x8000_0800u32.to_le_bytes(); // enable bit + device 1, reg 0
    let exit = VmExit::IoOut {
        port: 0xCF8,
        data: ExitData::from_slice(&addr_bytes),
    };
    dm.handle_exit(exit).unwrap();

    // Read back the config address from port 0xCF8
    let mut data = [0u8; 4];
    dm.handle_io_read(0xCF8, &mut data);
    assert_eq!(u32::from_le_bytes(data), 0x8000_0800);
}

#[test]
fn device_manager_pci_config_data_empty_slot_returns_all_ones() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    // Set config address: enable bit, device 5, register 0
    let addr_bytes = 0x8000_2800u32.to_le_bytes();
    let exit = VmExit::IoOut {
        port: 0xCF8,
        data: ExitData::from_slice(&addr_bytes),
    };
    dm.handle_exit(exit).unwrap();

    // Read config data from port 0xCFC — empty slot returns 0xFFFFFFFF
    let mut data = [0u8; 4];
    dm.handle_io_read(0xCFC, &mut data);
    assert_eq!(u32::from_le_bytes(data), 0xFFFF_FFFF);
}

#[test]
fn device_manager_pci_config_data_with_device() {
    use crate::transport::pci::PciDevice;
    use crate::transport::{DeviceType, VirtQueue, VirtioDevice, VirtioError};

    /// Minimal `VirtioDevice` for testing PCI bus wiring.
    struct StubVirtio;
    impl VirtioDevice for StubVirtio {
        fn device_type(&self) -> DeviceType {
            DeviceType::Block
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

    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    // Add a PCI device at slot 0 wrapping a stub virtio block device
    let virtio_dev: Arc<Mutex<dyn VirtioDevice>> = Arc::new(Mutex::new(StubVirtio));
    let pci_dev = PciDevice::new(virtio_dev, 1);
    let pci_dev = Arc::new(Mutex::new(pci_dev));
    dm.pci_bus()
        .unwrap()
        .lock()
        .unwrap()
        .add_device(0, pci_dev)
        .unwrap();

    // Set config address: enable bit, device 0, register 0 (vendor/device ID)
    let addr_bytes = 0x8000_0000u32.to_le_bytes();
    let exit = VmExit::IoOut {
        port: 0xCF8,
        data: ExitData::from_slice(&addr_bytes),
    };
    dm.handle_exit(exit).unwrap();

    // Read config data from port 0xCFC — should get vendor/device ID
    let mut data = [0u8; 4];
    dm.handle_io_read(0xCFC, &mut data);
    let value = u32::from_le_bytes(data);
    // Vendor ID = 0x1AF4, Device ID = 0x1042 (0x1040 + Block=2)
    assert_eq!(value & 0xFFFF, 0x1AF4, "vendor ID should be virtio");
    assert_eq!(
        value >> 16,
        0x1042,
        "device ID should be 0x1040 + block type"
    );
}

#[test]
fn device_manager_pci_disabled_returns_all_ones() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();

    // Set config address WITHOUT enable bit
    let addr_bytes = 0x0000_0000u32.to_le_bytes();
    let exit = VmExit::IoOut {
        port: 0xCF8,
        data: ExitData::from_slice(&addr_bytes),
    };
    dm.handle_exit(exit).unwrap();

    // Read config data — should return all ones since enable bit is not set
    let mut data = [0u8; 4];
    dm.handle_io_read(0xCFC, &mut data);
    assert_eq!(u32::from_le_bytes(data), 0xFFFF_FFFF);
}

#[test]
fn device_manager_enable_pci_twice_fails() {
    let mut dm = DeviceManager::new();
    dm.enable_pci().unwrap();
    // Second call should fail because PCI I/O range is already registered
    assert!(dm.enable_pci().is_err());
}
