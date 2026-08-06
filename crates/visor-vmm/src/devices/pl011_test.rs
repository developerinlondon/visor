use super::*;
use crate::devices::bus::BusDevice;
use crate::platform::event::MockInterruptEvent;
use std::sync::Arc;

/// Helper: create a Pl011 with a Vec<u8> output sink and mock interrupt.
fn make_pl011() -> (
    Pl011,
    Arc<MockInterruptEvent>,
    Arc<std::sync::Mutex<Vec<u8>>>,
) {
    let interrupt = Arc::new(MockInterruptEvent::new());
    let output_buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));

    struct SharedWriter(Arc<std::sync::Mutex<Vec<u8>>>);
    impl std::io::Write for SharedWriter {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    let writer = Box::new(SharedWriter(output_buf.clone()));
    let pl011 = Pl011::new(writer, interrupt.clone());
    (pl011, interrupt, output_buf)
}

/// Helper: write a 32-bit value to a BusDevice offset.
fn bus_write_u32(dev: &mut Pl011, offset: u64, val: u32) {
    let data = val.to_le_bytes();
    dev.write(offset, &data);
}

/// Helper: read a 32-bit value from a BusDevice offset.
fn bus_read_u32(dev: &mut Pl011, offset: u64) -> u32 {
    let mut data = [0u8; 4];
    dev.read(offset, &mut data);
    u32::from_le_bytes(data)
}

// ── UARTDR (0x00) — Data Register ───────────────────────────────────

#[test]
fn write_uartdr_sends_byte_to_output() {
    let (mut pl011, _, output) = make_pl011();
    bus_write_u32(&mut pl011, UARTDR, u32::from(b'V'));
    bus_write_u32(&mut pl011, UARTDR, u32::from(b'M'));
    let buf = output.lock().unwrap();
    assert_eq!(&buf[..], b"VM");
}

#[test]
fn read_uartdr_returns_zero_when_rx_empty() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, UARTDR), 0);
}

// ── UARTFR (0x18) — Flag Register ──────────────────────────────────

#[test]
fn read_uartfr_returns_tx_empty_rx_empty() {
    let (mut pl011, _, _) = make_pl011();
    let fr = bus_read_u32(&mut pl011, UARTFR);
    // TXFE (bit 7) | RXFE (bit 4) = 0x90
    assert_eq!(fr, 0x90, "UARTFR should indicate TX empty + RX empty");
}

// ── UARTCR (0x30) — Control Register ────────────────────────────────

#[test]
fn write_read_uartcr_roundtrip() {
    let (mut pl011, _, _) = make_pl011();
    bus_write_u32(&mut pl011, UARTCR, 0x0301); // UARTEN | TXE | RXE
    assert_eq!(bus_read_u32(&mut pl011, UARTCR), 0x0301);
}

// ── UARTLCR_H (0x2C) — Line Control ────────────────────────────────

#[test]
fn write_read_uartlcr_h_roundtrip() {
    let (mut pl011, _, _) = make_pl011();
    bus_write_u32(&mut pl011, UARTLCR_H, 0x60); // 8N1
    assert_eq!(bus_read_u32(&mut pl011, UARTLCR_H), 0x60);
}

// ── UARTIBRD / UARTFBRD (0x24, 0x28) — Baud Rate ───────────────────

#[test]
fn write_read_baud_rate_roundtrip() {
    let (mut pl011, _, _) = make_pl011();
    bus_write_u32(&mut pl011, UARTIBRD, 26);
    bus_write_u32(&mut pl011, UARTFBRD, 3);
    assert_eq!(bus_read_u32(&mut pl011, UARTIBRD), 26);
    assert_eq!(bus_read_u32(&mut pl011, UARTFBRD), 3);
}

// ── Peripheral ID registers ─────────────────────────────────────────

#[test]
fn read_periph_id0_returns_0x11() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, PERIPH_ID0), 0x11);
}

#[test]
fn read_periph_id1_returns_0x10() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, PERIPH_ID1), 0x10);
}

#[test]
fn read_periph_id2_returns_0x14() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, PERIPH_ID2), 0x14);
}

#[test]
fn read_periph_id3_returns_0x00() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, PERIPH_ID3), 0x00);
}

// ── Cell ID registers ───────────────────────────────────────────────

#[test]
fn read_cell_id0_returns_0x0d() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, CELL_ID0), 0x0D);
}

#[test]
fn read_cell_id1_returns_0xf0() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, CELL_ID1), 0xF0);
}

#[test]
fn read_cell_id2_returns_0x05() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, CELL_ID2), 0x05);
}

#[test]
fn read_cell_id3_returns_0xb1() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, CELL_ID3), 0xB1);
}

// ── UARTICR (0x44) — Interrupt Clear ────────────────────────────────

#[test]
fn write_uarticr_clears_raw_interrupt_status() {
    let (mut pl011, _, _) = make_pl011();
    // Simulate some raw interrupt bits set.
    bus_write_u32(&mut pl011, UARTIMSC, 0x30); // Enable TX + RX interrupts
    // Write to ICR to clear all interrupts.
    bus_write_u32(&mut pl011, UARTICR, 0x7FF);
    // RIS should be zero after clear.
    assert_eq!(bus_read_u32(&mut pl011, UARTRIS), 0);
}

// ── UARTIMSC (0x38) — Interrupt Mask Set/Clear ──────────────────────

#[test]
fn write_read_uartimsc_roundtrip() {
    let (mut pl011, _, _) = make_pl011();
    bus_write_u32(&mut pl011, UARTIMSC, 0x50);
    assert_eq!(bus_read_u32(&mut pl011, UARTIMSC), 0x50);
}

// ── UARTMIS (0x40) — Masked Interrupt Status ────────────────────────

#[test]
fn read_uartmis_returns_masked_status() {
    let (mut pl011, _, _) = make_pl011();
    // With no raw interrupts and no mask, MIS should be 0.
    assert_eq!(bus_read_u32(&mut pl011, UARTMIS), 0);
}

// ── Unknown offset ──────────────────────────────────────────────────

#[test]
fn unknown_offset_read_returns_zero() {
    let (mut pl011, _, _) = make_pl011();
    // Read from an offset not in the register map.
    assert_eq!(bus_read_u32(&mut pl011, 0x100), 0);
}

#[test]
fn unknown_offset_write_is_ignored() {
    let (mut pl011, _, _) = make_pl011();
    // Should not panic or corrupt state.
    bus_write_u32(&mut pl011, 0x100, 0xDEAD);
    // Verify other registers are unaffected.
    assert_eq!(bus_read_u32(&mut pl011, UARTFR), 0x90);
}

// ── UARTRSR (0x04) — Receive Status / Error Clear ───────────────────

#[test]
fn read_uartrsr_returns_zero() {
    let (mut pl011, _, _) = make_pl011();
    assert_eq!(bus_read_u32(&mut pl011, UARTRSR), 0);
}

#[test]
fn write_uartrsr_clears_errors() {
    let (mut pl011, _, _) = make_pl011();
    // Write any value to clear (no actual errors in this minimal impl).
    bus_write_u32(&mut pl011, UARTRSR, 0xFF);
    assert_eq!(bus_read_u32(&mut pl011, UARTRSR), 0);
}

// ── UARTIFLS (0x34) — Interrupt FIFO Level Select ───────────────────

#[test]
fn write_read_uartifls_roundtrip() {
    let (mut pl011, _, _) = make_pl011();
    bus_write_u32(&mut pl011, UARTIFLS, 0x12);
    assert_eq!(bus_read_u32(&mut pl011, UARTIFLS), 0x12);
}

// ── BusDevice trait conformance ─────────────────────────────────────

#[test]
fn bus_device_non_4byte_read_fills_zeros() {
    let (mut pl011, _, _) = make_pl011();
    // Single-byte read should still work (return low byte of register).
    let mut data = [0xFFu8; 1];
    pl011.read(UARTFR, &mut data);
    assert_eq!(data[0], 0x90_u32.to_le_bytes()[0]);
}
