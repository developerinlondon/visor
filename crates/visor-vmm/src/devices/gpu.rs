//! GPU passthrough via VFIO.
//!
//! Provides GPU device detection, VFIO GPU device wrapping, BAR passthrough,
//! DMA mapping, VGA arbitration, and GPU reset handling.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────────┐
//! │  GpuPassthrough  │  ── manages lifecycle
//! │                  │
//! │  detect_gpus()   │  ── scans sysfs /sys/bus/pci/devices/
//! │  prepare()       │  ── validates config, finds GPU
//! │  bar_regions()   │  ── BAR info from VFIO
//! │  setup_dma()     │  ── IOMMU mapping
//! │  reset()         │  ── FLR via sysfs
//! └─────────────────┘
//! ```
//!
//! # GPU Detection
//!
//! GPUs are detected by scanning PCI devices in sysfs for class codes
//! in the `0x03xxxx` range (display controllers):
//!
//! - `0x030000` — VGA compatible controller
//! - `0x030200` — 3D controller
//! - `0x038000` — Display controller (other)

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

// ── Constants ──────────────────────────────────────────────────────

/// Sysfs path for PCI device enumeration.
const SYSFS_PCI_DEVICES: &str = "/sys/bus/pci/devices";

/// PCI base class for display controllers.
const PCI_CLASS_DISPLAY: u32 = 0x03;

// ── Errors ─────────────────────────────────────────────────────────

/// Errors from GPU passthrough operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GpuError {
    /// No GPU found on the host.
    #[error("no GPU found on host")]
    NoGpuFound,

    /// GPU at the specified address was not found.
    #[error("GPU at {address} not found")]
    GpuNotFound {
        /// The PCI address that was not found.
        address: String,
    },

    /// GPU at the specified address is the boot VGA device.
    #[error("GPU at {address} is the boot VGA device and cannot be passed through")]
    BootVga {
        /// The PCI address of the boot VGA device.
        address: String,
    },

    /// Failed to detect GPUs via sysfs.
    #[error("failed to detect GPUs: {0}")]
    Detection(std::io::Error),

    /// Failed to bind GPU to vfio-pci driver.
    #[error("failed to bind GPU to vfio-pci: {0}")]
    VfioBind(String),

    /// Failed to set up DMA mappings.
    #[error("failed to set up DMA: {0}")]
    DmaSetup(String),

    /// Failed to reset GPU.
    #[error("failed to reset GPU at {address}: {source}")]
    Reset {
        /// PCI address of the GPU that failed to reset.
        address: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },

    /// VGA arbitration failed.
    #[error("VGA arbitration failed: {0}")]
    VgaArbitration(std::io::Error),

    /// General GPU passthrough error.
    #[error("GPU passthrough error: {0}")]
    Passthrough(String),
}

// ── Reset method ───────────────────────────────────────────────────

/// GPU reset method preference, ordered by priority.
///
/// Function Level Reset (FLR) is preferred as it only affects the
/// specific function, not the entire device or bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
#[non_exhaustive]
pub enum ResetMethod {
    /// Function Level Reset — resets only this PCI function.
    #[default]
    Flr = 0,
    /// PCI bus reset — resets all devices on the bus.
    BusReset = 1,
    /// Power Management reset — uses D3hot → D0 transition.
    PmReset = 2,
}

impl fmt::Display for ResetMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Flr => write!(f, "FLR"),
            Self::BusReset => write!(f, "bus reset"),
            Self::PmReset => write!(f, "PM reset"),
        }
    }
}

// ── BAR flags ──────────────────────────────────────────────────────

/// Flags describing a GPU BAR (Base Address Register) region.
///
/// Implemented as a bitflag wrapper around `u32`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BarFlags(u32);

impl BarFlags {
    /// Memory-mapped BAR region.
    pub const MEMORY: Self = Self(0b0001);
    /// I/O-mapped BAR region.
    pub const IO: Self = Self(0b0010);
    /// Prefetchable memory region.
    pub const PREFETCHABLE: Self = Self(0b0100);
    /// 64-bit addressable region.
    pub const BITS_64: Self = Self(0b1000);

    /// Returns `true` if all flags in `other` are set in `self`.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns `true` if no flags are set.
    #[must_use]
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Returns the raw bits.
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
}

impl std::ops::BitOr for BarFlags {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

impl std::ops::BitAnd for BarFlags {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self(self.0 & rhs.0)
    }
}

// ── BAR region ─────────────────────────────────────────────────────

/// A GPU BAR (Base Address Register) region descriptor.
///
/// Describes a single memory or I/O region of the GPU as exposed
/// through VFIO.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct GpuBarRegion {
    /// BAR index (0–5).
    pub index: u8,
    /// Offset within the VFIO device file descriptor.
    pub offset: u64,
    /// Size of the region in bytes.
    pub size: u64,
    /// Region flags.
    pub flags: BarFlags,
}

// ── DMA region ─────────────────────────────────────────────────────

/// A DMA memory region mapping for GPU access.
///
/// Represents a guest memory region that the GPU can access via IOMMU.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub struct DmaRegion {
    /// I/O Virtual Address (IOVA) visible to the GPU.
    pub iova: u64,
    /// Size of the mapped region in bytes.
    pub size: u64,
}

// ── GPU config ─────────────────────────────────────────────────────

/// Configuration for GPU passthrough.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GpuConfig {
    /// Explicit PCI address to passthrough, or `None` for auto-detect.
    pub pci_address: Option<String>,
    /// Whether to manage VGA arbitration (disable legacy VGA I/O decode).
    pub vga_arbitration: bool,
    /// Preferred reset method for GPU cleanup.
    pub reset_method: ResetMethod,
}

impl Default for GpuConfig {
    fn default() -> Self {
        Self {
            pci_address: None,
            vga_arbitration: true,
            reset_method: ResetMethod::Flr,
        }
    }
}

// ── GPU device ─────────────────────────────────────────────────────

/// A detected GPU on the host.
///
/// Populated from sysfs PCI device attributes.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct GpuDevice {
    /// PCI address (e.g. `"0000:01:00.0"`).
    pub pci_address: String,
    /// PCI vendor ID (e.g. `0x10de` for NVIDIA).
    pub vendor_id: u16,
    /// PCI device ID.
    pub device_id: u16,
    /// Human-readable device name.
    pub device_name: String,
    /// Human-readable vendor name.
    pub vendor_name: String,
    /// Full PCI class code (24-bit, e.g. `0x030000` for VGA).
    pub pci_class: u32,
    /// Currently bound kernel driver, if any.
    pub current_driver: Option<String>,
    /// IOMMU group number, if assigned.
    pub iommu_group: Option<u32>,
    /// Whether this is the boot VGA device (console output).
    pub is_boot_vga: bool,
}

impl fmt::Display for GpuDevice {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let driver = self.current_driver.as_deref().unwrap_or("none");
        let name = if self.device_name.is_empty() {
            &self.vendor_name
        } else {
            &self.device_name
        };
        write!(
            f,
            "{} {} [{:04x}:{:04x}] (driver: {driver})",
            self.pci_address, name, self.vendor_id, self.device_id,
        )
    }
}

// ── GPU passthrough ────────────────────────────────────────────────

/// Manages the GPU passthrough lifecycle.
///
/// Handles GPU detection, validation, and prepares the device for
/// VFIO passthrough. The actual VFIO binding requires the VFIO module
/// (layer 2) which may not be available yet.
#[derive(Debug)]
#[non_exhaustive]
pub struct GpuPassthrough {
    /// The GPU device being passed through.
    pub device: GpuDevice,
    /// Configuration used for this passthrough.
    pub config: GpuConfig,
    /// Original driver before VFIO bind (for restore on drop).
    pub original_driver: Option<String>,
    /// BAR regions discovered from VFIO.
    pub bar_regions: Vec<GpuBarRegion>,
    /// DMA regions currently mapped.
    pub dma_regions: Vec<DmaRegion>,
    /// Whether VGA arbitration was disabled.
    pub vga_disabled: bool,
}

impl GpuPassthrough {
    /// Prepares a GPU for passthrough based on the provided configuration.
    ///
    /// Detects GPUs on the host, selects the appropriate one (either by
    /// explicit PCI address or auto-detection), and validates it is suitable
    /// for passthrough (not boot VGA, has IOMMU group).
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::NoGpuFound`] if no passthrough-capable GPU is found.
    /// Returns [`GpuError::GpuNotFound`] if the specified PCI address doesn't match any GPU.
    /// Returns [`GpuError::BootVga`] if the selected GPU is the boot VGA device.
    /// Returns [`GpuError::Detection`] if sysfs scanning fails.
    pub fn prepare(config: &GpuConfig) -> Result<Self, GpuError> {
        let gpus = detect_gpus()?;

        let device = if let Some(ref addr) = config.pci_address {
            // Explicit PCI address — find it.
            let gpu = gpus
                .iter()
                .find(|g| g.pci_address == *addr)
                .ok_or_else(|| GpuError::GpuNotFound {
                    address: addr.clone(),
                })?;

            if gpu.is_boot_vga {
                return Err(GpuError::BootVga {
                    address: addr.clone(),
                });
            }

            gpu.clone()
        } else {
            // Auto-detect — find first passthrough-capable GPU.
            gpus.iter()
                .find(|g| is_passthrough_capable(g))
                .cloned()
                .ok_or(GpuError::NoGpuFound)?
        };

        let original_driver = device.current_driver.clone();

        Ok(Self {
            device,
            config: config.clone(),
            original_driver,
            bar_regions: Vec::new(),
            dma_regions: Vec::new(),
            vga_disabled: false,
        })
    }

    /// Returns the BAR regions for this GPU.
    ///
    /// Populated after VFIO binding (empty before that).
    #[must_use]
    pub fn bar_regions(&self) -> &[GpuBarRegion] {
        &self.bar_regions
    }

    /// Returns the DMA regions currently mapped for this GPU.
    #[must_use]
    pub fn dma_regions(&self) -> &[DmaRegion] {
        &self.dma_regions
    }

    /// Triggers a GPU reset via the sysfs `reset` file.
    ///
    /// Writes `"1"` to `/sys/bus/pci/devices/<addr>/reset` to trigger
    /// a Function Level Reset (FLR).
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::Reset`] if the reset file cannot be written.
    pub fn reset(&self) -> Result<(), GpuError> {
        let reset_path = PathBuf::from(SYSFS_PCI_DEVICES)
            .join(&self.device.pci_address)
            .join("reset");

        fs::write(&reset_path, "1").map_err(|e| GpuError::Reset {
            address: self.device.pci_address.clone(),
            source: e,
        })
    }

    /// Disables VGA arbitration by writing to `/dev/vga_arbiter`.
    ///
    /// This prevents the GPU from responding to legacy VGA I/O port
    /// and memory decode, which is required when passing through a
    /// VGA-class device to avoid conflicts with the host console.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::VgaArbitration`] if the arbiter file cannot be written.
    pub fn disable_vga_arbitration(&mut self) -> Result<(), GpuError> {
        let arbiter_path = Path::new("/dev/vga_arbiter");
        let cmd = format!("decodes none:PCI:{}", self.device.pci_address);
        fs::write(arbiter_path, cmd.as_bytes()).map_err(GpuError::VgaArbitration)?;
        self.vga_disabled = true;
        Ok(())
    }

    /// Sets up DMA mappings for guest memory regions.
    ///
    /// Each tuple in `guest_regions` is `(guest_phys_addr, size)`.
    /// These regions are recorded for later VFIO IOMMU mapping.
    ///
    /// # Errors
    ///
    /// Returns [`GpuError::DmaSetup`] if mapping fails.
    pub fn setup_dma(&mut self, guest_regions: &[(u64, u64)]) -> Result<(), GpuError> {
        self.dma_regions.clear();
        for &(iova, size) in guest_regions {
            if size == 0 {
                return Err(GpuError::DmaSetup(
                    "DMA region size must not be zero".into(),
                ));
            }
            self.dma_regions.push(DmaRegion { iova, size });
        }
        Ok(())
    }
}

// ── Public helpers ─────────────────────────────────────────────────

/// Detects GPUs on the host by scanning sysfs PCI devices.
///
/// Reads `/sys/bus/pci/devices/` and filters for devices whose PCI
/// class code has base class `0x03` (display controller).
///
/// # Errors
///
/// Returns [`GpuError::Detection`] if the sysfs directory cannot be read.
pub fn detect_gpus() -> Result<Vec<GpuDevice>, GpuError> {
    let pci_dir = Path::new(SYSFS_PCI_DEVICES);
    if !pci_dir.exists() {
        return Ok(Vec::new());
    }

    let entries = fs::read_dir(pci_dir).map_err(GpuError::Detection)?;
    let mut gpus = Vec::new();

    for entry in entries {
        let entry = entry.map_err(GpuError::Detection)?;
        let pci_address = entry.file_name().to_string_lossy().into_owned();

        let device_path = entry.path();

        let class = read_sysfs_hex(&device_path.join("class")).unwrap_or(0);
        if !is_gpu_class(class) {
            continue;
        }

        let vendor_id =
            u16::try_from(read_sysfs_hex(&device_path.join("vendor")).unwrap_or(0)).unwrap_or(0);
        let device_id =
            u16::try_from(read_sysfs_hex(&device_path.join("device")).unwrap_or(0)).unwrap_or(0);

        let current_driver = read_driver_name(&device_path);
        let iommu_group = read_iommu_group(&device_path);
        let is_boot_vga = read_sysfs_trim(&device_path.join("boot_vga")).is_some_and(|v| v == "1");

        let vname = vendor_name(vendor_id).to_owned();

        gpus.push(GpuDevice {
            pci_address,
            vendor_id,
            device_id,
            device_name: String::new(),
            vendor_name: vname,
            pci_class: class,
            current_driver,
            iommu_group,
            is_boot_vga,
        });
    }

    Ok(gpus)
}

/// Returns `true` if the PCI class code represents a display controller (GPU).
///
/// Matches any device with base class `0x03` (display controller), which
/// includes VGA controllers (`0x0300xx`), XGA (`0x0301xx`), 3D controllers
/// (`0x0302xx`), and other display controllers (`0x0380xx`).
#[must_use]
pub fn is_gpu_class(class_code: u32) -> bool {
    (class_code >> 16) == PCI_CLASS_DISPLAY
}

/// Returns `true` if the GPU is suitable for VFIO passthrough.
///
/// A GPU is passthrough-capable if:
/// - It is NOT the boot VGA device (needed for host console).
/// - It has an IOMMU group (required for VFIO isolation).
#[must_use]
pub fn is_passthrough_capable(gpu: &GpuDevice) -> bool {
    !gpu.is_boot_vga && gpu.iommu_group.is_some()
}

/// Maps a PCI vendor ID to a human-readable vendor name.
///
/// Returns `"Unknown"` for unrecognized vendor IDs.
#[must_use]
pub fn vendor_name(vendor_id: u16) -> &'static str {
    match vendor_id {
        0x10de => "NVIDIA",
        0x1002 => "AMD",
        0x8086 => "Intel",
        0x1a03 => "ASPEED Technology",
        0x1022 => "AMD (ATI)",
        0x15ad => "VMware",
        0x1ab8 => "Parallels",
        0x1414 => "Microsoft Hyper-V",
        _ => "Unknown",
    }
}

// ── Internal helpers ───────────────────────────────────────────────

/// Reads a sysfs file and parses it as a hex integer (with `0x` prefix).
fn read_sysfs_hex(path: &Path) -> Option<u32> {
    let content = fs::read_to_string(path).ok()?;
    let trimmed = content.trim().trim_start_matches("0x");
    u32::from_str_radix(trimmed, 16).ok()
}

/// Reads a sysfs file and returns its trimmed content.
fn read_sysfs_trim(path: &Path) -> Option<String> {
    fs::read_to_string(path).ok().map(|s| s.trim().to_owned())
}

/// Reads the driver name from a sysfs device directory.
///
/// The driver is a symlink at `<device>/driver` → `../../../bus/pci/drivers/<name>`.
fn read_driver_name(device_path: &Path) -> Option<String> {
    let driver_link = device_path.join("driver");
    let target = fs::read_link(driver_link).ok()?;
    target.file_name().map(|n| n.to_string_lossy().into_owned())
}

/// Reads the IOMMU group number from a sysfs device directory.
///
/// The IOMMU group is a symlink at `<device>/iommu_group` →
/// `../../../kernel/iommu_groups/<number>`.
fn read_iommu_group(device_path: &Path) -> Option<u32> {
    let group_link = device_path.join("iommu_group");
    let target = fs::read_link(group_link).ok()?;
    target
        .file_name()
        .and_then(|n| n.to_string_lossy().parse::<u32>().ok())
}

#[cfg(test)]
#[path = "gpu_test.rs"]
mod tests;
