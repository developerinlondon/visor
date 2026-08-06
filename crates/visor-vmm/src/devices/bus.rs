//! Generic I/O bus for routing port I/O and MMIO to devices.
//!
//! A [`Bus`] manages a set of [`BusDevice`] registrations, each occupying a
//! contiguous address range. Reads and writes are dispatched to the device
//! that owns the target address.
//!
//! The same `Bus` type works for both PIO (port I/O, small address space)
//! and MMIO (memory-mapped I/O, large address space).

use std::sync::{Arc, Mutex};

/// Trait for devices that respond to reads or writes in an address space.
///
/// Each method receives an `offset` relative to the device's registered
/// base address — the device does not need to know its absolute position.
pub trait BusDevice: Send {
    /// Reads data at `offset` from this device into `data`.
    fn read(&mut self, offset: u64, data: &mut [u8]);

    /// Writes `data` to this device at `offset`.
    fn write(&mut self, offset: u64, data: &[u8]);
}

/// Errors from bus operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BusError {
    /// The new device's address range overlaps an existing registration.
    #[error("device address range [{base:#x}, {base:#x}+{size:#x}) overlaps existing device")]
    Overlap {
        /// Base address of the conflicting registration.
        base: u64,
        /// Size of the conflicting registration.
        size: u64,
    },

    /// Cannot register a device with a zero-length range.
    #[error("cannot register device with zero-sized range")]
    ZeroSizedRange,
}

/// An entry in the bus: base address, size, and the device behind a mutex.
struct BusEntry {
    base: u64,
    size: u64,
    device: Arc<Mutex<dyn BusDevice>>,
}

/// A device container for routing reads and writes over an address space.
///
/// Devices are registered with a `(base, size)` range. The bus dispatches
/// I/O to the device whose range contains the target address, passing the
/// offset within that range.
///
/// Internally uses a sorted `Vec` for O(log n) lookup via binary search.
#[non_exhaustive]
pub struct Bus {
    entries: Vec<BusEntry>,
}

impl Bus {
    /// Creates an empty bus with no registered devices.
    #[must_use]
    pub fn new() -> Self {
        Self {
            entries: Vec::new(),
        }
    }

    /// Registers a device on the bus at `[base, base + size)`.
    ///
    /// # Errors
    ///
    /// Returns [`BusError::ZeroSizedRange`] if `size` is zero.
    /// Returns [`BusError::Overlap`] if the range overlaps any existing device.
    pub fn register(
        &mut self,
        base: u64,
        size: u64,
        device: Arc<Mutex<dyn BusDevice>>,
    ) -> Result<(), BusError> {
        if size == 0 {
            return Err(BusError::ZeroSizedRange);
        }

        // Check for overlaps with existing entries.
        for entry in &self.entries {
            if ranges_overlap(base, size, entry.base, entry.size) {
                return Err(BusError::Overlap { base, size });
            }
        }

        // Insert in sorted order by base address.
        let pos = self
            .entries
            .binary_search_by_key(&base, |e| e.base)
            .unwrap_or_else(|i| i);
        self.entries.insert(pos, BusEntry { base, size, device });

        Ok(())
    }

    /// Dispatches a read to the device at `addr`.
    ///
    /// Returns `Some(())` if a device was found and the read was dispatched,
    /// or `None` if no device owns the address.
    #[must_use]
    pub fn read(&self, addr: u64, data: &mut [u8]) -> Option<()> {
        let (entry_idx, offset) = self.find(addr)?;
        let entry = &self.entries[entry_idx];
        entry.device.lock().ok()?.read(offset, data);
        Some(())
    }

    /// Dispatches a write to the device at `addr`.
    ///
    /// Returns `Some(())` if a device was found and the write was dispatched,
    /// or `None` if no device owns the address.
    #[must_use]
    pub fn write(&self, addr: u64, data: &[u8]) -> Option<()> {
        let (entry_idx, offset) = self.find(addr)?;
        let entry = &self.entries[entry_idx];
        entry.device.lock().ok()?.write(offset, data);
        Some(())
    }

    /// Finds the entry index and offset for the given address.
    fn find(&self, addr: u64) -> Option<(usize, u64)> {
        // Binary search for the last entry with base <= addr.
        let idx = match self.entries.binary_search_by_key(&addr, |e| e.base) {
            Ok(i) => i,
            Err(0) => return None,
            Err(i) => i - 1,
        };

        let entry = &self.entries[idx];
        let offset = addr - entry.base;
        if offset < entry.size {
            Some((idx, offset))
        } else {
            None
        }
    }
}

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

/// Returns `true` if `[a_base, a_base+a_size)` overlaps `[b_base, b_base+b_size)`.
fn ranges_overlap(a_base: u64, a_size: u64, b_base: u64, b_size: u64) -> bool {
    a_base < b_base + b_size && b_base < a_base + a_size
}

#[cfg(test)]
#[path = "bus_test.rs"]
mod tests;
