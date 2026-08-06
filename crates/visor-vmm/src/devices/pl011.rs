//! PL011 UART emulator for ARM64 platforms.
//!
//! Implements the ARM `PrimeCell` UART (PL011) register interface as a
//! [`BusDevice`](super::bus::BusDevice). This is the standard UART on
//! ARM64 systems, replacing the 16550 used on `x86_64`.
//!
//! The FDT advertises `"arm,pl011"` at MMIO address `0x0900_0000` and
//! the kernel command line uses `earlycon=pl011,0x09000000`.
//!
//! # Register Map
//!
//! | Offset | Name        | Description                          |
//! |--------|-------------|--------------------------------------|
//! | 0x000  | UARTDR      | Data Register (RX read / TX write)   |
//! | 0x004  | UARTRSR     | Receive Status / Error Clear         |
//! | 0x018  | UARTFR      | Flag Register (read-only status)     |
//! | 0x024  | UARTIBRD    | Integer Baud Rate Divisor            |
//! | 0x028  | UARTFBRD    | Fractional Baud Rate Divisor         |
//! | 0x02C  | UARTLCR_H   | Line Control Register               |
//! | 0x030  | UARTCR      | Control Register                     |
//! | 0x034  | UARTIFLS    | Interrupt FIFO Level Select          |
//! | 0x038  | UARTIMSC    | Interrupt Mask Set/Clear             |
//! | 0x03C  | UARTRIS     | Raw Interrupt Status                 |
//! | 0x040  | UARTMIS     | Masked Interrupt Status              |
//! | 0x044  | UARTICR     | Interrupt Clear Register             |
//! | 0xFE0  | PeriphID0   | Peripheral ID byte 0 (0x11)          |
//! | 0xFE4  | PeriphID1   | Peripheral ID byte 1 (0x10)          |
//! | 0xFE8  | PeriphID2   | Peripheral ID byte 2 (0x14, r1p5)    |
//! | 0xFEC  | PeriphID3   | Peripheral ID byte 3 (0x00)          |
//! | 0xFF0  | CellID0     | PrimeCell ID byte 0 (0x0D)           |
//! | 0xFF4  | CellID1     | PrimeCell ID byte 1 (0xF0)           |
//! | 0xFF8  | CellID2     | PrimeCell ID byte 2 (0x05)           |
//! | 0xFFC  | CellID3     | PrimeCell ID byte 3 (0xB1)           |

use std::io::Write;
use std::sync::Arc;

use crate::devices::bus::BusDevice;
use crate::platform::event::InterruptEvent;

// ── Register offsets ────────────────────────────────────────────────

/// Data Register — write to TX, read for RX.
pub const UARTDR: u64 = 0x000;

/// Receive Status / Error Clear Register.
pub const UARTRSR: u64 = 0x004;

/// Flag Register (read-only) — TX/RX FIFO status.
pub const UARTFR: u64 = 0x018;

/// Integer Baud Rate Divisor.
pub const UARTIBRD: u64 = 0x024;

/// Fractional Baud Rate Divisor.
pub const UARTFBRD: u64 = 0x028;

/// Line Control Register (word length, parity, FIFOs).
pub const UARTLCR_H: u64 = 0x02C;

/// Control Register (enable, TX/RX enable).
pub const UARTCR: u64 = 0x030;

/// Interrupt FIFO Level Select.
pub const UARTIFLS: u64 = 0x034;

/// Interrupt Mask Set/Clear.
pub const UARTIMSC: u64 = 0x038;

/// Raw Interrupt Status.
pub const UARTRIS: u64 = 0x03C;

/// Masked Interrupt Status (RIS & IMSC).
pub const UARTMIS: u64 = 0x040;

/// Interrupt Clear Register (write-only).
pub const UARTICR: u64 = 0x044;

// ── Identification registers ────────────────────────────────────────

/// Peripheral ID byte 0.
pub const PERIPH_ID0: u64 = 0xFE0;
/// Peripheral ID byte 1.
pub const PERIPH_ID1: u64 = 0xFE4;
/// Peripheral ID byte 2.
pub const PERIPH_ID2: u64 = 0xFE8;
/// Peripheral ID byte 3.
pub const PERIPH_ID3: u64 = 0xFEC;

/// `PrimeCell` ID byte 0.
pub const CELL_ID0: u64 = 0xFF0;
/// `PrimeCell` ID byte 1.
pub const CELL_ID1: u64 = 0xFF4;
/// `PrimeCell` ID byte 2.
pub const CELL_ID2: u64 = 0xFF8;
/// `PrimeCell` ID byte 3.
pub const CELL_ID3: u64 = 0xFFC;

// ── Flag Register bits ──────────────────────────────────────────────

/// TX FIFO empty — always 1 (we consume writes immediately).
const FR_TXFE: u32 = 1 << 7;
/// RX FIFO empty — always 1 (no pending RX data in this minimal impl).
const FR_RXFE: u32 = 1 << 4;

/// Default UARTFR value: TX empty + RX empty = ready to write.
const DEFAULT_FLAGS: u32 = FR_TXFE | FR_RXFE;

// ── PL011 identification values ─────────────────────────────────────

const PERIPH_ID: [u32; 4] = [0x11, 0x10, 0x14, 0x00];
const CELL_ID: [u32; 4] = [0x0D, 0xF0, 0x05, 0xB1];

// ── Pl011 device ────────────────────────────────────────────────────

/// ARM PL011 UART emulator.
///
/// Provides register-level emulation of the PL011 UART for ARM64 VMM
/// serial console I/O. Writes to `UARTDR` are forwarded to the output
/// sink. Control and baud rate registers store values but have no
/// functional effect (virtual UART needs no real timing).
///
/// Implements [`BusDevice`] for registration on an MMIO bus.
pub struct Pl011 {
    /// TX output sink (guest → host).
    output: Box<dyn Write + Send>,
    /// Interrupt event for IRQ signaling.
    interrupt: Arc<dyn InterruptEvent>,

    // ── Stored register state ───────────────────────────────────────
    /// Control Register (UARTCR).
    control: u32,
    /// Line Control Register (`UARTLCR_H`).
    line_control: u32,
    /// Integer Baud Rate Divisor (UARTIBRD).
    int_baud: u32,
    /// Fractional Baud Rate Divisor (UARTFBRD).
    frac_baud: u32,
    /// Interrupt Mask Set/Clear (UARTIMSC).
    int_mask: u32,
    /// Raw Interrupt Status (UARTRIS).
    raw_int_status: u32,
    /// Interrupt FIFO Level Select (UARTIFLS).
    ifls: u32,
}

impl Pl011 {
    /// Creates a new PL011 UART emulator.
    ///
    /// `output` receives bytes written to `UARTDR` (guest → host TX).
    /// `interrupt` is triggered when interrupt conditions are met.
    #[must_use]
    pub fn new(output: Box<dyn Write + Send>, interrupt: Arc<dyn InterruptEvent>) -> Self {
        Self {
            output,
            interrupt,
            control: 0x0300, // TXE + RXE enabled by default
            line_control: 0,
            int_baud: 0,
            frac_baud: 0,
            int_mask: 0,
            raw_int_status: 0,
            ifls: 0x12, // Default: 1/2 full trigger level
        }
    }

    /// Returns a reference to the interrupt event.
    ///
    /// Use this to wire up irqfd: call `as_raw()` on the returned
    /// reference to get the file descriptor.
    #[must_use]
    pub fn interrupt_event(&self) -> &dyn InterruptEvent {
        &*self.interrupt
    }

    /// Reads a PL011 register at the given byte offset.
    ///
    /// Returns the 32-bit register value, or 0 for unrecognized offsets.
    #[must_use]
    fn read_register(&mut self, offset: u64) -> u32 {
        match offset {
            // No RX data / no errors — same as unrecognized (0).
            // UARTDR and UARTRSR intentionally fall through to wildcard.
            UARTFR => DEFAULT_FLAGS, // Always ready: TXFE | RXFE = 0x90
            UARTIBRD => self.int_baud,
            UARTFBRD => self.frac_baud,
            UARTLCR_H => self.line_control,
            UARTCR => self.control,
            UARTIFLS => self.ifls,
            UARTIMSC => self.int_mask,
            UARTRIS => self.raw_int_status,
            UARTMIS => self.raw_int_status & self.int_mask,

            // Identification registers (read-only)
            PERIPH_ID0 => PERIPH_ID[0],
            PERIPH_ID1 => PERIPH_ID[1],
            PERIPH_ID2 => PERIPH_ID[2],
            PERIPH_ID3 => PERIPH_ID[3],
            CELL_ID0 => CELL_ID[0],
            CELL_ID1 => CELL_ID[1],
            CELL_ID2 => CELL_ID[2],
            CELL_ID3 => CELL_ID[3],

            _ => 0,
        }
    }

    /// Writes a PL011 register at the given byte offset.
    fn write_register(&mut self, offset: u64, value: u32) {
        match offset {
            UARTDR => {
                // TX: send lowest byte to output sink.
                let byte = (value & 0xFF) as u8;
                let _ = self.output.write_all(&[byte]);
                let _ = self.output.flush();
            }
            // UARTRSR: any write clears error flags — no-op in minimal impl.
            // Intentionally falls through to wildcard.
            UARTIBRD => self.int_baud = value,
            UARTFBRD => self.frac_baud = value & 0x3F, // 6-bit field
            UARTLCR_H => self.line_control = value,
            UARTCR => self.control = value,
            UARTIFLS => self.ifls = value,
            UARTIMSC => self.int_mask = value,
            UARTICR => {
                // Clear the specified interrupt bits from raw status.
                self.raw_int_status &= !value;
            }
            // UARTFR, UARTRIS, UARTMIS, PeriphID*, CellID* are read-only.
            _ => {}
        }
    }
}

impl BusDevice for Pl011 {
    /// Reads from a PL011 register into `data`.
    ///
    /// Supports 1-byte and 4-byte reads. The 32-bit register value is
    /// written in little-endian byte order, truncated to `data.len()`.
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        let val = self.read_register(offset);
        let bytes = val.to_le_bytes();
        let len = data.len().min(bytes.len());
        data[..len].copy_from_slice(&bytes[..len]);
    }

    /// Writes to a PL011 register from `data`.
    ///
    /// Supports 1-byte and 4-byte writes. The value is interpreted as
    /// little-endian, zero-extended if fewer than 4 bytes are provided.
    fn write(&mut self, offset: u64, data: &[u8]) {
        let mut bytes = [0u8; 4];
        let len = data.len().min(4);
        bytes[..len].copy_from_slice(&data[..len]);
        let val = u32::from_le_bytes(bytes);
        self.write_register(offset, val);
    }
}

#[cfg(test)]
#[path = "pl011_test.rs"]
mod tests;
