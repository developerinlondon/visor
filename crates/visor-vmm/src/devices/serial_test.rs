use std::io::Write;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use crate::devices::bus::BusDevice;

use crate::platform::event::{InterruptEvent, MockInterruptEvent};

use super::{COM1_IRQ, COM1_PORT_BASE, COM1_PORT_COUNT, SerialDevice};

/// A `Write` implementation that appends to a shared `Vec<u8>`.
#[derive(Clone)]
struct SharedSink(Arc<Mutex<Vec<u8>>>);

impl Write for SharedSink {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Helper to create a `SerialDevice` backed by an in-memory output buffer.
fn make_serial() -> (SerialDevice, Arc<MockInterruptEvent>, Arc<Mutex<Vec<u8>>>) {
    let output: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = SharedSink(Arc::clone(&output));
    let interrupt = Arc::new(MockInterruptEvent::new());
    let dev = SerialDevice::new(Box::new(sink), Arc::clone(&interrupt) as Arc<_>);
    (dev, interrupt, output)
}

// ── Constants ────────────────────────────────────────────────────

#[test]
fn constants_match_com1_spec() {
    assert_eq!(COM1_PORT_BASE, 0x3F8);
    assert_eq!(COM1_PORT_COUNT, 8);
    assert_eq!(COM1_IRQ, 4);
}

// ── Serial Output Tests ─────────────────────────────────────────

#[test]
fn write_data_offset_produces_output() {
    let (mut dev, _, output) = make_serial();

    // Write byte 'H' to data register (offset 0)
    dev.write(0, b"H");
    dev.write(0, b"i");

    let captured = output.lock().unwrap();
    assert_eq!(&*captured, b"Hi");
}

#[test]
fn write_multiple_bytes_to_data_register() {
    let (mut dev, _, output) = make_serial();

    let message = b"Hello, serial!\n";
    for &byte in message {
        dev.write(0, &[byte]);
    }

    let captured = output.lock().unwrap();
    assert_eq!(&*captured, message);
}

// ── LSR Tests ───────────────────────────────────────────────────

#[test]
fn read_line_status_register() {
    let (mut dev, _, _) = make_serial();

    // Offset 5 is the Line Status Register (LSR).
    // Bit 5 (THR empty) and bit 6 (transmitter empty) should be set
    // when no data is pending.
    let mut buf = [0u8];
    dev.read(5, &mut buf);
    assert_ne!(buf[0] & 0x20, 0, "THRE bit should be set in LSR");
}

// ── IER Tests ───────────────────────────────────────────────────

#[test]
fn write_then_read_ier() {
    let (mut dev, _, _) = make_serial();

    // Write to IER (offset 1) to enable Received Data Available interrupt
    dev.write(1, &[0x01]);

    // Read back IER
    let mut buf = [0u8];
    dev.read(1, &mut buf);
    assert_eq!(buf[0] & 0x01, 0x01, "IER RDA bit should be set");
}

// ── BusDevice boundary tests ────────────────────────────────────

#[test]
fn bus_device_read_with_wrong_size_returns_unchanged() {
    let (mut dev, _, _) = make_serial();

    // Multi-byte read — serial only responds to 1-byte reads, should no-op
    let mut buf = [0xFFu8; 2];
    dev.read(0, &mut buf);
    assert_eq!(buf, [0xFF, 0xFF], "multi-byte read should be ignored");
}

#[test]
fn bus_device_write_with_wrong_size_is_ignored() {
    let (mut dev, _, output) = make_serial();

    // Multi-byte write — serial only accepts 1-byte writes, should be ignored
    dev.write(0, b"AB");

    let captured = output.lock().unwrap();
    assert!(captured.is_empty(), "multi-byte write should be ignored");
}

// ── Interrupt signaling ─────────────────────────────────────────

#[test]
fn interrupt_fires_on_thr_write_with_ier_enabled() {
    let (mut dev, interrupt, _) = make_serial();

    // Enable THR Empty interrupt (IER bit 1)
    dev.write(1, &[0x02]);
    let before = interrupt.trigger_count.load(Ordering::SeqCst);

    // Write to THR — should fire interrupt
    dev.write(0, &[b'A']);
    let after = interrupt.trigger_count.load(Ordering::SeqCst);
    assert!(after > before, "interrupt should have fired on THR write");
}

#[test]
fn interrupt_event_accessor_returns_valid_ref() {
    let (dev, interrupt, _) = make_serial();
    // as_raw() on the mock returns -1
    assert_eq!(dev.interrupt_event().as_raw(), interrupt.as_raw());
}

// ── RX path through BusDevice ───────────────────────────────────

#[test]
fn enqueue_rx_then_read_via_bus_device() {
    let (mut dev, _, _) = make_serial();

    // Enable RX Available interrupt (IER bit 0)
    dev.write(1, &[0x01]);

    // Enqueue a byte via the inner UART
    dev.enqueue_rx(0x41);

    // Read LSR — Data Ready should be set
    let mut lsr = [0u8];
    dev.read(5, &mut lsr);
    assert_ne!(lsr[0] & 0x01, 0, "LSR Data Ready should be set");

    // Read from RBR via BusDevice
    let mut rbr = [0u8];
    dev.read(0, &mut rbr);
    assert_eq!(rbr[0], 0x41);
}

// ── Offset overflow safety ──────────────────────────────────────

#[test]
fn bus_device_read_with_large_offset_returns_zero() {
    let (mut dev, _, _) = make_serial();

    // Offset > u8::MAX should be silently ignored
    let mut buf = [0xFFu8];
    dev.read(256, &mut buf);
    assert_eq!(buf[0], 0xFF, "read at offset 256 should be no-op");
}

#[test]
fn bus_device_write_with_large_offset_is_noop() {
    let (mut dev, _, output) = make_serial();

    // Offset > u8::MAX should be silently ignored
    dev.write(256, &[b'X']);

    let captured = output.lock().unwrap();
    assert!(captured.is_empty(), "write at offset 256 should be no-op");
}
