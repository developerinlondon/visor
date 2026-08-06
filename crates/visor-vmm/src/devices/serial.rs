//! UART 16550 serial device backed by visor-vmm's own emulator.
//!
//! [`SerialDevice`] wraps [`Uart16550`](super::uart::Uart16550) and implements
//! [`BusDevice`](crate::devices::bus::BusDevice) so it can be
//! registered on an I/O bus. The standard COM1 port occupies 8 bytes starting
//! at `0x3F8` with IRQ 4.
//!
//! Unlike the old `visor-machine` serial device, this one uses
//! [`InterruptEvent`](crate::platform::event::InterruptEvent) for interrupt
//! signaling instead of `EventFd`, enabling cross-platform support.
//!
//! # Example
//!
//! ```text
//! ┌──────────┐         ┌──────────────┐       ┌───────────┐
//! │  Guest   │ IoOut   │ Bus (PIO)    │       │  Serial   │
//! │  writes  │────────>│ 0x3F8..0x3FF │──────>│  Device   │──> output sink
//! │  0x3F8   │         └──────────────┘       └───────────┘
//! └──────────┘
//! ```

use std::io::Write;
use std::sync::Arc;

use crate::platform::event::InterruptEvent;

use super::uart::Uart16550;

/// COM1 I/O port base address.
pub const COM1_PORT_BASE: u16 = 0x3F8;

/// Number of I/O ports used by a UART 16550 device.
pub const COM1_PORT_COUNT: u64 = 8;

/// IRQ number for COM1.
pub const COM1_IRQ: u32 = 4;

/// Kernel console device name for the emulated serial port.
///
/// ARM64 uses PL011 UART (`ttyAMA0`), `x86_64` uses 8250/16550 (`ttyS0`).
/// Used by `visor-runtime` to construct the kernel command line.
#[cfg(target_arch = "aarch64")]
pub const CONSOLE_DEVICE_NAME: &str = "ttyAMA0";

/// Kernel console device name for the emulated serial port.
///
/// ARM64 uses PL011 UART (`ttyAMA0`), `x86_64` uses 8250/16550 (`ttyS0`).
/// Used by `visor-runtime` to construct the kernel command line.
#[cfg(not(target_arch = "aarch64"))]
pub const CONSOLE_DEVICE_NAME: &str = "ttyS0";

/// Early console kernel parameter for direct MMIO output before driver init.
///
/// ARM64 uses `earlycon=pl011,0x09000000` for the PL011 UART at its MMIO base.
/// `x86_64` uses an empty string (the 8250 earlycon is auto-detected).
/// Added to the kernel command line by `visor-runtime`.
#[cfg(target_arch = "aarch64")]
pub const EARLYCON_PARAM: &str = "earlycon=pl011,0x09000000";

/// Early console kernel parameter for direct MMIO output before driver init.
///
/// ARM64 uses `earlycon=pl011,0x09000000` for the PL011 UART at its MMIO base.
/// `x86_64` uses an empty string (the 8250 earlycon is auto-detected).
/// Added to the kernel command line by `visor-runtime`.
#[cfg(not(target_arch = "aarch64"))]
pub const EARLYCON_PARAM: &str = "";

/// A UART 16550 serial device backed by visor-vmm's own emulator.
///
/// Implements [`BusDevice`](crate::devices::bus::BusDevice) for
/// integration with the I/O bus. Only single-byte reads and writes are
/// forwarded to the underlying UART emulation; multi-byte accesses are
/// silently ignored (as is standard for 16550 register access).
pub struct SerialDevice {
    inner: Uart16550,
    interrupt: Arc<dyn InterruptEvent>,
}

impl SerialDevice {
    /// Creates a new serial device.
    ///
    /// `output` receives bytes written by the guest (THR → host).
    /// `interrupt` is used for IRQ signaling via KVM irqfd (or equivalent).
    #[must_use]
    pub fn new(output: Box<dyn Write + Send>, interrupt: Arc<dyn InterruptEvent>) -> Self {
        let inner = Uart16550::new(output, Arc::clone(&interrupt));
        Self { inner, interrupt }
    }

    /// Returns a reference to the interrupt event.
    ///
    /// Use this to wire up KVM irqfd: call `as_raw()` on the returned
    /// reference to get the file descriptor.
    #[must_use]
    pub fn interrupt_event(&self) -> &dyn InterruptEvent {
        &*self.interrupt
    }

    /// Enqueues a byte into the RX FIFO (host → guest input).
    ///
    /// Sets the Data Ready bit in LSR and raises an RX interrupt if enabled.
    pub fn enqueue_rx(&mut self, byte: u8) {
        self.inner.enqueue_rx(byte);
    }
}

impl crate::devices::bus::BusDevice for SerialDevice {
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        if data.len() != 1 {
            return;
        }
        if let Ok(off) = u8::try_from(offset) {
            data[0] = self.inner.read(off);
        }
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        if data.len() != 1 {
            return;
        }
        if let Ok(off) = u8::try_from(offset) {
            self.inner.write(off, data[0]);
        }
    }
}

#[cfg(test)]
#[path = "serial_test.rs"]
mod tests;
