//! Kernel boot protocol setup for guest VMs.
//!
//! Populates guest memory with the data structures required by the Linux boot
//! protocol and returns a [`BootConfig`] describing where the vCPU should
//! start execution.
//!
//! # Supported architectures
//!
//! - **`x86_64`**: Linux 64-bit boot protocol (direct boot to long mode)
//! - **`aarch64`**: ARM64 Linux boot protocol (FDT / device tree)

#![allow(unsafe_code)]

#[cfg(target_arch = "x86_64")]
pub mod x86_64;

#[cfg(target_arch = "aarch64")]
pub mod aarch64;

// ── Memory Layout Constants (x86_64) ────────────────────────────────────────
//
// These match the Linux boot protocol and Firecracker's layout. All addresses
// are guest physical addresses.

/// GDT (Global Descriptor Table) base address in guest memory.
#[cfg(target_arch = "x86_64")]
pub const BOOT_GDT_OFFSET: u64 = 0x0500;

/// IDT (Interrupt Descriptor Table) base address in guest memory.
#[cfg(target_arch = "x86_64")]
pub const BOOT_IDT_OFFSET: u64 = 0x0520;

/// Linux "zero page" / `boot_params` address in guest memory.
#[cfg(target_arch = "x86_64")]
pub const ZERO_PAGE_START: u64 = 0x7000;

/// Initial stack pointer for the boot CPU.
#[cfg(target_arch = "x86_64")]
pub const BOOT_STACK_POINTER: u64 = 0x8ff0;

/// PML4 page table base address (CR3 value).
#[cfg(target_arch = "x86_64")]
pub const PML4_START: u64 = 0x9000;

/// Page Directory Pointer Table Entry base address.
#[cfg(target_arch = "x86_64")]
pub const PDPTE_START: u64 = 0xa000;

/// Page Directory Entry base address.
#[cfg(target_arch = "x86_64")]
pub const PDE_START: u64 = 0xb000;

/// Kernel command line start address in guest memory.
#[cfg(target_arch = "x86_64")]
pub const CMDLINE_START: u64 = 0x2_0000;

/// Maximum kernel command line length in bytes (including NUL terminator).
#[cfg(target_arch = "x86_64")]
pub const CMDLINE_MAX_SIZE: usize = 2048;

/// Start of high memory — minimum kernel load address.
#[cfg(target_arch = "x86_64")]
pub const HIMEM_START: u64 = 0x10_0000;

/// Number of GDT entries (NULL, CODE, DATA, TSS).
#[cfg(target_arch = "x86_64")]
pub const BOOT_GDT_COUNT: usize = 4;

// ── GDT Segment Flags ──────────────────────────────────────────────────────

/// 64-bit code segment descriptor flags for Linux boot protocol.
#[cfg(target_arch = "x86_64")]
pub const GDT_FLAGS_CODE: u16 = 0xa09b;

/// Data segment descriptor flags for Linux boot protocol.
#[cfg(target_arch = "x86_64")]
pub const GDT_FLAGS_DATA: u16 = 0xc093;

/// TSS segment descriptor flags for Linux boot protocol.
#[cfg(target_arch = "x86_64")]
pub const GDT_FLAGS_TSS: u16 = 0x808b;

// ── Page Table Flags ───────────────────────────────────────────────────────

/// Present + Writable flags for PML4/PDPT entries.
#[cfg(target_arch = "x86_64")]
pub const PAGE_PRESENT_WRITABLE: u64 = 0x03;

/// Present + Writable + Page Size (2 MiB huge page) for PD entries.
#[cfg(target_arch = "x86_64")]
pub const PAGE_PRESENT_WRITABLE_HUGE: u64 = 0x83;

/// Number of 2 MiB page directory entries (512 × 2 MiB = 1 GiB identity map).
#[cfg(target_arch = "x86_64")]
pub const PDE_ENTRY_COUNT: u64 = 512;

// ── CR / EFER Bits ─────────────────────────────────────────────────────────

/// CR0: Protected Mode Enable.
#[cfg(target_arch = "x86_64")]
pub const X86_CR0_PE: u64 = 0x1;

/// CR0: Paging Enable.
#[cfg(target_arch = "x86_64")]
pub const X86_CR0_PG: u64 = 0x8000_0000;

/// CR4: Physical Address Extension.
#[cfg(target_arch = "x86_64")]
pub const X86_CR4_PAE: u64 = 0x20;

/// EFER: Long Mode Enable.
#[cfg(target_arch = "x86_64")]
pub const EFER_LME: u64 = 0x100;

/// EFER: Long Mode Active.
#[cfg(target_arch = "x86_64")]
pub const EFER_LMA: u64 = 0x400;

// ── Types ──────────────────────────────────────────────────────────────────

/// Boot configuration returned after setting up guest memory (`x86_64`).
///
/// Contains the register values the vCPU needs to start executing the kernel.
/// Layer 5 (vCPU) reads these values to initialize the processor state.
#[cfg(target_arch = "x86_64")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BootConfig {
    /// Kernel entry point address (set as RIP).
    pub entry_point: u64,

    /// Initial stack pointer (set as RSP and RBP).
    pub stack_pointer: u64,

    /// Address of `boot_params` / zero page (set as RSI).
    pub boot_params_addr: u64,

    /// PML4 page table base address (set as CR3).
    pub pml4_addr: u64,
}

/// Boot configuration returned after setting up guest memory (aarch64).
///
/// Contains the register values the vCPU needs to start executing the kernel.
/// PC is set to `entry_point`, X0 is set to `fdt_addr`.
#[cfg(target_arch = "aarch64")]
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct BootConfig {
    /// Kernel entry point address (set as PC on boot CPU).
    pub entry_point: u64,

    /// Address of the Flattened Device Tree blob (set as X0 on boot CPU).
    pub fdt_addr: u64,
}

/// Errors that can occur during kernel boot setup.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum BootError {
    /// Failed to read the kernel file.
    #[error("failed to read kernel file: {0}")]
    KernelRead(std::io::Error),

    /// The kernel file is not a valid ELF64 executable.
    #[error("invalid ELF: {reason}")]
    InvalidElf {
        /// Description of what's wrong with the ELF.
        reason: &'static str,
    },

    /// The kernel file is not a valid ARM64 Image.
    #[error("invalid ARM64 Image: {reason}")]
    InvalidImage {
        /// Description of what's wrong with the Image.
        reason: &'static str,
    },

    /// A `PT_LOAD` segment would be placed below `HIMEM_START`.
    #[error("kernel segment at {addr:#x} is below HIMEM_START ({himem:#x})")]
    SegmentBelowHimem {
        /// The offending segment's physical address.
        addr: u64,
        /// The minimum allowed address.
        himem: u64,
    },

    /// A kernel segment exceeds the guest memory size.
    #[error("kernel segment at {addr:#x} + {size:#x} exceeds guest memory")]
    SegmentOutOfBounds {
        /// Segment physical address.
        addr: u64,
        /// Segment size in bytes.
        size: u64,
    },

    /// Failed to write to guest memory.
    #[error("guest memory write failed: {0}")]
    MemoryWrite(#[from] crate::memory::MemoryError),

    /// The kernel command line exceeds the maximum allowed size.
    #[error("cmdline length {len} exceeds max {max}")]
    CmdlineTooLong {
        /// Actual command line length (including NUL).
        len: usize,
        /// Maximum allowed length.
        max: usize,
    },

    /// FDT (Flattened Device Tree) generation failed.
    #[error("FDT generation failed: {0}")]
    Fdt(String),
}

// ── Shared Utilities ─────────────────────────────────────────────────────

use std::fs::File;
use std::os::unix::io::AsRawFd;
use std::path::Path;
use std::ptr;

/// A read-only memory-mapped file. Unmaps on drop.
pub(crate) struct MappedFile {
    addr: *const u8,
    len: usize,
}

impl MappedFile {
    /// Returns the mapped data as a byte slice.
    fn as_slice(&self) -> &[u8] {
        // SAFETY: addr is a valid mmap region of `len` bytes,
        // and we hold it for the lifetime of this struct.
        unsafe { std::slice::from_raw_parts(self.addr, self.len) }
    }
}

impl std::ops::Deref for MappedFile {
    type Target = [u8];

    fn deref(&self) -> &[u8] {
        self.as_slice()
    }
}

impl Drop for MappedFile {
    fn drop(&mut self) {
        // SAFETY: addr was returned by mmap with size self.len.
        unsafe {
            libc::munmap(self.addr as *mut libc::c_void, self.len);
        }
    }
}

/// Memory-maps a file read-only. Zero-copy kernel loading.
///
/// # Errors
///
/// Returns [`BootError::KernelRead`] if the file cannot be opened or mapped.
pub(crate) fn mmap_file(path: &Path) -> Result<MappedFile, BootError> {
    let file = File::open(path).map_err(BootError::KernelRead)?;
    let metadata = file.metadata().map_err(BootError::KernelRead)?;
    let len = usize::try_from(metadata.len()).map_err(|_| {
        BootError::KernelRead(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "kernel file too large",
        ))
    })?;

    if len == 0 {
        return Err(BootError::KernelRead(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "kernel file is empty",
        )));
    }

    // SAFETY: We mmap the file as read-only (PROT_READ) with MAP_PRIVATE.
    // The file descriptor is kept open by the File handle for the duration
    // of this function. The MappedFile takes ownership of the mapping.
    let addr = unsafe {
        libc::mmap(
            ptr::null_mut(),
            len,
            libc::PROT_READ,
            libc::MAP_PRIVATE,
            file.as_raw_fd(),
            0,
        )
    };

    if addr == libc::MAP_FAILED {
        return Err(BootError::KernelRead(std::io::Error::last_os_error()));
    }

    Ok(MappedFile {
        addr: addr.cast::<u8>(),
        len,
    })
}
