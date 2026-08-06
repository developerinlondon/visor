//! Hypervisor platform abstraction.
//!
//! Defines the [`Platform`], [`VmOps`], and [`VcpuOps`] traits that abstract
//! over KVM (Linux), Apple Hypervisor Framework (macOS), and Windows
//! Hypervisor Platform (Windows). Concrete implementations are selected at
//! compile time based on the target OS.
pub mod event;
pub mod regs;

use event::InterruptEvent;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub(crate) use linux::open_tap_interface;
#[cfg(target_os = "linux")]
pub use linux::{KvmPlatform, KvmVcpu, KvmVm, LinuxEventFd};

#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
pub(crate) use macos::poll_kqueue_fd;
#[cfg(target_os = "macos")]
pub use macos::{HvfPlatform, HvfVcpu, HvfVm, MacosEventFd};

#[cfg(target_os = "windows")]
mod windows;
#[cfg(target_os = "windows")]
pub use windows::WhpPlatform;

use regs::{SpecialRegs, StandardRegs};

// ── VM exit data ────────────────────────────────────────────────────

/// Maximum size of inline data in a VM exit (I/O and MMIO accesses).
/// KVM I/O exits are 1/2/4 bytes; MMIO can be up to 8.
pub const VM_EXIT_DATA_MAX: usize = 8;

/// Inline byte buffer for VM exit data. Zero-allocation.
///
/// Stores up to [`VM_EXIT_DATA_MAX`] bytes inline with the length.
/// This avoids heap allocation on every I/O or MMIO exit.
#[derive(Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct ExitData {
    buf: [u8; VM_EXIT_DATA_MAX],
    len: u8,
}

impl ExitData {
    /// Creates an `ExitData` by copying from a slice.
    ///
    /// # Panics
    ///
    /// Panics if `data.len() > VM_EXIT_DATA_MAX`.
    #[must_use]
    pub fn from_slice(data: &[u8]) -> Self {
        assert!(
            data.len() <= VM_EXIT_DATA_MAX,
            "exit data too large: {} > {VM_EXIT_DATA_MAX}",
            data.len()
        );
        let mut buf = [0u8; VM_EXIT_DATA_MAX];
        let len = data.len();
        buf[..len].copy_from_slice(data);
        // The assert above guarantees len <= VM_EXIT_DATA_MAX (8), which fits in u8.
        #[allow(clippy::cast_possible_truncation)]
        let len_u8 = len as u8;
        Self { buf, len: len_u8 }
    }

    /// Returns the data as a byte slice.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.buf[..usize::from(self.len)]
    }

    /// Returns the number of bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        usize::from(self.len)
    }

    /// Returns `true` if the data is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl std::fmt::Debug for ExitData {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:02x?}", self.as_bytes())
    }
}

// ── VM exit types ───────────────────────────────────────────────────

/// Outcome of a single vCPU run iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VmExit {
    /// Guest executed a port I/O read.
    IoIn {
        /// I/O port number.
        port: u16,
        /// Width of the access in bytes.
        size: usize,
    },

    /// Guest executed a port I/O write.
    IoOut {
        /// I/O port number.
        port: u16,
        /// Data written by the guest (inline, no allocation).
        data: ExitData,
    },

    /// Guest performed an MMIO read.
    MmioRead {
        /// Physical address of the MMIO access.
        addr: u64,
        /// Width of the access in bytes.
        size: usize,
    },

    /// Guest performed an MMIO write.
    MmioWrite {
        /// Physical address of the MMIO access.
        addr: u64,
        /// Data written by the guest (inline, no allocation).
        data: ExitData,
    },

    /// Guest executed HLT instruction.
    Halt,

    /// Guest requested shutdown.
    Shutdown,

    /// Guest requested reboot / system reset.
    Reboot,
}

// ── Errors ──────────────────────────────────────────────────────────

/// Errors from hypervisor platform operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PlatformError {
    /// Platform ioctl or system call failed.
    #[error("platform ioctl failed: {0}")]
    System(#[from] std::io::Error),

    /// Hypervisor API version mismatch.
    #[error("expected API version {expected}, got {actual}")]
    ApiVersionMismatch {
        /// Expected API version.
        expected: i32,
        /// Actual API version.
        actual: i32,
    },

    /// The platform is not supported on this OS.
    #[error("platform unsupported on this OS")]
    Unsupported,
}

// ── Traits ──────────────────────────────────────────────────────────

/// Abstraction over a hardware virtualization platform.
///
/// Implementations must be able to open the hypervisor device, verify
/// the API version, and create VM instances.
pub trait Platform: Sized + Send + Sync {
    /// The VM handle type produced by this platform.
    type Vm: VmOps;

    /// Opens the hypervisor device (e.g. `/dev/kvm`).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the hypervisor device cannot be opened
    /// or the API version is unsupported.
    fn new() -> Result<Self, PlatformError>;

    /// Creates a new virtual machine.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the VM cannot be created.
    fn create_vm(&self) -> Result<Self::Vm, PlatformError>;
}

/// Operations on a virtual machine.
///
/// Provides methods to configure interrupt controllers, memory regions,
/// and create virtual CPUs.
pub trait VmOps: Send {
    /// The vCPU handle type produced by this VM.
    type Vcpu: VcpuOps;

    /// Creates the in-kernel IRQ chip (e.g. local APIC + IOAPIC).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the IRQ chip cannot be created.
    fn create_irq_chip(&self) -> Result<(), PlatformError>;

    /// Creates the in-kernel PIT (Programmable Interval Timer).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the PIT cannot be created.
    fn create_pit(&self) -> Result<(), PlatformError>;

    /// Registers a guest memory region with the hypervisor.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the memory region cannot be registered.
    ///
    /// # Safety
    ///
    /// `host_addr` must point to a valid memory region of at least `size` bytes
    /// that remains valid for the lifetime of the VM.
    fn register_memory(
        &self,
        slot: u32,
        guest_addr: u64,
        size: u64,
        host_addr: *mut u8,
    ) -> Result<(), PlatformError>;

    /// Registers an interrupt event for interrupt injection.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the interrupt event cannot be registered.
    fn register_irqfd(&self, event: &dyn InterruptEvent, gsi: u32) -> Result<(), PlatformError>;

    /// Creates a new virtual CPU.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the vCPU cannot be created.
    fn create_vcpu(&self, index: u64) -> Result<Self::Vcpu, PlatformError>;
}

/// Operations on a virtual CPU.
///
/// Provides methods to get/set registers and run the vCPU.
pub trait VcpuOps: Send {
    /// Sets general-purpose registers.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the registers cannot be set.
    fn set_regs(&self, regs: &StandardRegs) -> Result<(), PlatformError>;

    /// Gets general-purpose registers.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the registers cannot be read.
    fn get_regs(&self) -> Result<StandardRegs, PlatformError>;

    /// Sets special (system) registers.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the registers cannot be set.
    fn set_sregs(&self, sregs: &SpecialRegs) -> Result<(), PlatformError>;

    /// Gets special (system) registers.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the registers cannot be read.
    fn get_sregs(&self) -> Result<SpecialRegs, PlatformError>;

    /// Runs the vCPU until a VM exit occurs.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError`] if the run fails.
    fn run(&mut self) -> Result<VmExit, PlatformError>;
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
