use std::sync::{Arc, Mutex};

use super::*;

/// A dummy device that does nothing (default no-op read/write).
struct DummyDevice;

impl BusDevice for DummyDevice {
    fn read(&mut self, _offset: u64, _data: &mut [u8]) {}
    fn write(&mut self, _offset: u64, _data: &[u8]) {}
}

/// A device that fills reads with sequential bytes from offset and
/// verifies writes contain the expected sequence.
struct CountingDevice;

impl BusDevice for CountingDevice {
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        #[allow(clippy::cast_possible_truncation)]
        for (i, byte) in data.iter_mut().enumerate() {
            *byte = (offset as u8).wrapping_add(i as u8);
        }
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        #[allow(clippy::cast_possible_truncation)]
        for (i, byte) in data.iter().enumerate() {
            assert_eq!(*byte, (offset as u8).wrapping_add(i as u8));
        }
    }
}

// ── Registration Tests ────────────────────────────────────────────────

#[test]
fn register_single_device() {
    let mut bus = Bus::new();
    let dev = Arc::new(Mutex::new(DummyDevice));
    bus.register(0x100, 0x10, dev).unwrap();
}

#[test]
fn register_zero_size_fails() {
    let mut bus = Bus::new();
    let dev = Arc::new(Mutex::new(DummyDevice));
    let err = bus.register(0x100, 0, dev).unwrap_err();
    assert!(matches!(err, BusError::ZeroSizedRange));
}

#[test]
fn register_overlapping_exact_fails() {
    let mut bus = Bus::new();
    let d1 = Arc::new(Mutex::new(DummyDevice));
    let d2 = Arc::new(Mutex::new(DummyDevice));
    bus.register(0x100, 0x10, d1).unwrap();
    let err = bus.register(0x100, 0x10, d2).unwrap_err();
    assert!(matches!(err, BusError::Overlap { .. }));
}

#[test]
fn register_overlapping_partial_fails() {
    let mut bus = Bus::new();
    let d1 = Arc::new(Mutex::new(DummyDevice));
    let d2 = Arc::new(Mutex::new(DummyDevice));
    bus.register(0x100, 0x10, d1).unwrap();
    // Overlaps at 0x108..0x118
    let err = bus.register(0x108, 0x10, d2).unwrap_err();
    assert!(matches!(err, BusError::Overlap { .. }));
}

#[test]
fn register_overlapping_superset_fails() {
    let mut bus = Bus::new();
    let d1 = Arc::new(Mutex::new(DummyDevice));
    let d2 = Arc::new(Mutex::new(DummyDevice));
    bus.register(0x100, 0x10, d1).unwrap();
    // Superset range
    let err = bus.register(0x0F0, 0x30, d2).unwrap_err();
    assert!(matches!(err, BusError::Overlap { .. }));
}

#[test]
fn register_adjacent_succeeds() {
    let mut bus = Bus::new();
    let d1 = Arc::new(Mutex::new(DummyDevice));
    let d2 = Arc::new(Mutex::new(DummyDevice));
    // [0x100, 0x110) and [0x110, 0x120) — adjacent, no overlap
    bus.register(0x100, 0x10, d1).unwrap();
    bus.register(0x110, 0x10, d2).unwrap();
}

#[test]
fn register_multiple_non_overlapping() {
    let mut bus = Bus::new();
    let d1 = Arc::new(Mutex::new(DummyDevice));
    let d2 = Arc::new(Mutex::new(DummyDevice));
    let d3 = Arc::new(Mutex::new(DummyDevice));
    bus.register(0x3F8, 8, d1).unwrap();
    bus.register(0x2F8, 8, d2).unwrap();
    bus.register(0x1000, 0x100, d3).unwrap();
}

// ── Lookup / Read / Write Tests ───────────────────────────────────────

#[test]
fn read_unregistered_address_returns_none() {
    let bus = Bus::new();
    let mut buf = [0u8; 4];
    assert!(bus.read(0x100, &mut buf).is_none());
}

#[test]
fn write_unregistered_address_returns_none() {
    let bus = Bus::new();
    assert!(bus.write(0x100, &[0x42]).is_none());
}

#[test]
fn read_write_dispatch_to_device() {
    let mut bus = Bus::new();
    let dev = Arc::new(Mutex::new(CountingDevice));
    bus.register(0x100, 0x10, dev).unwrap();

    // Read at base → offset 0 → bytes [0, 1, 2, 3]
    let mut buf = [0u8; 4];
    bus.read(0x100, &mut buf).unwrap();
    assert_eq!(buf, [0, 1, 2, 3]);

    // Read at base+5 → offset 5 → bytes [5, 6, 7, 8]
    bus.read(0x105, &mut buf).unwrap();
    assert_eq!(buf, [5, 6, 7, 8]);

    // Write at base → offset 0 → CountingDevice asserts [0, 1, 2, 3]
    bus.write(0x100, &[0, 1, 2, 3]).unwrap();

    // Write at base+5 → offset 5
    bus.write(0x105, &[5, 6, 7, 8]).unwrap();
}

#[test]
fn read_write_past_end_returns_none() {
    let mut bus = Bus::new();
    let dev = Arc::new(Mutex::new(DummyDevice));
    bus.register(0x100, 0x10, dev).unwrap();

    // Address 0x110 is past the end of [0x100, 0x110)
    let mut buf = [0u8; 1];
    assert!(bus.read(0x110, &mut buf).is_none());
    assert!(bus.write(0x110, &[0]).is_none());
}

#[test]
fn multiple_devices_dispatch_correctly() {
    let mut bus = Bus::new();
    let d1 = Arc::new(Mutex::new(CountingDevice));
    let d2 = Arc::new(Mutex::new(CountingDevice));
    bus.register(0x100, 0x10, d1).unwrap();
    bus.register(0x200, 0x10, d2).unwrap();

    let mut buf = [0u8; 2];

    // First device: base 0x100, addr 0x103 → offset 3
    bus.read(0x103, &mut buf).unwrap();
    assert_eq!(buf, [3, 4]);

    // Second device: base 0x200, addr 0x207 → offset 7
    bus.read(0x207, &mut buf).unwrap();
    assert_eq!(buf, [7, 8]);
}
