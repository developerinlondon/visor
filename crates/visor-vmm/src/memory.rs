//! Guest physical memory management.
//!
//! Allocates demand-paged memory via `mmap(MAP_ANONYMOUS | MAP_PRIVATE | MAP_NORESERVE)`
//! and registers it with the hypervisor as a guest memory region. Optionally
//! supports huge pages (2 MiB / 1 GiB) for reduced TLB pressure on large VMs.
//!
//! # Memory Model
//!
//! Guest memory is demand-paged: the `mmap` call creates virtual address space
//! but consumes zero physical RAM until pages are touched. A 10 GiB VM running
//! `echo hello` uses only ~30-50 MiB of physical RAM.
//!
//! When huge pages are requested via [`GuestMemory::with_huge_pages`], the allocator
//! attempts `MAP_HUGETLB` with the appropriate size flag. If the kernel cannot satisfy
//! the request (no huge pages configured, size not aligned, etc.), it transparently
//! falls back to regular 4 KiB pages.
//!
//! # Safety
//!
//! This module uses `unsafe` for `mmap`/`munmap` system calls. All unsafe blocks
//! have `// SAFETY:` comments documenting their invariants.

#![allow(unsafe_code)]

use std::fmt;
#[cfg(target_os = "linux")]
use std::fs;
#[cfg(target_os = "linux")]
use std::path::Path;
use std::ptr;

use crate::platform::{PlatformError, VmOps};

/// Errors from guest memory operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MemoryError {
    /// `mmap` allocation failed.
    #[error("mmap failed for {size} bytes: {source}")]
    Allocation {
        /// Requested allocation size.
        size: usize,
        /// Underlying OS error.
        source: std::io::Error,
    },

    /// Guest address is outside the allocated memory region.
    #[error("guest address {addr:#x} out of bounds (memory size: {size:#x})")]
    OutOfBounds {
        /// The invalid guest address.
        addr: u64,
        /// Total guest memory size.
        size: usize,
    },

    /// Access would extend past the end of guest memory.
    #[error("access at {addr:#x} + {len} bytes exceeds memory size {size:#x}")]
    AccessOverflow {
        /// Start address of the access.
        addr: u64,
        /// Length of the access in bytes.
        len: usize,
        /// Total guest memory size.
        size: usize,
    },

    /// Hypervisor memory region registration failed.
    #[error("memory region registration failed: {0}")]
    Registration(#[from] PlatformError),

    /// Failed to detect huge page availability from sysfs.
    #[error("huge page detection failed: {detail}")]
    HugePageDetection {
        /// Description of what went wrong.
        detail: String,
    },
}

/// Requested huge page size for guest memory allocation.
///
/// When passed to [`GuestMemory::with_huge_pages`], the allocator will attempt
/// to use the requested huge page size. If unavailable, it falls back to
/// regular 4 KiB pages transparently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum HugePageSize {
    /// Regular 4 KiB pages (no huge pages).
    #[default]
    None,
    /// 2 MiB huge pages (`MAP_HUGE_2MB`).
    TwoMiB,
    /// 1 GiB huge pages (`MAP_HUGE_1GB`).
    OneGiB,
}

impl HugePageSize {
    /// Returns the page size in bytes, or 0 for [`HugePageSize::None`].
    #[must_use]
    pub const fn size_bytes(self) -> usize {
        match self {
            Self::None => 0,
            Self::TwoMiB => 2 * 1024 * 1024,
            Self::OneGiB => 1024 * 1024 * 1024,
        }
    }

    /// Returns the `MAP_HUGETLB | MAP_HUGE_*` flags for `mmap`, or 0 for [`HugePageSize::None`].
    #[cfg(target_os = "linux")]
    const fn mmap_flags(self) -> libc::c_int {
        match self {
            Self::None => 0,
            Self::TwoMiB => libc::MAP_HUGETLB | libc::MAP_HUGE_2MB,
            Self::OneGiB => libc::MAP_HUGETLB | libc::MAP_HUGE_1GB,
        }
    }
}

impl fmt::Display for HugePageSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::None => f.write_str("none"),
            Self::TwoMiB => f.write_str("2 MiB"),
            Self::OneGiB => f.write_str("1 GiB"),
        }
    }
}

/// Information about available huge pages on the system.
///
/// Only available on Linux, where huge pages are managed via sysfs.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HugePageInfo {
    /// Total number of 2 MiB huge pages configured.
    pub two_mib_total: u64,
    /// Number of 2 MiB huge pages currently free.
    pub two_mib_available: u64,
    /// Total number of 1 GiB huge pages configured.
    pub one_gib_total: u64,
    /// Number of 1 GiB huge pages currently free.
    pub one_gib_available: u64,
}

#[cfg(target_os = "linux")]
impl HugePageInfo {
    /// Returns `true` if the requested huge page size has free pages available.
    ///
    /// Always returns `true` for [`HugePageSize::None`].
    #[must_use]
    pub const fn has(self, size: HugePageSize) -> bool {
        match size {
            HugePageSize::None => true,
            HugePageSize::TwoMiB => self.two_mib_available > 0,
            HugePageSize::OneGiB => self.one_gib_available > 0,
        }
    }
}

/// Default sysfs path for huge page information.
#[cfg(target_os = "linux")]
const HUGEPAGES_SYSFS_PATH: &str = "/sys/kernel/mm/hugepages";

/// Detects available huge pages by reading `/sys/kernel/mm/hugepages/`.
///
/// Returns [`HugePageInfo`] with counts for 2 MiB and 1 GiB huge pages.
/// If the sysfs directory does not exist (e.g. non-Linux), returns all zeros.
///
/// # Errors
///
/// Returns [`MemoryError::HugePageDetection`] if sysfs files exist but cannot be parsed.
#[cfg(target_os = "linux")]
pub fn detect_huge_pages() -> Result<HugePageInfo, MemoryError> {
    detect_huge_pages_from(HUGEPAGES_SYSFS_PATH)
}

/// Detects available huge pages from a custom sysfs path.
///
/// This is the same as [`detect_huge_pages`] but allows specifying a custom
/// path for testing or non-standard sysfs mounts.
///
/// # Errors
///
/// Returns [`MemoryError::HugePageDetection`] if sysfs files exist but cannot be parsed.
#[cfg(target_os = "linux")]
pub fn detect_huge_pages_from(sysfs_path: &str) -> Result<HugePageInfo, MemoryError> {
    let base = Path::new(sysfs_path);
    if !base.exists() {
        return Ok(HugePageInfo {
            two_mib_total: 0,
            two_mib_available: 0,
            one_gib_total: 0,
            one_gib_available: 0,
        });
    }

    let two_mib_dir = base.join("hugepages-2048kB");
    let one_gib_dir = base.join("hugepages-1048576kB");

    Ok(HugePageInfo {
        two_mib_total: read_sysfs_u64(&two_mib_dir.join("nr_hugepages"))?,
        two_mib_available: read_sysfs_u64(&two_mib_dir.join("free_hugepages"))?,
        one_gib_total: read_sysfs_u64(&one_gib_dir.join("nr_hugepages"))?,
        one_gib_available: read_sysfs_u64(&one_gib_dir.join("free_hugepages"))?,
    })
}

/// Reads a single `u64` value from a sysfs file. Returns 0 if the file does not exist.
#[cfg(target_os = "linux")]
fn read_sysfs_u64(path: &Path) -> Result<u64, MemoryError> {
    match fs::read_to_string(path) {
        Ok(contents) => {
            contents
                .trim()
                .parse::<u64>()
                .map_err(|e| MemoryError::HugePageDetection {
                    detail: format!("failed to parse {}: {e}", path.display()),
                })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(e) => Err(MemoryError::HugePageDetection {
            detail: format!("failed to read {}: {e}", path.display()),
        }),
    }
}

/// A contiguous region of guest physical memory backed by anonymous mmap.
///
/// The memory is demand-paged (`MAP_NORESERVE`) — physical RAM is consumed
/// only when pages are written to. Drop unmaps the memory automatically.
///
/// Optionally backed by huge pages when created via [`GuestMemory::with_huge_pages`].
pub struct GuestMemory {
    host_addr: *mut u8,
    size: usize,
    guest_base: u64,
    huge_pages: bool,
}

impl fmt::Debug for GuestMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("GuestMemory")
            .field("size", &self.size)
            .field("guest_base", &format_args!("{:#x}", self.guest_base))
            .field("huge_pages", &self.huge_pages)
            .finish_non_exhaustive()
    }
}

// SAFETY: The mmap region is process-wide. Sending the pointer between threads
// is safe because we never create aliasing mutable references — all access goes
// through methods that take &self with explicit offset calculations.
unsafe impl Send for GuestMemory {}

// SAFETY: All read/write methods use atomic-safe (non-overlapping) pointer
// arithmetic from a single base. No interior mutability races are possible
// because each guest address maps to a unique host address.
unsafe impl Sync for GuestMemory {}

impl GuestMemory {
    /// Allocates `size` bytes of demand-paged guest memory starting at guest
    /// physical address `guest_base`.
    ///
    /// The memory is backed by `mmap(MAP_ANONYMOUS | MAP_PRIVATE | MAP_NORESERVE)`.
    /// No physical RAM is consumed until pages are accessed.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Allocation`] if the `mmap` call fails.
    pub fn new(size: usize, guest_base: u64) -> Result<Self, MemoryError> {
        // SAFETY: mmap with MAP_ANONYMOUS does not reference any file.
        // MAP_PRIVATE ensures writes are copy-on-write (not shared).
        // MAP_NORESERVE avoids reserving swap for the entire region.
        // We check for MAP_FAILED before using the pointer.
        let addr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_PRIVATE | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };

        if addr == libc::MAP_FAILED {
            return Err(MemoryError::Allocation {
                size,
                source: std::io::Error::last_os_error(),
            });
        }

        Ok(Self {
            host_addr: addr.cast::<u8>(),
            size,
            guest_base,
            huge_pages: false,
        })
    }

    /// Allocates guest memory with optional huge page backing.
    ///
    /// Attempts to allocate using the requested [`HugePageSize`]. If the kernel
    /// cannot satisfy the request (no huge pages available, size not aligned to
    /// the huge page boundary, etc.), transparently falls back to regular 4 KiB pages.
    ///
    /// For [`HugePageSize::None`], this behaves identically to [`GuestMemory::new`].
    ///
    /// Only available on Linux, where `MAP_HUGETLB` is supported.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Allocation`] if both the huge page and fallback
    /// `mmap` calls fail.
    #[cfg(target_os = "linux")]
    pub fn with_huge_pages(
        size: usize,
        guest_base: u64,
        huge_page_size: HugePageSize,
    ) -> Result<Self, MemoryError> {
        if huge_page_size == HugePageSize::None {
            return Self::new(size, guest_base);
        }

        // Huge pages require size aligned to the huge page boundary.
        let page_bytes = huge_page_size.size_bytes();
        if size % page_bytes != 0 {
            return Self::new(size, guest_base);
        }

        let extra_flags = huge_page_size.mmap_flags();

        // SAFETY: mmap with MAP_ANONYMOUS | MAP_HUGETLB does not reference any file.
        // MAP_PRIVATE ensures writes are copy-on-write. We check for MAP_FAILED
        // before using the pointer. MAP_NORESERVE is intentionally omitted for
        // huge pages because they are pre-allocated in the kernel pool.
        let addr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_ANONYMOUS | libc::MAP_PRIVATE | extra_flags,
                -1,
                0,
            )
        };

        if addr == libc::MAP_FAILED {
            // Huge page allocation failed — fall back to regular pages.
            return Self::new(size, guest_base);
        }

        Ok(Self {
            host_addr: addr.cast::<u8>(),
            size,
            guest_base,
            huge_pages: true,
        })
    }

    /// Returns `true` if this memory region is backed by huge pages.
    #[must_use]
    pub const fn using_huge_pages(&self) -> bool {
        self.huge_pages
    }

    /// Returns the host-side base pointer for this memory region.
    #[must_use]
    pub fn host_addr(&self) -> *mut u8 {
        self.host_addr
    }

    /// Returns the total size of this memory region in bytes.
    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }

    /// Returns the guest physical base address.
    #[must_use]
    pub fn guest_base(&self) -> u64 {
        self.guest_base
    }

    /// Computes the byte offset from guest base, returning an error if out of bounds.
    fn guest_offset(&self, guest_addr: u64) -> Result<usize, MemoryError> {
        let raw = guest_addr
            .checked_sub(self.guest_base)
            .ok_or(MemoryError::OutOfBounds {
                addr: guest_addr,
                size: self.size,
            })?;
        usize::try_from(raw).map_err(|_| MemoryError::OutOfBounds {
            addr: guest_addr,
            size: self.size,
        })
    }

    /// Translates a guest physical address to a host pointer.
    ///
    /// Returns `None` if the address is outside this memory region.
    #[must_use]
    pub fn guest_to_host(&self, guest_addr: u64) -> Option<*mut u8> {
        let offset = guest_addr.checked_sub(self.guest_base)?;
        let offset = usize::try_from(offset).ok()?;
        if offset >= self.size {
            return None;
        }
        // SAFETY: offset is verified to be within the mmap region bounds.
        Some(unsafe { self.host_addr.add(offset) })
    }

    /// Writes `data` to guest memory at `guest_addr`.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::OutOfBounds`] if the start address is invalid,
    /// or [`MemoryError::AccessOverflow`] if the write would exceed the region.
    pub fn write_bytes(&self, guest_addr: u64, data: &[u8]) -> Result<(), MemoryError> {
        let host_ptr = self
            .guest_to_host(guest_addr)
            .ok_or(MemoryError::OutOfBounds {
                addr: guest_addr,
                size: self.size,
            })?;

        let offset = self.guest_offset(guest_addr)?;
        if offset + data.len() > self.size {
            return Err(MemoryError::AccessOverflow {
                addr: guest_addr,
                len: data.len(),
                size: self.size,
            });
        }

        // SAFETY: host_ptr is within the mmap region (verified above),
        // and offset + data.len() <= self.size (also verified).
        unsafe {
            ptr::copy_nonoverlapping(data.as_ptr(), host_ptr, data.len());
        }
        Ok(())
    }

    /// Reads `len` bytes from guest memory at `guest_addr`.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::OutOfBounds`] if the start address is invalid,
    /// or [`MemoryError::AccessOverflow`] if the read would exceed the region.
    pub fn read_bytes(&self, guest_addr: u64, len: usize) -> Result<Vec<u8>, MemoryError> {
        let host_ptr = self
            .guest_to_host(guest_addr)
            .ok_or(MemoryError::OutOfBounds {
                addr: guest_addr,
                size: self.size,
            })?;

        let offset = self.guest_offset(guest_addr)?;
        if offset + len > self.size {
            return Err(MemoryError::AccessOverflow {
                addr: guest_addr,
                len,
                size: self.size,
            });
        }

        let mut buf = vec![0u8; len];
        // SAFETY: host_ptr is within the mmap region (verified above),
        // and offset + len <= self.size (also verified).
        unsafe {
            ptr::copy_nonoverlapping(host_ptr, buf.as_mut_ptr(), len);
        }
        Ok(buf)
    }

    /// Registers this memory region with the hypervisor as a guest physical memory slot.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError::Registration`] if the hypervisor rejects the region.
    pub fn register<V: VmOps>(&self, vm: &V, slot: u32) -> Result<(), MemoryError> {
        vm.register_memory(slot, self.guest_base, self.size as u64, self.host_addr)?;
        Ok(())
    }

    /// Creates a `GuestMemory` from a pre-existing mmap region.
    ///
    /// # Safety
    ///
    /// The caller must ensure:
    /// - `host_addr` points to a valid `mmap` region of exactly `size` bytes
    /// - The region was allocated with `mmap` and must be freed with `munmap`
    /// - No other code will call `munmap` on this region (ownership transfers here)
    pub fn from_raw_mmap(host_addr: *mut u8, size: usize, guest_base: u64) -> Self {
        Self {
            host_addr,
            size,
            guest_base,
            huge_pages: false,
        }
    }

    /// Creates a `GuestMemory` backed by a shared memory file descriptor.
    ///
    /// Maps the given fd with `MAP_SHARED` so both parent and worker processes
    /// see the same physical pages. The fd must have been created via
    /// `shm_open()` or equivalent and sized to at least `size` bytes.
    ///
    /// # Errors
    ///
    /// Returns [`MemoryError`] if the `mmap` call fails.
    pub fn from_shared_fd(fd: std::os::fd::RawFd, size: usize, guest_base: u64) -> Result<Self, MemoryError> {
        let addr = crate::shared_memory::mmap_shared_fd(fd, size)?;
        Ok(Self {
            host_addr: addr,
            size,
            guest_base,
            huge_pages: false,
        })
    }
}

impl Drop for GuestMemory {
    fn drop(&mut self) {
        // SAFETY: host_addr was returned by a successful mmap call with size
        // self.size. We only call munmap once (in Drop), and the pointer is
        // not used after this.
        unsafe {
            libc::munmap(self.host_addr.cast(), self.size);
        }
    }
}

#[cfg(test)]
#[path = "memory_test.rs"]
mod tests;
