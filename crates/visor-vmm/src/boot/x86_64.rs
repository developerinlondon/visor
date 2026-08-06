//! `x86_64` Linux boot protocol setup.
//!
//! Implements the Linux 64-bit boot protocol: loads a vmlinux ELF into guest
//! memory, sets up GDT, identity-mapped page tables (1 GiB), writes the kernel
//! command line, and populates the zero page (`boot_params`).
//!
//! # Boot Memory Map
//!
//! ```text
//! 0x0000_0500  GDT (4 entries × 8 bytes = 32 bytes)
//! 0x0000_0520  IDT (zeroed, 8 bytes)
//! 0x0000_7000  Zero page / `boot_params` (4096 bytes)
//! 0x0000_8ff0  Boot stack pointer
//! 0x0000_9000  PML4 page table
//! 0x0000_a000  PDPT page table
//! 0x0000_b000  PD page table (512 × 2 MiB entries)
//! 0x0002_0000  Kernel command line (up to 2048 bytes)
//! 0x0010_0000+ Kernel `PT_LOAD` segments (at ELF `PhysAddr`)
//! ```

use std::path::Path;
use std::ptr;

use super::{
    BOOT_GDT_COUNT, BOOT_GDT_OFFSET, BOOT_IDT_OFFSET, BOOT_STACK_POINTER, BootConfig, BootError,
    CMDLINE_MAX_SIZE, CMDLINE_START, GDT_FLAGS_CODE, GDT_FLAGS_DATA, GDT_FLAGS_TSS, HIMEM_START,
    PAGE_PRESENT_WRITABLE, PAGE_PRESENT_WRITABLE_HUGE, PDE_ENTRY_COUNT, PDE_START, PDPTE_START,
    PML4_START, ZERO_PAGE_START, mmap_file,
};
use crate::memory::GuestMemory;

/// Constructs a GDT segment descriptor from flags, base, and limit.
///
/// Encodes the fields into the 8-byte format expected by x86 hardware.
/// Derived from Linux `arch/x86/include/asm/segment.h`.
#[must_use]
pub fn gdt_entry(flags: u16, base: u32, limit: u32) -> u64 {
    ((u64::from(base) & 0xff00_0000) << (56 - 24))
        | ((u64::from(flags) & 0x0000_f0ff) << 40)
        | ((u64::from(limit) & 0x000f_0000) << (48 - 16))
        | ((u64::from(base) & 0x00ff_ffff) << 16)
        | (u64::from(limit) & 0x0000_ffff)
}

/// Sets up everything needed to boot a Linux kernel in 64-bit long mode.
///
/// 1. Parses the vmlinux ELF and copies `PT_LOAD` segments into guest memory
/// 2. Writes the GDT (NULL, CODE, DATA, TSS) at [`BOOT_GDT_OFFSET`]
/// 3. Writes identity-mapped page tables (PML4 → PDPT → PD, 1 GiB via 2 MiB pages)
/// 4. Writes the kernel command line at [`CMDLINE_START`]
/// 5. Populates the zero page (`boot_params`) at [`ZERO_PAGE_START`]
///
/// Returns a [`BootConfig`] with the register values for the vCPU.
///
/// # Errors
///
/// Returns [`BootError`] if the kernel file cannot be read, is not a valid
/// ELF64 executable, has segments outside guest memory, or if the command
/// line is too long.
pub fn configure_boot(
    memory: &GuestMemory,
    kernel_path: &Path,
    cmdline: &str,
) -> Result<BootConfig, BootError> {
    let kernel_data = mmap_file(kernel_path)?;
    let entry_point = load_elf(memory, &kernel_data)?;
    write_gdt(memory)?;
    write_page_tables(memory)?;
    write_cmdline(memory, cmdline)?;
    write_boot_params(memory, cmdline)?;

    Ok(BootConfig {
        entry_point,
        stack_pointer: BOOT_STACK_POINTER,
        boot_params_addr: ZERO_PAGE_START,
        pml4_addr: PML4_START,
    })
}

/// Sets up boot memory structures without loading a kernel.
///
/// This is useful for tests that write raw machine code to guest memory
/// and only need the GDT, page tables, cmdline, and boot params populated.
///
/// # Errors
///
/// Returns [`BootError`] if any memory write fails.
pub fn configure_boot_memory(memory: &GuestMemory, cmdline: &str) -> Result<(), BootError> {
    write_gdt(memory)?;
    write_page_tables(memory)?;
    write_cmdline(memory, cmdline)?;
    write_boot_params(memory, cmdline)?;
    Ok(())
}

// ── ELF Loading ────────────────────────────────────────────────────────────

/// ELF64 magic bytes.
const ELF_MAGIC: [u8; 4] = [0x7f, b'E', b'L', b'F'];

/// ELF class: 64-bit.
const ELFCLASS64: u8 = 2;

/// ELF data: little-endian.
const ELFDATA2LSB: u8 = 1;

/// ELF type: executable.
const ET_EXEC: u16 = 2;

/// ELF machine: x86-64.
const EM_X86_64: u16 = 62;

/// ELF program header type: loadable segment.
const PT_LOAD: u32 = 1;

/// Size of the ELF64 file header.
const ELF64_EHDR_SIZE: usize = 64;

/// Size of an ELF64 program header entry.
const ELF64_PHDR_SIZE: usize = 56;

/// Parses an ELF64 executable and copies `PT_LOAD` segments into guest memory.
///
/// Returns the entry point address from the ELF header.
///
/// # Errors
///
/// Returns [`BootError::InvalidElf`] if the data is not a valid ELF64 x86-64
/// executable, or [`BootError::SegmentOutOfBounds`] / [`BootError::SegmentBelowHimem`]
/// if a segment cannot be placed in guest memory.
fn load_elf(memory: &GuestMemory, data: &[u8]) -> Result<u64, BootError> {
    let header = parse_elf_header(data)?;
    load_elf_segments(memory, data, &header)?;
    Ok(header.entry)
}

/// Parsed ELF64 header fields needed for loading.
struct ElfHeader {
    /// Kernel entry point address.
    entry: u64,
    /// Program header table offset in file.
    phoff: u64,
    /// Size of each program header entry.
    phentsize: u16,
    /// Number of program header entries.
    phnum: u16,
}

/// Validates and parses an ELF64 x86-64 executable header.
///
/// # Errors
///
/// Returns [`BootError::InvalidElf`] if the data is not a valid ELF64 x86-64 executable.
fn parse_elf_header(data: &[u8]) -> Result<ElfHeader, BootError> {
    if data.len() < ELF64_EHDR_SIZE {
        return Err(BootError::InvalidElf {
            reason: "file too small for ELF64 header",
        });
    }

    // Validate ELF magic
    if data[0..4] != ELF_MAGIC {
        return Err(BootError::InvalidElf {
            reason: "bad ELF magic",
        });
    }

    // Validate ELF class (64-bit) and data (little-endian)
    if data[4] != ELFCLASS64 {
        return Err(BootError::InvalidElf {
            reason: "not ELF64 (wrong class)",
        });
    }
    if data[5] != ELFDATA2LSB {
        return Err(BootError::InvalidElf {
            reason: "not little-endian",
        });
    }

    // Validate type = EXEC and machine = x86-64
    let e_type = u16::from_le_bytes([data[16], data[17]]);
    if e_type != ET_EXEC {
        return Err(BootError::InvalidElf {
            reason: "not an executable (wrong e_type)",
        });
    }

    let e_machine = u16::from_le_bytes([data[18], data[19]]);
    if e_machine != EM_X86_64 {
        return Err(BootError::InvalidElf {
            reason: "not x86-64 (wrong e_machine)",
        });
    }

    // Parse header fields
    let entry = u64::from_le_bytes(data[24..32].try_into().map_err(|_| BootError::InvalidElf {
        reason: "truncated entry point",
    })?);

    let phoff = u64::from_le_bytes(data[32..40].try_into().map_err(|_| BootError::InvalidElf {
        reason: "truncated phoff",
    })?);

    let phentsize =
        u16::from_le_bytes(data[54..56].try_into().map_err(|_| BootError::InvalidElf {
            reason: "truncated phentsize",
        })?);

    let phnum = u16::from_le_bytes(data[56..58].try_into().map_err(|_| BootError::InvalidElf {
        reason: "truncated phnum",
    })?);

    if usize::from(phentsize) < ELF64_PHDR_SIZE {
        return Err(BootError::InvalidElf {
            reason: "program header entry too small",
        });
    }

    Ok(ElfHeader {
        entry,
        phoff,
        phentsize,
        phnum,
    })
}

/// Loads all `PT_LOAD` segments from an ELF into guest memory.
///
/// # Errors
///
/// Returns [`BootError`] if a segment is out of bounds or the file data is truncated.
fn load_elf_segments(
    memory: &GuestMemory,
    data: &[u8],
    header: &ElfHeader,
) -> Result<(), BootError> {
    for i in 0..u32::from(header.phnum) {
        let ph_offset = usize::try_from(header.phoff).map_err(|_| BootError::InvalidElf {
            reason: "phoff too large",
        })? + usize::from(header.phentsize) * i as usize;

        if ph_offset + ELF64_PHDR_SIZE > data.len() {
            return Err(BootError::InvalidElf {
                reason: "program header extends past end of file",
            });
        }

        let ph = &data[ph_offset..ph_offset + ELF64_PHDR_SIZE];

        let p_type = u32::from_le_bytes([ph[0], ph[1], ph[2], ph[3]]);
        if p_type != PT_LOAD {
            continue;
        }

        load_single_segment(memory, data, ph)?;
    }

    Ok(())
}

/// Loads a single `PT_LOAD` segment into guest memory.
fn load_single_segment(memory: &GuestMemory, data: &[u8], ph: &[u8]) -> Result<(), BootError> {
    let p_offset = u64::from_le_bytes(ph[8..16].try_into().map_err(|_| BootError::InvalidElf {
        reason: "truncated p_offset",
    })?);

    // Use physical address (not virtual) for guest placement
    let p_paddr = u64::from_le_bytes(ph[24..32].try_into().map_err(|_| BootError::InvalidElf {
        reason: "truncated p_paddr",
    })?);

    let p_filesz =
        u64::from_le_bytes(ph[32..40].try_into().map_err(|_| BootError::InvalidElf {
            reason: "truncated p_filesz",
        })?);

    let p_memsz = u64::from_le_bytes(ph[40..48].try_into().map_err(|_| BootError::InvalidElf {
        reason: "truncated p_memsz",
    })?);

    if p_paddr < HIMEM_START {
        return Err(BootError::SegmentBelowHimem {
            addr: p_paddr,
            himem: HIMEM_START,
        });
    }

    // Check segment fits in guest memory
    let segment_end = p_paddr
        .checked_add(p_memsz)
        .ok_or(BootError::SegmentOutOfBounds {
            addr: p_paddr,
            size: p_memsz,
        })?;
    let mem_end = memory.guest_base() + memory.size() as u64;
    if segment_end > mem_end {
        return Err(BootError::SegmentOutOfBounds {
            addr: p_paddr,
            size: p_memsz,
        });
    }

    // Copy file data into guest memory
    let file_start = usize::try_from(p_offset).map_err(|_| BootError::InvalidElf {
        reason: "p_offset too large",
    })?;
    let file_len = usize::try_from(p_filesz).map_err(|_| BootError::InvalidElf {
        reason: "p_filesz too large",
    })?;

    if file_start + file_len > data.len() {
        return Err(BootError::InvalidElf {
            reason: "segment file data extends past end of file",
        });
    }

    memory.write_bytes(p_paddr, &data[file_start..file_start + file_len])?;

    // BSS: zero the region between p_filesz and p_memsz.
    // Uses direct pointer write to avoid heap-allocating a zeroes buffer.
    if p_memsz > p_filesz {
        let bss_start = p_paddr + p_filesz;
        let bss_len = usize::try_from(p_memsz - p_filesz).map_err(|_| BootError::InvalidElf {
            reason: "bss region too large",
        })?;
        let host_ptr = memory
            .guest_to_host(bss_start)
            .ok_or(BootError::SegmentOutOfBounds {
                addr: bss_start,
                size: p_memsz - p_filesz,
            })?;
        // SAFETY: host_ptr is valid for bss_len bytes within our mmap region
        // (bounds already checked via segment_end above).
        unsafe { ptr::write_bytes(host_ptr, 0, bss_len) };
    }

    Ok(())
}

// ── GDT Setup ──────────────────────────────────────────────────────────────

/// Writes the Global Descriptor Table to guest memory at [`BOOT_GDT_OFFSET`].
///
/// GDT layout (Linux 64-bit boot protocol):
/// - Entry 0: NULL descriptor
/// - Entry 1: CODE segment (64-bit, ring 0)
/// - Entry 2: DATA segment (ring 0)
/// - Entry 3: TSS segment
///
/// Also writes a zeroed IDT at [`BOOT_IDT_OFFSET`].
///
/// # Errors
///
/// Returns [`BootError::MemoryWrite`] if the write fails.
fn write_gdt(memory: &GuestMemory) -> Result<(), BootError> {
    let gdt: [u64; BOOT_GDT_COUNT] = [
        gdt_entry(0, 0, 0),                     // NULL
        gdt_entry(GDT_FLAGS_CODE, 0, 0xf_ffff), // CODE
        gdt_entry(GDT_FLAGS_DATA, 0, 0xf_ffff), // DATA
        gdt_entry(GDT_FLAGS_TSS, 0, 0xf_ffff),  // TSS
    ];

    for (i, entry) in gdt.iter().enumerate() {
        let addr = BOOT_GDT_OFFSET + (i as u64) * 8;
        memory.write_bytes(addr, &entry.to_le_bytes())?;
    }

    // Write zeroed IDT
    memory.write_bytes(BOOT_IDT_OFFSET, &0u64.to_le_bytes())?;

    Ok(())
}

// ── Page Tables ────────────────────────────────────────────────────────────

/// Writes identity-mapped page tables for the first 1 GiB of guest memory.
///
/// Page table structure:
/// - PML4\[0\] → PDPT (at [`PDPTE_START`])
/// - PDPT\[0\] → PD (at [`PDE_START`])
/// - PD\[0..512\] → 512 × 2 MiB huge pages (identity map 0..1 GiB)
///
/// # Errors
///
/// Returns [`BootError::MemoryWrite`] if any write fails.
fn write_page_tables(memory: &GuestMemory) -> Result<(), BootError> {
    // PML4[0] → PDPT
    let pml4_entry = PDPTE_START | PAGE_PRESENT_WRITABLE;
    memory.write_bytes(PML4_START, &pml4_entry.to_le_bytes())?;

    // PDPT[0] → PD
    let pdpte_entry = PDE_START | PAGE_PRESENT_WRITABLE;
    memory.write_bytes(PDPTE_START, &pdpte_entry.to_le_bytes())?;

    // PD[0..512] → 2 MiB identity-mapped pages
    for i in 0..PDE_ENTRY_COUNT {
        let pde_entry = (i << 21) | PAGE_PRESENT_WRITABLE_HUGE;
        let addr = PDE_START + i * 8;
        memory.write_bytes(addr, &pde_entry.to_le_bytes())?;
    }

    Ok(())
}

// ── Kernel Command Line ────────────────────────────────────────────────────

/// Writes the kernel command line to guest memory at [`CMDLINE_START`].
///
/// The command line is NUL-terminated. Maximum length is [`CMDLINE_MAX_SIZE`]
/// bytes including the NUL terminator.
///
/// # Errors
///
/// Returns [`BootError::CmdlineTooLong`] if the command line (with NUL) exceeds
/// the maximum size, or [`BootError::MemoryWrite`] if the write fails.
fn write_cmdline(memory: &GuestMemory, cmdline: &str) -> Result<(), BootError> {
    let len_with_nul = cmdline.len() + 1;
    if len_with_nul > CMDLINE_MAX_SIZE {
        return Err(BootError::CmdlineTooLong {
            len: len_with_nul,
            max: CMDLINE_MAX_SIZE,
        });
    }

    let mut buf = Vec::with_capacity(len_with_nul);
    buf.extend_from_slice(cmdline.as_bytes());
    buf.push(0); // NUL terminator

    memory.write_bytes(CMDLINE_START, &buf)?;

    Ok(())
}

// ── Boot Params (Zero Page) ────────────────────────────────────────────────

// Linux `boot_params` struct field offsets (from `linux/arch/x86/include/uapi/asm/bootparam.h`)
// The struct is 4096 bytes total. We only set the fields we need.

/// Offset of `hdr.type_of_loader` in `boot_params`.
const BP_TYPE_OF_LOADER: u64 = 0x210;

/// Offset of `hdr.boot_flag` in `boot_params`.
const BP_BOOT_FLAG: u64 = 0x1fe;

/// Offset of `hdr.header` magic in `boot_params`.
const BP_HEADER_MAGIC: u64 = 0x202;

/// Offset of `hdr.cmd_line_ptr` in `boot_params`.
const BP_CMD_LINE_PTR: u64 = 0x228;

/// Offset of `hdr.kernel_alignment` in `boot_params`.
const BP_KERNEL_ALIGNMENT: u64 = 0x230;

/// Offset of `e820_entries` count in `boot_params`.
const BP_E820_ENTRIES: u64 = 0x1e8;

/// Offset of `e820_table` array in `boot_params`.
const BP_E820_TABLE: u64 = 0x2d0;

/// Size of a single e820 entry: u64 addr + u64 size + u32 type = 20 bytes.
const E820_ENTRY_SIZE: u64 = 20;

/// e820 type: usable RAM.
const E820_RAM: u32 = 1;

/// e820 type: reserved (not usable by the OS).
const E820_RESERVED: u32 = 2;

/// End of conventional low memory (below EBDA). Matches Firecracker/Cloud Hypervisor.
const EBDA_START: u64 = 0x9_FC00;

/// Writes a single e820 entry to the zero page.
///
/// Each entry is 20 bytes: u64 addr + u64 size + u32 type.
/// The entry index is tracked by `count`, which is incremented after each write.
///
/// # Errors
///
/// Returns [`BootError::MemoryWrite`] if the write fails.
fn write_e820_entry(
    memory: &GuestMemory,
    count: &mut u8,
    addr: u64,
    size: u64,
    mem_type: u32,
) -> Result<(), BootError> {
    let offset = ZERO_PAGE_START + BP_E820_TABLE + u64::from(*count) * E820_ENTRY_SIZE;
    memory.write_bytes(offset, &addr.to_le_bytes())?;
    memory.write_bytes(offset + 8, &size.to_le_bytes())?;
    memory.write_bytes(offset + 16, &mem_type.to_le_bytes())?;
    *count += 1;
    Ok(())
}

/// Populates the zero page (`boot_params`) at [`ZERO_PAGE_START`].
///
/// Sets minimal fields required by the Linux kernel:
/// - `boot_flag` = `0xAA55`
/// - `header` magic = `HdrS` (`0x5372_6448`)
/// - `type_of_loader` = `0xFF` (undefined bootloader)
/// - `cmd_line_ptr` = [`CMDLINE_START`]
/// - `kernel_alignment` = `0x0100_0000` (16 MiB)
/// - e820 memory map with proper low/high memory split
///
/// # Errors
///
/// Returns [`BootError::MemoryWrite`] if any write fails.
fn write_boot_params(memory: &GuestMemory, cmdline: &str) -> Result<(), BootError> {
    // Zero the entire 4096-byte page first
    let zeroes = vec![0u8; 4096];
    memory.write_bytes(ZERO_PAGE_START, &zeroes)?;

    // boot_flag = 0xAA55
    memory.write_bytes(ZERO_PAGE_START + BP_BOOT_FLAG, &0xAA55u16.to_le_bytes())?;

    // header magic = "HdrS"
    memory.write_bytes(
        ZERO_PAGE_START + BP_HEADER_MAGIC,
        &0x5372_6448u32.to_le_bytes(),
    )?;

    // type_of_loader = 0xFF
    memory.write_bytes(ZERO_PAGE_START + BP_TYPE_OF_LOADER, &[0xFF])?;

    // cmd_line_ptr
    let cmdline_ptr = u32::try_from(CMDLINE_START).map_err(|_| BootError::InvalidElf {
        reason: "CMDLINE_START does not fit in u32",
    })?;
    memory.write_bytes(
        ZERO_PAGE_START + BP_CMD_LINE_PTR,
        &cmdline_ptr.to_le_bytes(),
    )?;

    // kernel_alignment = 16 MiB
    memory.write_bytes(
        ZERO_PAGE_START + BP_KERNEL_ALIGNMENT,
        &0x0100_0000u32.to_le_bytes(),
    )?;

    // cmdline_size — offset 0x238
    let cmdline_size = u32::try_from(cmdline.len()).map_err(|_| BootError::InvalidElf {
        reason: "cmdline length does not fit in u32",
    })?;
    memory.write_bytes(ZERO_PAGE_START + 0x238, &cmdline_size.to_le_bytes())?;

    // e820 memory map — properly split into low + high regions.
    // The kernel expects low memory (0..EBDA) and high memory (1MiB..end)
    // to be reported separately, with the ISA hole (0x9FC00..0x100000)
    // either reserved or omitted.
    let mem_end = memory.guest_base() + memory.size() as u64;
    let mut e820_count: u8 = 0;

    if mem_end > HIMEM_START {
        // Entry 0: low conventional memory [0, EBDA_START)
        write_e820_entry(memory, &mut e820_count, 0, EBDA_START, E820_RAM)?;

        // Entry 1: reserved ISA hole [EBDA_START, 0x100000)
        write_e820_entry(
            memory,
            &mut e820_count,
            EBDA_START,
            HIMEM_START - EBDA_START,
            E820_RESERVED,
        )?;

        // Entry 2: high memory [0x100000, end of RAM)
        write_e820_entry(
            memory,
            &mut e820_count,
            HIMEM_START,
            mem_end - HIMEM_START,
            E820_RAM,
        )?;
    } else {
        // Small memory (test scenarios): single entry covering all RAM
        write_e820_entry(memory, &mut e820_count, 0, mem_end, E820_RAM)?;
    }

    // Write e820_entries count
    memory.write_bytes(ZERO_PAGE_START + BP_E820_ENTRIES, &[e820_count])?;

    Ok(())
}

#[cfg(test)]
#[path = "x86_64_test.rs"]
mod tests;
