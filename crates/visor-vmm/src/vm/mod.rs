//! Portable VM boot facade and exit handling types.
//!
//! This module provides the platform-agnostic API for booting a microVM.
//! All platform-specific details (KVM vs HVF, `x86_64` vs `aarch64`, ACPI vs FDT,
//! PIO vs MMIO) are handled internally — callers see a single [`boot()`]
//! function and a uniform [`BootedVm`] result.
//!
//! # Exit Handling
//!
//! The [`ExitHandler`] trait, [`ExitAction`] enum, and [`VcpuError`] type
//! define the portable interface for handling VM exits. [`DeviceManager`]
//! implements `ExitHandler` to dispatch exits to device buses.
//!
//! # Architecture
//!
//! ```text
//! visor-runtime                    visor-vmm::vm
//!   build_cmdline()  ──────────►  boot(&VmConfig)
//!   kernel_path()                   │
//!                                   ├─ platform (KVM / HVF)
//!                                   ├─ boot (x86_64 / aarch64)
//!                                   ├─ devices (serial, block, vsock)
//!                                   └─ BootedVm { serial_output, ... }
//! ```

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;

// Re-export configure_vcpu_boot_regs so crate-level callers (and tests) can access it.
#[cfg(target_os = "macos")]
pub(crate) use macos::configure_vcpu_boot_regs;

use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
#[cfg(target_os = "macos")]
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::devices::DeviceManager;
use crate::devices::vsock_muxer::VsockMuxer;
use crate::guest_virtualization::{GuestVirtualizationError, GuestVirtualizationMode};
use crate::memory::GuestMemory;
#[cfg(all(test, target_os = "macos"))]
use crate::platform::Platform;
#[cfg(target_os = "macos")]
use crate::platform::VmOps;
use crate::transport::mmio::MmioTransport;

// Re-export VM exit types from platform (single source of truth).
pub use crate::platform::VmExit;
pub use crate::platform::{ExitData, VM_EXIT_DATA_MAX};

// ── Exit Handling Types ──────────────────────────────────────────────

/// Errors from vCPU operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VcpuError {
    /// Failed to create the vCPU.
    #[error("failed to create vCPU: {0}")]
    Create(std::io::Error),

    /// Failed to set general-purpose registers.
    #[error("failed to set registers: {0}")]
    SetRegs(std::io::Error),

    /// Failed to set special registers (sregs).
    #[error("failed to set special registers: {0}")]
    SetSregs(std::io::Error),

    /// Failed to get special registers (sregs).
    #[error("failed to get special registers: {0}")]
    GetSregs(std::io::Error),

    /// Failed to set FPU state.
    #[error("failed to set FPU: {0}")]
    SetFpu(std::io::Error),

    /// Failed to set MSRs.
    #[error("failed to set MSRs: {0}")]
    SetMsrs(std::io::Error),

    /// Not all MSRs were written.
    #[error("only {written} of {total} MSRs were set")]
    MsrsIncomplete {
        /// Number of MSRs actually written.
        written: usize,
        /// Number of MSRs requested.
        total: usize,
    },

    /// Hypervisor run failed.
    #[error("vCPU run failed: {0}")]
    Run(std::io::Error),

    /// Hypervisor reported a fatal entry failure.
    #[error("entry failure: hardware_entry_failure_reason={reason:#x}, cpu={cpu}")]
    FailEntry {
        /// Hardware-reported failure reason.
        reason: u64,
        /// CPU index.
        cpu: u32,
    },

    /// Hypervisor internal error.
    #[error("hypervisor internal error")]
    InternalError,

    /// Boot setup error.
    #[error("boot setup failed: {0}")]
    Boot(#[from] crate::boot::BootError),

    /// Failed to get supported CPUID from hypervisor.
    #[error("failed to get supported CPUID: {0}")]
    GetCpuid(std::io::Error),

    /// Failed to set CPUID on the vCPU.
    #[error("failed to set CPUID: {0}")]
    SetCpuid(std::io::Error),
}

/// Action the run loop should take after handling a VM exit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ExitAction {
    /// Continue running the vCPU.
    Continue,

    /// Stop the vCPU run loop.
    Stop,
}

/// Trait for handling VM exits dispatched from the run loop.
///
/// Implement this on your device manager or VM controller to handle
/// I/O, MMIO, and lifecycle events.
///
/// The `handle_io_read` and `handle_mmio_read` methods are called **inside**
/// the run loop while the hypervisor data buffer is still live, allowing device
/// responses to be written directly into the shared memory region.
pub trait ExitHandler {
    /// Handle a VM exit and return the action the run loop should take.
    ///
    /// # Errors
    ///
    /// Implementations may return errors for unrecoverable conditions.
    fn handle_exit(&mut self, exit: VmExit) -> Result<ExitAction, VcpuError>;

    /// Handle a port I/O read (guest `IN` instruction).
    ///
    /// Called with the I/O port and a mutable buffer pointing into the
    /// hypervisor shared memory. Write device response data into `data`.
    ///
    /// The default fills `data` with `0xFF` (no device).
    fn handle_io_read(&mut self, _port: u16, data: &mut [u8]) {
        data.fill(0xFF);
    }

    /// Handle an MMIO read (guest load from device-mapped address).
    ///
    /// Called with the physical address and a mutable buffer pointing into
    /// the hypervisor shared memory. Write device response data into `data`.
    ///
    /// The default fills `data` with `0xFF` (no device).
    fn handle_mmio_read(&mut self, _addr: u64, data: &mut [u8]) {
        data.fill(0xFF);
    }
}

// ── VM Configuration ─────────────────────────────────────────────────

/// Host-side networking configuration for a guest NIC.
///
/// This is the VMM-facing network description derived by the runtime after it
/// decides the guest's IP settings. The VMM uses it to create the host
/// interface, program NAT, and attach the virtio-net device at boot.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct NetworkConfig {
    /// Host interface name used for the guest NIC backend.
    pub interface_name: String,
    /// Optional shared bridge name for logical multi-guest networks.
    pub bridge_name: Option<String>,
    /// Guest IPv4 address assigned inside the VM.
    pub guest_ip: Ipv4Addr,
    /// Host-side gateway IPv4 address exposed to the guest.
    pub gateway_ip: Ipv4Addr,
    /// IPv4 netmask for the guest interface.
    pub netmask: Ipv4Addr,
}

impl NetworkConfig {
    /// Creates a new VMM network configuration.
    #[must_use]
    pub fn new(
        interface_name: &str,
        guest_ip: Ipv4Addr,
        gateway_ip: Ipv4Addr,
        netmask: Ipv4Addr,
    ) -> Self {
        Self {
            interface_name: interface_name.to_owned(),
            bridge_name: None,
            guest_ip,
            gateway_ip,
            netmask,
        }
    }

    /// Places this attachment on a named shared host bridge.
    #[must_use]
    pub fn with_bridge(mut self, bridge_name: impl Into<String>) -> Self {
        self.bridge_name = Some(bridge_name.into());
        self
    }

    /// Returns the subnet in CIDR form used for NAT setup.
    #[must_use]
    pub fn subnet_cidr(&self) -> String {
        let prefix = u32::from(self.netmask).leading_ones();
        let subnet = Ipv4Addr::from(u32::from(self.guest_ip) & u32::from(self.netmask));
        format!("{subnet}/{prefix}")
    }
}

/// File-backed block device attached in addition to the rootfs disk.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct DataDiskConfig {
    /// Host path to the backing file.
    pub path: PathBuf,
    /// Whether the guest should see the disk as read-only.
    pub read_only: bool,
}

impl DataDiskConfig {
    /// Creates a new additional block-device configuration.
    #[must_use]
    pub fn new(path: PathBuf, read_only: bool) -> Self {
        Self { path, read_only }
    }
}

/// Configuration for booting a microVM.
///
/// This is the portable input to [`boot()`]. All platform-specific details
/// (memory layout, boot protocol, device wiring) are derived internally.
#[derive(Debug)]
#[non_exhaustive]
pub struct VmConfig<'a> {
    /// Path to the kernel binary (ELF on `x86_64`, Image on `aarch64`).
    pub kernel_path: &'a Path,
    /// Kernel command line string.
    pub cmdline: &'a str,
    /// Path to the rootfs ext4 image.
    pub rootfs_path: &'a Path,
    /// Guest RAM size in MiB (minimum 64 MiB).
    pub memory_mib: u32,
    /// Number of virtual CPUs (currently capped to 1).
    pub vcpus: u32,
    /// Guest vsock context ID.
    pub guest_cid: u32,
    /// Guest virtualization profile for CPU feature exposure.
    pub guest_virtualization: GuestVirtualizationMode,
    /// Shared directories to expose to the guest via virtio-fs.
    pub shared_dirs: Vec<std::path::PathBuf>,
    /// Additional file-backed block devices to expose to the guest.
    pub data_disks: Vec<DataDiskConfig>,
    /// Optional guest networking configuration.
    pub network: Option<NetworkConfig>,
    /// Guest networking attachments.
    pub networks: Vec<NetworkConfig>,
    /// Pre-allocated guest memory for process-per-VM mode (macOS worker).
    /// When `Some`, `boot()` uses this memory instead of allocating fresh.
    #[cfg(target_os = "macos")]
    pub guest_memory: Option<std::sync::Arc<GuestMemory>>,
}

impl<'a> VmConfig<'a> {
    /// Creates a new VM configuration.
    #[must_use]
    pub fn new(
        kernel_path: &'a Path,
        cmdline: &'a str,
        rootfs_path: &'a Path,
        memory_mib: u32,
        vcpus: u32,
        guest_cid: u32,
    ) -> Self {
        Self {
            kernel_path,
            cmdline,
            rootfs_path,
            memory_mib,
            vcpus,
            guest_cid,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        }
    }
}

/// Configuration for restoring a microVM from a snapshot.
///
/// Unlike [`VmConfig`], this does NOT include `kernel_path`, `rootfs_path`,
/// or `cmdline` — those are baked into the snapshot's memory image.
/// The snapshot directory must contain `memory.bin` and `cpu_state.json`
/// as produced by [`crate::snapshot::save_bundle()`].
#[derive(Debug)]
#[non_exhaustive]
pub struct SnapshotRestoreConfig {
    /// Path to the snapshot directory containing `memory.bin` and `cpu_state.json`.
    pub snapshot_dir: PathBuf,
    /// Guest RAM size in MiB (must match the original snapshot).
    pub memory_mib: u32,
    /// Guest vsock context ID.
    pub guest_cid: u32,
    /// Guest virtualization profile for CPU feature exposure.
    pub guest_virtualization: GuestVirtualizationMode,
    /// Shared directories to expose to the guest via virtio-fs.
    pub shared_dirs: Vec<PathBuf>,
    /// Additional file-backed block devices to expose to the guest.
    pub data_disks: Vec<DataDiskConfig>,
    /// Optional guest networking configuration.
    pub network: Option<NetworkConfig>,
    /// Guest networking attachments.
    pub networks: Vec<NetworkConfig>,
}

impl SnapshotRestoreConfig {
    /// Creates a new snapshot restore configuration.
    #[must_use]
    pub fn new(snapshot_dir: PathBuf, memory_mib: u32, guest_cid: u32) -> Self {
        Self {
            snapshot_dir,
            memory_mib,
            guest_cid,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
        }
    }

    /// Creates a snapshot restore configuration with shared directories.
    #[must_use]
    pub fn with_shared_dirs(
        snapshot_dir: PathBuf,
        memory_mib: u32,
        guest_cid: u32,
        shared_dirs: Vec<PathBuf>,
    ) -> Self {
        Self {
            snapshot_dir,
            memory_mib,
            guest_cid,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs,
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
        }
    }
}

/// How vCPU registers should be initialized.
///
/// Determines whether `run_vcpu()` uses the normal boot register setup
/// or skips it (for snapshot restore where registers are pre-loaded).
#[cfg(target_os = "macos")]
#[derive(Debug)]
#[non_exhaustive]
pub(crate) enum CpuInitMode {
    /// Normal boot — configure registers from scratch (`entry_point` + `fdt_addr`).
    Boot {
        /// Kernel entry point address.
        entry_point: u64,
        /// FDT base address.
        fdt_addr: u64,
    },
    /// Snapshot restore — registers were already loaded by the restore path.
    Restore,
}

/// Thread-safe serial output capture.
///
/// Wraps `Arc<Mutex<Vec<u8>>>` and implements [`Write`](std::io::Write) so
/// it can be used as the output sink for [`SerialDevice`](crate::devices::serial::SerialDevice).
#[derive(Debug, Clone)]
pub struct SerialOutput {
    inner: Arc<Mutex<Vec<u8>>>,
}

impl SerialOutput {
    /// Creates a new, empty serial output buffer.
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Returns a snapshot of all captured bytes.
    #[must_use]
    pub fn as_bytes(&self) -> Vec<u8> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl Default for SerialOutput {
    fn default() -> Self {
        Self::new()
    }
}

impl std::io::Write for SerialOutput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut locked = self
            .inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        locked.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Result of booting a microVM.
///
/// Contains everything needed to run the VM and capture output.
/// The caller spawns a vCPU thread using [`run_vcpu`] and monitors
/// the serial output.
#[non_exhaustive]
pub struct BootedVm {
    /// Guest memory (shared between transport and vCPU thread).
    pub memory: Arc<GuestMemory>,
    /// Device manager with all devices wired.
    pub device_mgr: DeviceManager,
    /// Serial output capture buffer.
    pub serial_output: SerialOutput,
    /// Kill flag to signal the vCPU thread to exit.
    pub kill_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Vsock muxer for bridging guest vsock to host UDS.
    pub vsock_muxer: Option<VsockMuxer>,
    /// Vsock transport for run-loop RX polling when host data is pending.
    pub(crate) vsock_transport: Option<Arc<Mutex<MmioTransport>>>,
    /// Net transports for run-loop RX polling when guest networking is enabled.
    pub(crate) net_transports: Vec<Arc<Mutex<MmioTransport>>>,
    /// Flag set by vmnet callback when packets are available for RX.
    #[cfg(target_os = "macos")]
    pub(crate) net_has_pending: Option<Arc<AtomicBool>>,
    /// Platform-specific internals (opaque to callers).
    inner: BootedVmInner,
}

/// Host-side helper for polling guest vsock RX delivery outside the vCPU loop.
///
/// Linux currently uses this from the runtime to keep host-initiated vsock
/// traffic moving while the guest is blocked inside the hypervisor run call.
#[derive(Clone)]
#[non_exhaustive]
pub struct VsockRxPoller {
    transport: Arc<Mutex<MmioTransport>>,
}

impl VsockRxPoller {
    #[must_use]
    fn new(transport: Arc<Mutex<MmioTransport>>) -> Self {
        Self { transport }
    }

    /// Processes one pending guest vsock RX cycle.
    ///
    /// Returns `true` when host-side data was delivered into the guest queue.
    #[must_use]
    pub fn poll_once(&self) -> bool {
        match self.transport.lock() {
            Ok(transport) => transport.process_external_queue(0),
            Err(_) => false,
        }
    }
}

/// Host-side network RX poller for platforms that need explicit TAP polling.
///
/// Linux uses this from the runtime to keep host-to-guest virtio-net traffic
/// moving while the guest is blocked inside the hypervisor run call.
#[derive(Clone)]
#[non_exhaustive]
pub struct NetRxPoller {
    transports: Vec<Arc<Mutex<MmioTransport>>>,
}

impl NetRxPoller {
    #[must_use]
    fn new(transports: Vec<Arc<Mutex<MmioTransport>>>) -> Self {
        Self { transports }
    }

    /// Processes one pending guest network RX cycle.
    ///
    /// Returns `true` when host-side packet data was delivered into the guest
    /// virtio-net receive queue.
    #[must_use]
    pub fn poll_once(&self) -> bool {
        let mut delivered = false;
        for transport in &self.transports {
            if let Ok(locked) = transport.lock() {
                delivered |= locked.process_external_queue(0);
            }
        }
        delivered
    }
}

impl BootedVm {
    /// Returns a host-side vsock poller when the booted VM exposes one.
    #[must_use]
    pub fn vsock_rx_poller(&self) -> Option<VsockRxPoller> {
        self.vsock_transport
            .as_ref()
            .map(|transport| VsockRxPoller::new(Arc::clone(transport)))
    }

    /// Returns a host-side network RX poller when the booted VM exposes one.
    #[must_use]
    pub fn net_rx_poller(&self) -> Option<NetRxPoller> {
        (!self.net_transports.is_empty()).then(|| NetRxPoller::new(self.net_transports.clone()))
    }
}

/// Platform-specific boot state, kept opaque from visor-runtime.
#[cfg(target_os = "linux")]
struct BootedVmInner {
    #[allow(dead_code)]
    platform: crate::platform::KvmPlatform,
    #[allow(dead_code)]
    vm: crate::platform::KvmVm,
    vcpu: crate::vcpu::Vcpu,
    #[allow(dead_code)]
    networks: Vec<linux::NetworkResources>,
}

#[cfg(target_os = "macos")]
struct BootedVmInner {
    #[allow(dead_code)]
    platform: crate::platform::HvfPlatform,
    vm: crate::platform::HvfVm,
    /// How to initialize vCPU registers in the run loop.
    cpu_init_mode: CpuInitMode,
}

// ── Portable constants ───────────────────────────────────────────────

/// Minimum guest memory: 64 MiB.
const MIN_MEMORY_MIB: u32 = 64;

/// Memory slot index for the main RAM region.
const MEMORY_SLOT: u32 = 0;

// ── Boot Implementation ──────────────────────────────────────────────

/// Boots a microVM from the given configuration.
///
/// Internally selects the correct platform (KVM on Linux, HVF on macOS)
/// and boot protocol (`x86_64` or `aarch64`). Wires serial, block, and vsock
/// devices, and returns a [`BootedVm`] ready for the vCPU thread.
///
/// # Errors
///
/// Returns an error if any step of the boot sequence fails (platform init,
/// memory allocation, kernel loading, device setup).
pub fn boot(config: &VmConfig<'_>) -> Result<BootedVm, VmBootError> {
    #[cfg(target_os = "linux")]
    {
        linux::boot_linux(config)
    }
    #[cfg(target_os = "macos")]
    {
        macos::boot_macos(config)
    }
}

/// Restores a microVM from a snapshot directory (fast-path).
///
/// Skips kernel loading and rootfs creation. Instead, restores guest
/// memory via `mmap(MAP_PRIVATE)` copy-on-write from `memory.bin`
/// and vCPU registers from `cpu_state.json`. Device objects are freshly
/// created at the same MMIO addresses used during the original boot.
///
/// This provides sub-5ms VM startup regardless of memory size.
///
/// # Errors
///
/// Returns an error if the snapshot directory is missing, the memory
/// file doesn't match the expected size, or device setup fails.
pub fn boot_from_snapshot(config: &SnapshotRestoreConfig) -> Result<BootedVm, VmBootError> {
    #[cfg(target_os = "linux")]
    {
        linux::restore_linux(config)
    }
    #[cfg(target_os = "macos")]
    {
        macos::restore_macos(config)
    }
}

/// Saves a booted VM as a reusable snapshot bundle.
///
/// Captures guest memory and vCPU state into `snapshot_dir`. The caller is
/// responsible for persisting any external disk artifacts needed for restore.
///
/// # Errors
///
/// Returns an error if the snapshot directory cannot be created or the VMM
/// snapshot bundle save fails.
pub fn save_snapshot(booted: &BootedVm, snapshot_dir: &Path) -> Result<(), VmBootError> {
    std::fs::create_dir_all(snapshot_dir).map_err(crate::snapshot::SnapshotError::Io)?;

    #[cfg(target_os = "linux")]
    {
        crate::snapshot::save_bundle(
            booted.inner.vcpu.fd(),
            &booted.memory,
            snapshot_dir,
            Vec::new(),
        )?;
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let _ = booted;
        let _ = snapshot_dir;
        Err(VmBootError::Device(
            "snapshot save is not yet implemented for macOS".to_owned(),
        ))
    }
}

/// Runs the vCPU in a loop, dispatching exits to the device manager.
///
/// This is the portable vCPU run loop. Call it from the vCPU thread.
/// Returns when the guest shuts down, the kill flag is set, or a fatal
/// error occurs.
///
/// # Errors
///
/// Returns [`VcpuError`] on fatal hypervisor errors.
pub fn run_vcpu(booted: &mut BootedVm) -> Result<(), VcpuError> {
    #[cfg(target_os = "linux")]
    {
        linux::run_loop(
            &mut booted.inner.vcpu,
            &booted.kill_flag,
            &mut booted.device_mgr,
            booted.vsock_transport.as_ref(),
            &booted.net_transports,
        )
    }
    #[cfg(target_os = "macos")]
    {
        let mut vcpu = booted
            .inner
            .vm
            .create_vcpu(0)
            .map_err(|e| VcpuError::Create(std::io::Error::other(e)))?;

        match &booted.inner.cpu_init_mode {
            CpuInitMode::Boot {
                entry_point,
                fdt_addr,
            } => {
                configure_vcpu_boot_regs(&vcpu, *entry_point, *fdt_addr, 0)?;
            }
            CpuInitMode::Restore => {
                // Snapshot restore: registers already loaded, skip boot config.
            }
        }

        macos::run_loop(
            &mut vcpu,
            &booted.inner.vm,
            &booted.kill_flag,
            &mut booted.device_mgr,
            booted.vsock_transport.as_ref(),
            booted.net_has_pending.as_ref(),
            booted.net_transports.first(),
        )
    }
}

/// Runs the vCPU in a loop with a custom exit handler, capturing registers.
///
/// Like [`run_vcpu()`] but accepts an arbitrary [`ExitHandler`] implementation
/// instead of using the built-in [`DeviceManager`]. After the run loop ends,
/// captures the vCPU's register state into a [`VcpuRunResult`].
///
/// This is the primary API for diagnostic tools like `vm_debug` that need
/// to inspect registers after execution and implement custom exit logging.
///
/// # Errors
///
/// Returns [`VcpuError`] on fatal hypervisor errors.
pub fn run_vcpu_with_handler(
    booted: &mut BootedVm,
    handler: &mut dyn ExitHandler,
) -> Result<VcpuRunResult, VcpuError> {
    #[cfg(target_os = "linux")]
    {
        let result = linux::run_loop(
            &mut booted.inner.vcpu,
            &booted.kill_flag,
            handler,
            booted.vsock_transport.as_ref(),
            &booted.net_transports,
        );
        let regs = booted.inner.vcpu.fd().get_regs().ok().map(Into::into);
        let sregs = booted.inner.vcpu.fd().get_sregs().ok().map(Into::into);

        result.map(|()| VcpuRunResult { regs, sregs })
    }
    #[cfg(target_os = "macos")]
    {
        use crate::platform::VcpuOps;

        let mut vcpu = booted
            .inner
            .vm
            .create_vcpu(0)
            .map_err(|e| VcpuError::Create(std::io::Error::other(e)))?;

        match &booted.inner.cpu_init_mode {
            CpuInitMode::Boot {
                entry_point,
                fdt_addr,
            } => {
                configure_vcpu_boot_regs(&vcpu, *entry_point, *fdt_addr, 0)?;
            }
            CpuInitMode::Restore => {}
        }

        let result = macos::run_loop(
            &mut vcpu,
            &booted.inner.vm,
            &booted.kill_flag,
            handler,
            booted.vsock_transport.as_ref(),
            booted.net_has_pending.as_ref(),
            booted.net_transports.first(),
        );

        // Capture registers before the vCPU is dropped, regardless of run result.
        let regs = vcpu.get_regs().ok();
        let sregs = vcpu.get_sregs().ok();

        result.map(|()| VcpuRunResult { regs, sregs })
    }
}

/// Errors from the VM boot sequence.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VmBootError {
    /// Platform (hypervisor) operation failed.
    #[error("platform error: {0}")]
    Platform(#[from] crate::platform::PlatformError),

    /// Boot setup (kernel loading, page tables, FDT) failed.
    #[error("boot error: {0}")]
    Boot(#[from] crate::boot::BootError),

    /// Guest memory allocation or write failed.
    #[error("memory error: {0}")]
    Memory(#[from] crate::memory::MemoryError),

    /// Device setup failed.
    #[error("device setup error: {0}")]
    Device(String),

    /// Snapshot restore failed.
    #[error("snapshot error: {0}")]
    Snapshot(#[from] crate::snapshot::SnapshotError),

    /// vCPU setup failed.
    #[error("vCPU error: {0}")]
    Vcpu(#[from] VcpuError),

    /// Guest virtualization policy is unsupported on this platform.
    #[error("guest virtualization error: {0}")]
    GuestVirtualization(#[from] GuestVirtualizationError),
}

/// State captured at vCPU exit.
///
/// Returned by [`run_vcpu_with_handler()`] to allow callers to inspect
/// the vCPU register state after the run loop completes.
#[derive(Debug)]
#[non_exhaustive]
pub struct VcpuRunResult {
    /// General-purpose registers at exit, if available.
    pub regs: Option<crate::platform::regs::StandardRegs>,
    /// Special (system) registers at exit, if available.
    pub sregs: Option<crate::platform::regs::SpecialRegs>,
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

#[cfg(test)]
#[path = "snapshot_restore_test.rs"]
mod snapshot_restore_tests;
