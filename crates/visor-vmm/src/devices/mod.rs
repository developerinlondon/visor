//! Device bus abstraction and emulated hardware devices.
//!
//! This module provides:
//!
//! - [`bus::Bus`] — a generic I/O bus that routes reads/writes to registered
//!   [`bus::BusDevice`] implementations.
//! - [`serial::SerialDevice`] — UART 16550 serial port backed by our own [`uart::Uart16550`] emulator.
//! - [`DeviceManager`] — top-level device orchestrator implementing
//!   [`ExitHandler`](crate::vm::ExitHandler) to route VM exits to the
//!   appropriate bus.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────┐               ┌─────────────────┐
//! │  vCPU    │  VmExit       │  DeviceManager   │
//! │  run     │──────────────>│                  │
//! │  loop    │               │  pio_bus ────> Serial (COM1)
//! └──────────┘               │             ├─> PciBus (0xCF8/0xCFC)
//!                            │  mmio_bus ───> (virtio devices)
//!                            └─────────────────┘
//! ```

pub mod balloon;
pub mod block;
pub mod bus;
pub mod fs;
pub mod gpu;
pub mod net;
#[cfg(target_arch = "aarch64")]
pub mod pl011;
pub mod rng;
pub mod serial;
pub mod uart;
pub mod vfio;
pub mod vsock;
pub mod vsock_muxer;

use std::sync::{Arc, Mutex};

use crate::transport::pci_bus::PciBus;
use crate::vm::{ExitAction, ExitHandler, VcpuError, VmExit};

use bus::{Bus, BusDevice};

// ── PCI I/O port constants ─────────────────────────────────────────

/// PCI configuration address register port.
const PCI_CONFIG_ADDRESS: u16 = 0xCF8;

/// Size of PCI config I/O region (address + data = 8 bytes).
const PCI_CONFIG_IO_SIZE: u64 = 8;

// ── Reboot I/O port constants ──────────────────────────────────────────

/// Keyboard controller command port (PS/2, port 0x64).
/// Writing `KBD_RESET_CMD` here triggers a CPU reset.
const KBD_CMD_PORT: u16 = 0x64;

/// Keyboard controller reset command byte (0xFE).
/// Linux's `emergency_restart()` writes this to `KBD_CMD_PORT`.
const KBD_RESET_CMD: u8 = 0xFE;

/// PCI reset control register (port 0xCF9).
/// Bit 2 triggers a hard reset; bit 1 selects reset type.
const PCI_RESET_PORT: u16 = 0xCF9;

/// Bit mask for the hard reset flag in `PCI_RESET_PORT`.
const PCI_RESET_BIT: u8 = 0x04;

/// Fast A20 / system reset port (0x92).
/// Bit 0 triggers a system reset; bit 1 controls the A20 gate.
const FAST_RESET_PORT: u16 = 0x92;

/// Bit mask for the reset flag in `FAST_RESET_PORT`.
const FAST_RESET_BIT: u8 = 0x01;

/// Top-level device orchestrator that routes VM exits to I/O buses.
///
/// Holds separate buses for port I/O (PIO) and memory-mapped I/O (MMIO).
/// Optionally manages a [`PciBus`] for PCI config space access via
/// ports `0xCF8`/`0xCFC`. Implements [`ExitHandler`] so it can be plugged
/// into the vCPU run loop.
#[non_exhaustive]
pub struct DeviceManager {
    /// Port I/O bus (x86 IN/OUT instructions, typically 0x0000–0xFFFF).
    pub pio_bus: Bus,

    /// Memory-mapped I/O bus (MMIO accesses outside guest RAM).
    pub mmio_bus: Bus,

    /// PCI bus registered on the PIO bus at 0xCF8 (optional, created via [`Self::enable_pci`]).
    pci_bus: Option<Arc<Mutex<PciBus>>>,
}

impl DeviceManager {
    /// Creates a new device manager with empty PIO and MMIO buses.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pio_bus: Bus::new(),
            mmio_bus: Bus::new(),
            pci_bus: None,
        }
    }

    /// Enables PCI support by creating a [`PciBus`] and registering it on
    /// the PIO bus at ports `0xCF8` (config address) and `0xCFC` (config data).
    ///
    /// After calling this, use [`pci_bus`](Self::pci_bus) to access the PCI bus
    /// and add devices to it.
    ///
    /// # Errors
    ///
    /// Returns a [`bus::BusError`] if the PCI config I/O port range
    /// `[0xCF8, 0xD00)` overlaps an existing PIO registration.
    pub fn enable_pci(&mut self) -> Result<(), bus::BusError> {
        let pci_bus = Arc::new(Mutex::new(PciBus::new()));
        self.pio_bus.register(
            u64::from(PCI_CONFIG_ADDRESS),
            PCI_CONFIG_IO_SIZE,
            Arc::clone(&pci_bus) as Arc<Mutex<dyn BusDevice>>,
        )?;
        self.pci_bus = Some(pci_bus);
        Ok(())
    }

    /// Returns a reference to the PCI bus, if PCI has been enabled via [`enable_pci`](Self::enable_pci).
    #[must_use]
    pub fn pci_bus(&self) -> Option<&Arc<Mutex<PciBus>>> {
        self.pci_bus.as_ref()
    }
}

impl Default for DeviceManager {
    fn default() -> Self {
        Self::new()
    }
}

impl ExitHandler for DeviceManager {
    /// Routes VM exits to the appropriate bus or lifecycle action.
    ///
    /// - `IoOut`: checks for x86 reboot I/O ports first, then dispatches to the PIO bus.
    /// - `IoIn`: continue (reads handled by [`handle_io_read`](ExitHandler::handle_io_read)).
    /// - `MmioWrite`: dispatches to the MMIO bus.
    /// - `MmioRead`: continue (reads handled by [`handle_mmio_read`](ExitHandler::handle_mmio_read)).
    /// - `Halt`: continue (guest idle, KVM resumes on next interrupt).
    /// - `Shutdown`, `Reboot`: returns [`ExitAction::Stop`].
    ///
    /// # Reboot port detection
    ///
    /// When a Linux guest panics and enters `emergency_restart()`, it writes
    /// to well-known I/O ports in a tight loop trying to reset the CPU:
    ///
    /// - **Port 0x64** (keyboard controller) with data `0xFE` — CPU reset command
    /// - **Port 0xCF9** (PCI reset control) with bit 2 set — hard reset
    /// - **Port 0x92** (fast reset) with bit 0 set — system reset
    ///
    /// Without intercepting these, the VMM would spin at 99% CPU forever.
    ///
    /// # Errors
    ///
    /// Returns [`VcpuError`] only if the underlying bus operation encounters
    /// an unrecoverable error. Currently all bus errors are silently ignored
    /// (returning `Continue`), matching typical VMM behavior for unhandled I/O.
    fn handle_exit(&mut self, exit: VmExit) -> Result<ExitAction, VcpuError> {
        match exit {
            VmExit::IoOut { port, data } => {
                if is_reboot_io(port, data.as_bytes()) {
                    return Ok(ExitAction::Stop);
                }
                let _ = self.pio_bus.write(u64::from(port), data.as_bytes());
                Ok(ExitAction::Continue)
            }
            VmExit::MmioWrite { addr, data } => {
                let _ = self.mmio_bus.write(addr, data.as_bytes());
                Ok(ExitAction::Continue)
            }
            VmExit::IoIn { .. } | VmExit::MmioRead { .. } | VmExit::Halt => {
                Ok(ExitAction::Continue)
            }
            VmExit::Shutdown | VmExit::Reboot => Ok(ExitAction::Stop),
        }
    }

    /// Reads from a PIO device into the KVM data buffer.
    ///
    /// Called by the run loop while the `kvm_run` mutable slice is live.
    /// If no device is registered at the port, `data` is filled with `0xFF`.
    fn handle_io_read(&mut self, port: u16, data: &mut [u8]) {
        if self.pio_bus.read(u64::from(port), data).is_none() {
            data.fill(0xFF);
        }
    }

    /// Reads from an MMIO device into the KVM data buffer.
    ///
    /// Called by the run loop while the `kvm_run` mutable slice is live.
    /// If no device is registered at the address, `data` is filled with `0xFF`.
    fn handle_mmio_read(&mut self, addr: u64, data: &mut [u8]) {
        if self.mmio_bus.read(addr, data).is_none() {
            data.fill(0xFF);
        }
    }
}

/// Checks whether an I/O write targets an x86 reboot port with the reset command byte.
///
/// Returns `true` if the guest is attempting a CPU reset via one of:
/// - Port 0x64 (keyboard controller) with data `0xFE`
/// - Port 0xCF9 (PCI reset control) with bit 2 set
/// - Port 0x92 (fast reset) with bit 0 set
///
/// Returns `false` if `data` is empty (no reset byte to inspect).
fn is_reboot_io(port: u16, data: &[u8]) -> bool {
    let Some(&byte) = data.first() else {
        return false;
    };
    match port {
        KBD_CMD_PORT => byte == KBD_RESET_CMD,
        PCI_RESET_PORT => byte & PCI_RESET_BIT != 0,
        FAST_RESET_PORT => byte & FAST_RESET_BIT != 0,
        _ => false,
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
