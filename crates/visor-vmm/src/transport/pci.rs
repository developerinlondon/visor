//! PCI transport for virtio devices.
//!
//! [`PciDevice`] represents a PCI Type 0 function exposing a virtio device
//! with standard PCI config space, BAR regions, and MSI-X interrupt support.
//!
//! # Config space layout
//!
//! ```text
//! ┌────────┬──────────────────┬───────────────────────────────────┐
//! │ Offset │ Field            │ Value                             │
//! ├────────┼──────────────────┼───────────────────────────────────┤
//! │ 0x00   │ Vendor ID        │ 0x1AF4 (Red Hat / virtio)         │
//! │ 0x02   │ Device ID        │ 0x1040 + device_type              │
//! │ 0x04   │ Command          │ writable                          │
//! │ 0x06   │ Status           │ 0x0010 (capabilities list)        │
//! │ 0x0E   │ Header Type      │ 0x00 (Type 0)                     │
//! │ 0x10   │ BAR 0..5         │ size-detection via all-ones write │
//! │ 0x2C   │ Subsystem Vendor │ 0x1AF4                            │
//! │ 0x34   │ Cap Pointer      │ 0x40 (MSI-X)                      │
//! │ 0x40   │ MSI-X Cap        │ ID=0x11, table in BAR 4           │
//! └────────┴──────────────────┴───────────────────────────────────┘
//! ```

use std::sync::{Arc, Mutex};

use crate::platform::event::InterruptEvent;

use crate::memory::GuestMemory;

use super::{DeviceType, VirtioDevice};

/// PCI vendor ID for virtio devices (Red Hat).
pub const VIRTIO_PCI_VENDOR_ID: u16 = 0x1AF4;

/// PCI device ID base for modern virtio devices.
/// Final device ID = `VIRTIO_PCI_DEVICE_ID_BASE + device_type`.
pub const VIRTIO_PCI_DEVICE_ID_BASE: u16 = 0x1040;

/// MSI-X capability ID per the PCI spec.
const MSIX_CAP_ID: u8 = 0x11;

/// Config space offset where the MSI-X capability starts.
const MSIX_CAP_OFFSET: u8 = 0x40;

/// Number of BAR registers in a Type 0 PCI header.
const NUM_BARS: usize = 6;

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from PCI device operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PciError {
    /// BAR index is out of range (must be 0..6).
    #[error("BAR index {index} out of range (max 5)")]
    InvalidBar {
        /// The invalid BAR index.
        index: usize,
    },

    /// Offset is out of bounds for the BAR region.
    #[error("BAR {bar} offset {offset:#x} out of bounds (size {size:#x})")]
    BarOffsetOutOfBounds {
        /// BAR index.
        bar: usize,
        /// Requested offset.
        offset: u64,
        /// BAR region size.
        size: u32,
    },
}

// ── Supporting types ────────────────────────────────────────────────

/// A single MSI-X table entry (16 bytes per the PCI spec).
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct MsixTableEntry {
    /// Message address (lower 32 bits).
    pub msg_addr_lo: u32,
    /// Message address (upper 32 bits).
    pub msg_addr_hi: u32,
    /// Message data.
    pub msg_data: u32,
    /// Vector control (bit 0 = masked).
    pub vector_ctrl: u32,
}

/// A BAR (Base Address Register) region descriptor.
#[derive(Debug, Clone, Copy, Default)]
#[non_exhaustive]
pub struct BarRegion {
    /// Size of the region in bytes. Zero means the BAR is not implemented.
    pub size: u32,
    /// Whether this is an I/O BAR (vs memory BAR).
    pub is_io: bool,
}

// ── PciDevice ───────────────────────────────────────────────────────

/// PCI Type 0 device wrapping a virtio backend.
///
/// Provides PCI config space access (vendor/device IDs, BARs, capabilities)
/// and BAR-mapped regions (MSI-X table and PBA).
#[non_exhaustive]
pub struct PciDevice {
    /// PCI Type 0 configuration space (256 bytes).
    config: [u8; 256],
    /// BAR region descriptors (size and type).
    bars: [BarRegion; NUM_BARS],
    /// MSI-X table entries.
    msix_table: Vec<MsixTableEntry>,
    /// MSI-X Pending Bit Array (one u64 per 64 vectors).
    msix_pba: Vec<u64>,
    /// Whether MSI-X is enabled.
    msix_enabled: bool,
    /// The wrapped virtio device backend.
    device: Arc<Mutex<dyn VirtioDevice>>,
    /// Guest memory reference for I/O processing.
    memory: Option<Arc<GuestMemory>>,
    /// IRQ event for signaling the guest.
    irq_evt: Option<Arc<dyn InterruptEvent>>,
}

impl PciDevice {
    /// Creates a new PCI device wrapping the given virtio device.
    ///
    /// The config space is initialized with:
    /// - Vendor ID: `0x1AF4` (Red Hat / virtio)
    /// - Device ID: `0x1040 + device_type`
    /// - Header Type: `0x00` (Type 0)
    /// - Subsystem Vendor ID: `0x1AF4`
    /// - Capabilities Pointer: `0x40` (MSI-X capability)
    ///
    /// MSI-X capability is placed at config offset `0x40` with `num_vectors`
    /// table entries mapped through BAR 4.
    #[must_use]
    pub fn new(device: Arc<Mutex<dyn VirtioDevice>>, num_vectors: u16) -> Self {
        let device_type = device.lock().map_or(DeviceType::Block, |d| d.device_type());
        let device_id = VIRTIO_PCI_DEVICE_ID_BASE + device_type as u16;

        let mut config = [0u8; 256];

        // Vendor ID (0x00)
        config[0x00..0x02].copy_from_slice(&VIRTIO_PCI_VENDOR_ID.to_le_bytes());
        // Device ID (0x02)
        config[0x02..0x04].copy_from_slice(&device_id.to_le_bytes());
        // Status register (0x06): capabilities list bit (bit 4)
        config[0x06] = 0x10;
        // Header Type (0x0E): Type 0
        config[0x0E] = 0x00;
        // Subsystem Vendor ID (0x2C)
        config[0x2C..0x2E].copy_from_slice(&VIRTIO_PCI_VENDOR_ID.to_le_bytes());
        // Capabilities Pointer (0x34)
        config[0x34] = MSIX_CAP_OFFSET;

        // ── MSI-X capability at offset 0x40 ──
        config[usize::from(MSIX_CAP_OFFSET)] = MSIX_CAP_ID; // Cap ID
        config[usize::from(MSIX_CAP_OFFSET) + 1] = 0x00; // Next pointer (end of chain)
        // Message Control: table size = num_vectors - 1 (bits 10:0)
        let table_size = num_vectors.saturating_sub(1);
        config[usize::from(MSIX_CAP_OFFSET) + 2] = (table_size & 0xFF) as u8;
        config[usize::from(MSIX_CAP_OFFSET) + 3] = ((table_size >> 8) & 0x07) as u8;
        // Table Offset/BIR: BAR 4 (BIR = 4), offset = 0
        config[usize::from(MSIX_CAP_OFFSET) + 4] = 0x04; // BIR = 4
        // PBA Offset/BIR: BAR 4, offset after table (16-byte aligned)
        let pba_offset = (u32::from(num_vectors) * 16 + 0xF) & !0xF;
        let pba_bir = pba_offset | 0x04; // BIR = 4
        config[usize::from(MSIX_CAP_OFFSET) + 8..usize::from(MSIX_CAP_OFFSET) + 12]
            .copy_from_slice(&pba_bir.to_le_bytes());

        // ── BAR setup ──
        let mut bars = [BarRegion::default(); NUM_BARS];
        // BAR 4: MSI-X table + PBA region (minimum 4 KiB, rounded to power of 2)
        let pba_size = u32::from(num_vectors).div_ceil(64) * 8;
        let bar4_size = (pba_offset + pba_size).next_power_of_two().max(4096);
        bars[4] = BarRegion {
            size: bar4_size,
            is_io: false,
        };

        // Set BAR type bits in config space
        for (i, bar) in bars.iter().enumerate() {
            if bar.size > 0 && bar.is_io {
                config[0x10 + i * 4] = 0x01; // I/O space indicator
            }
        }

        // ── MSI-X table (vectors masked by default) ──
        let msix_table = vec![
            MsixTableEntry {
                vector_ctrl: 1,
                ..MsixTableEntry::default()
            };
            num_vectors as usize
        ];
        let pba_entries = usize::from(num_vectors).div_ceil(64);
        let msix_pba = vec![0u64; pba_entries];

        Self {
            config,
            bars,
            msix_table,
            msix_pba,
            msix_enabled: false,
            device,
            memory: None,
            irq_evt: None,
        }
    }

    /// Reads from the PCI configuration space.
    ///
    /// Out-of-range bytes are filled with `0xFF`.
    pub fn read_config(&self, offset: u8, data: &mut [u8]) {
        for (i, byte) in data.iter_mut().enumerate() {
            let addr = offset as usize + i;
            if addr < self.config.len() {
                *byte = self.config[addr];
            } else {
                *byte = 0xFF;
            }
        }
    }

    /// Writes to the PCI configuration space.
    ///
    /// Handles special write semantics for BAR registers (size detection)
    /// and MSI-X message control (enable/disable). Read-only fields are
    /// silently ignored.
    pub fn write_config(&mut self, offset: u8, data: &[u8]) {
        let start = offset as usize;

        for (i, &byte) in data.iter().enumerate() {
            let addr = start + i;
            match addr {
                // BAR range (0x10-0x27): handled as aligned 4-byte write below
                0x10..=0x27 => {}
                // Command register (0x04-0x05) and Interrupt Line (0x3C): writable
                0x04..=0x05 | 0x3C => self.config[addr] = byte,
                // MSI-X message control high byte (enable + function mask)
                a if a == usize::from(MSIX_CAP_OFFSET) + 3 => {
                    let writable = byte & 0xC0;
                    let readonly = self.config[addr] & 0x3F;
                    self.config[addr] = writable | readonly;
                    self.msix_enabled = (self.config[addr] & 0x80) != 0;
                }
                _ => {}
            }
        }

        // Handle aligned 4-byte BAR writes (size detection / base address)
        if (0x10..=0x24).contains(&start) && (start - 0x10) % 4 == 0 && data.len() == 4 {
            let bar_idx = (start - 0x10) / 4;
            if bar_idx < NUM_BARS {
                let value = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
                self.write_bar_register(bar_idx, value);
            }
        }
    }

    /// Handles a write to a BAR register in config space.
    ///
    /// Applies the size mask so that reading back reveals the BAR size
    /// (standard PCI BAR size detection protocol).
    fn write_bar_register(&mut self, bar_idx: usize, value: u32) {
        let bar = &self.bars[bar_idx];
        if bar.size == 0 {
            return;
        }

        let bar_offset = 0x10 + bar_idx * 4;
        let (type_bits, addr_mask) = if bar.is_io {
            (0x01u32, !0x03u32)
        } else {
            (0x00u32, !0x0Fu32)
        };

        let size_mask = !(bar.size - 1);
        let new_value = (value & size_mask & addr_mask) | type_bits;
        self.config[bar_offset..bar_offset + 4].copy_from_slice(&new_value.to_le_bytes());
    }

    /// Reads from a BAR-mapped region.
    ///
    /// Currently BAR 4 contains the MSI-X table and PBA.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidBar`] if `bar_idx >= 6`.
    /// Returns [`PciError::BarOffsetOutOfBounds`] if `offset` exceeds the region.
    pub fn read_bar(&self, bar_idx: usize, offset: u64, data: &mut [u8]) -> Result<(), PciError> {
        if bar_idx >= NUM_BARS {
            return Err(PciError::InvalidBar { index: bar_idx });
        }
        let bar = &self.bars[bar_idx];
        if bar.size == 0 || offset >= u64::from(bar.size) {
            return Err(PciError::BarOffsetOutOfBounds {
                bar: bar_idx,
                offset,
                size: bar.size,
            });
        }

        if bar_idx == 4 {
            self.read_msix_region(offset, data);
        } else {
            data.fill(0);
        }
        Ok(())
    }

    /// Writes to a BAR-mapped region.
    ///
    /// Currently BAR 4 contains the MSI-X table and PBA.
    ///
    /// # Errors
    ///
    /// Returns [`PciError::InvalidBar`] if `bar_idx >= 6`.
    /// Returns [`PciError::BarOffsetOutOfBounds`] if `offset` exceeds the region.
    pub fn write_bar(&mut self, bar_idx: usize, offset: u64, data: &[u8]) -> Result<(), PciError> {
        if bar_idx >= NUM_BARS {
            return Err(PciError::InvalidBar { index: bar_idx });
        }
        let bar = &self.bars[bar_idx];
        if bar.size == 0 || offset >= u64::from(bar.size) {
            return Err(PciError::BarOffsetOutOfBounds {
                bar: bar_idx,
                offset,
                size: bar.size,
            });
        }

        if bar_idx == 4 {
            self.write_msix_region(offset, data);
        }
        Ok(())
    }

    /// Returns whether MSI-X is currently enabled.
    #[must_use]
    pub fn msix_enabled(&self) -> bool {
        self.msix_enabled
    }

    /// Returns a reference to the MSI-X table entries.
    #[must_use]
    pub fn msix_table(&self) -> &[MsixTableEntry] {
        &self.msix_table
    }

    /// Returns a reference to the MSI-X Pending Bit Array.
    #[must_use]
    pub fn msix_pba(&self) -> &[u64] {
        &self.msix_pba
    }

    /// Returns a clone of the inner device `Arc`.
    #[must_use]
    pub fn device(&self) -> Arc<Mutex<dyn VirtioDevice>> {
        Arc::clone(&self.device)
    }

    /// Sets the guest memory reference for I/O processing.
    pub fn set_memory(&mut self, memory: Arc<GuestMemory>) {
        self.memory = Some(memory);
    }

    /// Sets the IRQ event for signaling the guest.
    pub fn set_irq_evt(&mut self, evt: Arc<dyn InterruptEvent>) {
        self.irq_evt = Some(evt);
    }

    // ── MSI-X BAR region helpers ────────────────────────────────────

    /// Reads from the MSI-X table/PBA region in BAR 4.
    fn read_msix_region(&self, offset: u64, data: &mut [u8]) {
        let table_bytes = self.msix_table.len() * 16;
        let Some(off) = usize::try_from(offset).ok() else {
            return;
        };

        if off < table_bytes {
            let entry_idx = off / 16;
            let entry_off = off % 16;
            if entry_idx < self.msix_table.len() {
                let entry_bytes = msix_entry_to_bytes(&self.msix_table[entry_idx]);
                for (i, byte) in data.iter_mut().enumerate() {
                    let pos = entry_off + i;
                    if pos < 16 {
                        *byte = entry_bytes[pos];
                    }
                }
            }
        }
        // PBA reads for offsets past the table would go here.
    }

    /// Writes to the MSI-X table/PBA region in BAR 4.
    fn write_msix_region(&mut self, offset: u64, data: &[u8]) {
        let table_bytes = self.msix_table.len() * 16;
        let Some(off) = usize::try_from(offset).ok() else {
            return;
        };

        if off < table_bytes {
            let entry_idx = off / 16;
            let entry_off = off % 16;
            if entry_idx < self.msix_table.len() {
                let mut entry_bytes = msix_entry_to_bytes(&self.msix_table[entry_idx]);
                for (i, &byte) in data.iter().enumerate() {
                    let pos = entry_off + i;
                    if pos < 16 {
                        entry_bytes[pos] = byte;
                    }
                }
                self.msix_table[entry_idx] = msix_entry_from_bytes(&entry_bytes);
            }
        }
        // PBA writes for offsets past the table would go here.
    }
}

impl std::fmt::Debug for PciDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PciDevice")
            .field("bars", &self.bars)
            .field("msix_enabled", &self.msix_enabled)
            .field("msix_table_len", &self.msix_table.len())
            .field("device", &self.device)
            .finish_non_exhaustive()
    }
}

// ── MSI-X serialization helpers ─────────────────────────────────────

fn msix_entry_to_bytes(entry: &MsixTableEntry) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[0..4].copy_from_slice(&entry.msg_addr_lo.to_le_bytes());
    bytes[4..8].copy_from_slice(&entry.msg_addr_hi.to_le_bytes());
    bytes[8..12].copy_from_slice(&entry.msg_data.to_le_bytes());
    bytes[12..16].copy_from_slice(&entry.vector_ctrl.to_le_bytes());
    bytes
}

fn msix_entry_from_bytes(bytes: &[u8; 16]) -> MsixTableEntry {
    MsixTableEntry {
        msg_addr_lo: u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]),
        msg_addr_hi: u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]),
        msg_data: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
        vector_ctrl: u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]),
    }
}

#[cfg(test)]
#[path = "pci_test.rs"]
mod tests;
