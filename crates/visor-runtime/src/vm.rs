//! VM lifecycle: boot, run, and capture output.
//!
//! This module wires together all lower layers (visor-vmm, visor-kernel,
//! visor-init) to boot a Linux microVM from a rootfs ext4 image.
//!
//! # Architecture
//!
//! ```text
//! ┌──────────┐    ┌─────────────┐    ┌───────────────┐
//! │ boot_vm  │───>│ visor_vmm   │───>│ visor-init    │
//! │          │    │ ::vm::boot()│    │ (guest PID 1) │
//! └──────────┘    └──────┬──────┘    └───────────────┘
//!                        │ serial output
//!                        ▼
//!                   SerialOutput (Arc<Mutex<Vec<u8>>>)
//! ```

use std::fmt;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::JoinHandle;

use anyhow::Context;
use base64::Engine as _;
use tokio::sync::oneshot;
use visor_types::GuestVirtualizationMode;

// Re-export SerialOutput from visor-vmm (used by backend.rs, vm_debug, etc.).
pub use visor_vmm::vm::SerialOutput;

#[cfg(test)]
#[path = "vm_test.rs"]
mod tests;

// ── Constants ────────────────────────────────────────────────────

/// Exit code marker printed by visor-init before exiting.
const EXIT_CODE_MARKER: &str = "VISOR_EXIT_CODE=";
/// Marker emitted by visor-init immediately before user command output begins.
const STDOUT_BEGIN_MARKER: &str = "VISOR_STDOUT_BEGIN";
/// Marker emitted by visor-init after the user command exits.
const STDOUT_END_MARKER: &str = "VISOR_STDOUT_END";
/// Kernel panic wait-status marker emitted when PID 1 exits.
const PANIC_EXIT_CODE_MARKER: &str = "exitcode=0x";

/// Marker line printed by the kernel when it starts visor-init.
/// All output after this line and before the exit code marker is user stdout.
const INIT_MARKER: &str = "Run /sbin/visor-init as init process";

/// Kernel panic line — always follows user output + exit code.
const KERNEL_PANIC_PREFIX: &str = "Kernel panic";
/// Control-plane diagnostics emitted by visor-init itself.
const VISOR_INIT_LOG_PREFIX: &str = "visor-init:";

// ── VM Exit Types ────────────────────────────────────────────────

/// Information about how a VM exited.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VmExitInfo {
    /// Exit code parsed from serial output (or 1 if not found).
    pub exit_code: i32,
    /// How the vCPU run loop terminated.
    pub reason: VmExitReason,
}

/// Reason the VM stopped.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum VmExitReason {
    /// Guest performed an orderly shutdown.
    Shutdown,
    /// Guest requested a reboot.
    Reboot,
    /// Guest halted.
    Halt,
    /// vCPU run loop encountered an error.
    Error(String),
}

impl fmt::Display for VmExitReason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Shutdown => write!(f, "shutdown"),
            Self::Reboot => write!(f, "reboot"),
            Self::Halt => write!(f, "halt"),
            Self::Error(msg) => write!(f, "error: {msg}"),
        }
    }
}

// ── VmHandle ─────────────────────────────────────────────────────

/// Handle to a running VM.
///
/// Holds the vCPU thread join handle, a oneshot receiver for completion
/// signaling, the serial output buffer, and the rootfs path for cleanup.
#[non_exhaustive]
pub struct VmHandle {
    /// vCPU thread join handle.
    pub thread: Option<JoinHandle<()>>,
    /// Receives exit info when the vCPU thread finishes.
    pub completion_rx: Option<oneshot::Receiver<VmExitInfo>>,
    /// Shared flag to signal the vCPU thread to exit.
    pub kill_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Serial output captured from the guest.
    pub serial_output: SerialOutput,
    /// Path to the rootfs ext4 image (for cleanup).
    pub rootfs_path: PathBuf,
    /// Guest vsock context ID.
    pub cid: u32,
}

impl fmt::Debug for VmHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmHandle")
            .field("rootfs_path", &self.rootfs_path)
            .finish_non_exhaustive()
    }
}

impl Drop for VmHandle {
    fn drop(&mut self) {
        // Only clean up if we still own the vCPU thread. After
        // `take_parts()`, ownership moves to VmLiveState and this
        // Drop becomes a no-op — the kill_flag must NOT be set
        // because the vCPU thread is still legitimately running.
        if let Some(thread) = self.thread.take() {
            self.kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            let _ = thread.join();
        }
    }
}

impl VmHandle {
    /// Takes ownership of live fields for transfer into [`VmLiveState`].
    ///
    /// After this call, [`Drop`] is a no-op because the thread handle is gone.
    pub(crate) fn take_parts(&mut self) -> VmHandleParts {
        VmHandleParts {
            thread: self.thread.take(),
            kill_flag: Arc::clone(&self.kill_flag),
            completion_rx: self.completion_rx.take(),
            serial_output: self.serial_output.clone(),
        }
    }
}

/// Extracted parts from a [`VmHandle`] for transfer into live state.
pub(crate) struct VmHandleParts {
    pub(crate) thread: Option<JoinHandle<()>>,
    pub(crate) kill_flag: Arc<std::sync::atomic::AtomicBool>,
    pub(crate) completion_rx: Option<oneshot::Receiver<VmExitInfo>>,
    pub(crate) serial_output: SerialOutput,
}

/// Storage resources attached during VM boot.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct BootStorage<'a> {
    /// Shared host directories exposed to the guest.
    pub shared_dirs: &'a [std::path::PathBuf],
    /// Additional block devices attached to the guest.
    pub data_disks: &'a [visor_vmm::vm::DataDiskConfig],
    /// Pre-allocated guest memory for macOS process-per-VM workers.
    #[cfg(target_os = "macos")]
    pub guest_memory: Option<std::sync::Arc<visor_vmm::memory::GuestMemory>>,
}

impl<'a> BootStorage<'a> {
    /// Creates a new boot-storage bundle.
    #[must_use]
    pub fn new(
        shared_dirs: &'a [std::path::PathBuf],
        data_disks: &'a [visor_vmm::vm::DataDiskConfig],
    ) -> Self {
        Self {
            shared_dirs,
            data_disks,
            #[cfg(target_os = "macos")]
            guest_memory: None,
        }
    }

    /// Attach pre-allocated guest memory (macOS worker path).
    #[must_use]
    #[cfg(target_os = "macos")]
    pub fn with_guest_memory(
        mut self,
        guest_memory: Option<std::sync::Arc<visor_vmm::memory::GuestMemory>>,
    ) -> Self {
        self.guest_memory = guest_memory;
        self
    }
}

/// Memory, CPU, and vsock sizing for a booted VM.
#[derive(Clone, Copy, Debug)]
#[non_exhaustive]
pub struct VmBootSpec {
    /// Guest memory size in MiB.
    pub memory_mib: u32,
    /// Number of guest vCPUs.
    pub vcpus: u32,
    /// Guest `AF_VSOCK` context ID.
    pub cid: u32,
    /// Guest virtualization profile.
    pub guest_virtualization: GuestVirtualizationMode,
}

impl VmBootSpec {
    /// Creates a new VM boot specification.
    #[must_use]
    pub const fn new(memory_mib: u32, vcpus: u32, cid: u32) -> Self {
        Self {
            memory_mib,
            vcpus,
            cid,
            guest_virtualization: GuestVirtualizationMode::Standard,
        }
    }

    /// Selects the guest virtualization profile for the VM.
    #[must_use]
    pub fn with_guest_virtualization(
        mut self,
        guest_virtualization: GuestVirtualizationMode,
    ) -> Self {
        self.guest_virtualization = guest_virtualization;
        self
    }
}

// ── build_cmdline ────────────────────────────────────────────────

/// Builds the kernel command line for booting a visor microVM.
///
/// The command line includes:
/// - `console={device}` — serial console (`ttyAMA0` on ARM64, `ttyS0` on `x86_64`)
/// - `earlycon=pl011,0x09000000` — early console via direct MMIO (ARM64 only)
/// - `reboot=t` — triple-fault on reboot (clean KVM exit)
/// - `panic=-1` — reboot on panic (triggers KVM exit)
/// - `root=/dev/vda rw` — rootfs on virtio-blk device
/// - `init=/sbin/visor-init` — use visor-init as PID 1
/// - `visor.config=<base64>` — base64-encoded JSON `RunConfig`
///
/// # Errors
///
/// Returns an error if JSON serialization or base64 encoding fails.
pub fn build_cmdline(config: &visor_init::config::RunConfig) -> anyhow::Result<String> {
    let json = config.to_json().context("serialize RunConfig to JSON")?;
    let engine = base64::engine::general_purpose::STANDARD_NO_PAD;
    let encoded = engine.encode(json.as_bytes());

    let console = visor_vmm::devices::serial::CONSOLE_DEVICE_NAME;
    let earlycon = visor_vmm::devices::serial::EARLYCON_PARAM;
    let cmdline = format!(
        "console={console} {earlycon} reboot=t panic=-1 root=/dev/vda rw init=/sbin/visor-init \
         initcall_debug keep_bootcon loglevel=7 \
         visor.config={encoded}"
    );

    Ok(cmdline)
}

// ── visor_init_path ──────────────────────────────────────────────

/// Resolves the path to the `visor-init` static binary.
///
/// The target triple is determined by the host architecture since the
/// guest VM runs the same architecture as the host hypervisor.
///
/// Search order:
/// 1. `VISOR_INIT_PATH` environment variable
/// 2. Newest existing `target/{triple}/{profile}/visor-init` dev build
/// 4. `/usr/libexec/visor/visor-init` (installed)
///
/// # Errors
///
/// Returns an error if no viable path is found.
pub fn visor_init_path() -> anyhow::Result<PathBuf> {
    let target_triple = if cfg!(target_arch = "aarch64") {
        "aarch64-unknown-linux-musl"
    } else {
        "x86_64-unknown-linux-musl"
    };

    // 1. Environment override
    if let Ok(path) = std::env::var("VISOR_INIT_PATH") {
        let p = PathBuf::from(&path);
        if p.is_file() {
            return Ok(p);
        }
    }

    // 2. Newest existing dev build path
    if let Some(path) = newest_existing_path(&visor_init_dev_path_candidates(target_triple)) {
        return Ok(path);
    }

    // 4. Installed path
    let installed = PathBuf::from("/usr/libexec/visor/visor-init");
    if installed.is_file() {
        return Ok(installed);
    }

    anyhow::bail!(
        "visor-init binary not found. Build it with: \
         cargo build -p visor-init --release --target {target_triple} \
         or set VISOR_INIT_PATH"
    )
}

fn visor_init_dev_path_candidates(target_triple: &str) -> Vec<PathBuf> {
    visor_init_dev_path_candidates_for_roots(target_triple, &search_roots())
}

fn visor_init_dev_path_candidates_for_roots(
    target_triple: &str,
    roots: &[PathBuf],
) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    for root in roots {
        candidates.push(root.join(format!("target/{target_triple}/release/visor-init")));
        candidates.push(root.join(format!("target/{target_triple}/debug/visor-init")));
    }
    for target_dir in cargo_target_dirs(roots) {
        candidates.push(target_dir.join(format!("{target_triple}/release/visor-init")));
        candidates.push(target_dir.join(format!("{target_triple}/debug/visor-init")));
    }
    dedupe_paths(candidates)
}

fn search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();

    if let Ok(current_dir) = std::env::current_dir() {
        roots.extend(current_dir.ancestors().map(Path::to_path_buf));
    }

    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    roots.extend(manifest_dir.ancestors().map(Path::to_path_buf));

    dedupe_paths(roots)
}

fn cargo_target_dirs(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut target_dirs = Vec::new();
    for root in roots {
        for relative_config_path in [".cargo/config.toml", ".cargo/config"] {
            let config_path = root.join(relative_config_path);
            let Some(target_dir) = cargo_target_dir_from_config(&config_path) else {
                continue;
            };
            let resolved = if target_dir.is_absolute() {
                target_dir
            } else {
                root.join(target_dir)
            };
            target_dirs.push(resolved);
            break;
        }
    }
    dedupe_paths(target_dirs)
}

fn cargo_target_dir_from_config(config_path: &Path) -> Option<PathBuf> {
    let contents = std::fs::read_to_string(config_path).ok()?;
    let parsed: toml::Value = toml::from_str(&contents).ok()?;
    let target_dir = parsed.get("build")?.get("target-dir")?.as_str()?;
    Some(PathBuf::from(target_dir))
}

fn dedupe_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut unique = Vec::new();
    for path in paths {
        if !unique.iter().any(|candidate| candidate == &path) {
            unique.push(path);
        }
    }
    unique
}

fn newest_existing_path(candidates: &[PathBuf]) -> Option<PathBuf> {
    candidates
        .iter()
        .filter_map(|path| {
            let metadata = std::fs::metadata(path).ok()?;
            if !metadata.is_file() {
                return None;
            }
            Some((metadata.modified().ok(), path))
        })
        .max_by_key(|(modified, _)| *modified)
        .map(|(_, path)| path.clone())
}

// ── parse_exit_code ──────────────────────────────────────────────

/// Parses the VM exit code from serial output.
///
/// Looks for the last line matching `VISOR_EXIT_CODE=N` and returns `N`.
/// If the explicit marker is missing, falls back to the kernel panic
/// `exitcode=0x...` wait status that Linux emits when PID 1 dies.
/// If no marker is found, returns `1` (generic failure).
#[must_use]
pub fn parse_exit_code(serial_output: &[u8]) -> i32 {
    let text = String::from_utf8_lossy(serial_output);
    text.lines()
        .rev()
        .find_map(|line| {
            let trimmed = line.trim();
            trimmed
                .strip_prefix(EXIT_CODE_MARKER)
                .and_then(|s| s.parse::<i32>().ok())
        })
        .or_else(|| text.lines().rev().find_map(parse_kernel_panic_exit_code))
        .unwrap_or(1)
}

fn parse_kernel_panic_exit_code(line: &str) -> Option<i32> {
    let hex = line
        .trim()
        .split(PANIC_EXIT_CODE_MARKER)
        .nth(1)?
        .chars()
        .take_while(char::is_ascii_hexdigit)
        .collect::<String>();
    if hex.is_empty() {
        return None;
    }

    let wait_status = u32::from_str_radix(&hex, 16).ok()?;
    let signal = wait_status & 0x7F;
    if signal == 0 {
        i32::try_from((wait_status >> 8) & 0xFF).ok()
    } else {
        i32::try_from(signal).ok().map(|signal| 128 + signal)
    }
}

// ── extract_stdout ───────────────────────────────────────────────

/// Extracts user-visible stdout from raw serial output.
///
/// The kernel prints boot log to serial, then the init marker
/// `Run /sbin/visor-init as init process` appears, followed by user
/// output from the entrypoint process. This function extracts only the
/// lines between the init marker and the exit code marker (or kernel panic).
///
/// If no init marker is found, falls back to filtering lines that
/// don't look like kernel log.
#[must_use]
pub fn extract_stdout(serial_output: &[u8]) -> String {
    let text = String::from_utf8_lossy(serial_output);
    if text.contains(STDOUT_BEGIN_MARKER) {
        return extract_marked_stdout(&text);
    }

    extract_legacy_stdout(&text)
}

fn extract_marked_stdout(text: &str) -> String {
    let mut result = String::new();
    let mut capturing = false;

    for line in text.lines() {
        let trimmed = line.trim();

        if trimmed == STDOUT_BEGIN_MARKER {
            capturing = true;
            continue;
        }

        if !capturing {
            continue;
        }

        if let Some((before_marker, _)) = line.split_once(STDOUT_END_MARKER) {
            let trimmed_before = before_marker.trim();
            if !trimmed_before.is_empty() && !trimmed_before.starts_with(VISOR_INIT_LOG_PREFIX) {
                result.push_str(trimmed_before);
                result.push('\n');
            }
            break;
        }

        if let Some((before_marker, _)) = line.split_once(EXIT_CODE_MARKER) {
            let trimmed_before = before_marker.trim();
            if !trimmed_before.is_empty() && !trimmed_before.starts_with(VISOR_INIT_LOG_PREFIX) {
                result.push_str(trimmed_before);
                result.push('\n');
            }
            break;
        }

        if trimmed.starts_with(VISOR_INIT_LOG_PREFIX) {
            continue;
        }

        result.push_str(trimmed);
        result.push('\n');
    }

    result
}

fn extract_legacy_stdout(text: &str) -> String {
    let mut result = String::new();
    let mut after_init = false;

    for line in text.lines() {
        let trimmed = line.trim();

        // Look for init marker to start capturing
        if trimmed.contains(INIT_MARKER) {
            after_init = true;
            continue;
        }

        if !after_init {
            continue;
        }

        // Stop at exit code marker or kernel panic
        if trimmed.starts_with(EXIT_CODE_MARKER) || trimmed.starts_with(KERNEL_PANIC_PREFIX) {
            break;
        }

        // Internal init-process diagnostics are not container stdout.
        if trimmed.starts_with(VISOR_INIT_LOG_PREFIX) {
            continue;
        }

        // Skip empty lines
        if trimmed.is_empty() {
            continue;
        }

        result.push_str(trimmed);
        result.push('\n');
    }

    result
}

fn build_vmm_network_config(
    guest_network: Option<&visor_init::config::NetworkConfig>,
    vm_id: &str,
    cid: u32,
) -> anyhow::Result<Option<visor_vmm::vm::NetworkConfig>> {
    let Some(guest_network) = guest_network else {
        return Ok(None);
    };

    let guest_ip = parse_network_addr(&guest_network.address, "address")?;
    let gateway_ip = parse_network_addr(&guest_network.gateway, "gateway")?;
    let netmask = parse_network_addr(&guest_network.netmask, "netmask")?;

    Ok(Some(visor_vmm::vm::NetworkConfig::new(
        &network_interface_name(vm_id, cid, 0),
        guest_ip,
        gateway_ip,
        netmask,
    )))
}

fn build_vmm_network_configs(
    guest_networks: &[visor_init::config::NetworkConfig],
    vm_id: &str,
    cid: u32,
) -> anyhow::Result<Vec<visor_vmm::vm::NetworkConfig>> {
    let mut attachments = Vec::with_capacity(guest_networks.len());

    for (index, guest_network) in guest_networks.iter().enumerate() {
        let guest_ip = parse_network_addr(&guest_network.address, "address")?;
        let gateway_ip = parse_network_addr(&guest_network.gateway, "gateway")?;
        let netmask = parse_network_addr(&guest_network.netmask, "netmask")?;
        let mut attachment = visor_vmm::vm::NetworkConfig::new(
            &network_interface_name(vm_id, cid, index),
            guest_ip,
            gateway_ip,
            netmask,
        );
        if let Some(network_name) = guest_network.name.as_ref().filter(|name| !name.is_empty()) {
            attachment = attachment.with_bridge(shared_network_bridge_name(network_name));
        }
        attachments.push(attachment);
    }

    Ok(attachments)
}

fn network_interface_name(vm_id: &str, cid: u32, index: usize) -> String {
    let suffix: String = vm_id
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|ch| ch.to_ascii_lowercase())
        .take(10)
        .collect();
    let attachment = format!("{index:x}");

    if suffix.is_empty() {
        format!("vsr{cid:x}{attachment}")
    } else {
        format!("vsr{suffix}{attachment}")
    }
}

fn shared_network_bridge_name(network_name: &str) -> String {
    let hash = fnv1a64(network_name.as_bytes());
    format!("vsrbr{:08x}", (hash & 0xffff_ffff) as u32)
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
    const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = FNV_OFFSET;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

fn parse_network_addr(value: &str, label: &str) -> anyhow::Result<Ipv4Addr> {
    value
        .parse()
        .with_context(|| format!("parse guest network {label} '{value}' as IPv4"))
}

fn map_guest_virtualization_mode(
    mode: GuestVirtualizationMode,
) -> visor_vmm::guest_virtualization::GuestVirtualizationMode {
    match mode {
        GuestVirtualizationMode::Standard => {
            visor_vmm::guest_virtualization::GuestVirtualizationMode::Standard
        }
        GuestVirtualizationMode::Nested => {
            visor_vmm::guest_virtualization::GuestVirtualizationMode::Nested
        }
        _ => visor_vmm::guest_virtualization::GuestVirtualizationMode::Standard,
    }
}

// ── boot_vm ─────────────────────────────────────────────────────

/// Boots a microVM with the given rootfs and configuration.
///
/// Delegates to [`visor_vmm::vm::boot()`] for all platform-specific setup
/// (KVM on Linux, HVF on macOS) and spawns a vCPU thread using
/// [`visor_vmm::vm::run_vcpu()`].
///
/// Returns a [`VmHandle`] for monitoring completion and reading output.
///
/// # Errors
///
/// Returns an error if any step of the boot sequence fails.
pub fn boot_vm(
    vm_id: &str,
    config: &visor_init::config::RunConfig,
    rootfs_path: &Path,
    spec: VmBootSpec,
    storage: BootStorage<'_>,
) -> anyhow::Result<VmHandle> {
    boot_vm_internal(vm_id, config, rootfs_path, spec, storage, None)
}

/// Boots a microVM and persists a reusable snapshot bundle before first run.
///
/// # Errors
///
/// Returns an error if booting the VM or writing the snapshot bundle fails.
pub fn boot_vm_with_snapshot(
    vm_id: &str,
    config: &visor_init::config::RunConfig,
    rootfs_path: &Path,
    spec: VmBootSpec,
    storage: BootStorage<'_>,
    snapshot_dir: &Path,
) -> anyhow::Result<VmHandle> {
    boot_vm_internal(
        vm_id,
        config,
        rootfs_path,
        spec,
        storage,
        Some(snapshot_dir),
    )
}

fn boot_vm_internal(
    vm_id: &str,
    config: &visor_init::config::RunConfig,
    rootfs_path: &Path,
    spec: VmBootSpec,
    storage: BootStorage<'_>,
    snapshot_dir: Option<&Path>,
) -> anyhow::Result<VmHandle> {
    let boot_start = std::time::Instant::now();

    let cmdline = build_cmdline(config).context("build kernel command line")?;
    let kernel_path = visor_kernel::kernel_path();

    let mut vm_config = visor_vmm::vm::VmConfig::new(
        &kernel_path,
        &cmdline,
        rootfs_path,
        spec.memory_mib,
        spec.vcpus,
        spec.cid,
    );
    vm_config.shared_dirs = storage.shared_dirs.to_vec();
    vm_config.data_disks = storage.data_disks.to_vec();
    vm_config.guest_virtualization = map_guest_virtualization_mode(spec.guest_virtualization);
    #[cfg(target_os = "macos")]
    {
        vm_config.guest_memory = storage.guest_memory.clone();
    }
    let effective_networks = config.effective_networks();
    vm_config.network = build_vmm_network_config(config.network.as_ref(), vm_id, spec.cid)
        .context("build VMM network config")?;
    vm_config.networks = build_vmm_network_configs(&effective_networks, vm_id, spec.cid)
        .context("build VMM network attachment configs")?;

    let t0 = std::time::Instant::now();
    let mut booted = visor_vmm::vm::boot(&vm_config).context("boot microVM")?;
    let hypervisor_ms = t0.elapsed().as_millis();

    if let Some(snapshot_dir) = snapshot_dir {
        save_snapshot_artifact(&booted, rootfs_path, snapshot_dir)
            .context("save VM snapshot artifact")?;
    }

    spawn_vsock_poller(booted.vsock_rx_poller(), Arc::clone(&booted.kill_flag));
    spawn_net_poller(booted.net_rx_poller(), Arc::clone(&booted.kill_flag));

    // Spawn vsock muxer if present (macOS — on Linux, vsock uses AF_VSOCK natively).
    if let Some(muxer) = booted.vsock_muxer.take() {
        let listener_path = muxer.listener_path();
        // Remove stale socket from a previous run.
        let _ = std::fs::remove_file(&listener_path);
        let listener =
            tokio::net::UnixListener::bind(&listener_path).context("bind vsock muxer listener")?;
        tokio::spawn(async move {
            if let Err(e) = muxer.run(listener).await {
                tracing::warn!(error = %e, "vsock muxer exited with error");
            }
        });
    }

    // Clone shared state before moving BootedVm into the vCPU thread.
    let serial_output = booted.serial_output.clone();
    let kill_flag = Arc::clone(&booted.kill_flag);

    let (completion_tx, completion_rx) = oneshot::channel::<VmExitInfo>();
    let rootfs_owned = rootfs_path.to_path_buf();

    let thread = std::thread::Builder::new()
        .name("visor-vcpu-0".into())
        .spawn(move || {
            let result = visor_vmm::vm::run_vcpu(&mut booted);

            let exit_info = match result {
                Ok(()) => VmExitInfo {
                    exit_code: 0,
                    reason: VmExitReason::Shutdown,
                },
                Err(e) => VmExitInfo {
                    exit_code: 1,
                    reason: VmExitReason::Error(e.to_string()),
                },
            };

            // Send completion signal; if receiver dropped, that's fine.
            let _ = completion_tx.send(exit_info);
        })
        .context("spawn vCPU thread")?;

    let boot_total_ms = boot_start.elapsed().as_millis();
    tracing::info!(
        cid = spec.cid,
        hypervisor_ms,
        boot_total_ms,
        "VM boot sequence complete"
    );

    Ok(VmHandle {
        thread: Some(thread),
        completion_rx: Some(completion_rx),
        kill_flag,
        serial_output,
        rootfs_path: rootfs_owned,
        cid: spec.cid,
    })
}

fn spawn_vsock_poller(
    poller: Option<visor_vmm::vm::VsockRxPoller>,
    kill_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let Some(poller) = poller else {
        return;
    };

    std::thread::spawn(move || {
        while !kill_flag.load(std::sync::atomic::Ordering::Acquire) {
            let _ = poller.poll_once();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
}

fn spawn_net_poller(
    poller: Option<visor_vmm::vm::NetRxPoller>,
    kill_flag: Arc<std::sync::atomic::AtomicBool>,
) {
    let Some(poller) = poller else {
        return;
    };

    std::thread::spawn(move || {
        while !kill_flag.load(std::sync::atomic::Ordering::Acquire) {
            let _ = poller.poll_once();
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    });
}

fn save_snapshot_artifact(
    booted: &visor_vmm::vm::BootedVm,
    rootfs_path: &Path,
    snapshot_dir: &Path,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(snapshot_dir)
        .with_context(|| format!("create snapshot directory {}", snapshot_dir.display()))?;
    visor_vmm::vm::save_snapshot(booted, snapshot_dir).context("save VMM snapshot bundle")?;
    std::fs::copy(rootfs_path, snapshot_dir.join("rootfs.ext4")).with_context(|| {
        format!(
            "copy rootfs image from {} into snapshot bundle",
            rootfs_path.display()
        )
    })?;
    Ok(())
}

// ── boot_vm_from_snapshot ────────────────────────────────────────

/// Boots a microVM from a pre-saved snapshot (fast-path).
///
/// Skips OCI pull and kernel loading entirely. Restores guest memory
/// from `memory.bin` via `mmap(MAP_PRIVATE)` COW and vCPU registers
/// from `cpu_state.json`. Provides sub-5ms VM startup.
///
/// Returns a [`VmHandle`] for monitoring completion and reading output.
///
/// # Errors
///
/// Returns an error if the snapshot directory is missing or invalid,
/// or if platform initialization fails.
pub fn boot_vm_from_snapshot(
    vm_id: &str,
    snapshot_dir: &Path,
    spec: VmBootSpec,
    storage: BootStorage<'_>,
    guest_networks: &[visor_init::config::NetworkConfig],
) -> anyhow::Result<VmHandle> {
    let boot_start = std::time::Instant::now();

    let mut restore_config = visor_vmm::vm::SnapshotRestoreConfig::with_shared_dirs(
        snapshot_dir.to_path_buf(),
        spec.memory_mib,
        spec.cid,
        storage.shared_dirs.to_vec(),
    );
    restore_config.data_disks = storage.data_disks.to_vec();
    restore_config.guest_virtualization = map_guest_virtualization_mode(spec.guest_virtualization);
    restore_config.network = build_vmm_network_config(guest_networks.first(), vm_id, spec.cid)
        .context("build VMM network config for snapshot restore")?;
    restore_config.networks = build_vmm_network_configs(guest_networks, vm_id, spec.cid)
        .context("build VMM network attachment configs for snapshot restore")?;

    if spec.vcpus > 1 {
        tracing::warn!(
            requested_vcpus = spec.vcpus,
            effective_vcpus = 1,
            "multi-vCPU not yet supported for snapshot restore, capping to 1"
        );
    }

    let t0 = std::time::Instant::now();
    let mut booted = visor_vmm::vm::boot_from_snapshot(&restore_config)
        .context("restore microVM from snapshot")?;
    let restore_ms = t0.elapsed().as_millis();

    spawn_vsock_poller(booted.vsock_rx_poller(), Arc::clone(&booted.kill_flag));
    spawn_net_poller(booted.net_rx_poller(), Arc::clone(&booted.kill_flag));

    // Spawn vsock muxer if present (macOS).
    if let Some(muxer) = booted.vsock_muxer.take() {
        let listener_path = muxer.listener_path();
        let _ = std::fs::remove_file(&listener_path);
        let listener =
            tokio::net::UnixListener::bind(&listener_path).context("bind vsock muxer listener")?;
        tokio::spawn(async move {
            if let Err(e) = muxer.run(listener).await {
                tracing::warn!(error = %e, "vsock muxer exited with error");
            }
        });
    }

    // Clone shared state before moving BootedVm into the vCPU thread.
    let serial_output = booted.serial_output.clone();
    let kill_flag = Arc::clone(&booted.kill_flag);

    let (completion_tx, completion_rx) = oneshot::channel::<VmExitInfo>();

    let thread = std::thread::Builder::new()
        .name("visor-vcpu-0".into())
        .spawn(move || {
            let result = visor_vmm::vm::run_vcpu(&mut booted);

            let exit_info = match result {
                Ok(()) => VmExitInfo {
                    exit_code: 0,
                    reason: VmExitReason::Shutdown,
                },
                Err(e) => VmExitInfo {
                    exit_code: 1,
                    reason: VmExitReason::Error(e.to_string()),
                },
            };

            let _ = completion_tx.send(exit_info);
        })
        .context("spawn vCPU thread")?;

    let boot_total_ms = boot_start.elapsed().as_millis();
    tracing::info!(
        cid = spec.cid,
        restore_ms,
        boot_total_ms,
        "VM snapshot restore complete"
    );

    Ok(VmHandle {
        thread: Some(thread),
        completion_rx: Some(completion_rx),
        kill_flag,
        serial_output,
        rootfs_path: PathBuf::new(), // No rootfs file for snapshot restore
        cid: spec.cid,
    })
}
