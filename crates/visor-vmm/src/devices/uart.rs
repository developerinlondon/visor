//! Pure-Rust UART 16550 emulator.
//!
//! Replaces the `vm-superio` dependency with a minimal implementation
//! sufficient for VMM serial console I/O. Uses [`InterruptEvent`] for
//! cross-platform interrupt signaling.
//!
//! # Register Map (offsets 0–7)
//!
//! | Offset | DLAB=0 Read | DLAB=0 Write | DLAB=1 R/W |
//! |--------|-------------|--------------|------------|
//! | 0 | RBR | THR | DLL |
//! | 1 | IER | IER | DLH |
//! | 2 | IIR | FCR | IIR/FCR |
//! | 3 | LCR | LCR | LCR |
//! | 4 | MCR | MCR | MCR |
//! | 5 | LSR | — | LSR |
//! | 6 | MSR | — | MSR |
//! | 7 | SCR | SCR | SCR |
//!
//! # Interrupt model
//!
//! This emulator uses the same additive/subtractive IIR model as
//! `vm-superio`. Interrupt identification bits are **added** when an
//! interrupt condition is asserted and **deleted** when the condition is
//! cleared (e.g., on an IIR read). The `IIR_NONE` bit (bit 0 = 1) is
//! set only when all interrupt bits have been cleared. The Linux 8250
//! driver depends on this exact behavior.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::Arc;

use crate::platform::event::InterruptEvent;

// ── Register offsets ───────────────────────────────────────────────
const DATA_OFFSET: u8 = 0; // RBR (read) / THR (write) when DLAB=0, DLL when DLAB=1
const IER_OFFSET: u8 = 1; // IER when DLAB=0, DLH when DLAB=1
const IIR_OFFSET: u8 = 2; // IIR (read) / FCR (write)
const LCR_OFFSET: u8 = 3;
const MCR_OFFSET: u8 = 4;
const LSR_OFFSET: u8 = 5;
const MSR_OFFSET: u8 = 6;
const SCR_OFFSET: u8 = 7;

// ── FIFO size ────────────────────────────────────────────────────
const FIFO_SIZE: usize = 0x40;

// ── IER bits ──────────────────────────────────────────────────────
const IER_RDA_BIT: u8 = 0x01;
const IER_THR_EMPTY_BIT: u8 = 0x02;
const IER_VALID_BITS: u8 = 0x0F;

// ── IIR bits ─────────────────────────────────────────────────────
/// FIFO capability bits — returned in IIR to indicate 16550A mode.
/// The Linux 8250 driver uses these to detect the UART type.
const IIR_FIFO_BITS: u8 = 0xC0;
const IIR_NONE_BIT: u8 = 0x01; // No interrupt pending
const IIR_THR_EMPTY_BIT: u8 = 0x02;
const IIR_RDA_BIT: u8 = 0x04;

// ── LSR bits ──────────────────────────────────────────────────────
const LSR_DATA_READY_BIT: u8 = 0x01;
const LSR_EMPTY_THR_BIT: u8 = 0x20;
const LSR_IDLE_BIT: u8 = 0x40;

// ── MCR bits ──────────────────────────────────────────────────────
const MCR_LOOP_BIT: u8 = 0x10;
const MCR_OUT2_BIT: u8 = 0x08;

// ── MSR bits ──────────────────────────────────────────────────────
const MSR_CTS_BIT: u8 = 0x10;
const MSR_DSR_BIT: u8 = 0x20;
const MSR_DCD_BIT: u8 = 0x80;

// ── Defaults ─────────────────────────────────────────────────────
const DEFAULT_INTERRUPT_ENABLE: u8 = 0x00;
const DEFAULT_INTERRUPT_IDENTIFICATION: u8 = IIR_NONE_BIT;
const DEFAULT_LINE_STATUS: u8 = LSR_EMPTY_THR_BIT | LSR_IDLE_BIT;
const DEFAULT_LINE_CONTROL: u8 = 0x03;
const DEFAULT_MODEM_CONTROL: u8 = MCR_OUT2_BIT;
const DEFAULT_MODEM_STATUS: u8 = MSR_DSR_BIT | MSR_CTS_BIT | MSR_DCD_BIT;
const DEFAULT_BAUD_DIVISOR_LOW: u8 = 0x0C;
const DEFAULT_BAUD_DIVISOR_HIGH: u8 = 0x00;

// ── Uart16550 ─────────────────────────────────────────────────────

/// Pure-Rust UART 16550 emulator.
///
/// Provides register-level emulation of the 16550 UART for VMM serial
/// console I/O. Supports THR output, RBR/RX FIFO input, interrupt
/// generation via [`InterruptEvent`], and DLAB-based divisor latch access.
///
/// The interrupt model matches `vm-superio`'s additive/subtractive IIR
/// approach exactly, which is what the Linux 8250 kernel driver expects.
///
/// # Simplifications
///
/// This emulator skips features not needed in a VMM context:
/// - Actual baud rate handling (divisor values accepted but ignored)
/// - Break/parity/framing errors
/// - Hardware flow control and DMA mode
/// - FIFO trigger levels (always immediate)
pub struct Uart16550 {
    // Registers
    interrupt_enable: u8,
    interrupt_identification: u8,
    line_control: u8,
    modem_control: u8,
    line_status: u8,
    modem_status: u8,
    scratch: u8,
    baud_divisor_low: u8,
    baud_divisor_high: u8,

    // RX FIFO
    in_buffer: VecDeque<u8>,

    // I/O
    output: Box<dyn Write + Send>,
    interrupt_evt: Arc<dyn InterruptEvent>,
}

impl Uart16550 {
    /// Creates a new UART 16550 emulator.
    ///
    /// `output` receives bytes written to the THR register (guest → host).
    /// `interrupt` is triggered when interrupt conditions are met.
    #[must_use]
    pub fn new(output: Box<dyn Write + Send>, interrupt: Arc<dyn InterruptEvent>) -> Self {
        Self {
            interrupt_enable: DEFAULT_INTERRUPT_ENABLE,
            interrupt_identification: DEFAULT_INTERRUPT_IDENTIFICATION,
            line_control: DEFAULT_LINE_CONTROL,
            modem_control: DEFAULT_MODEM_CONTROL,
            line_status: DEFAULT_LINE_STATUS,
            modem_status: DEFAULT_MODEM_STATUS,
            scratch: 0,
            baud_divisor_low: DEFAULT_BAUD_DIVISOR_LOW,
            baud_divisor_high: DEFAULT_BAUD_DIVISOR_HIGH,
            in_buffer: VecDeque::new(),
            output,
            interrupt_evt: interrupt,
        }
    }

    /// Reads a register at the given offset (0–7).
    ///
    /// Returns 0 for unrecognized offsets.
    #[must_use]
    pub fn read(&mut self, offset: u8) -> u8 {
        match offset {
            DATA_OFFSET if self.is_dlab_set() => self.baud_divisor_low,
            DATA_OFFSET => self.read_rbr(),
            IER_OFFSET if self.is_dlab_set() => self.baud_divisor_high,
            IER_OFFSET => self.interrupt_enable,
            IIR_OFFSET => {
                // Enable FIFO capability bits (16550A detection by Linux 8250 driver).
                let iir = self.interrupt_identification | IIR_FIFO_BITS;
                // Reset IIR on read — standard 16550 behavior.
                self.reset_iir();
                iir
            }
            LCR_OFFSET => self.line_control,
            MCR_OFFSET => self.modem_control,
            LSR_OFFSET => self.line_status,
            MSR_OFFSET => self.modem_status,
            SCR_OFFSET => self.scratch,
            _ => 0,
        }
    }

    /// Writes a value to a register at the given offset (0–7).
    pub fn write(&mut self, offset: u8, value: u8) {
        match offset {
            DATA_OFFSET if self.is_dlab_set() => self.baud_divisor_low = value,
            DATA_OFFSET => {
                if self.is_in_loop_mode() {
                    // In loopback mode, THR output goes directly to RX FIFO.
                    if self.in_buffer.len() < FIFO_SIZE {
                        self.in_buffer.push_back(value);
                        self.set_lsr_rda_bit();
                        self.received_data_interrupt();
                    }
                } else {
                    // Send byte to output sink. Errors are silently ignored
                    // (matching real UART behavior — bytes are just lost).
                    let _ = self.output.write_all(&[value]);
                    let _ = self.output.flush();
                    // THR empty interrupt fires irrespective of write success
                    // (driver must not block waiting for output).
                    self.thr_empty_interrupt();
                }
            }
            IER_OFFSET if self.is_dlab_set() => self.baud_divisor_high = value,
            // Only enable interrupts valid on 16550A (and below).
            IER_OFFSET => self.interrupt_enable = value & IER_VALID_BITS,
            LCR_OFFSET => self.line_control = value,
            MCR_OFFSET => self.modem_control = value,
            SCR_OFFSET => self.scratch = value,
            // FCR and other offsets — accept but ignore.
            _ => {}
        }
    }

    /// Enqueues bytes into the RX FIFO (host → guest input).
    ///
    /// Returns the number of bytes actually enqueued. If the FIFO is already
    /// full when called with a non-empty slice, returns `None`.
    ///
    /// Sets the Data Ready bit in LSR and raises an RX interrupt if enabled.
    pub fn enqueue_raw_bytes(&mut self, input: &[u8]) -> Option<usize> {
        if self.is_in_loop_mode() {
            return Some(0);
        }
        if input.is_empty() {
            return Some(0);
        }
        let capacity = FIFO_SIZE.saturating_sub(self.in_buffer.len());
        if capacity == 0 {
            return None;
        }
        let count = capacity.min(input.len());
        self.in_buffer.extend(&input[..count]);
        self.set_lsr_rda_bit();
        self.received_data_interrupt();
        Some(count)
    }

    /// Enqueues a single byte into the RX FIFO (host → guest input).
    ///
    /// Convenience wrapper around [`enqueue_raw_bytes`](Self::enqueue_raw_bytes)
    /// for single-byte input. If the FIFO is full, the byte is silently dropped.
    pub fn enqueue_rx(&mut self, byte: u8) {
        let _ = self.enqueue_raw_bytes(&[byte]);
    }

    /// Returns how much space is available in the RX FIFO.
    #[must_use]
    pub fn fifo_capacity(&self) -> usize {
        FIFO_SIZE - self.in_buffer.len()
    }

    // ── Internal helpers (match vm-superio's model exactly) ───────────

    fn is_dlab_set(&self) -> bool {
        (self.line_control & 0x80) != 0
    }

    fn is_rda_interrupt_enabled(&self) -> bool {
        (self.interrupt_enable & IER_RDA_BIT) != 0
    }

    fn is_thr_interrupt_enabled(&self) -> bool {
        (self.interrupt_enable & IER_THR_EMPTY_BIT) != 0
    }

    fn is_in_loop_mode(&self) -> bool {
        (self.modem_control & MCR_LOOP_BIT) != 0
    }

    fn set_lsr_rda_bit(&mut self) {
        self.line_status |= LSR_DATA_READY_BIT;
    }

    fn clear_lsr_rda_bit(&mut self) {
        self.line_status &= !LSR_DATA_READY_BIT;
    }

    /// Adds interrupt identification bits (clears the NONE bit).
    fn add_interrupt(&mut self, interrupt_bits: u8) {
        self.interrupt_identification &= !IIR_NONE_BIT;
        self.interrupt_identification |= interrupt_bits;
    }

    /// Removes interrupt identification bits. If all cleared, sets NONE bit.
    fn del_interrupt(&mut self, interrupt_bits: u8) {
        self.interrupt_identification &= !interrupt_bits;
        if self.interrupt_identification == 0x00 {
            self.interrupt_identification = IIR_NONE_BIT;
        }
    }

    /// Fires the THR-empty interrupt if enabled and not already signaled.
    ///
    /// The deduplication check (`IIR_THR_EMPTY_BIT == 0`) prevents
    /// redundant interrupt triggers, matching vm-superio's behavior.
    fn thr_empty_interrupt(&mut self) {
        if self.is_thr_interrupt_enabled()
            && (self.interrupt_identification & IIR_THR_EMPTY_BIT == 0)
        {
            self.add_interrupt(IIR_THR_EMPTY_BIT);
            let _ = self.interrupt_evt.trigger();
        }
    }

    /// Fires the received-data-available interrupt if enabled and not already signaled.
    fn received_data_interrupt(&mut self) {
        if self.is_rda_interrupt_enabled() && (self.interrupt_identification & IIR_RDA_BIT == 0) {
            self.add_interrupt(IIR_RDA_BIT);
            let _ = self.interrupt_evt.trigger();
        }
    }

    /// Resets IIR to the default (no pending interrupts).
    fn reset_iir(&mut self) {
        self.interrupt_identification = DEFAULT_INTERRUPT_IDENTIFICATION;
    }

    /// Reads from the Receiver Buffer Register.
    fn read_rbr(&mut self) -> u8 {
        // Clear RDA interrupt identification on data read.
        self.del_interrupt(IIR_RDA_BIT);
        let byte = self.in_buffer.pop_front().unwrap_or(0);
        if self.in_buffer.is_empty() {
            self.clear_lsr_rda_bit();
        }
        byte
    }
}

#[cfg(test)]
#[path = "uart_test.rs"]
mod tests;
