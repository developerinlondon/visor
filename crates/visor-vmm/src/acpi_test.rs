//! Tests for ACPI table generation.

use acpi_tables::rsdp::Rsdp;

use super::*;
use crate::memory::GuestMemory;

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Allocates 4 MiB of guest memory for ACPI tests.
fn test_memory() -> GuestMemory {
    GuestMemory::new(4 * 1024 * 1024, 0).unwrap()
}

/// Reads a 4-byte ACPI table signature from guest memory.
fn read_signature(mem: &GuestMemory, addr: u64) -> [u8; 4] {
    let bytes = mem.read_bytes(addr, 4).unwrap();
    bytes.try_into().unwrap()
}

/// Reads the SDT `length` field (bytes 4..8) from an ACPI table header.
fn read_table_len(mem: &GuestMemory, addr: u64) -> u32 {
    let bytes = mem.read_bytes(addr + 4, 4).unwrap();
    u32::from_le_bytes(bytes.try_into().unwrap())
}

/// Returns `true` if the byte-level checksum of the given data is valid (sum == 0).
fn checksum_valid(data: &[u8]) -> bool {
    data.iter().fold(0u8, |acc, x| acc.wrapping_add(*x)) == 0
}

/// Finds the DSDT guest address (immediately after the RSDP).
fn dsdt_addr() -> u64 {
    RSDP_ADDR + Rsdp::len() as u64
}

/// Finds the FADT guest address (immediately after the DSDT).
fn fadt_addr(mem: &GuestMemory) -> u64 {
    let addr = dsdt_addr();
    addr + u64::from(read_table_len(mem, addr))
}

/// Finds the MADT guest address (immediately after the FADT).
fn madt_addr(mem: &GuestMemory) -> u64 {
    let addr = fadt_addr(mem);
    addr + u64::from(read_table_len(mem, addr))
}

/// Finds the XSDT guest address (immediately after the MADT).
fn xsdt_addr(mem: &GuestMemory) -> u64 {
    let addr = madt_addr(mem);
    addr + u64::from(read_table_len(mem, addr))
}

// ── RSDP Tests ───────────────────────────────────────────────────────────────

#[test]
fn returns_rsdp_address() {
    let mem = test_memory();
    let addr = create_acpi_tables(&mem, 1, &[]).unwrap();
    assert_eq!(addr, RSDP_ADDR);
}

#[test]
fn rsdp_signature_valid() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let sig = mem.read_bytes(RSDP_ADDR, 8).unwrap();
    assert_eq!(&sig, b"RSD PTR ");
}

#[test]
fn rsdp_checksum_valid() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let data = mem.read_bytes(RSDP_ADDR, Rsdp::len()).unwrap();
    assert!(checksum_valid(&data), "RSDP checksum invalid");
}

#[test]
fn rsdp_points_to_xsdt() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let expected_xsdt = xsdt_addr(&mem);
    // xsdt_addr field is at RSDP offset 24 (8 bytes, u64)
    let bytes = mem.read_bytes(RSDP_ADDR + 24, 8).unwrap();
    let xsdt_ptr = u64::from_le_bytes(bytes.try_into().unwrap());
    assert_eq!(xsdt_ptr, expected_xsdt, "RSDP must point to XSDT");
}

// ── DSDT Tests ───────────────────────────────────────────────────────────────

#[test]
fn dsdt_follows_rsdp() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    assert_eq!(read_signature(&mem, dsdt_addr()), *b"DSDT");
}

#[test]
fn dsdt_checksum_valid() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let addr = dsdt_addr();
    let len = read_table_len(&mem, addr) as usize;
    let data = mem.read_bytes(addr, len).unwrap();
    assert!(checksum_valid(&data), "DSDT checksum invalid");
}

// ── FADT Tests ───────────────────────────────────────────────────────────────

#[test]
fn fadt_follows_dsdt() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    // FADT uses "FACP" as its ACPI signature
    assert_eq!(read_signature(&mem, fadt_addr(&mem)), *b"FACP");
}

#[test]
fn fadt_checksum_valid() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let addr = fadt_addr(&mem);
    let len = read_table_len(&mem, addr) as usize;
    let data = mem.read_bytes(addr, len).unwrap();
    assert!(checksum_valid(&data), "FADT checksum invalid");
}

#[test]
fn fadt_headless_flag_set_without_hw_reduced() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let addr = fadt_addr(&mem);
    // flags field is at FADT offset 112 (4 bytes, u32)
    let bytes = mem.read_bytes(addr + 112, 4).unwrap();
    let flags = u32::from_le_bytes(bytes.try_into().unwrap());
    assert_eq!(
        flags & (1 << 20),
        0,
        "HW_REDUCED_ACPI flag must NOT be set (we need legacy ISA IRQs)"
    );
    assert_ne!(flags & (1 << 12), 0, "HEADLESS flag must be set");
}

#[test]
fn fadt_points_to_dsdt_via_x_dsdt() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let addr = fadt_addr(&mem);
    let expected_dsdt = dsdt_addr();
    // x_dsdt field is at FADT offset 140 (8 bytes, u64)
    let bytes = mem.read_bytes(addr + 140, 8).unwrap();
    let x_dsdt = u64::from_le_bytes(bytes.try_into().unwrap());
    assert_eq!(x_dsdt, expected_dsdt, "FADT x_dsdt must point to DSDT");
    // 32-bit dsdt field at offset 40 must be zero (we use 64-bit only)
    let bytes32 = mem.read_bytes(addr + 40, 4).unwrap();
    let dsdt32 = u32::from_le_bytes(bytes32.try_into().unwrap());
    assert_eq!(dsdt32, 0, "FADT 32-bit dsdt field must be zero");
}

// ── MADT Tests ───────────────────────────────────────────────────────────────

#[test]
fn madt_follows_fadt() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    // MADT uses "APIC" as its ACPI signature
    assert_eq!(read_signature(&mem, madt_addr(&mem)), *b"APIC");
}

#[test]
fn madt_checksum_valid() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let addr = madt_addr(&mem);
    let len = read_table_len(&mem, addr) as usize;
    let data = mem.read_bytes(addr, len).unwrap();
    assert!(checksum_valid(&data), "MADT checksum invalid");
}

#[test]
fn madt_single_vcpu_has_ioapic_and_one_lapic() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let len = read_table_len(&mem, madt_addr(&mem));
    // MADT header: 44 bytes, IoApic: 12 bytes, 1× LAPIC: 8 bytes
    assert_eq!(len, 44 + 12 + 8);
}

#[test]
fn madt_multi_vcpu_has_correct_lapic_count() {
    let mem = test_memory();
    create_acpi_tables(&mem, 4, &[]).unwrap();
    let len = read_table_len(&mem, madt_addr(&mem));
    // MADT header: 44, IoApic: 12, 4× LAPIC: 32
    assert_eq!(len, 44 + 12 + 4 * 8);
}

#[test]
fn madt_max_vcpus() {
    let mem = test_memory();
    create_acpi_tables(&mem, 255, &[]).unwrap();
    let len = read_table_len(&mem, madt_addr(&mem));
    // MADT header: 44, IoApic: 12, 255× LAPIC: 2040
    assert_eq!(len, 44 + 12 + 255 * 8);
}

#[test]
fn madt_lapic_base_address_set() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let addr = madt_addr(&mem);
    // Local APIC address is at MADT offset 36 (4 bytes, u32)
    let bytes = mem.read_bytes(addr + 36, 4).unwrap();
    let lapic_addr = u32::from_le_bytes(bytes.try_into().unwrap());
    assert_eq!(lapic_addr, LAPIC_ADDR);
}

// ── XSDT Tests ───────────────────────────────────────────────────────────────

#[test]
fn xsdt_follows_madt() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    assert_eq!(read_signature(&mem, xsdt_addr(&mem)), *b"XSDT");
}

#[test]
fn xsdt_checksum_valid() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let addr = xsdt_addr(&mem);
    let len = read_table_len(&mem, addr) as usize;
    let data = mem.read_bytes(addr, len).unwrap();
    assert!(checksum_valid(&data), "XSDT checksum invalid");
}

#[test]
fn xsdt_contains_fadt_and_madt_entries() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let addr = xsdt_addr(&mem);
    let len = read_table_len(&mem, addr);
    // XSDT header: 36 bytes + 2 entries × 8 bytes = 52
    assert_eq!(len, 36 + 2 * 8);
    // First entry (offset 36) must point to FADT
    let bytes = mem.read_bytes(addr + 36, 8).unwrap();
    let entry0 = u64::from_le_bytes(bytes.try_into().unwrap());
    assert_eq!(entry0, fadt_addr(&mem), "XSDT entry 0 must point to FADT");
    // Second entry (offset 44) must point to MADT
    let bytes = mem.read_bytes(addr + 44, 8).unwrap();
    let entry1 = u64::from_le_bytes(bytes.try_into().unwrap());
    assert_eq!(entry1, madt_addr(&mem), "XSDT entry 1 must point to MADT");
}

// ── Error Cases ──────────────────────────────────────────────────────────────

#[test]
fn zero_vcpus_returns_error() {
    let mem = test_memory();
    let result = create_acpi_tables(&mem, 0, &[]);
    assert!(result.is_err());
}

// ── Full Layout Tests ────────────────────────────────────────────────────────

#[test]
fn all_tables_fit_within_ebda_to_himem() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    let xsdt_end = xsdt_addr(&mem) + u64::from(read_table_len(&mem, xsdt_addr(&mem)));
    // All tables must fit between EBDA (0xa0000) and HIMEM (0x100000)
    assert!(
        xsdt_end < 0x10_0000,
        "ACPI tables extend past HIMEM boundary"
    );
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn tables_do_not_overlap_boot_structures() {
    let mem = test_memory();
    create_acpi_tables(&mem, 1, &[]).unwrap();
    // Boot structures end at PDE_START + 512*8 = 0xb000 + 0x1000 = 0xc000
    // ACPI tables start at 0xa0000 — well above
    assert!(
        RSDP_ADDR > 0xc000,
        "ACPI tables must not overlap page tables"
    );
    // Kernel cmdline is at 0x20000 — also below
    assert!(RSDP_ADDR > 0x2_0000, "ACPI tables must not overlap cmdline");
}
