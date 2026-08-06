use super::*;
use crate::platform::event::MockInterruptEvent;
use std::sync::Arc;
use std::sync::atomic::Ordering;

/// Helper: create a Uart16550 with a Vec<u8> output and mock interrupt.
fn make_uart() -> (
    Uart16550,
    Arc<MockInterruptEvent>,
    Arc<std::sync::Mutex<Vec<u8>>>,
) {
    let interrupt = Arc::new(MockInterruptEvent::new());
    let output_buf = Arc::new(std::sync::Mutex::new(Vec::<u8>::new()));

    // Create a writer that appends to the shared buffer.
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
    let uart = Uart16550::new(writer, interrupt.clone());
    (uart, interrupt, output_buf)
}

// ── Register basics ──────────────────────────────────────────────

#[test]
fn register_read_write_roundtrip() {
    let (mut uart, _, _) = make_uart();
    // SCR is at offset 7, should be read/write with no side effects.
    uart.write(SCR_OFFSET, 0xAB);
    assert_eq!(uart.read(SCR_OFFSET), 0xAB);
    uart.write(SCR_OFFSET, 0x42);
    assert_eq!(uart.read(SCR_OFFSET), 0x42);
}

// ── THR / output ─────────────────────────────────────────────────

#[test]
fn thr_write_sends_to_output() {
    let (mut uart, _, output) = make_uart();
    // Write a byte to THR (offset 0, DLAB=0).
    uart.write(DATA_OFFSET, b'H');
    uart.write(DATA_OFFSET, b'i');
    let buf = output.lock().unwrap();
    assert_eq!(&buf[..], b"Hi");
}

// ── RX FIFO ──────────────────────────────────────────────────────

#[test]
fn rx_fifo_enqueue_dequeue() {
    let (mut uart, _, _) = make_uart();
    uart.enqueue_rx(0x41);
    // LSR bit 0 should be set (Data Ready).
    assert_ne!(
        uart.read(LSR_OFFSET) & LSR_DATA_READY_BIT,
        0,
        "LSR Data Ready should be set"
    );
    // Read RBR (offset 0, DLAB=0).
    let byte = uart.read(DATA_OFFSET);
    assert_eq!(byte, 0x41);
    // After reading, if FIFO is empty, Data Ready should be cleared.
    assert_eq!(
        uart.read(LSR_OFFSET) & LSR_DATA_READY_BIT,
        0,
        "LSR Data Ready should be cleared"
    );
}

// ── IIR / interrupts ─────────────────────────────────────────────

#[test]
fn iir_priority_thre_over_none() {
    let (mut uart, _, _) = make_uart();
    // Enable THR Empty interrupt (IER bit 1).
    uart.write(IER_OFFSET, IER_THR_EMPTY_BIT);
    // Write to THR — should trigger THR Empty interrupt.
    uart.write(DATA_OFFSET, b'X');
    // IIR should show THR Empty (0x02) with FIFO bits.
    let iir = uart.read(IIR_OFFSET);
    assert_eq!(
        iir,
        IIR_THR_EMPTY_BIT | IIR_FIFO_BITS,
        "IIR should indicate THR Empty with FIFO bits"
    );
    // After reading IIR, identification should reset to NONE.
    assert_eq!(
        uart.interrupt_identification,
        DEFAULT_INTERRUPT_IDENTIFICATION
    );
}

#[test]
fn interrupt_fires_on_thr_write() {
    let (mut uart, interrupt, _) = make_uart();
    // Enable THR Empty interrupt (IER bit 1).
    uart.write(IER_OFFSET, IER_THR_EMPTY_BIT);
    let before = interrupt.trigger_count.load(Ordering::SeqCst);
    // Write to THR — should fire interrupt.
    uart.write(DATA_OFFSET, b'A');
    let after = interrupt.trigger_count.load(Ordering::SeqCst);
    assert!(after > before, "interrupt should have fired on THR write");
}

#[test]
fn thr_interrupt_dedup() {
    // vm-superio only fires the THR interrupt if the IIR_THR_EMPTY_BIT
    // is not already set. Writing two bytes without reading IIR should
    // fire only once.
    let (mut uart, interrupt, _) = make_uart();
    uart.write(IER_OFFSET, IER_THR_EMPTY_BIT);
    let before = interrupt.trigger_count.load(Ordering::SeqCst);
    uart.write(DATA_OFFSET, b'A');
    uart.write(DATA_OFFSET, b'B');
    let after = interrupt.trigger_count.load(Ordering::SeqCst);
    assert_eq!(
        after - before,
        1,
        "THR interrupt should fire only once (dedup)"
    );
}

#[test]
fn rda_interrupt_fires_on_enqueue() {
    let (mut uart, interrupt, _) = make_uart();
    uart.write(IER_OFFSET, IER_RDA_BIT);
    let before = interrupt.trigger_count.load(Ordering::SeqCst);
    uart.enqueue_rx(b'Z');
    let after = interrupt.trigger_count.load(Ordering::SeqCst);
    assert!(after > before, "RDA interrupt should fire on enqueue");
    // IIR should show RDA.
    let iir = uart.read(IIR_OFFSET);
    assert_ne!(iir & IIR_RDA_BIT, 0, "IIR should indicate RDA");
}

#[test]
fn rda_interrupt_cleared_on_data_read() {
    let (mut uart, _, _) = make_uart();
    uart.write(IER_OFFSET, IER_RDA_BIT);
    uart.enqueue_raw_bytes(&[b'a', b'b', b'c']).unwrap();

    // Read all three bytes — RDA should be cleared after each read.
    for &expected in &[b'a', b'b', b'c'] {
        assert_ne!(
            uart.read(LSR_OFFSET) & LSR_DATA_READY_BIT,
            0,
            "data ready should be set"
        );
        assert_eq!(uart.read(DATA_OFFSET), expected);
        // After reading a byte, the RDA interrupt bit should be cleared.
        assert_eq!(
            uart.interrupt_identification, DEFAULT_INTERRUPT_IDENTIFICATION,
            "IIR should reset after reading data"
        );
    }
    // After all bytes read, LSR data ready should be cleared.
    assert_eq!(uart.read(LSR_OFFSET) & LSR_DATA_READY_BIT, 0);
}

// ── DLAB ─────────────────────────────────────────────────────────

#[test]
fn dlab_switches_register_meaning() {
    let (mut uart, _, _) = make_uart();
    // Set IER to a known value first (offset 1, DLAB=0).
    uart.write(IER_OFFSET, 0x03);
    assert_eq!(uart.read(IER_OFFSET) & IER_VALID_BITS, 0x03);

    // Set DLAB (LCR bit 7).
    uart.write(LCR_OFFSET, 0x80);
    // Now offset 0 = DLL, offset 1 = DLH.
    uart.write(DATA_OFFSET, 0x0C); // DLL
    uart.write(IER_OFFSET, 0x00); // DLH
    assert_eq!(uart.read(DATA_OFFSET), 0x0C);
    assert_eq!(uart.read(IER_OFFSET), 0x00);

    // Clear DLAB.
    uart.write(LCR_OFFSET, 0x00);
    // IER should still be 0x03 (DLAB writes didn't affect it).
    assert_eq!(uart.read(IER_OFFSET) & IER_VALID_BITS, 0x03);
}

// ── LSR Data Ready ───────────────────────────────────────────────

#[test]
fn lsr_data_ready_set_on_enqueue() {
    let (mut uart, _, _) = make_uart();
    // Initially LSR Data Ready should be 0.
    assert_eq!(uart.read(LSR_OFFSET) & LSR_DATA_READY_BIT, 0);
    uart.enqueue_rx(0x55);
    assert_ne!(
        uart.read(LSR_OFFSET) & LSR_DATA_READY_BIT,
        0,
        "LSR Data Ready should be set after enqueue"
    );
}

#[test]
fn lsr_data_ready_cleared_on_read() {
    let (mut uart, _, _) = make_uart();
    uart.enqueue_rx(0x55);
    assert_ne!(uart.read(LSR_OFFSET) & LSR_DATA_READY_BIT, 0);
    // Read the byte from RBR.
    let _byte = uart.read(DATA_OFFSET);
    // FIFO is now empty, Data Ready should be cleared.
    assert_eq!(
        uart.read(LSR_OFFSET) & LSR_DATA_READY_BIT,
        0,
        "LSR Data Ready should be cleared after RBR read"
    );
}

// ── FIFO capacity ────────────────────────────────────────────────

#[test]
fn fifo_capacity_and_overflow() {
    let (mut uart, _, _) = make_uart();
    assert_eq!(uart.fifo_capacity(), FIFO_SIZE);

    // Fill the FIFO completely.
    let data: Vec<u8> = (0..FIFO_SIZE as u8).collect();
    let written = uart.enqueue_raw_bytes(&data).unwrap();
    assert_eq!(written, FIFO_SIZE);
    assert_eq!(uart.fifo_capacity(), 0);

    // Attempting to enqueue more should return None (full).
    assert!(uart.enqueue_raw_bytes(&[0xFF]).is_none());

    // Read one byte to free space.
    let _ = uart.read(DATA_OFFSET);
    assert_eq!(uart.fifo_capacity(), 1);
    assert_eq!(uart.enqueue_raw_bytes(&[0xFF]).unwrap(), 1);
}

// ── Default register values ──────────────────────────────────────

#[test]
fn default_register_values() {
    let (mut uart, _, _) = make_uart();
    // LSR should have THR Empty + Idle bits set.
    assert_eq!(uart.read(LSR_OFFSET), DEFAULT_LINE_STATUS);
    // LCR default: 8-bit word length.
    assert_eq!(uart.read(LCR_OFFSET), DEFAULT_LINE_CONTROL);
    // MCR default: OUT2 set.
    assert_eq!(uart.read(MCR_OFFSET), DEFAULT_MODEM_CONTROL);
    // MSR default: DSR + CTS + DCD.
    assert_eq!(uart.read(MSR_OFFSET), DEFAULT_MODEM_STATUS);
    // IER default: no interrupts.
    assert_eq!(uart.read(IER_OFFSET), DEFAULT_INTERRUPT_ENABLE);
    // IIR default: no pending + FIFO bits (FIFO bits are added on read).
    assert_eq!(uart.read(IIR_OFFSET), IIR_NONE_BIT | IIR_FIFO_BITS);
}

// ── IER write does NOT trigger interrupt ─────────────────────────

#[test]
fn ier_write_does_not_trigger_interrupt() {
    let (mut uart, interrupt, _) = make_uart();
    let before = interrupt.trigger_count.load(Ordering::SeqCst);
    // Simply enabling THR interrupt in IER should NOT fire an interrupt.
    uart.write(IER_OFFSET, IER_THR_EMPTY_BIT);
    let after = interrupt.trigger_count.load(Ordering::SeqCst);
    assert_eq!(
        before, after,
        "writing to IER should not trigger an interrupt"
    );
}
