//! ACPI table generation for `x86_64` microVMs.
//!
//! Generates the minimal set of ACPI tables required for Linux to boot:
//! RSDP → XSDT → {FADT, MADT (IOAPIC + LAPICs), DSDT (virtio devices)}.
//!
//! Tables are written contiguously to guest memory starting at [`RSDP_ADDR`]
//! (0xA\_0000, the EBDA start on x86). The kernel finds the RSDP via the
//! `boot_params.acpi_rsdp_addr` field (offset 0x70 in the zero page).
//!
//! # DSDT Device Entries
//!
//! The DSDT contains AML device entries for each virtio-mmio device. This is
//! critical for the kernel to set up interrupt routing via ACPI. Without these
//! entries, `request_irq()` fails with `-EINVAL` because the IRQ descriptor
//! is never allocated in the kernel's IRQ domain.
//!
//! Each virtio device is described as:
//! ```text
//! Device (V000) {
//!     Name (_HID, "LNRO0005")    // virtio-mmio device
//!     Name (_UID, 0)
//!     Name (_CCA, 1)              // cache-coherent
//!     Name (_CRS, ResourceTemplate() {
//!         Memory32Fixed (ReadWrite, 0xD0000000, 0x1000)
//!         Interrupt (ResourceConsumer, Level, ActiveHigh, Shared) { 5 }
//!     })
//! }
//! ```

use acpi_tables::Aml;
use acpi_tables::aml;
use acpi_tables::fadt::{FADTBuilder, Flags};
use acpi_tables::madt::{
    EnabledStatus, IoApic, LocalInterruptController, MADT, ProcessorLocalApic,
};
use acpi_tables::rsdp::Rsdp;
use acpi_tables::sdt::Sdt;
use acpi_tables::xsdt::XSDT;

use crate::memory::{GuestMemory, MemoryError};

// ── Constants ────────────────────────────────────────────────────────────────

/// Guest physical address where the RSDP is placed (EBDA start on x86).
///
/// This is the same address used by Cloud Hypervisor and Firecracker.
pub const RSDP_ADDR: u64 = 0xa_0000;

/// Standard x86 I/O APIC base address.
pub const IOAPIC_ADDR: u32 = 0xfec0_0000;

/// Standard x86 Local APIC base address.
pub const LAPIC_ADDR: u32 = 0xfee0_0000;

/// OEM ID used in all ACPI tables (6 bytes, space-padded).
const OEM_ID: [u8; 6] = *b"VISOR ";

/// OEM revision for all tables.
const OEM_REVISION: u32 = 1;

/// DSDT revision (ACPI 6.x).
const DSDT_REVISION: u8 = 6;

// ── Errors ───────────────────────────────────────────────────────────────────

/// Errors that can occur during ACPI table generation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum AcpiError {
    /// Failed to write an ACPI table to guest memory.
    #[error("failed to write ACPI tables to guest memory: {0}")]
    MemoryWrite(#[from] MemoryError),

    /// Invalid vCPU count (must be 1..=255).
    #[error("invalid vCPU count {0}: must be 1..=255")]
    InvalidVcpuCount(u16),

    /// Too many MMIO devices.
    #[error("too many MMIO devices: {0} (max 256)")]
    TooManyDevices(usize),
}

// ── Device Info ─────────────────────────────────────────────────────────────

/// Descriptor for a virtio-mmio device to be included in the ACPI DSDT.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct MmioDeviceInfo {
    /// Guest physical base address of the MMIO region.
    pub base_addr: u64,
    /// Size of the MMIO region in bytes.
    pub size: u64,
    /// GSI (Global System Interrupt) number for this device.
    pub gsi: u32,
}

impl MmioDeviceInfo {
    /// Creates a new MMIO device descriptor.
    #[must_use]
    pub fn new(base_addr: u64, size: u64, gsi: u32) -> Self {
        Self {
            base_addr,
            size,
            gsi,
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────────
/// Creates and writes ACPI tables to guest memory.
///
/// Generates the minimum ACPI tables for a Linux kernel compiled with
/// `CONFIG_ACPI=y`:
///
/// - **RSDP** at [`RSDP_ADDR`] — root pointer, found via `boot_params.acpi_rsdp_addr`
/// - **DSDT** — device description with AML entries for virtio-mmio devices
/// - **FADT** — fixed hardware description with `HEADLESS` flag (NOT HW\_REDUCED — we
///   provide a full IRQ chip so legacy ISA interrupts like COM1 serial work)
/// - **MADT** — interrupt controller: 1× I/O APIC + N× Local APIC (one per vCPU)
/// - **XSDT** — extended system description, points to FADT and MADT
///
/// Tables are laid out contiguously: `RSDP | DSDT | FADT | MADT | XSDT`.
///
/// Returns the guest physical address of the RSDP, which the caller must write
/// to `boot_params.acpi_rsdp_addr` (offset 0x70 in the zero page).
///
/// # Errors
///
/// Returns [`AcpiError::InvalidVcpuCount`] if `num_vcpus` is 0,
/// [`AcpiError::TooManyDevices`] if more than 256 devices are provided,
/// or [`AcpiError::MemoryWrite`] if writing tables to guest memory fails.
pub fn create_acpi_tables(
    memory: &GuestMemory,
    num_vcpus: u8,
    mmio_devices: &[MmioDeviceInfo],
) -> Result<u64, AcpiError> {
    if num_vcpus == 0 {
        return Err(AcpiError::InvalidVcpuCount(0));
    }

    if mmio_devices.len() > 256 {
        return Err(AcpiError::TooManyDevices(mmio_devices.len()));
    }

    // Build DSDT with virtio-mmio device AML entries
    let dsdt_aml_body = build_dsdt_aml(mmio_devices);
    let mut dsdt = Sdt::new(
        *b"DSDT",
        36, // minimum header size — append_slice will grow it
        DSDT_REVISION,
        OEM_ID,
        *b"VISORDSD",
        OEM_REVISION,
    );
    dsdt.append_slice(&dsdt_aml_body);
    let dsdt_bytes = serialize(&dsdt);

    // Pre-calculate contiguous table addresses
    let dsdt_addr = RSDP_ADDR + Rsdp::len() as u64;
    let fadt_addr = dsdt_addr + dsdt_bytes.len() as u64;

    // Build FADT (points to DSDT via 64-bit x_dsdt field).
    // NOT HW_REDUCED — we provide a full IRQ chip (8259 PIC + IOAPIC) so
    // legacy ISA interrupts (e.g. IRQ 4 for COM1 serial) work properly.
    // HW_REDUCED mode disables legacy interrupt routing, which prevents the
    // 8250 serial driver from requesting IRQ 4 for tty operation.
    let fadt = FADTBuilder::new(OEM_ID, *b"VISORFAD", OEM_REVISION)
        .dsdt_64(dsdt_addr)
        .flag(Flags::Headless)
        .finalize();
    let fadt_bytes = serialize(&fadt);
    let madt_addr = fadt_addr + fadt_bytes.len() as u64;

    // Build MADT (1× IOAPIC at standard address + N× LAPIC)
    let mut madt = MADT::new(
        OEM_ID,
        *b"VISORMAD",
        OEM_REVISION,
        LocalInterruptController::Address(LAPIC_ADDR),
    );
    madt.add_structure(IoApic::new(0, IOAPIC_ADDR, 0));
    for i in 0..num_vcpus {
        madt.add_structure(ProcessorLocalApic::new(i, i, EnabledStatus::Enabled));
    }
    let madt_bytes = serialize(&madt);
    let xsdt_addr = madt_addr + madt_bytes.len() as u64;

    // Build XSDT (entries point to FADT and MADT)
    let mut xsdt = XSDT::new(OEM_ID, *b"VISORXSD", OEM_REVISION);
    xsdt.add_entry(fadt_addr);
    xsdt.add_entry(madt_addr);
    let xsdt_bytes = serialize(&xsdt);

    // Build RSDP (points to XSDT, auto-checksummed)
    let rsdp = Rsdp::new(OEM_ID, xsdt_addr);
    let rsdp_bytes = serialize(&rsdp);

    // Write all tables contiguously to guest memory
    memory.write_bytes(RSDP_ADDR, &rsdp_bytes)?;
    memory.write_bytes(dsdt_addr, &dsdt_bytes)?;
    memory.write_bytes(fadt_addr, &fadt_bytes)?;
    memory.write_bytes(madt_addr, &madt_bytes)?;
    memory.write_bytes(xsdt_addr, &xsdt_bytes)?;

    Ok(RSDP_ADDR)
}

// ── DSDT AML Generation ─────────────────────────────────────────────────────

/// Builds AML bytecode for the DSDT body containing virtio-mmio device entries.
///
/// Each device is described under `\_SB_` scope with:
/// - `_HID = "LNRO0005"` (standard virtio-mmio hardware ID)
/// - `_UID = <device index>`
/// - `_CCA = 1` (cache-coherent access)
/// - `_CRS` resource template with MMIO range and interrupt
///
/// This matches the Firecracker DSDT generation approach and is required for
/// the kernel to set up IRQ descriptors for virtio-mmio device interrupts.
fn build_dsdt_aml(devices: &[MmioDeviceInfo]) -> Vec<u8> {
    let mut scope_children_bytes: Vec<u8> = Vec::new();

    for (i, dev) in devices.iter().enumerate() {
        // Build device name: V000, V001, V002, ...
        let dev_name = format!("V{i:03}");

        // Build _CRS resource template with Memory32Fixed + Interrupt
        // MMIO base and size are x86 32-bit addresses by design.
        #[allow(clippy::cast_possible_truncation)]
        let base_addr_u32 = dev.base_addr as u32;
        #[allow(clippy::cast_possible_truncation)]
        let size_u32 = dev.size as u32;
        let mem_resource = aml::Memory32Fixed::new(
            true, // read-write
            base_addr_u32,
            size_u32,
        );
        let irq_resource = aml::Interrupt::new(
            true,  // consumer
            false, // level-triggered
            false, // active-high
            false, // not shared
            dev.gsi,
        );
        let crs = aml::ResourceTemplate::new(vec![&mem_resource, &irq_resource]);

        // Build Name objects
        let hid = aml::Name::new("_HID".into(), &"LNRO0005");
        let uid_val = i as u64;
        let uid = aml::Name::new("_UID".into(), &uid_val);
        let cca = aml::Name::new("_CCA".into(), &aml::ONE);
        let crs_name = aml::Name::new("_CRS".into(), &crs);

        // Build Device node
        let device = aml::Device::new(dev_name.as_str().into(), vec![&hid, &uid, &cca, &crs_name]);
        device.to_aml_bytes(&mut scope_children_bytes);
    }

    // Wrap all devices in \_SB_ scope
    if scope_children_bytes.is_empty() {
        return Vec::new();
    }

    let mut dsdt_bytes: Vec<u8> = Vec::new();
    // Build scope manually: SCOPEOP + PkgLength + NameString + body
    // (We can't use aml::Scope directly because it expects &dyn Aml
    // references, but we already serialized the device children to bytes.)
    let scope_name_bytes = {
        let mut buf = Vec::new();
        let path: aml::Path = "_SB_".into();
        path.to_aml_bytes(&mut buf);
        buf
    };
    let body_len = scope_name_bytes.len() + scope_children_bytes.len();
    let pkg_length = encode_pkg_length(body_len);

    dsdt_bytes.push(0x10); // SCOPEOP
    dsdt_bytes.extend_from_slice(&pkg_length);
    dsdt_bytes.extend_from_slice(&scope_name_bytes);
    dsdt_bytes.extend_from_slice(&scope_children_bytes);

    dsdt_bytes
}

/// Encodes an AML `PkgLength` value.
///
/// The encoding follows the ACPI AML specification:
/// - 0..=0x3F: 1 byte (6-bit length)
/// - 0x40..=0xFFF: 2 bytes
/// - 0x1000..=0xFFFFF: 3 bytes
/// - `0x10_0000..=0xFFF_FFFF`: 4 bytes
// All casts to u8 below are preceded by bit masks (& 0x0F or & 0xFF)
// that guarantee the value fits in a u8.
#[allow(clippy::cast_possible_truncation)]
fn encode_pkg_length(mut data_len: usize) -> Vec<u8> {
    // PkgLength includes itself in the count
    // We need to account for the PkgLength bytes themselves
    let mut pkg_bytes = Vec::with_capacity(4);

    // Try 1-byte encoding: data_len + 1 must fit in 6 bits (0..=0x3E data)
    if data_len < 0x3F {
        pkg_bytes.push((data_len + 1) as u8);
        return pkg_bytes;
    }

    // Try 2-byte encoding: data_len + 2 must fit in 12 bits
    data_len += 2;
    if data_len <= 0xFFF {
        pkg_bytes.push(((data_len & 0x0F) as u8) | (1 << 6));
        pkg_bytes.push(((data_len >> 4) & 0xFF) as u8);
        return pkg_bytes;
    }
    data_len -= 2;

    // Try 3-byte encoding: data_len + 3 must fit in 20 bits
    data_len += 3;
    if data_len <= 0xF_FFFF {
        pkg_bytes.push(((data_len & 0x0F) as u8) | (2 << 6));
        pkg_bytes.push(((data_len >> 4) & 0xFF) as u8);
        pkg_bytes.push(((data_len >> 12) & 0xFF) as u8);
        return pkg_bytes;
    }
    data_len -= 3;

    // 4-byte encoding: data_len + 4 must fit in 28 bits
    data_len += 4;
    pkg_bytes.push(((data_len & 0x0F) as u8) | (3 << 6));
    pkg_bytes.push(((data_len >> 4) & 0xFF) as u8);
    pkg_bytes.push(((data_len >> 12) & 0xFF) as u8);
    pkg_bytes.push(((data_len >> 20) & 0xFF) as u8);
    pkg_bytes
}

/// Serializes an ACPI table to a byte vector via the [`Aml`] trait.
fn serialize(aml: &dyn Aml) -> Vec<u8> {
    let mut bytes = Vec::new();
    aml.to_aml_bytes(&mut bytes);
    bytes
}

#[cfg(test)]
#[path = "acpi_test.rs"]
mod tests;
