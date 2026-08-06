//! VFIO device passthrough framework.
//!
//! Provides abstractions for binding host PCI devices to the VFIO subsystem
//! for safe device passthrough to guest VMs. The VFIO (Virtual Function I/O)
//! framework uses IOMMU hardware to isolate device DMA, preventing the guest
//! from accessing host memory outside its assigned regions.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────────────────────────────────┐
//! │  VfioContainer                       │
//! │  /dev/vfio/vfio                      │
//! │  ├── API version check               │
//! │  ├── IOMMU type 1 setup              │
//! │  └── DMA mapping                     │
//! │                                      │
//! │  VfioGroup                           │
//! │  /dev/vfio/<group_id>                │
//! │  ├── viability check                 │
//! │  ├── container attachment            │
//! │  └── device FD acquisition           │
//! │                                      │
//! │  VfioDevice                          │
//! │  ├── region info (BARs)              │
//! │  ├── interrupt setup (MSI-X)         │
//! │  └── device reset                    │
//! └──────────────────────────────────────┘
//! ```
//!
//! # Usage
//!
//! ```rust,no_run
//! use visor_vmm::devices::vfio::{VfioContainer, VfioGroup, VfioPciAddress};
//!
//! let addr = "0000:03:00.0".parse::<VfioPciAddress>().unwrap();
//! let container = VfioContainer::open().unwrap();
//! let group_id = VfioPciAddress::find_iommu_group(&addr).unwrap();
//! let group = VfioGroup::open(group_id).unwrap();
//! ```

#![allow(unsafe_code)]

use std::fmt;
use std::fs;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::str::FromStr;

// ── VFIO ioctl constants ────────────────────────────────────────────
//
// Computed from the Linux kernel _IO macro:
//   _IO(type, nr)  = (type << 8) | nr
//   _IOR(type, nr) = (2 << 30) | (size << 16) | (type << 8) | nr
//   _IOW(type, nr) = (1 << 30) | (size << 16) | (type << 8) | nr
//
// VFIO_TYPE = ';' = 0x3B, VFIO_BASE = 100

/// VFIO ioctl type byte (';').
const VFIO_TYPE: u64 = 0x3B;

/// VFIO ioctl base number.
const VFIO_BASE: u64 = 100;

/// `VFIO_GET_API_VERSION` — `_IO(';', 100)`.
const VFIO_GET_API_VERSION: libc::c_ulong = ((VFIO_TYPE << 8) | VFIO_BASE) as libc::c_ulong;

/// `VFIO_CHECK_EXTENSION` — `_IO(';', 101)`.
const VFIO_CHECK_EXTENSION: libc::c_ulong = ((VFIO_TYPE << 8) | (VFIO_BASE + 1)) as libc::c_ulong;

/// `VFIO_SET_IOMMU` — `_IO(';', 102)`.
const VFIO_SET_IOMMU: libc::c_ulong = ((VFIO_TYPE << 8) | (VFIO_BASE + 2)) as libc::c_ulong;

/// `VFIO_GROUP_GET_STATUS` — `_IO(';', 103)`.
const VFIO_GROUP_GET_STATUS: libc::c_ulong = ((VFIO_TYPE << 8) | (VFIO_BASE + 3)) as libc::c_ulong;

/// `VFIO_GROUP_SET_CONTAINER` — `_IO(';', 104)`.
const VFIO_GROUP_SET_CONTAINER: libc::c_ulong =
    ((VFIO_TYPE << 8) | (VFIO_BASE + 4)) as libc::c_ulong;

/// `VFIO_GROUP_GET_DEVICE_FD` — `_IO(';', 106)`.
const VFIO_GROUP_GET_DEVICE_FD: libc::c_ulong =
    ((VFIO_TYPE << 8) | (VFIO_BASE + 6)) as libc::c_ulong;

/// `VFIO_DEVICE_GET_INFO` — `_IO(';', 107)`.
const VFIO_DEVICE_GET_INFO: libc::c_ulong = ((VFIO_TYPE << 8) | (VFIO_BASE + 7)) as libc::c_ulong;

/// `VFIO_DEVICE_GET_REGION_INFO` — `_IO(';', 108)`.
const VFIO_DEVICE_GET_REGION_INFO: libc::c_ulong =
    ((VFIO_TYPE << 8) | (VFIO_BASE + 8)) as libc::c_ulong;

/// `VFIO_DEVICE_RESET` — `_IO(';', 111)`.
const VFIO_DEVICE_RESET: libc::c_ulong = ((VFIO_TYPE << 8) | (VFIO_BASE + 11)) as libc::c_ulong;

/// `VFIO_IOMMU_MAP_DMA` — `_IO(';', 113)`.
const VFIO_IOMMU_MAP_DMA: libc::c_ulong = ((VFIO_TYPE << 8) | (VFIO_BASE + 13)) as libc::c_ulong;

/// `VFIO_IOMMU_UNMAP_DMA` — `_IO(';', 114)`.
const VFIO_IOMMU_UNMAP_DMA: libc::c_ulong = ((VFIO_TYPE << 8) | (VFIO_BASE + 14)) as libc::c_ulong;

/// `VFIO_DEVICE_SET_IRQS` — `_IO(';', 110)`.
const VFIO_DEVICE_SET_IRQS: libc::c_ulong = ((VFIO_TYPE << 8) | (VFIO_BASE + 10)) as libc::c_ulong;

/// Expected VFIO API version.
const VFIO_API_VERSION: i32 = 0;

/// IOMMU type 1 (x86 page tables).
const VFIO_TYPE1_IOMMU: libc::c_ulong = 1;

/// VFIO group status: group is viable (all devices bound to VFIO).
const VFIO_GROUP_FLAGS_VIABLE: u32 = 1;

/// DMA map flag: readable by device.
const VFIO_DMA_MAP_FLAG_READ: u32 = 1;

/// DMA map flag: writable by device.
const VFIO_DMA_MAP_FLAG_WRITE: u32 = 2;

/// VFIO IRQ action: set data eventfd.
const VFIO_IRQ_SET_DATA_EVENTFD: u32 = 1 << 2;

/// VFIO IRQ action: set action trigger.
const VFIO_IRQ_SET_ACTION_TRIGGER: u32 = 3 << 3;

/// VFIO IRQ index for MSI-X.
const VFIO_PCI_MSIX_IRQ_INDEX: u32 = 2;

// ── Kernel structs (repr(C)) ────────────────────────────────────────

/// VFIO group status (from `vfio_group_status`).
#[repr(C)]
struct VfioGroupStatus {
    argsz: u32,
    flags: u32,
}

/// VFIO device info (from `vfio_device_info`).
#[repr(C)]
struct VfioDeviceInfo {
    argsz: u32,
    flags: u32,
    num_regions: u32,
    num_irqs: u32,
}

/// VFIO region info (from `vfio_region_info`).
#[repr(C)]
struct VfioRegionInfoKern {
    argsz: u32,
    flags: u32,
    index: u32,
    cap_offset: u32,
    size: u64,
    offset: u64,
}

/// VFIO DMA map request (from `vfio_iommu_type1_dma_map`).
#[repr(C)]
struct VfioDmaMapKern {
    argsz: u32,
    flags: u32,
    vaddr: u64,
    iova: u64,
    size: u64,
}

/// VFIO DMA unmap request (from `vfio_iommu_type1_dma_unmap`).
#[repr(C)]
struct VfioDmaUnmapKern {
    argsz: u32,
    flags: u32,
    iova: u64,
    size: u64,
}

/// VFIO IRQ set header (from `vfio_irq_set`).
#[repr(C)]
struct VfioIrqSet {
    argsz: u32,
    flags: u32,
    index: u32,
    start: u32,
    count: u32,
    // followed by data (eventfds)
}

// Precomputed argsz values for VFIO kernel structs.
// These repr(C) structs are 8–32 bytes; truncation from usize is impossible.
#[allow(clippy::cast_possible_truncation)]
const ARGSZ_GROUP_STATUS: u32 = size_of::<VfioGroupStatus>() as u32;
#[allow(clippy::cast_possible_truncation)]
const ARGSZ_DEVICE_INFO: u32 = size_of::<VfioDeviceInfo>() as u32;
#[allow(clippy::cast_possible_truncation)]
const ARGSZ_REGION_INFO: u32 = size_of::<VfioRegionInfoKern>() as u32;
#[allow(clippy::cast_possible_truncation)]
const ARGSZ_DMA_MAP: u32 = size_of::<VfioDmaMapKern>() as u32;
#[allow(clippy::cast_possible_truncation)]
const ARGSZ_DMA_UNMAP: u32 = size_of::<VfioDmaUnmapKern>() as u32;

// ── Error types ─────────────────────────────────────────────────────

/// Errors from VFIO operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VfioError {
    /// Failed to open VFIO container (`/dev/vfio/vfio`).
    #[error("failed to open VFIO container: {0}")]
    OpenContainer(io::Error),

    /// VFIO API version mismatch.
    #[error("VFIO API version mismatch: expected {expected}, got {actual}")]
    ApiVersion {
        /// Expected API version.
        expected: i32,
        /// Actual API version returned by kernel.
        actual: i32,
    },

    /// Failed to set IOMMU type.
    #[error("failed to set IOMMU type: {0}")]
    SetIommuType(io::Error),

    /// Failed to open VFIO group.
    #[error("failed to open VFIO group {group_id}: {source}")]
    OpenGroup {
        /// IOMMU group ID.
        group_id: u32,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// VFIO group is not viable (not all devices bound to VFIO).
    #[error("VFIO group {group_id} is not viable (not all devices bound to vfio-pci)")]
    GroupNotViable {
        /// IOMMU group ID.
        group_id: u32,
    },

    /// Failed to attach group to container.
    #[error("failed to attach group to container: {0}")]
    AttachGroup(io::Error),

    /// Failed to get device FD from group.
    #[error("failed to get device FD for {device}: {source}")]
    GetDevice {
        /// PCI device address string.
        device: String,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Failed to query device region info.
    #[error("failed to query region {index} info: {source}")]
    RegionInfo {
        /// Region index.
        index: u32,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Failed to map DMA region.
    #[error("failed to map DMA at IOVA {iova:#x}: {source}")]
    DmaMap {
        /// IOVA address that failed to map.
        iova: u64,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Failed to unmap DMA region.
    #[error("failed to unmap DMA at IOVA {iova:#x}: {source}")]
    DmaUnmap {
        /// IOVA address that failed to unmap.
        iova: u64,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Failed to reset device.
    #[error("failed to reset VFIO device: {0}")]
    DeviceReset(io::Error),

    /// Failed to parse PCI address.
    #[error("invalid PCI address '{input}': expected format DDDD:BB:DD.F")]
    ParseAddress {
        /// The input string that failed to parse.
        input: String,
    },

    /// Failed to unbind device from current driver.
    #[error("failed to unbind device {address}: {source}")]
    UnbindDevice {
        /// PCI address string.
        address: String,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Failed to bind device to vfio-pci driver.
    #[error("failed to bind {address} to vfio-pci: {source}")]
    BindVfioPci {
        /// PCI address string.
        address: String,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Failed to restore original driver binding.
    #[error("failed to restore driver for {address}: {source}")]
    RestoreDriver {
        /// PCI address string.
        address: String,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Failed to read sysfs attribute.
    #[error("failed to read sysfs for {address}: {source}")]
    ReadSysfs {
        /// PCI address string.
        address: String,
        /// The underlying I/O error.
        source: io::Error,
    },

    /// Failed to set up device interrupts.
    #[error("failed to set up IRQ for device: {0}")]
    IrqSetup(io::Error),
}

// ── PCI address ─────────────────────────────────────────────────────

/// A PCI address in `DDDD:BB:DD.F` (domain:bus:device.function) format.
///
/// # Parsing
///
/// ```
/// use visor_vmm::devices::vfio::VfioPciAddress;
///
/// let addr: VfioPciAddress = "0000:03:00.0".parse().unwrap();
/// assert_eq!(addr.domain(), 0);
/// assert_eq!(addr.bus(), 3);
/// assert_eq!(addr.device(), 0);
/// assert_eq!(addr.function(), 0);
/// ```
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct VfioPciAddress {
    domain: u16,
    bus: u8,
    device: u8,
    function: u8,
}

impl VfioPciAddress {
    /// Creates a new PCI address from individual components.
    #[must_use]
    pub const fn new(domain: u16, bus: u8, device: u8, function: u8) -> Self {
        Self {
            domain,
            bus,
            device,
            function,
        }
    }

    /// Returns the PCI domain (segment).
    #[must_use]
    pub const fn domain(&self) -> u16 {
        self.domain
    }

    /// Returns the PCI bus number.
    #[must_use]
    pub const fn bus(&self) -> u8 {
        self.bus
    }

    /// Returns the PCI device number.
    #[must_use]
    pub const fn device(&self) -> u8 {
        self.device
    }

    /// Returns the PCI function number.
    #[must_use]
    pub const fn function(&self) -> u8 {
        self.function
    }

    /// Returns the sysfs path for this PCI device.
    ///
    /// # Example
    ///
    /// ```
    /// use visor_vmm::devices::vfio::VfioPciAddress;
    ///
    /// let addr = VfioPciAddress::new(0, 3, 0, 0);
    /// assert_eq!(addr.sysfs_path().to_str().unwrap(), "/sys/bus/pci/devices/0000:03:00.0");
    /// ```
    #[must_use]
    pub fn sysfs_path(&self) -> PathBuf {
        PathBuf::from(format!("/sys/bus/pci/devices/{self}"))
    }

    /// Finds the IOMMU group ID for this PCI device.
    ///
    /// Reads the `iommu_group` symlink under the device's sysfs directory
    /// to determine which IOMMU group the device belongs to.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::ReadSysfs`] if the sysfs path cannot be read
    /// or the symlink target cannot be parsed.
    pub fn find_iommu_group(addr: &Self) -> Result<u32, VfioError> {
        let group_link = addr.sysfs_path().join("iommu_group");
        let target = fs::read_link(&group_link).map_err(|e| VfioError::ReadSysfs {
            address: addr.to_string(),
            source: e,
        })?;
        let group_name =
            target
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| VfioError::ReadSysfs {
                    address: addr.to_string(),
                    source: io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid iommu_group symlink",
                    ),
                })?;
        group_name.parse::<u32>().map_err(|_| VfioError::ReadSysfs {
            address: addr.to_string(),
            source: io::Error::new(
                io::ErrorKind::InvalidData,
                format!("non-numeric IOMMU group: {group_name}"),
            ),
        })
    }

    /// Returns the current driver bound to this PCI device, if any.
    ///
    /// Reads the `driver` symlink under the device's sysfs directory.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::ReadSysfs`] on I/O errors other than "not found".
    pub fn current_driver(addr: &Self) -> Result<Option<String>, VfioError> {
        let driver_link = addr.sysfs_path().join("driver");
        match fs::read_link(&driver_link) {
            Ok(target) => {
                let name = target
                    .file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("unknown")
                    .to_string();
                Ok(Some(name))
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(VfioError::ReadSysfs {
                address: addr.to_string(),
                source: e,
            }),
        }
    }

    /// Unbinds this device from its current driver.
    ///
    /// Writes the PCI address to the driver's `unbind` file in sysfs.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::UnbindDevice`] if the unbind write fails.
    pub fn unbind_device(addr: &Self) -> Result<(), VfioError> {
        let unbind_path = addr.sysfs_path().join("driver/unbind");
        fs::write(&unbind_path, addr.to_string()).map_err(|e| VfioError::UnbindDevice {
            address: addr.to_string(),
            source: e,
        })
    }

    /// Binds this device to the `vfio-pci` driver.
    ///
    /// Writes the PCI address to `/sys/bus/pci/drivers/vfio-pci/bind`.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::BindVfioPci`] if the bind write fails.
    pub fn bind_to_vfio(addr: &Self) -> Result<(), VfioError> {
        let bind_path = Path::new("/sys/bus/pci/drivers/vfio-pci/bind");
        fs::write(bind_path, addr.to_string()).map_err(|e| VfioError::BindVfioPci {
            address: addr.to_string(),
            source: e,
        })
    }
}

impl FromStr for VfioPciAddress {
    type Err = VfioError;

    /// Parses a PCI address from `DDDD:BB:DD.F` format.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::ParseAddress`] if the format is invalid.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let make_err = || VfioError::ParseAddress {
            input: s.to_string(),
        };

        // Split "DDDD:BB:DD.F"
        let parts: Vec<&str> = s.split(':').collect();
        if parts.len() != 3 {
            return Err(make_err());
        }

        let domain = u16::from_str_radix(parts[0], 16).map_err(|_| make_err())?;
        let bus = u8::from_str_radix(parts[1], 16).map_err(|_| make_err())?;

        // Split "DD.F"
        let dev_fn: Vec<&str> = parts[2].split('.').collect();
        if dev_fn.len() != 2 {
            return Err(make_err());
        }

        let device = u8::from_str_radix(dev_fn[0], 16).map_err(|_| make_err())?;
        let function = u8::from_str_radix(dev_fn[1], 16).map_err(|_| make_err())?;

        // PCI device number is 5 bits (0-31), function is 3 bits (0-7)
        if device > 31 || function > 7 {
            return Err(make_err());
        }

        Ok(Self {
            domain,
            bus,
            device,
            function,
        })
    }
}

impl fmt::Display for VfioPciAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:04x}:{:02x}:{:02x}.{:01x}",
            self.domain, self.bus, self.device, self.function
        )
    }
}

// ── Public info types ───────────────────────────────────────────────

/// Information about a VFIO device region (BAR).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VfioRegionInfo {
    /// Region index (0-5 for BARs, 6+ for special regions).
    pub index: u32,
    /// Region size in bytes.
    pub size: u64,
    /// Offset for `mmap()`/`pread()` on the device FD.
    pub offset: u64,
    /// Region capability flags.
    pub flags: u32,
}

impl VfioRegionInfo {
    /// Creates a new region info.
    #[must_use]
    pub const fn new(index: u32, size: u64, offset: u64, flags: u32) -> Self {
        Self {
            index,
            size,
            offset,
            flags,
        }
    }

    /// Returns `true` if the region is readable.
    #[must_use]
    pub const fn is_readable(&self) -> bool {
        self.flags & 1 != 0
    }

    /// Returns `true` if the region is writable.
    #[must_use]
    pub const fn is_writable(&self) -> bool {
        self.flags & 2 != 0
    }

    /// Returns `true` if the region supports `mmap`.
    #[must_use]
    pub const fn is_mmapable(&self) -> bool {
        self.flags & 4 != 0
    }
}

/// A DMA mapping request for VFIO IOMMU.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VfioDmaMap {
    /// I/O virtual address (guest physical address for DMA).
    pub iova: u64,
    /// Size of the mapping in bytes.
    pub size: u64,
    /// Host virtual address of the mapping.
    pub user_addr: u64,
    /// Mapping flags (read/write).
    pub flags: u32,
}

impl VfioDmaMap {
    /// Creates a new DMA mapping request with read+write access.
    #[must_use]
    pub const fn new_rw(iova: u64, size: u64, user_addr: u64) -> Self {
        Self {
            iova,
            size,
            user_addr,
            flags: VFIO_DMA_MAP_FLAG_READ | VFIO_DMA_MAP_FLAG_WRITE,
        }
    }
}

// ── VFIO container ──────────────────────────────────────────────────

/// VFIO container wrapping `/dev/vfio/vfio`.
///
/// The container is the top-level VFIO object that manages IOMMU
/// configuration and DMA mappings. Groups are attached to a container
/// to share the same IOMMU domain.
#[non_exhaustive]
pub struct VfioContainer {
    fd: OwnedFd,
}

impl fmt::Debug for VfioContainer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VfioContainer")
            .field("fd", &self.fd.as_raw_fd())
            .finish()
    }
}

impl VfioContainer {
    /// Opens the VFIO container device (`/dev/vfio/vfio`).
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::OpenContainer`] if `/dev/vfio/vfio` cannot be opened.
    /// Returns [`VfioError::ApiVersion`] if the kernel API version is unexpected.
    pub fn open() -> Result<Self, VfioError> {
        let raw_fd = unsafe {
            // SAFETY: Opening /dev/vfio/vfio is a standard VFIO operation.
            // The path is a well-known kernel device node.
            libc::open(c"/dev/vfio/vfio".as_ptr(), libc::O_RDWR | libc::O_CLOEXEC)
        };
        if raw_fd < 0 {
            return Err(VfioError::OpenContainer(io::Error::last_os_error()));
        }

        let fd = unsafe {
            // SAFETY: raw_fd is a valid file descriptor returned by open(2) above.
            OwnedFd::from_raw_fd(raw_fd)
        };

        let container = Self { fd };

        // Verify API version
        let version = container.api_version()?;
        if version != VFIO_API_VERSION {
            return Err(VfioError::ApiVersion {
                expected: VFIO_API_VERSION,
                actual: version,
            });
        }

        Ok(container)
    }

    /// Returns the VFIO API version reported by the kernel.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::OpenContainer`] on ioctl failure.
    fn api_version(&self) -> Result<i32, VfioError> {
        let ret = unsafe {
            // SAFETY: VFIO_GET_API_VERSION is a read-only ioctl on the container fd.
            // It takes no arguments and returns the API version number.
            libc::ioctl(self.fd.as_raw_fd(), VFIO_GET_API_VERSION)
        };
        if ret < 0 {
            return Err(VfioError::OpenContainer(io::Error::last_os_error()));
        }
        Ok(ret)
    }

    /// Checks whether the kernel supports a given VFIO extension.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::OpenContainer`] on ioctl failure.
    pub fn check_extension(&self, extension: libc::c_ulong) -> Result<bool, VfioError> {
        let ret = unsafe {
            // SAFETY: VFIO_CHECK_EXTENSION is a read-only ioctl that checks
            // whether the given extension ID is supported. extension is passed
            // as the ioctl argument.
            libc::ioctl(self.fd.as_raw_fd(), VFIO_CHECK_EXTENSION, extension)
        };
        if ret < 0 {
            return Err(VfioError::OpenContainer(io::Error::last_os_error()));
        }
        Ok(ret > 0)
    }

    /// Sets the IOMMU type for this container to type 1 (x86 page tables).
    ///
    /// Must be called after at least one group is attached.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::SetIommuType`] on failure.
    pub fn set_iommu_type1(&self) -> Result<(), VfioError> {
        let ret = unsafe {
            // SAFETY: VFIO_SET_IOMMU configures the IOMMU model for the container.
            // VFIO_TYPE1_IOMMU is a well-known constant for x86 page-table IOMMU.
            libc::ioctl(self.fd.as_raw_fd(), VFIO_SET_IOMMU, VFIO_TYPE1_IOMMU)
        };
        if ret < 0 {
            return Err(VfioError::SetIommuType(io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Maps a region of host memory for device DMA via the IOMMU.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::DmaMap`] on failure.
    pub fn map_dma(&self, mapping: &VfioDmaMap) -> Result<(), VfioError> {
        let mut dma = VfioDmaMapKern {
            argsz: ARGSZ_DMA_MAP,
            flags: mapping.flags,
            vaddr: mapping.user_addr,
            iova: mapping.iova,
            size: mapping.size,
        };
        let ret = unsafe {
            // SAFETY: VFIO_IOMMU_MAP_DMA creates an IOMMU mapping. The dma struct
            // is correctly sized and initialized with valid fields. The kernel validates
            // the mapping parameters (alignment, overlaps, permissions).
            libc::ioctl(self.fd.as_raw_fd(), VFIO_IOMMU_MAP_DMA, &mut dma)
        };
        if ret < 0 {
            return Err(VfioError::DmaMap {
                iova: mapping.iova,
                source: io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Unmaps a previously mapped DMA region.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::DmaUnmap`] on failure.
    pub fn unmap_dma(&self, iova: u64, size: u64) -> Result<(), VfioError> {
        let mut unmap = VfioDmaUnmapKern {
            argsz: ARGSZ_DMA_UNMAP,
            flags: 0,
            iova,
            size,
        };
        let ret = unsafe {
            // SAFETY: VFIO_IOMMU_UNMAP_DMA removes an IOMMU mapping. The struct
            // is correctly sized and the kernel validates the region.
            libc::ioctl(self.fd.as_raw_fd(), VFIO_IOMMU_UNMAP_DMA, &mut unmap)
        };
        if ret < 0 {
            return Err(VfioError::DmaUnmap {
                iova,
                source: io::Error::last_os_error(),
            });
        }
        Ok(())
    }

    /// Returns the raw file descriptor of the container.
    #[must_use]
    pub fn as_raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

// ── VFIO group ──────────────────────────────────────────────────────

/// VFIO IOMMU group wrapping `/dev/vfio/<group_id>`.
///
/// A group represents a set of devices that share an IOMMU context.
/// All devices in a group must be bound to VFIO for any device in the
/// group to be usable.
#[non_exhaustive]
pub struct VfioGroup {
    fd: OwnedFd,
    group_id: u32,
}

impl fmt::Debug for VfioGroup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VfioGroup")
            .field("fd", &self.fd.as_raw_fd())
            .field("group_id", &self.group_id)
            .finish()
    }
}

impl VfioGroup {
    /// Opens a VFIO group device (`/dev/vfio/<group_id>`).
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::OpenGroup`] if the group device cannot be opened.
    /// Returns [`VfioError::GroupNotViable`] if not all devices in the group
    /// are bound to VFIO drivers.
    pub fn open(group_id: u32) -> Result<Self, VfioError> {
        let path = format!("/dev/vfio/{group_id}\0");
        let raw_fd = unsafe {
            // SAFETY: Opening the VFIO group device node. The path is NUL-terminated
            // and refers to a well-known kernel device.
            libc::open(
                path.as_ptr().cast::<libc::c_char>(),
                libc::O_RDWR | libc::O_CLOEXEC,
            )
        };
        if raw_fd < 0 {
            return Err(VfioError::OpenGroup {
                group_id,
                source: io::Error::last_os_error(),
            });
        }

        let fd = unsafe {
            // SAFETY: raw_fd is a valid file descriptor returned by open(2).
            OwnedFd::from_raw_fd(raw_fd)
        };

        let group = Self { fd, group_id };

        // Check viability
        if !group.is_viable()? {
            return Err(VfioError::GroupNotViable { group_id });
        }

        Ok(group)
    }

    /// Checks whether the group is viable (all devices bound to VFIO).
    fn is_viable(&self) -> Result<bool, VfioError> {
        let mut status = VfioGroupStatus {
            argsz: ARGSZ_GROUP_STATUS,
            flags: 0,
        };
        let ret = unsafe {
            // SAFETY: VFIO_GROUP_GET_STATUS reads the group status. The status
            // struct is correctly sized with argsz set.
            libc::ioctl(self.fd.as_raw_fd(), VFIO_GROUP_GET_STATUS, &mut status)
        };
        if ret < 0 {
            return Err(VfioError::OpenGroup {
                group_id: self.group_id,
                source: io::Error::last_os_error(),
            });
        }
        Ok(status.flags & VFIO_GROUP_FLAGS_VIABLE != 0)
    }

    /// Attaches this group to a VFIO container.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::AttachGroup`] on failure.
    pub fn attach_to_container(&self, container: &VfioContainer) -> Result<(), VfioError> {
        let container_fd = container.as_raw_fd();
        let ret = unsafe {
            // SAFETY: VFIO_GROUP_SET_CONTAINER associates the group with the container.
            // The container_fd is a valid VFIO container file descriptor.
            libc::ioctl(self.fd.as_raw_fd(), VFIO_GROUP_SET_CONTAINER, &container_fd)
        };
        if ret < 0 {
            return Err(VfioError::AttachGroup(io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Gets a device FD from this group for the specified PCI address.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::GetDevice`] on failure.
    pub fn get_device(&self, addr: &VfioPciAddress) -> Result<VfioDevice, VfioError> {
        let addr_str = format!("{addr}\0");
        let raw_fd = unsafe {
            // SAFETY: VFIO_GROUP_GET_DEVICE_FD returns a file descriptor for the
            // device identified by the NUL-terminated PCI address string.
            libc::ioctl(
                self.fd.as_raw_fd(),
                VFIO_GROUP_GET_DEVICE_FD,
                addr_str.as_ptr(),
            )
        };
        if raw_fd < 0 {
            return Err(VfioError::GetDevice {
                device: addr.to_string(),
                source: io::Error::last_os_error(),
            });
        }

        let fd = unsafe {
            // SAFETY: raw_fd is a valid file descriptor returned by the ioctl.
            OwnedFd::from_raw_fd(raw_fd)
        };

        // Get device info
        let mut info = VfioDeviceInfo {
            argsz: ARGSZ_DEVICE_INFO,
            flags: 0,
            num_regions: 0,
            num_irqs: 0,
        };
        let ret = unsafe {
            // SAFETY: VFIO_DEVICE_GET_INFO reads device capabilities. The info
            // struct is correctly sized with argsz set.
            libc::ioctl(fd.as_raw_fd(), VFIO_DEVICE_GET_INFO, &mut info)
        };
        if ret < 0 {
            return Err(VfioError::GetDevice {
                device: addr.to_string(),
                source: io::Error::last_os_error(),
            });
        }

        Ok(VfioDevice {
            fd,
            num_regions: info.num_regions,
            num_irqs: info.num_irqs,
        })
    }

    /// Returns the group ID.
    #[must_use]
    pub const fn group_id(&self) -> u32 {
        self.group_id
    }
}

// ── VFIO device ─────────────────────────────────────────────────────

/// A VFIO device obtained from a group.
///
/// Provides access to device regions (BARs), interrupt configuration,
/// and device reset. The device FD is closed on drop.
#[non_exhaustive]
pub struct VfioDevice {
    fd: OwnedFd,
    num_regions: u32,
    num_irqs: u32,
}

impl fmt::Debug for VfioDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VfioDevice")
            .field("fd", &self.fd.as_raw_fd())
            .field("num_regions", &self.num_regions)
            .field("num_irqs", &self.num_irqs)
            .finish()
    }
}

impl VfioDevice {
    /// Returns the number of device regions.
    #[must_use]
    pub const fn num_regions(&self) -> u32 {
        self.num_regions
    }

    /// Returns the number of device IRQ types.
    #[must_use]
    pub const fn num_irqs(&self) -> u32 {
        self.num_irqs
    }

    /// Returns information about a device region.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::RegionInfo`] if the ioctl fails or the index is invalid.
    pub fn region_info(&self, index: u32) -> Result<VfioRegionInfo, VfioError> {
        let mut info = VfioRegionInfoKern {
            argsz: ARGSZ_REGION_INFO,
            flags: 0,
            index,
            cap_offset: 0,
            size: 0,
            offset: 0,
        };
        let ret = unsafe {
            // SAFETY: VFIO_DEVICE_GET_REGION_INFO reads region capabilities.
            // The info struct is correctly sized with argsz and index set.
            libc::ioctl(self.fd.as_raw_fd(), VFIO_DEVICE_GET_REGION_INFO, &mut info)
        };
        if ret < 0 {
            return Err(VfioError::RegionInfo {
                index,
                source: io::Error::last_os_error(),
            });
        }

        Ok(VfioRegionInfo {
            index: info.index,
            size: info.size,
            offset: info.offset,
            flags: info.flags,
        })
    }

    /// Resets the VFIO device.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::DeviceReset`] if the reset ioctl fails.
    pub fn reset(&self) -> Result<(), VfioError> {
        let ret = unsafe {
            // SAFETY: VFIO_DEVICE_RESET performs a device-level reset. This is
            // a standard VFIO operation and only affects this device.
            libc::ioctl(self.fd.as_raw_fd(), VFIO_DEVICE_RESET)
        };
        if ret < 0 {
            return Err(VfioError::DeviceReset(io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Sets up MSI-X interrupt routing via eventfd.
    ///
    /// Configures the device to signal the given eventfd file descriptors
    /// when MSI-X interrupts fire.
    ///
    /// # Errors
    ///
    /// Returns [`VfioError::IrqSetup`] if the ioctl fails.
    pub fn setup_msix_irqs(&self, eventfds: &[i32]) -> Result<(), VfioError> {
        let count = u32::try_from(eventfds.len()).map_err(|_| {
            VfioError::IrqSetup(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many eventfds",
            ))
        })?;
        let header_size = size_of::<VfioIrqSet>();
        let data_size = std::mem::size_of_val(eventfds);
        let total_size = header_size + data_size;

        // Allocate buffer for header + eventfd array
        let mut buf = vec![0u8; total_size];

        let argsz = u32::try_from(total_size).map_err(|_| {
            VfioError::IrqSetup(io::Error::new(
                io::ErrorKind::InvalidInput,
                "IRQ set buffer too large",
            ))
        })?;

        // Fill header
        let header = VfioIrqSet {
            argsz,
            flags: VFIO_IRQ_SET_DATA_EVENTFD | VFIO_IRQ_SET_ACTION_TRIGGER,
            index: VFIO_PCI_MSIX_IRQ_INDEX,
            start: 0,
            count,
        };

        // Copy header to buffer
        let header_bytes: &[u8] = unsafe {
            // SAFETY: VfioIrqSet is repr(C) and we read exactly its size.
            std::slice::from_raw_parts(std::ptr::from_ref(&header).cast::<u8>(), header_size)
        };
        buf[..header_size].copy_from_slice(header_bytes);

        // Copy eventfd array after header
        let fd_bytes: &[u8] = unsafe {
            // SAFETY: eventfds is a slice of i32; reading as bytes is safe for repr purposes.
            std::slice::from_raw_parts(eventfds.as_ptr().cast::<u8>(), data_size)
        };
        buf[header_size..].copy_from_slice(fd_bytes);

        let ret = unsafe {
            // SAFETY: VFIO_DEVICE_SET_IRQS configures interrupt routing. The buffer
            // contains a correctly-formatted VfioIrqSet header followed by eventfd data.
            libc::ioctl(self.fd.as_raw_fd(), VFIO_DEVICE_SET_IRQS, buf.as_ptr())
        };
        if ret < 0 {
            return Err(VfioError::IrqSetup(io::Error::last_os_error()));
        }
        Ok(())
    }

    /// Returns the raw file descriptor of the device.
    #[must_use]
    pub fn as_raw_fd(&self) -> i32 {
        self.fd.as_raw_fd()
    }
}

#[cfg(test)]
#[path = "vfio_test.rs"]
mod tests;
