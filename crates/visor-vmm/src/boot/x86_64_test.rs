//! Tests for `x86_64` boot protocol setup.

use std::io::Write;
use std::path::PathBuf;

use super::*;
use crate::memory::GuestMemory;

// ── Test Helpers ───────────────────────────────────────────────────────────

/// Creates a minimal valid `ELF64` x86-64 executable with one `PT_LOAD` segment.
///
/// The segment is placed at `phys_addr` with `data` as its content.
/// Entry point is set to `entry`.
#[allow(clippy::cast_possible_truncation)]
fn make_test_elf(entry: u64, phys_addr: u64, data: &[u8]) -> Vec<u8> {
    let mut elf = Vec::new();

    // ELF header (64 bytes)
    // e_ident: magic + class + data + version + padding
    elf.extend_from_slice(&[0x7f, b'E', b'L', b'F']); // magic
    elf.push(2); // ELFCLASS64
    elf.push(1); // ELFDATA2LSB
    elf.push(1); // EV_CURRENT
    elf.extend_from_slice(&[0; 9]); // OS/ABI + padding

    // e_type = ET_EXEC (2)
    elf.extend_from_slice(&2u16.to_le_bytes());
    // e_machine = EM_X86_64 (62)
    elf.extend_from_slice(&62u16.to_le_bytes());
    // e_version
    elf.extend_from_slice(&1u32.to_le_bytes());
    // e_entry
    elf.extend_from_slice(&entry.to_le_bytes());
    // e_phoff = 64 (right after header)
    elf.extend_from_slice(&64u64.to_le_bytes());
    // e_shoff = 0 (no section headers)
    elf.extend_from_slice(&0u64.to_le_bytes());
    // e_flags
    elf.extend_from_slice(&0u32.to_le_bytes());
    // e_ehsize = 64
    elf.extend_from_slice(&64u16.to_le_bytes());
    // e_phentsize = 56
    elf.extend_from_slice(&56u16.to_le_bytes());
    // e_phnum = 1
    elf.extend_from_slice(&1u16.to_le_bytes());
    // e_shentsize = 0
    elf.extend_from_slice(&0u16.to_le_bytes());
    // e_shnum = 0
    elf.extend_from_slice(&0u16.to_le_bytes());
    // e_shstrndx = 0
    elf.extend_from_slice(&0u16.to_le_bytes());

    assert_eq!(elf.len(), 64, "ELF header must be 64 bytes");

    // Program header (56 bytes)
    // p_type = PT_LOAD (1)
    elf.extend_from_slice(&1u32.to_le_bytes());
    // p_flags = PF_R | PF_X (5)
    elf.extend_from_slice(&5u32.to_le_bytes());
    // p_offset = 4096 (data starts at page-aligned offset)
    let p_offset: u64 = 4096;
    elf.extend_from_slice(&p_offset.to_le_bytes());
    // p_vaddr (not used, set to phys_addr + kernel virtual base)
    elf.extend_from_slice(&(phys_addr + 0xffff_ffff_8000_0000).to_le_bytes());
    // p_paddr
    elf.extend_from_slice(&phys_addr.to_le_bytes());
    // p_filesz
    elf.extend_from_slice(&(data.len() as u64).to_le_bytes());
    // p_memsz (same as filesz for this test, no BSS)
    elf.extend_from_slice(&(data.len() as u64).to_le_bytes());
    // p_align
    elf.extend_from_slice(&0x20_0000u64.to_le_bytes());

    assert_eq!(elf.len(), 120, "header + 1 phdr must be 120 bytes");

    // Pad to p_offset
    elf.resize(p_offset as usize, 0);

    // Segment data
    elf.extend_from_slice(data);

    elf
}

/// Creates a test ELF with a BSS section (`p_memsz` > `p_filesz`).
#[allow(clippy::cast_possible_truncation)]
fn make_test_elf_with_bss(entry: u64, phys_addr: u64, data: &[u8], bss_size: u64) -> Vec<u8> {
    let mut elf = make_test_elf(entry, phys_addr, data);

    // Patch p_memsz in the program header (offset 64 + 40 = 104)
    let new_memsz = data.len() as u64 + bss_size;
    elf[104..112].copy_from_slice(&new_memsz.to_le_bytes());

    elf
}

/// Writes a test ELF to a temporary file and returns the path.
fn write_elf_to_tempfile(elf_data: &[u8]) -> (tempfile::NamedTempFile, PathBuf) {
    let mut f = crate::testutil::named_temp_file("visor-vmm-boot-").unwrap();
    f.write_all(elf_data).unwrap();
    f.flush().unwrap();
    let path = f.path().to_path_buf();
    (f, path)
}

/// Reads a little-endian u64 from guest memory at the given address.
fn read_u64(memory: &GuestMemory, addr: u64) -> u64 {
    let bytes = memory.read_bytes(addr, 8).unwrap();
    u64::from_le_bytes(bytes.try_into().unwrap())
}

/// Reads a little-endian u32 from guest memory at the given address.
fn read_u32(memory: &GuestMemory, addr: u64) -> u32 {
    let bytes = memory.read_bytes(addr, 4).unwrap();
    u32::from_le_bytes(bytes.try_into().unwrap())
}

/// Reads a little-endian u16 from guest memory at the given address.
fn read_u16(memory: &GuestMemory, addr: u64) -> u16 {
    let bytes = memory.read_bytes(addr, 2).unwrap();
    u16::from_le_bytes(bytes.try_into().unwrap())
}

// ── GDT Entry Tests ────────────────────────────────────────────────────────

#[test]
fn gdt_entry_null() {
    assert_eq!(gdt_entry(0, 0, 0), 0);
}

#[test]
fn gdt_entry_code_segment() {
    // Linux 64-bit code segment: flags=0xa09b, base=0, limit=0xfffff
    let entry = gdt_entry(0xa09b, 0, 0xf_ffff);
    // Expected value from Firecracker test: 0xaf_9b00_0000_ffff
    assert_eq!(entry, 0xaf_9b00_0000_ffff);
}

#[test]
fn gdt_entry_data_segment() {
    let entry = gdt_entry(0xc093, 0, 0xf_ffff);
    // Expected: 0xcf_9300_0000_ffff
    assert_eq!(entry, 0xcf_9300_0000_ffff);
}

#[test]
fn gdt_entry_tss_segment() {
    let entry = gdt_entry(0x808b, 0, 0xf_ffff);
    // Expected: 0x8f_8b00_0000_ffff
    assert_eq!(entry, 0x8f_8b00_0000_ffff);
}

// ── ELF Loading Tests ──────────────────────────────────────────────────────

#[test]
fn load_elf_copies_segment_to_guest_memory() {
    let segment_data = b"Hello from the kernel!";
    let phys_addr = HIMEM_START; // 1 MiB
    let entry = 0x10_1000u64;
    let elf = make_test_elf(entry, phys_addr, segment_data);

    let memory = GuestMemory::new(64 * 1024 * 1024, 0).unwrap(); // 64 MiB
    let result = load_elf(&memory, &elf).unwrap();

    assert_eq!(result, entry);

    // Verify segment data was written to guest memory at phys_addr
    let read_back = memory.read_bytes(phys_addr, segment_data.len()).unwrap();
    assert_eq!(&read_back, segment_data);
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn load_elf_zeroes_bss_region() {
    let segment_data = b"code";
    let bss_size = 256u64;
    let phys_addr = HIMEM_START;
    let elf = make_test_elf_with_bss(0x10_0000, phys_addr, segment_data, bss_size);

    let memory = GuestMemory::new(64 * 1024 * 1024, 0).unwrap();

    // Write non-zero data where BSS will go to verify it gets zeroed
    let garbage = vec![0xAA; bss_size as usize];
    memory
        .write_bytes(phys_addr + segment_data.len() as u64, &garbage)
        .unwrap();

    load_elf(&memory, &elf).unwrap();

    // BSS region should be zeroed
    let bss = memory
        .read_bytes(phys_addr + segment_data.len() as u64, bss_size as usize)
        .unwrap();
    assert!(bss.iter().all(|&b| b == 0), "BSS region not zeroed");
}

#[test]
fn load_elf_rejects_non_elf() {
    let memory = GuestMemory::new(4096, 0).unwrap();
    let mut bad_data = vec![0u8; 64]; // big enough for header check
    bad_data[0..15].copy_from_slice(b"not an elf file");
    let result = load_elf(&memory, &bad_data);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("bad ELF magic"),
        "unexpected error: {err}"
    );
}

#[test]
fn load_elf_rejects_truncated_file() {
    let memory = GuestMemory::new(4096, 0).unwrap();
    let result = load_elf(&memory, &[0x7f, b'E', b'L', b'F']);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("too small"),
        "unexpected error: {err}"
    );
}

#[test]
fn load_elf_rejects_32bit() {
    let mut elf = vec![0u8; 64];
    elf[0..4].copy_from_slice(&[0x7f, b'E', b'L', b'F']);
    elf[4] = 1; // ELFCLASS32

    let memory = GuestMemory::new(4096, 0).unwrap();
    let result = load_elf(&memory, &elf);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("not ELF64"),
        "unexpected error: {err}"
    );
}

#[test]
fn load_elf_rejects_segment_below_himem() {
    // Place segment at 0x1000 — below HIMEM_START (0x10_0000)
    let elf = make_test_elf(0x1000, 0x1000, b"bad");

    let memory = GuestMemory::new(64 * 1024 * 1024, 0).unwrap();
    let result = load_elf(&memory, &elf);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("below HIMEM_START"),
        "unexpected error: {err}"
    );
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn load_elf_rejects_segment_exceeding_memory() {
    // Place a segment that's larger than guest memory
    let elf = make_test_elf(HIMEM_START, HIMEM_START, &[0; 1024]);

    // Only 2 MiB of memory, but segment at 1 MiB + 1024 bytes — should fit,
    // but let's test with tiny memory
    let memory = GuestMemory::new(HIMEM_START as usize + 512, 0).unwrap();
    let result = load_elf(&memory, &elf);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("exceeds guest memory"),
        "unexpected error: {err}"
    );
}

// ── GDT Write Tests ────────────────────────────────────────────────────────

#[test]
fn write_gdt_writes_four_entries() {
    let memory = GuestMemory::new(64 * 1024, 0).unwrap();
    write_gdt(&memory).unwrap();

    // NULL entry
    assert_eq!(read_u64(&memory, BOOT_GDT_OFFSET), 0);

    // CODE entry
    assert_eq!(read_u64(&memory, BOOT_GDT_OFFSET + 8), 0xaf_9b00_0000_ffff);

    // DATA entry
    assert_eq!(read_u64(&memory, BOOT_GDT_OFFSET + 16), 0xcf_9300_0000_ffff);

    // TSS entry
    assert_eq!(read_u64(&memory, BOOT_GDT_OFFSET + 24), 0x8f_8b00_0000_ffff);
}

#[test]
fn write_gdt_writes_zeroed_idt() {
    let memory = GuestMemory::new(64 * 1024, 0).unwrap();

    // Write non-zero first to verify it gets zeroed
    memory.write_bytes(BOOT_IDT_OFFSET, &[0xFF; 8]).unwrap();

    write_gdt(&memory).unwrap();
    assert_eq!(read_u64(&memory, BOOT_IDT_OFFSET), 0);
}

// ── Page Table Tests ───────────────────────────────────────────────────────

#[test]
fn write_page_tables_pml4_points_to_pdpt() {
    let memory = GuestMemory::new(64 * 1024, 0).unwrap();
    write_page_tables(&memory).unwrap();

    let pml4_entry = read_u64(&memory, PML4_START);
    assert_eq!(pml4_entry, PDPTE_START | PAGE_PRESENT_WRITABLE);
    assert_eq!(pml4_entry, 0xa003);
}

#[test]
fn write_page_tables_pdpt_points_to_pd() {
    let memory = GuestMemory::new(64 * 1024, 0).unwrap();
    write_page_tables(&memory).unwrap();

    let pdpte_entry = read_u64(&memory, PDPTE_START);
    assert_eq!(pdpte_entry, PDE_START | PAGE_PRESENT_WRITABLE);
    assert_eq!(pdpte_entry, 0xb003);
}

#[test]
fn write_page_tables_pd_identity_maps_1gib() {
    let memory = GuestMemory::new(64 * 1024, 0).unwrap();
    write_page_tables(&memory).unwrap();

    // Verify all 512 PD entries identity-map 2 MiB pages
    for i in 0..512u64 {
        let pde = read_u64(&memory, PDE_START + i * 8);
        let expected = (i << 21) | PAGE_PRESENT_WRITABLE_HUGE;
        assert_eq!(pde, expected, "PDE[{i}] = {pde:#x}, expected {expected:#x}");
    }
}

#[test]
fn write_page_tables_first_pde_entry() {
    let memory = GuestMemory::new(64 * 1024, 0).unwrap();
    write_page_tables(&memory).unwrap();

    // First entry maps 0x0000_0000..0x001f_ffff
    assert_eq!(read_u64(&memory, PDE_START), 0x83);
}

#[test]
fn write_page_tables_last_pde_entry() {
    let memory = GuestMemory::new(64 * 1024, 0).unwrap();
    write_page_tables(&memory).unwrap();

    // Last entry (511) maps 0x3fe0_0000..0x3fff_ffff
    let expected = (511u64 << 21) | 0x83;
    assert_eq!(read_u64(&memory, PDE_START + 511 * 8), expected);
}

// ── Cmdline Tests ──────────────────────────────────────────────────────────

#[test]
fn write_cmdline_writes_nul_terminated_string() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();
    let cmdline = "console=ttyS0 reboot=k panic=1";
    write_cmdline(&memory, cmdline).unwrap();

    let read_back = memory.read_bytes(CMDLINE_START, cmdline.len() + 1).unwrap();
    assert_eq!(&read_back[..cmdline.len()], cmdline.as_bytes());
    assert_eq!(read_back[cmdline.len()], 0, "missing NUL terminator");
}

#[test]
fn write_cmdline_empty_string() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();
    write_cmdline(&memory, "").unwrap();

    // Should have just a NUL byte
    let read_back = memory.read_bytes(CMDLINE_START, 1).unwrap();
    assert_eq!(read_back[0], 0);
}

#[test]
fn write_cmdline_rejects_too_long() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();
    let cmdline = "x".repeat(CMDLINE_MAX_SIZE); // exactly max, +1 for NUL exceeds
    let result = write_cmdline(&memory, &cmdline);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("exceeds max"),
        "unexpected error: {err}"
    );
}

#[test]
fn write_cmdline_accepts_max_minus_one() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();
    let cmdline = "x".repeat(CMDLINE_MAX_SIZE - 1); // exactly fills with NUL
    write_cmdline(&memory, &cmdline).unwrap();
}

// ── Boot Params Tests ──────────────────────────────────────────────────────

#[test]
fn write_boot_params_sets_boot_flag() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();
    write_boot_params(&memory, "").unwrap();

    assert_eq!(read_u16(&memory, ZERO_PAGE_START + BP_BOOT_FLAG), 0xAA55);
}

#[test]
fn write_boot_params_sets_header_magic() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();
    write_boot_params(&memory, "").unwrap();

    assert_eq!(
        read_u32(&memory, ZERO_PAGE_START + BP_HEADER_MAGIC),
        0x5372_6448 // "HdrS"
    );
}

#[test]
fn write_boot_params_sets_loader_type() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();
    write_boot_params(&memory, "").unwrap();

    let loader = memory
        .read_bytes(ZERO_PAGE_START + BP_TYPE_OF_LOADER, 1)
        .unwrap();
    assert_eq!(loader[0], 0xFF);
}

#[test]
#[allow(clippy::cast_possible_truncation)]
fn write_boot_params_sets_cmdline_ptr() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();
    write_boot_params(&memory, "").unwrap();

    assert_eq!(
        read_u32(&memory, ZERO_PAGE_START + BP_CMD_LINE_PTR),
        CMDLINE_START as u32
    );
}

#[test]
fn write_boot_params_sets_kernel_alignment() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();
    write_boot_params(&memory, "").unwrap();

    assert_eq!(
        read_u32(&memory, ZERO_PAGE_START + BP_KERNEL_ALIGNMENT),
        0x0100_0000 // 16 MiB
    );
}

#[test]
fn write_boot_params_sets_e820_map_small_memory() {
    // Small memory (< 1 MiB) uses a single e820 entry covering all RAM.
    let mem_size: usize = 256 * 1024;
    let memory = GuestMemory::new(mem_size, 0).unwrap();
    write_boot_params(&memory, "").unwrap();

    // e820_entries = 1 (single entry for small memory)
    let entries = memory
        .read_bytes(ZERO_PAGE_START + BP_E820_ENTRIES, 1)
        .unwrap();
    assert_eq!(entries[0], 1);

    // e820 entry 0: addr=0, size=mem_size, type=E820_RAM
    let e820_addr = read_u64(&memory, ZERO_PAGE_START + BP_E820_TABLE);
    let e820_size = read_u64(&memory, ZERO_PAGE_START + BP_E820_TABLE + 8);
    let e820_type = read_u32(&memory, ZERO_PAGE_START + BP_E820_TABLE + 16);

    assert_eq!(e820_addr, 0);
    assert_eq!(e820_size, mem_size as u64);
    assert_eq!(e820_type, E820_RAM);
}

#[test]
fn write_boot_params_sets_e820_map_large_memory() {
    // Large memory (> 1 MiB) splits into 3 entries:
    // [0, EBDA_START) = RAM, [EBDA_START, HIMEM_START) = RESERVED, [HIMEM_START, end) = RAM
    let mem_size: usize = 128 * 1024 * 1024; // 128 MiB
    let memory = GuestMemory::new(mem_size, 0).unwrap();
    write_boot_params(&memory, "").unwrap();

    // e820_entries = 3
    let entries = memory
        .read_bytes(ZERO_PAGE_START + BP_E820_ENTRIES, 1)
        .unwrap();
    assert_eq!(entries[0], 3);

    // Entry 0: low conventional memory [0, EBDA_START)
    let entry0_addr = read_u64(&memory, ZERO_PAGE_START + BP_E820_TABLE);
    let entry0_size = read_u64(&memory, ZERO_PAGE_START + BP_E820_TABLE + 8);
    let entry0_type = read_u32(&memory, ZERO_PAGE_START + BP_E820_TABLE + 16);
    assert_eq!(entry0_addr, 0);
    assert_eq!(entry0_size, EBDA_START);
    assert_eq!(entry0_type, E820_RAM);

    // Entry 1: reserved ISA hole [EBDA_START, HIMEM_START)
    let e1_off = BP_E820_TABLE + E820_ENTRY_SIZE;
    let entry1_addr = read_u64(&memory, ZERO_PAGE_START + e1_off);
    let entry1_size = read_u64(&memory, ZERO_PAGE_START + e1_off + 8);
    let entry1_type = read_u32(&memory, ZERO_PAGE_START + e1_off + 16);
    assert_eq!(entry1_addr, EBDA_START);
    assert_eq!(entry1_size, HIMEM_START - EBDA_START);
    assert_eq!(entry1_type, E820_RESERVED);

    // Entry 2: high memory [HIMEM_START, end)
    let e2_off = BP_E820_TABLE + 2 * E820_ENTRY_SIZE;
    let entry2_addr = read_u64(&memory, ZERO_PAGE_START + e2_off);
    let entry2_size = read_u64(&memory, ZERO_PAGE_START + e2_off + 8);
    let entry2_type = read_u32(&memory, ZERO_PAGE_START + e2_off + 16);
    assert_eq!(entry2_addr, HIMEM_START);
    assert_eq!(entry2_size, mem_size as u64 - HIMEM_START);
    assert_eq!(entry2_type, E820_RAM);
}

#[test]
fn write_boot_params_zeroes_page_first() {
    let memory = GuestMemory::new(256 * 1024, 0).unwrap();

    // Write garbage to zero page
    let garbage = vec![0xAA; 4096];
    memory.write_bytes(ZERO_PAGE_START, &garbage).unwrap();

    write_boot_params(&memory, "").unwrap();

    // Read a field that should be zero (e.g. offset 0x100, which we don't set)
    let zeroed = memory.read_bytes(ZERO_PAGE_START + 0x100, 8).unwrap();
    assert!(
        zeroed.iter().all(|&b| b == 0),
        "zero page not properly cleared"
    );
}

// ── Integration: configure_boot() ──────────────────────────────────────────

#[test]
fn configure_boot_returns_valid_boot_config() {
    let segment_data = vec![0xCC; 64]; // fake kernel code
    let entry = 0x10_1000u64;
    let elf = make_test_elf(entry, HIMEM_START, &segment_data);
    let (_tmp, path) = write_elf_to_tempfile(&elf);

    let memory = GuestMemory::new(64 * 1024 * 1024, 0).unwrap();
    let config = configure_boot(&memory, &path, "console=ttyS0").unwrap();

    assert_eq!(config.entry_point, entry);
    assert_eq!(config.stack_pointer, BOOT_STACK_POINTER);
    assert_eq!(config.boot_params_addr, ZERO_PAGE_START);
    assert_eq!(config.pml4_addr, PML4_START);
}

#[test]
fn configure_boot_populates_all_structures() {
    let segment_data = vec![0x90; 128]; // NOP sled
    let entry = HIMEM_START + 0x1000;
    let elf = make_test_elf(entry, HIMEM_START, &segment_data);
    let (_tmp, path) = write_elf_to_tempfile(&elf);

    let memory = GuestMemory::new(64 * 1024 * 1024, 0).unwrap();
    configure_boot(&memory, &path, "console=ttyS0 reboot=k panic=1").unwrap();

    // Verify GDT was written
    assert_eq!(read_u64(&memory, BOOT_GDT_OFFSET), 0); // NULL
    assert_ne!(read_u64(&memory, BOOT_GDT_OFFSET + 8), 0); // CODE

    // Verify page tables
    assert_eq!(read_u64(&memory, PML4_START), 0xa003);
    assert_eq!(read_u64(&memory, PDPTE_START), 0xb003);

    // Verify cmdline
    let cmd = memory.read_bytes(CMDLINE_START, 11).unwrap();
    assert_eq!(&cmd[..7], b"console");

    // Verify boot params
    assert_eq!(read_u16(&memory, ZERO_PAGE_START + BP_BOOT_FLAG), 0xAA55);

    // Verify kernel data was loaded
    let kernel = memory.read_bytes(HIMEM_START, segment_data.len()).unwrap();
    assert_eq!(kernel, segment_data);
}

#[test]
fn configure_boot_fails_with_nonexistent_kernel() {
    let memory = GuestMemory::new(64 * 1024 * 1024, 0).unwrap();
    let result = configure_boot(
        &memory,
        &PathBuf::from("/nonexistent/kernel"),
        "console=ttyS0",
    );
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        format!("{err}").contains("failed to read kernel"),
        "unexpected error: {err}"
    );
}

#[test]
fn configure_boot_with_real_kernel() {
    let kernel_path = PathBuf::from("/var/lib/visor/kernel/vmlinux-x86_64");
    if !kernel_path.exists() {
        // Skip if kernel not present
        return;
    }

    // Need enough memory for the kernel (PT_LOAD at 0x100_0000 + ~23 MiB)
    let memory = GuestMemory::new(64 * 1024 * 1024, 0).unwrap();
    let config = configure_boot(&memory, &kernel_path, "console=ttyS0 reboot=k panic=1").unwrap();

    // The real kernel entry point should be > HIMEM_START
    assert!(
        config.entry_point >= HIMEM_START,
        "entry point {:#x} below HIMEM_START",
        config.entry_point
    );
    assert_eq!(config.stack_pointer, BOOT_STACK_POINTER);
    assert_eq!(config.boot_params_addr, ZERO_PAGE_START);
    assert_eq!(config.pml4_addr, PML4_START);
}
