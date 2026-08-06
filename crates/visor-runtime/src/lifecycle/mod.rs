//! VM lifecycle abstraction.
//!
//! The [`VmLifecycle`] trait decouples the VM boot/stop/kill mechanics from
//! the higher-level orchestration in [`VmmBackend`](crate::backend::VmmBackend).
//!
//! On Linux, [`InProcessLifecycle`] runs VMs as threads in the daemon process
//! (the current model — KVM supports unlimited VMs per process).
//!
//! On macOS, [`WorkerProcessLifecycle`] spawns a child process per VM
//! to work around HVF's one-VM-per-process kernel constraint.

// Both in_process and worker_process are always compiled (needed for tests).
// On macOS, only worker_process is used in production; on Linux, only in_process.
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub(crate) mod in_process;
pub(crate) mod worker_protocol;
pub(crate) mod worker_pool;
pub(crate) mod vm_worker;
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) mod worker_process;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;

use crate::backend::VmLiveState;
use crate::vm::VmExitInfo;

/// Configuration bundle for booting a VM.
///
/// Aggregates everything the lifecycle implementation needs to boot a VM,
/// without exposing it to OCI-level concerns (image pull, rootfs build).
#[non_exhaustive]
pub(crate) struct VmBootConfig<'a> {
    /// Stable VM identifier used for host network interface naming.
    pub vm_id: &'a str,
    /// visor-init run configuration (cmd, env, workdir, volumes, mode).
    pub run_config: &'a visor_init::config::RunConfig,
    /// Path to the ext4 rootfs image.
    pub rootfs_path: &'a Path,
    /// Guest memory in MiB.
    pub memory_mib: u32,
    /// Number of virtual CPUs.
    pub vcpus: u32,
    /// Vsock context ID for this guest.
    pub cid: u32,
    /// Host directories to share with the guest via virtio-fs.
    pub shared_dirs: &'a [PathBuf],
    /// Port mappings for this VM (used to set up port forwarding).
    pub port_config: &'a visor_types::VmConfig,
    /// Temp directory path for rootfs cleanup on stop.
    pub tmp_dir: PathBuf,
}

/// Configuration for restoring a VM from a snapshot.
#[non_exhaustive]
pub(crate) struct VmSnapshotConfig<'a> {
    /// Stable VM identifier used for host network interface naming.
    pub vm_id: &'a str,
    /// Path to the snapshot directory (`memory.bin`, `cpu_state.json`).
    pub snapshot_dir: &'a Path,
    /// Guest memory in MiB.
    pub memory_mib: u32,
    /// Number of virtual CPUs.
    pub vcpus: u32,
    /// Vsock context ID for this guest.
    pub cid: u32,
    /// Host directories to share with the guest.
    pub shared_dirs: &'a [PathBuf],
    /// Port mappings for this VM.
    pub port_config: &'a visor_types::VmConfig,
}

/// Result of a synchronous (run-to-completion) VM execution.
#[non_exhaustive]
pub(crate) struct VmRunResult {
    /// Exit info from the vCPU run loop.
    pub exit_info: VmExitInfo,
    /// Raw serial output captured from the guest.
    pub serial_bytes: Vec<u8>,
}

/// Result of stopping or killing a VM.
#[non_exhaustive]
pub(crate) struct VmStopResult {
    /// Raw serial output captured before the VM stopped.
    pub serial_bytes: Vec<u8>,
}

/// Platform-specific VM lifecycle management.
///
/// Abstracts how VMs are booted, stopped, and killed. On Linux, VMs run as
/// threads in the daemon process. On macOS (future), each VM runs in a
/// separate worker process.
///
/// # Thread Safety
///
/// Implementations must be `Send + Sync` to allow sharing across async tasks.
#[async_trait]
pub(crate) trait VmLifecycle: Send + Sync {
    /// Boot a VM in detached mode (returns immediately with live state).
    ///
    /// The caller is responsible for storing the returned [`VmLiveState`]
    /// and monitoring the VM's completion.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM fails to boot (hypervisor error,
    /// invalid config, resource exhaustion).
    async fn boot(&self, config: VmBootConfig<'_>) -> anyhow::Result<VmLiveState>;

    /// Boot a VM from a pre-saved snapshot (fast restore path).
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot is invalid or platform init fails.
    async fn boot_from_snapshot(
        &self,
        config: VmSnapshotConfig<'_>,
    ) -> anyhow::Result<VmLiveState>;

    /// Run a VM synchronously to completion (blocking until exit).
    ///
    /// Used for non-detached `docker run` / `visor run` flows.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM fails to boot or the run loop errors.
    async fn run_to_completion(&self, config: VmBootConfig<'_>) -> anyhow::Result<VmRunResult>;

    /// Stop a VM gracefully via its live state.
    ///
    /// Sends a shutdown signal via vsock, waits for the vCPU thread
    /// to finish (with timeout), and cleans up resources.
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown signal fails.
    async fn stop(
        &self,
        state: VmLiveState,
        timeout_secs: u64,
    ) -> anyhow::Result<VmStopResult>;

    /// Force-kill a VM immediately (no graceful shutdown).
    ///
    /// Sets the kill flag and joins the vCPU thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the thread join fails.
    async fn kill(&self, state: VmLiveState) -> anyhow::Result<VmStopResult>;
}

/// Creates the platform-appropriate [`VmLifecycle`] implementation.
///
/// On macOS, returns [`WorkerProcessLifecycle`] which spawns a child
/// process per VM (required by HVF's one-VM-per-process constraint).
/// On Linux, returns [`InProcessLifecycle`] which runs VMs as threads.
#[must_use]
pub(crate) fn create_lifecycle(
    connector: Arc<dyn crate::backend::VsockConnector>,
) -> Arc<dyn VmLifecycle> {
    #[cfg(target_os = "macos")]
    {
        Arc::new(worker_process::WorkerProcessLifecycle::new(connector))
    }
    #[cfg(not(target_os = "macos"))]
    {
        Arc::new(in_process::InProcessLifecycle::new(connector))
    }
}
