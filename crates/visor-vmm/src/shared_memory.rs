//! POSIX shared memory helpers for process-per-VM guest RAM sharing.
//!
//! On macOS, Apple's Hypervisor.framework (HVF) limits each process to a single
//! VM. To support multiple VMs, visor spawns one worker process per VM. The
//! parent daemon creates a POSIX shared memory region (`shm_open`) for guest RAM,
//! maps it with `MAP_SHARED`, and the worker re-opens the region by name via
//! [`SharedMemoryRegion::open_existing`] (since file descriptors are not inherited
//! across `posix_spawn`/`exec`).
//!
//! # Usage
//!
//! **Parent (daemon):**
//! ```text
//! let region = SharedMemoryRegion::create("/vsr-3-abc12345", 512 * MIB)?;
//! // Spawn worker, passing region.name() in config
//! // After worker sends Ready, call region.unlink()
//! ```
//!
//! **Worker (child process):**
//! ```text
//! let region = SharedMemoryRegion::open_existing(shm_name, size)?;
//! let memory = GuestMemory::from_shared_fd(region.fd(), size, guest_base)?;
//! ```
//!
//! # Safety
//!
//! This module uses `unsafe` for `shm_open`, `shm_unlink`, `ftruncate`, `mmap`,
//! and `munmap` system calls. All unsafe blocks have `// SAFETY:` comments.

#![allow(unsafe_code)]

use std::ffi::CString;
use std::os::fd::RawFd;
use std::ptr;

use crate::memory::MemoryError;

/// Errors specific to shared memory operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SharedMemoryError {
    /// `shm_open` failed.
    #[error("shm_open failed for '{name}': {source}")]
    Open {
        /// Shared memory object name.
        name: String,
        /// Underlying OS error.
        source: std::io::Error,
    },

    /// `ftruncate` failed.
    #[error("ftruncate failed for '{name}' (size={size}): {source}")]
    Truncate {
        /// Shared memory object name.
        name: String,
        /// Requested size in bytes.
        size: usize,
        /// Underlying OS error.
        source: std::io::Error,
    },

    /// `mmap` failed on the shared memory fd.
    #[error("mmap(MAP_SHARED) failed for '{name}' (size={size}): {source}")]
    Mmap {
        /// Shared memory object name.
        name: String,
        /// Requested size in bytes.
        size: usize,
        /// Underlying OS error.
        source: std::io::Error,
    },

    /// `shm_unlink` failed.
    #[error("shm_unlink failed for '{name}': {source}")]
    Unlink {
        /// Shared memory object name.
        name: String,
        /// Underlying OS error.
        source: std::io::Error,
    },

    /// The shared memory name contains a null byte.
    #[error("shared memory name contains null byte: '{name}'")]
    InvalidName {
        /// The offending name.
        name: String,
    },

    /// `mmap` failed when mapping an inherited fd in the worker.
    #[error("mmap(MAP_SHARED) failed for inherited fd {fd} (size={size}): {source}")]
    MmapFd {
        /// The file descriptor number.
        fd: i32,
        /// Requested size in bytes.
        size: usize,
        /// Underlying OS error.
        source: std::io::Error,
    },
}

/// A POSIX shared memory region backed by `shm_open` + `mmap(MAP_SHARED)`.
///
/// Created by the parent daemon for guest RAM. The worker process re-opens
/// the region by name via [`SharedMemoryRegion::open_existing`], since file
/// descriptors are not inherited across `posix_spawn`/`exec`.
/// Both processes see the same physical pages.
///
/// Drop unmaps the region and closes the fd. The parent is responsible for
/// calling [`unlink`] to remove the named shared memory object after the
/// worker has opened it.
pub struct SharedMemoryRegion {
    /// Name passed to `shm_open` (e.g., "/visor-vm-abc123").
    name: String,
    /// File descriptor from `shm_open`.
    fd: RawFd,
    /// Base pointer from `mmap(MAP_SHARED)`.
    addr: *mut u8,
    /// Size of the region in bytes.
    size: usize,
}

impl std::fmt::Debug for SharedMemoryRegion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SharedMemoryRegion")
            .field("name", &self.name)
            .field("fd", &self.fd)
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

// SAFETY: The mmap region is process-wide. Sending the pointer between threads
// is safe because all access goes through methods with explicit offset calculations,
// same as GuestMemory.
unsafe impl Send for SharedMemoryRegion {}

// SAFETY: All access is via the raw pointer with non-overlapping offset arithmetic.
// No interior mutability races are possible.
unsafe impl Sync for SharedMemoryRegion {}

impl SharedMemoryRegion {
    /// Creates a new POSIX shared memory region.
    ///
    /// Calls `shm_open(O_CREAT | O_RDWR)`, `ftruncate` to set the size, and
    /// `mmap(MAP_SHARED)` to map the region into this process.
    ///
    /// The `name` must start with `/` and not contain embedded null bytes
    /// (POSIX requirement).
    ///
    /// # Errors
    ///
    /// Returns [`SharedMemoryError`] if any system call fails.
    pub fn create(name: &str, size: usize) -> Result<Self, SharedMemoryError> {
        let c_name = CString::new(name).map_err(|_| SharedMemoryError::InvalidName {
            name: name.to_owned(),
        })?;

        // SAFETY: shm_open creates or opens a POSIX shared memory object.
        // O_CREAT | O_RDWR creates a new object or opens existing with read-write.
        // 0o600 gives owner read-write permissions only.
        // We check for -1 (error) before using the fd.
        let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_CREAT | libc::O_RDWR, 0o600) };
        if fd == -1 {
            return Err(SharedMemoryError::Open {
                name: name.to_owned(),
                source: std::io::Error::last_os_error(),
            });
        }

        // SAFETY: ftruncate sets the size of the shared memory object.
        // fd is a valid file descriptor from shm_open above.
        // We check for -1 (error) before proceeding.
        #[allow(clippy::useless_conversion)]
        let ret = unsafe { libc::ftruncate(fd, i64::try_from(size).unwrap_or(i64::MAX)) };
        if ret == -1 {
            let err = std::io::Error::last_os_error();
            // Clean up the fd and shm object on failure.
            unsafe {
                libc::close(fd);
                libc::shm_unlink(c_name.as_ptr());
            }
            return Err(SharedMemoryError::Truncate {
                name: name.to_owned(),
                size,
                source: err,
            });
        }

        // SAFETY: mmap with MAP_SHARED maps the shm fd into our address space.
        // fd is valid from shm_open, size was set by ftruncate.
        // We check for MAP_FAILED before using the pointer.
        let addr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_NORESERVE,
                fd,
                0,
            )
        };

        if addr == libc::MAP_FAILED {
            let err = std::io::Error::last_os_error();
            unsafe {
                libc::close(fd);
                libc::shm_unlink(c_name.as_ptr());
            }
            return Err(SharedMemoryError::Mmap {
                name: name.to_owned(),
                size,
                source: err,
            });
        }

        Ok(Self {
            name: name.to_owned(),
            fd,
            addr: addr.cast::<u8>(),
            size,
        })
    }

    /// Opens an existing POSIX shared memory object by name (without creating).
    ///
    /// Calls `shm_open(O_RDWR)` (no `O_CREAT`) and `mmap(MAP_SHARED)` to map
    /// the region into this process. Used by worker processes to re-open the
    /// shared memory created by the parent, since file descriptors are not
    /// inherited across `posix_spawn`/`exec`.
    ///
    /// # Errors
    ///
    /// Returns [`SharedMemoryError`] if `shm_open` or `mmap` fails.
    pub fn open_existing(name: &str, size: usize) -> Result<Self, SharedMemoryError> {
        let c_name = CString::new(name).map_err(|_| SharedMemoryError::InvalidName {
            name: name.to_owned(),
        })?;

        // SAFETY: shm_open opens an existing POSIX shared memory object.
        // O_RDWR opens with read-write access. We do NOT pass O_CREAT,
        // so this fails if the object does not exist.
        // We check for -1 (error) before using the fd.
        let fd = unsafe { libc::shm_open(c_name.as_ptr(), libc::O_RDWR, 0) };
        if fd == -1 {
            return Err(SharedMemoryError::Open {
                name: name.to_owned(),
                source: std::io::Error::last_os_error(),
            });
        }

        // SAFETY: mmap with MAP_SHARED maps the shm fd into our address space.
        // fd is valid from shm_open above, size is caller-provided.
        // We check for MAP_FAILED before using the pointer.
        let addr = unsafe {
            libc::mmap(
                ptr::null_mut(),
                size,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED | libc::MAP_NORESERVE,
                fd,
                0,
            )
        };

        if addr == libc::MAP_FAILED {
            let err = std::io::Error::last_os_error();
            // SAFETY: close the fd on mmap failure.
            unsafe {
                libc::close(fd);
            }
            return Err(SharedMemoryError::Mmap {
                name: name.to_owned(),
                size,
                source: err,
            });
        }

        Ok(Self {
            name: name.to_owned(),
            fd,
            addr: addr.cast::<u8>(),
            size,
        })
    }

    /// Returns the raw file descriptor for this shared memory region.
    ///
    /// The fd can be inherited by child processes via `fork()`.
    #[must_use]
    pub fn fd(&self) -> RawFd {
        self.fd
    }

    /// Returns the mmap'd base pointer.
    #[must_use]
    pub fn as_ptr(&self) -> *mut u8 {
        self.addr
    }

    /// Returns the size of the shared memory region in bytes.
    #[must_use]
    pub fn size(&self) -> usize {
        self.size
    }

    /// Returns the POSIX shared memory name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Unlinks (removes) the named shared memory object from the filesystem.
    ///
    /// After unlinking, existing mappings remain valid, but no new processes
    /// can open the object by name. Call this after all workers have inherited
    /// the fd.
    ///
    /// # Errors
    ///
    /// Returns [`SharedMemoryError::Unlink`] if `shm_unlink` fails.
    pub fn unlink(&self) -> Result<(), SharedMemoryError> {
        let c_name = CString::new(self.name.as_str()).map_err(|_| {
            SharedMemoryError::InvalidName {
                name: self.name.clone(),
            }
        })?;
        // SAFETY: shm_unlink removes the named shared memory object.
        // The name is a valid C string. Existing mappings remain valid.
        let ret = unsafe { libc::shm_unlink(c_name.as_ptr()) };
        if ret == -1 {
            return Err(SharedMemoryError::Unlink {
                name: self.name.clone(),
                source: std::io::Error::last_os_error(),
            });
        }
        Ok(())
    }
}

impl Drop for SharedMemoryRegion {
    fn drop(&mut self) {
        // SAFETY: addr was returned by a successful mmap call, and size matches.
        // We only call munmap once (in Drop). The pointer is not used after this.
        unsafe {
            libc::munmap(self.addr.cast(), self.size);
            libc::close(self.fd);
        }
    }
}

/// Maps an inherited shared memory file descriptor into the current process.
///
/// Called by the worker process after `fork()` to map the parent-created
/// shared memory region into its own address space.
///
/// Returns a raw pointer suitable for [`GuestMemory::from_shared_fd`].
///
/// # Safety
///
/// The caller must ensure `fd` is a valid file descriptor pointing to a
/// shared memory region of at least `size` bytes.
///
/// # Errors
///
/// Returns [`SharedMemoryError::MmapFd`] if `mmap` fails.
pub fn mmap_shared_fd(fd: RawFd, size: usize) -> Result<*mut u8, SharedMemoryError> {
    // SAFETY: mmap with MAP_SHARED on the inherited fd.
    // The fd was created by the parent via shm_open and inherited through fork().
    // We check for MAP_FAILED before returning the pointer.
    let addr = unsafe {
        libc::mmap(
            ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_SHARED | libc::MAP_NORESERVE,
            fd,
            0,
        )
    };

    if addr == libc::MAP_FAILED {
        return Err(SharedMemoryError::MmapFd {
            fd,
            size,
            source: std::io::Error::last_os_error(),
        });
    }

    Ok(addr.cast::<u8>())
}

/// Convenience: unlink a shared memory object by name (standalone function).
///
/// Useful when the parent needs to clean up without holding a `SharedMemoryRegion`.
/// Ignores `ENOENT` (already unlinked).
///
/// # Errors
///
/// Returns [`SharedMemoryError::Unlink`] on failure (except `ENOENT`).
pub fn unlink_shared_memory(name: &str) -> Result<(), SharedMemoryError> {
    let c_name = CString::new(name).map_err(|_| SharedMemoryError::InvalidName {
        name: name.to_owned(),
    })?;
    // SAFETY: shm_unlink removes the named shared memory object.
    let ret = unsafe { libc::shm_unlink(c_name.as_ptr()) };
    if ret == -1 {
        let err = std::io::Error::last_os_error();
        if err.raw_os_error() == Some(libc::ENOENT) {
            return Ok(()); // Already gone, not an error.
        }
        return Err(SharedMemoryError::Unlink {
            name: name.to_owned(),
            source: err,
        });
    }
    Ok(())
}

impl From<SharedMemoryError> for MemoryError {
    fn from(e: SharedMemoryError) -> Self {
        match e {
            SharedMemoryError::Mmap { size, source, .. }
            | SharedMemoryError::MmapFd { size, source, .. } => {
                MemoryError::Allocation { size, source }
            }
            other => MemoryError::Allocation {
                size: 0,
                source: std::io::Error::other(other.to_string()),
            },
        }
    }
}

#[cfg(test)]
#[path = "shared_memory_test.rs"]
mod tests;
