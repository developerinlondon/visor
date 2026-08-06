//! PCI bus for routing port I/O config space access to PCI devices.
//!
//! [`PciBus`] implements [`BusDevice`] for port I/O, routing config
//! address/data register accesses (ports `0xCF8`/`0xCFC`) to the
//! appropriate [`PciDevice`].
//!
//! # Config address format (port 0xCF8)
//!
//! ```text
//! ┌────┬──────────┬────────┬──────────┬──────────┬──────────────────┐
//! │ 31 │ 30:24    │ 23:16  │ 15:11    │ 10:8     │ 7:2   │ 1:0     │
//! │ EN │ reserved │ bus    │ device#  │ function │ reg   │ 00      │
//! └────┴──────────┴────────┴──────────┴──────────┴───────┴─────────┘
//! ```

use std::sync::{Arc, Mutex};

use crate::devices::bus::BusDevice;

use super::pci::PciDevice;

/// Number of device slots on the PCI bus.
const MAX_DEVICES: usize = 32;

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from PCI bus operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PciBusError {
    /// Slot index is out of range.
    #[error("PCI slot {slot} out of range (max {})", MAX_DEVICES - 1)]
    InvalidSlot {
        /// The invalid slot number.
        slot: usize,
    },

    /// Slot is already occupied by another device.
    #[error("PCI slot {slot} is already occupied")]
    SlotOccupied {
        /// The occupied slot number.
        slot: usize,
    },
}

// ── PciBus ──────────────────────────────────────────────────────────

/// PCI bus managing up to 32 device slots.
///
/// Implements [`BusDevice`] for port I/O:
/// - Offset 0 (port `0xCF8`): Configuration Address register
/// - Offset 4 (port `0xCFC`): Configuration Data register
#[non_exhaustive]
pub struct PciBus {
    /// Device slots (32 total, matching PCI spec).
    devices: Vec<Option<Arc<Mutex<PciDevice>>>>,
    /// Current config address register value (port 0xCF8).
    config_address: u32,
}

impl PciBus {
    /// Creates an empty PCI bus with 32 slots.
    #[must_use]
    pub fn new() -> Self {
        Self {
            devices: vec![None; MAX_DEVICES],
            config_address: 0,
        }
    }

    /// Adds a device to the given slot (0..31).
    ///
    /// # Errors
    ///
    /// Returns [`PciBusError::InvalidSlot`] if `slot >= 32`.
    /// Returns [`PciBusError::SlotOccupied`] if the slot already has a device.
    pub fn add_device(
        &mut self,
        slot: usize,
        device: Arc<Mutex<PciDevice>>,
    ) -> Result<(), PciBusError> {
        if slot >= MAX_DEVICES {
            return Err(PciBusError::InvalidSlot { slot });
        }
        if self.devices[slot].is_some() {
            return Err(PciBusError::SlotOccupied { slot });
        }
        self.devices[slot] = Some(device);
        Ok(())
    }

    /// Returns the device at the given slot, if any.
    #[must_use]
    pub fn device(&self, slot: usize) -> Option<&Arc<Mutex<PciDevice>>> {
        self.devices.get(slot)?.as_ref()
    }

    /// Extracts the device number (bits 15:11) from the config address.
    fn device_number(&self) -> usize {
        ((self.config_address >> 11) & 0x1F) as usize
    }

    /// Extracts the register offset (bits 7:2, 4-byte aligned) from the config address.
    fn register_offset(&self) -> u8 {
        (self.config_address & 0xFC) as u8
    }

    /// Returns whether the enable bit (bit 31) is set in the config address.
    fn is_enabled(&self) -> bool {
        (self.config_address & 0x8000_0000) != 0
    }
}

impl Default for PciBus {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for PciBus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let num_devices = self.devices.iter().filter(|d| d.is_some()).count();
        f.debug_struct("PciBus")
            .field("num_devices", &num_devices)
            .field("config_address", &self.config_address)
            .finish_non_exhaustive()
    }
}

impl BusDevice for PciBus {
    fn read(&mut self, offset: u64, data: &mut [u8]) {
        match offset {
            // Config Address register (port 0xCF8)
            0 if data.len() == 4 => {
                data.copy_from_slice(&self.config_address.to_le_bytes());
            }
            // Config Data register (port 0xCFC)
            4 if data.len() == 4 => {
                if !self.is_enabled() {
                    data.copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
                    return;
                }
                let dev_num = self.device_number();
                let reg_off = self.register_offset();
                match self.devices.get(dev_num) {
                    Some(Some(dev)) => {
                        if let Ok(locked) = dev.lock() {
                            locked.read_config(reg_off, data);
                        } else {
                            data.copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
                        }
                    }
                    _ => {
                        // Empty slot: return all ones per PCI spec
                        data.copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
                    }
                }
            }
            _ => {
                data.fill(0xFF);
            }
        }
    }

    fn write(&mut self, offset: u64, data: &[u8]) {
        match offset {
            // Config Address register (port 0xCF8)
            0 if data.len() == 4 => {
                self.config_address = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
            }
            // Config Data register (port 0xCFC)
            4 if data.len() == 4 => {
                if !self.is_enabled() {
                    return;
                }
                let dev_num = self.device_number();
                let reg_off = self.register_offset();
                if let Some(Some(dev)) = self.devices.get(dev_num) {
                    if let Ok(mut locked) = dev.lock() {
                        locked.write_config(reg_off, data);
                    }
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
#[path = "pci_bus_test.rs"]
mod tests;
