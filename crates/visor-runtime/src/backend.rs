//! Execution backend implementation.
//!
//! The [`ExecutionBackend`] trait and shared types live in `visor-types`.
//! This module provides the concrete [`VmmBackend`] implementation that uses
//! `visor-vmm` to run OCI containers as hypervisor-accelerated microVMs.

use std::collections::HashMap;
use std::fmt;
use std::io::Write as _;
use std::net::Ipv4Addr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use anyhow::Context;
use async_trait::async_trait;
use base64::Engine as _;
use tokio::sync::RwLock;
use uuid::Uuid;

pub use visor_types::{
    AsyncIoStream, ExecRequest, ExecResult, ExecutionBackend, FIRST_GUEST_CID, GuestNetworkLink,
    PortMapping, VmConfig, VmInfo, VmState, VolumeMount,
};

use crate::vsock::client::{VSOCK_AGENT_PORT, VsockClient};
#[cfg(test)]
#[path = "backend_test.rs"]
mod tests;

// ── VmmBackend ─────────────────────────────────────────────────────

#[derive(Debug, Default)]
struct ResolvedVmStorage {
    shared_dirs: Vec<PathBuf>,
    data_disks: Vec<visor_vmm::vm::DataDiskConfig>,
    guest_volumes: Vec<visor_init::config::VolumeConfig>,
}

/// Per-VM live state for running VMs (not serializable, not cloneable).
///
/// Holds the vCPU thread handle, completion signal, serial output, and
/// vsock CID. Created during `boot_vm()` and consumed during `stop()`.
pub(crate) struct VmLiveState {
    /// Vsock context ID for this guest (CID 3+).
    pub(crate) cid: u32,
    /// vCPU thread join handle (taken on stop).
    pub(crate) thread: Option<std::thread::JoinHandle<()>>,
    /// Shared flag to signal the vCPU thread to exit.
    pub(crate) kill_flag: Arc<std::sync::atomic::AtomicBool>,
    /// Receives exit info when the vCPU thread finishes (taken on stop).
    pub(crate) completion_rx: Option<tokio::sync::oneshot::Receiver<crate::vm::VmExitInfo>>,
    /// Serial output buffer.
    pub(crate) serial_output: crate::vm::SerialOutput,
    /// Temp directory for rootfs cleanup.
    pub(crate) tmp_dir: std::path::PathBuf,
    /// RAII handle for port-forwarding rules (pfctl/iptables).
    /// Dropped on VM stop to clean up forwarding rules.
    pub(crate) port_forward_handle: Option<Box<dyn visor_vmm::net::PortForwardHandle>>,
}

/// Trait abstracting vsock communication with guest VMs.
///
/// The real implementation connects via `AF_VSOCK`; tests inject a mock.
#[async_trait]
pub(crate) trait VsockConnector: Send + Sync {
    /// Execute a command inside the guest via vsock.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection fails or the guest returns an error.
    async fn exec_cmd(&self, cid: u32, req: &ExecRequest) -> anyhow::Result<ExecResult>;

    /// Start a streaming command inside the guest via vsock.
    ///
    /// # Errors
    ///
    /// Returns an error if the streaming session cannot be established.
    async fn exec_stream_cmd(
        &self,
        cid: u32,
        req: &ExecRequest,
    ) -> anyhow::Result<Box<dyn AsyncIoStream>>;

    /// Copy a tar archive into the guest filesystem via the guest agent.
    ///
    /// # Errors
    ///
    /// Returns an error if the guest agent connection or copy RPC fails.
    async fn copy_to_guest(&self, cid: u32, archive: &[u8], dest: &str) -> anyhow::Result<()>;

    /// Request graceful shutdown of the guest via vsock.
    ///
    /// # Errors
    ///
    /// Returns an error if the connection or shutdown request fails.
    async fn shutdown(&self, cid: u32) -> anyhow::Result<()>;
}

/// Creates the platform-appropriate comms backend.
pub(crate) fn comms_backend() -> visor_vmm::comms::PlatformCommsBackend {
    visor_vmm::comms::create_comms_backend()
}

/// Real vsock connector using platform-specific sockets.
struct RealVsockConnector;

/// Maximum time to wait for a freshly booted detached guest to accept exec.
const EXEC_AGENT_CONNECT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);

/// Poll interval while waiting for the guest exec agent to come up.
const EXEC_AGENT_RETRY_INTERVAL: std::time::Duration = std::time::Duration::from_millis(250);

async fn connect_exec_client_with_retry(
    cid: u32,
) -> anyhow::Result<VsockClient<Box<dyn visor_vmm::comms::AsyncStream>>> {
    let backend = comms_backend();
    let start = std::time::Instant::now();

    loop {
        match VsockClient::connect(&backend, cid, VSOCK_AGENT_PORT).await {
            Ok(client) => return Ok(client),
            Err(error) if start.elapsed() < EXEC_AGENT_CONNECT_TIMEOUT => {
                tracing::debug!(
                    cid,
                    error = %error,
                    "guest exec agent not ready yet, retrying"
                );
                tokio::time::sleep(EXEC_AGENT_RETRY_INTERVAL).await;
            }
            Err(error) => {
                return Err(error).context("connect to guest agent for exec");
            }
        }
    }
}

async fn connect_exec_stream_with_retry(
    cid: u32,
    req: &ExecRequest,
) -> anyhow::Result<Box<dyn AsyncIoStream>> {
    let backend = comms_backend();
    let workdir = req.working_dir.clone().unwrap_or_else(|| "/".to_owned());
    let start = std::time::Instant::now();

    loop {
        match VsockClient::connect_exec_stream(
            &backend,
            cid,
            VSOCK_AGENT_PORT,
            req.cmd.clone(),
            req.env.clone(),
            workdir.clone(),
            req.tty,
        )
        .await
        {
            Ok(stream) => return Ok(Box::new(stream) as Box<dyn AsyncIoStream>),
            Err(
                error @ (crate::vsock::client::VsockError::Connect { .. }
                | crate::vsock::client::VsockError::Timeout { .. }),
            ) if start.elapsed() < EXEC_AGENT_CONNECT_TIMEOUT => {
                tracing::debug!(
                    cid,
                    error = %error,
                    "guest streaming exec agent not ready yet, retrying"
                );
                tokio::time::sleep(EXEC_AGENT_RETRY_INTERVAL).await;
            }
            Err(error) => {
                return Err(error).context("connect to guest agent for streaming exec");
            }
        }
    }
}

#[async_trait]
impl VsockConnector for RealVsockConnector {
    async fn exec_cmd(&self, cid: u32, req: &ExecRequest) -> anyhow::Result<ExecResult> {
        let mut client = connect_exec_client_with_retry(cid).await?;
        let workdir = req.working_dir.clone().unwrap_or_else(|| "/".to_owned());
        let result = client
            .exec(req.cmd.clone(), req.env.clone(), workdir)
            .await
            .context("exec command in guest")?;
        Ok(ExecResult::new(
            result.exit_code,
            result.stdout,
            result.stderr,
        ))
    }

    async fn exec_stream_cmd(
        &self,
        cid: u32,
        req: &ExecRequest,
    ) -> anyhow::Result<Box<dyn AsyncIoStream>> {
        connect_exec_stream_with_retry(cid, req).await
    }

    async fn copy_to_guest(&self, cid: u32, archive: &[u8], dest: &str) -> anyhow::Result<()> {
        let mut client = connect_exec_client_with_retry(cid).await?;
        let encoded_archive = encode_guest_archive(archive).context("encode guest archive")?;
        client
            .copy_files(encoded_archive, dest.to_owned())
            .await
            .context("copy files into guest")?;
        Ok(())
    }

    async fn shutdown(&self, cid: u32) -> anyhow::Result<()> {
        let backend = comms_backend();
        let mut client = VsockClient::connect(&backend, cid, VSOCK_AGENT_PORT)
            .await
            .context("connect to guest agent for shutdown")?;
        client
            .shutdown()
            .await
            .context("send shutdown to guest agent")?;
        Ok(())
    }
}

/// Hypervisor-accelerated execution backend.
///
/// Orchestrates the full lifecycle: OCI image pull, layer merge,
/// rootfs build, KVM boot, vCPU run, and output capture.
///
/// # Thread Safety
///
/// All state is behind `Arc<RwLock<...>>`, making `VmmBackend` safe to share
/// across async tasks.
#[derive(Clone)]
pub struct VmmBackend {
    vms: Arc<RwLock<HashMap<String, VmInfo>>>,
    live_vms: Arc<RwLock<HashMap<String, VmLiveState>>>,
    vm_configs: Arc<RwLock<HashMap<String, VmConfig>>>,
    connector: Arc<dyn VsockConnector>,
    lifecycle: Arc<dyn crate::lifecycle::VmLifecycle>,
    next_cid: Arc<AtomicU32>,
    image_store_path: PathBuf,
}

impl fmt::Debug for VmmBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("VmmBackend")
            .field("vm_count", &"<locked>")
            .finish_non_exhaustive()
    }
}

impl VmmBackend {
    fn name_exists(vms: &HashMap<String, VmInfo>, name: &str) -> bool {
        vms.values().any(|vm| vm.name.as_deref() == Some(name))
    }

    fn choose_unique_vm_name_with_generator(
        vms: &HashMap<String, VmInfo>,
        requested_name: Option<&str>,
        mut generate_name: impl FnMut() -> String,
    ) -> anyhow::Result<String> {
        const AUTO_NAME_MAX_ATTEMPTS: usize = 32;

        if let Some(name) = requested_name {
            anyhow::ensure!(!name.is_empty(), "vm name must not be empty");
            anyhow::ensure!(
                !Self::name_exists(vms, name),
                "vm name '{name}' already exists"
            );
            return Ok(name.to_owned());
        }

        for _ in 0..AUTO_NAME_MAX_ATTEMPTS {
            let candidate = generate_name();
            if !Self::name_exists(vms, &candidate) {
                return Ok(candidate);
            }
        }

        anyhow::bail!("failed to allocate unique VM name after {AUTO_NAME_MAX_ATTEMPTS} attempts");
    }

    fn choose_unique_vm_name(
        vms: &HashMap<String, VmInfo>,
        requested_name: Option<&str>,
    ) -> anyhow::Result<String> {
        Self::choose_unique_vm_name_with_generator(vms, requested_name, crate::names::generate_name)
    }

    /// Create a new, empty `VmmBackend` with real vsock connectivity.
    #[must_use]
    pub fn new() -> Self {
        Self::with_image_store_path(default_image_store_path())
    }

    /// Create a new backend using an explicit OCI image-store directory.
    ///
    /// This lets the runtime, Docker shim, and build service share the same
    /// local image store so locally built or loaded tags can be executed
    /// without re-pulling them from a registry.
    #[must_use]
    pub fn with_image_store_path(image_store_path: PathBuf) -> Self {
        let connector: Arc<dyn VsockConnector> = Arc::new(RealVsockConnector);
        let lifecycle = crate::lifecycle::create_lifecycle(Arc::clone(&connector));
        Self {
            vms: Arc::new(RwLock::new(HashMap::new())),
            live_vms: Arc::new(RwLock::new(HashMap::new())),
            vm_configs: Arc::new(RwLock::new(HashMap::new())),
            connector,
            lifecycle,
            next_cid: Arc::new(AtomicU32::new(FIRST_GUEST_CID)),
            image_store_path,
        }
    }

    /// Create a backend with a custom vsock connector (for testing).
    #[cfg(test)]
    pub(crate) fn with_connector(connector: Arc<dyn VsockConnector>) -> Self {
        let lifecycle = crate::lifecycle::create_lifecycle(Arc::clone(&connector));
        Self {
            vms: Arc::new(RwLock::new(HashMap::new())),
            live_vms: Arc::new(RwLock::new(HashMap::new())),
            vm_configs: Arc::new(RwLock::new(HashMap::new())),
            connector,
            lifecycle,
            next_cid: Arc::new(AtomicU32::new(FIRST_GUEST_CID)),
            image_store_path: default_image_store_path(),
        }
    }

    /// Allocate the next guest CID (monotonically increasing from 3).
    fn allocate_cid(&self) -> u32 {
        self.next_cid.fetch_add(1, Ordering::Relaxed)
    }

    /// Insert a pre-built [`VmInfo`] into the backend state.
    ///
    /// Used by tests that need to register a VM without running the full
    /// OCI pull + KVM boot pipeline.
    #[cfg(test)]
    pub(crate) async fn insert_vm(&self, info: VmInfo) {
        self.restore_vm(info).await;
    }

    /// Insert a pre-built [`VmInfo`] with a vsock CID for exec/stop testing.
    #[cfg(test)]
    pub(crate) async fn insert_vm_with_cid(&self, info: VmInfo, cid: u32) {
        let id = info.id.clone();
        let config = fallback_vm_config_for_info(&info);
        self.restore_vm_with_config(info, config).await;
        self.live_vms.write().await.insert(
            id,
            VmLiveState {
                cid,
                thread: None,
                kill_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
                completion_rx: None,
                serial_output: crate::vm::SerialOutput::new(),
                tmp_dir: std::path::PathBuf::new(),
                port_forward_handle: None,
            },
        );
    }
}

#[async_trait]
impl crate::pool::health::RunningVmProvider for VmmBackend {
    async fn running_vms(&self) -> Vec<(String, u32)> {
        let vms = self.vms.read().await;
        vms.values()
            .filter(|vm| vm.state == VmState::Running)
            .filter_map(|vm| vm.cid.map(|cid| (vm.id.clone(), cid)))
            .collect()
    }
}

impl VmmBackend {
    /// Returns the IDs of all VMs in the `Running` state.
    pub async fn running_vm_ids(&self) -> Vec<String> {
        let vms = self.vms.read().await;
        vms.values()
            .filter(|vm| vm.state == VmState::Running)
            .map(|vm| vm.id.clone())
            .collect()
    }

    /// Force-stop all running VMs during daemon shutdown.
    ///
    /// This ensures host-side resources such as TAP devices, NAT rules, and
    /// vCPU threads are torn down before the daemon process exits.
    pub async fn shutdown_all_running_vms(&self) {
        let running_ids = self.running_vm_ids().await;

        for id in running_ids {
            if let Err(error) = self.kill(&id).await {
                tracing::warn!(vm_id = id, error = %error, "failed to kill VM during daemon shutdown");
            }
        }
    }

    /// Returns information about a specific VM, or `None` if not found.
    #[must_use]
    pub async fn get_vm_info(&self, id: &str) -> Option<VmInfo> {
        let vms = self.vms.read().await;
        vms.get(id).cloned()
    }

    /// Returns the stored VM configuration for a VM, if available.
    #[must_use]
    pub async fn get_vm_config(&self, id: &str) -> Option<VmConfig> {
        let configs = self.vm_configs.read().await;
        configs.get(id).cloned()
    }

    /// Inserts a [`VmInfo`] into the backend (used by restore).
    pub async fn restore_vm(&self, info: VmInfo) {
        let config = fallback_vm_config_for_info(&info);
        self.restore_vm_with_config(info, config).await;
    }

    /// Inserts a [`VmInfo`] and its associated [`VmConfig`] into the backend.
    pub async fn restore_vm_with_config(&self, info: VmInfo, config: VmConfig) {
        self.vm_configs
            .write()
            .await
            .insert(info.id.clone(), config);
        self.vms.write().await.insert(info.id.clone(), info);
    }
}

impl VmmBackend {
    /// Resolve a user-provided ID or name prefix to a full VM ID.
    ///
    /// Matches in order:
    /// 1. Exact ID match
    /// 2. Exact name match
    /// 3. ID prefix match (must be unambiguous)
    ///
    /// # Errors
    ///
    /// Returns an error if no VM matches, or if the prefix is ambiguous.
    pub async fn resolve_id(&self, input: &str) -> anyhow::Result<String> {
        let vms = self.vms.read().await;
        if vms.contains_key(input) {
            return Ok(input.to_owned());
        }
        let exact_name_matches: Vec<(&String, &VmInfo)> = vms
            .iter()
            .filter(|(_, vm)| vm.name.as_deref() == Some(input))
            .collect();
        match exact_name_matches.len() {
            0 => {}
            1 => return Ok(exact_name_matches[0].0.clone()),
            n => anyhow::bail!("ambiguous VM name \"{input}\" matches {n} VMs"),
        }
        let matches: Vec<&String> = vms.keys().filter(|id| id.starts_with(input)).collect();
        match matches.len() {
            0 => anyhow::bail!("vm not found: {input}"),
            1 => Ok(matches[0].clone()),
            n => anyhow::bail!("ambiguous VM ID prefix \"{input}\" matches {n} VMs"),
        }
    }
}

impl Default for VmmBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for VmmBackend {
    /// Create and run a VM from an OCI image.
    ///
    /// When `config.detach` is `false` (default), runs the full pipeline:
    /// OCI pull → rootfs build → KVM boot → wait → capture output.
    ///
    /// When `config.detach` is `true`, boots the VM and returns immediately
    /// with `VmState::Running`. Use `exec()` and `stop()` to interact.
    ///
    /// # Errors
    ///
    /// Returns an error if any step of the pipeline fails.
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        let id = Uuid::new_v4().to_string();
        let cid = self.allocate_cid();

        // Store initial "creating" state.
        let mut info = VmInfo::new(
            id.clone(),
            config.image.clone(),
            VmState::Creating,
            crate::timeutil::utc_now_iso8601(),
            config.memory_mib,
            config.vcpus,
        );
        info.ports.clone_from(&config.ports);
        info.cid = Some(cid);
        {
            let mut vms = self.vms.write().await;
            info.name = Some(Self::choose_unique_vm_name(&vms, config.name.as_deref())?);
            vms.insert(id.clone(), info.clone());
        }
        self.vm_configs
            .write()
            .await
            .insert(id.clone(), config.clone());

        if config.detach {
            // Detach mode: boot and return immediately.
            match self.boot_pipeline(&id, &config, cid).await {
                Ok(live_state) => {
                    self.live_vms.write().await.insert(id.clone(), live_state);
                    let mut vms = self.vms.write().await;
                    if let Some(vm) = vms.get_mut(&id) {
                        vm.state = VmState::Running;
                    }
                    let result = vms.get(&id).cloned().context("vm disappeared after create");

                    // Spawn background task to monitor VM completion and
                    // transition state from Running → Stopped when the
                    // vCPU exits naturally (e.g. `docker run` short-lived
                    // containers).
                    let backend = self.clone();
                    let id_clone = id.clone();
                    tokio::spawn(async move {
                        backend.monitor_vm_completion(&id_clone).await;
                    });

                    result
                }
                Err(e) => {
                    let mut vms = self.vms.write().await;
                    if let Some(vm) = vms.get_mut(&id) {
                        vm.state = VmState::Failed;
                    }
                    Err(e)
                }
            }
        } else {
            // Sync mode: run to completion.
            match self.run_pipeline(&id, &config, cid).await {
                Ok(result) => {
                    let mut vms = self.vms.write().await;
                    if let Some(vm) = vms.get_mut(&id) {
                        vm.state = VmState::Stopped;
                        vm.exit_code = result.exit_code;
                    }
                    Ok(result)
                }
                Err(e) => {
                    let mut vms = self.vms.write().await;
                    if let Some(vm) = vms.get_mut(&id) {
                        vm.state = VmState::Failed;
                    }
                    Err(e)
                }
            }
        }
    }

    /// Create a VM from a pre-saved snapshot (fast restore path).
    ///
    /// Skips OCI pull entirely. Restores guest memory via
    /// `mmap(MAP_PRIVATE)` COW from `memory.bin` and vCPU registers
    /// from `cpu_state.json`. Provides sub-5ms VM startup.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot is invalid or platform init fails.
    async fn create_from_snapshot(
        &self,
        config: VmConfig,
        snapshot_dir: &std::path::Path,
    ) -> anyhow::Result<VmInfo> {
        let id = uuid::Uuid::new_v4().to_string();
        let cid = self.allocate_cid();

        let mut info = VmInfo::new(
            id.clone(),
            config.image.clone(),
            VmState::Creating,
            crate::timeutil::utc_now_iso8601(),
            config.memory_mib,
            config.vcpus,
        );
        info.ports.clone_from(&config.ports);
        info.cid = Some(cid);
        {
            let mut vms = self.vms.write().await;
            info.name = Some(Self::choose_unique_vm_name(&vms, config.name.as_deref())?);
            vms.insert(id.clone(), info.clone());
        }
        self.vm_configs
            .write()
            .await
            .insert(id.clone(), config.clone());

        let pipeline_start = std::time::Instant::now();

        let tmp_dir = create_vm_temp_dir(&id).context("create snapshot restore temp dir")?;
        let storage = resolve_vm_storage(&config.volumes, &tmp_dir)
            .context("resolve VM storage devices for snapshot restore")?;
        let guest_networks = guest_network_configs_for_vm(&config, cid);

        let t0 = std::time::Instant::now();
        let mut handle = crate::vm::boot_vm_from_snapshot(
            &id,
            snapshot_dir,
            crate::vm::VmBootSpec::new(config.memory_mib, config.vcpus, cid)
                .with_guest_virtualization(config.guest_virtualization),
            crate::vm::BootStorage::new(&storage.shared_dirs, &storage.data_disks),
            &guest_networks,
        )
        .context("snapshot fast-path restore")?;
        let restore_ms = t0.elapsed().as_millis();

        // Port forwarding (same as boot_pipeline).
        let t1 = std::time::Instant::now();
        let port_forward_handle = setup_port_forwards(&config, &guest_networks)
            .context("setup port forwards for snapshot VM")?;
        let pf_ms = t1.elapsed().as_millis();

        let total_ms = pipeline_start.elapsed().as_millis();
        tracing::info!(
            vm_id = &*id,
            cid,
            restore_ms,
            port_forward_ms = pf_ms,
            total_ms,
            "VM restored from snapshot (fast-path)"
        );

        // Store live state.
        let parts = handle.take_parts();
        self.live_vms.write().await.insert(
            id.clone(),
            VmLiveState {
                cid,
                thread: parts.thread,
                kill_flag: parts.kill_flag,
                completion_rx: parts.completion_rx,
                serial_output: parts.serial_output,
                tmp_dir,
                port_forward_handle,
            },
        );

        // Update state to Running.
        let mut vms = self.vms.write().await;
        if let Some(vm) = vms.get_mut(&id) {
            vm.state = VmState::Running;
        }
        vms.get(&id)
            .cloned()
            .context("vm disappeared after snapshot restore")
    }

    /// List all VMs.
    ///
    /// # Errors
    ///
    /// Currently infallible.
    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        let vms = self.vms.read().await;
        Ok(vms.values().cloned().collect())
    }

    /// Get a VM by its ID.
    ///
    /// # Errors
    ///
    /// Returns an error if no VM with the given ID exists.
    async fn get(&self, id: &str) -> anyhow::Result<VmInfo> {
        let id = &self.resolve_id(id).await?;
        let vms = self.vms.read().await;
        vms.get(id.as_str())
            .cloned()
            .context(format!("vm not found: {id}"))
    }

    /// Execute a command inside a running VM via vsock.
    ///
    /// Connects to the guest's visor-init agent over virtio-vsock and sends
    /// a JSON-RPC exec request. Returns the command's stdout, stderr, and exit code.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found, not running, or the vsock
    /// communication fails.
    async fn exec(&self, id: &str, req: ExecRequest) -> anyhow::Result<ExecResult> {
        let id = &self.resolve_id(id).await?;
        // Validate VM state.
        let cid = {
            let vms = self.vms.read().await;
            let vm = vms.get(id).context(format!("vm not found: {id}"))?;
            anyhow::ensure!(
                vm.state == VmState::Running,
                "vm {id} is not running (state: {:?})",
                vm.state
            );
            let live = self.live_vms.read().await;
            live.get(id)
                .map(|s| s.cid)
                .context(format!("vm {id} has no live state (no vsock CID)"))?
        };

        // Send exec request over vsock to visor-init.
        self.connector
            .exec_cmd(cid, &req)
            .await
            .with_context(|| format!("exec in vm {id} (CID {cid})"))
    }

    /// Start a streaming exec session inside a running VM via vsock.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found, not running, or the streaming
    /// vsock session cannot be established.
    async fn exec_stream(
        &self,
        id: &str,
        req: ExecRequest,
    ) -> anyhow::Result<Box<dyn AsyncIoStream>> {
        let id = &self.resolve_id(id).await?;
        let cid = {
            let vms = self.vms.read().await;
            let vm = vms.get(id).context(format!("vm not found: {id}"))?;
            anyhow::ensure!(
                vm.state == VmState::Running,
                "vm {id} is not running (state: {:?})",
                vm.state
            );
            let live = self.live_vms.read().await;
            live.get(id)
                .map(|s| s.cid)
                .context(format!("vm {id} has no live state (no vsock CID)"))?
        };

        self.connector
            .exec_stream_cmd(cid, &req)
            .await
            .with_context(|| format!("start streaming exec in vm {id} (CID {cid})"))
    }

    /// Copy a tar archive into a running VM.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found, not running, or the archive
    /// transfer to the guest agent fails.
    async fn copy_to_guest(&self, id: &str, archive: Vec<u8>, dest: &str) -> anyhow::Result<()> {
        let id = &self.resolve_id(id).await?;
        let cid = {
            let vms = self.vms.read().await;
            let vm = vms.get(id).context(format!("vm not found: {id}"))?;
            anyhow::ensure!(
                vm.state == VmState::Running,
                "vm {id} is not running (state: {:?})",
                vm.state
            );
            let live = self.live_vms.read().await;
            live.get(id)
                .map(|s| s.cid)
                .context(format!("vm {id} has no live state (no vsock CID)"))?
        };

        self.connector
            .copy_to_guest(cid, &archive, dest)
            .await
            .with_context(|| format!("copy archive into vm {id} (CID {cid})"))
    }

    /// Stop a running VM via vsock shutdown.
    ///
    /// Sends a shutdown request to visor-init, waits for the vCPU thread
    /// to complete (with timeout), and transitions the VM to `Stopped`.
    ///
    /// If the VM is already stopped, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found.
    async fn stop(&self, id: &str, timeout_secs: u64) -> anyhow::Result<()> {
        let id = &self.resolve_id(id).await?;
        // Check current state — already stopped is a no-op.
        {
            let vms = self.vms.read().await;
            let vm = vms.get(id).context(format!("vm not found: {id}"))?;
            if vm.state == VmState::Stopped || vm.state == VmState::Failed {
                return Ok(());
            }
        }

        // Try graceful vsock shutdown first, then force-kill via kill_flag.
        let live = self.live_vms.write().await.remove(id);
        if let Some(mut state) = live {
            // 1. Try graceful shutdown via vsock (using the grace period).
            //    If timeout_secs == 0, skip vsock entirely (behaves like kill).
            let graceful = if timeout_secs == 0 {
                false
            } else {
                match tokio::time::timeout(
                    std::time::Duration::from_secs(timeout_secs),
                    self.connector.shutdown(state.cid),
                )
                .await
                {
                    Ok(Ok(())) => true,
                    Ok(Err(e)) => {
                        tracing::warn!(vm_id = id, cid = state.cid, error = %e, "vsock shutdown failed, forcing stop");
                        false
                    }
                    Err(_) => {
                        tracing::warn!(
                            vm_id = id,
                            cid = state.cid,
                            "vsock shutdown timed out after {timeout_secs}s, forcing stop"
                        );
                        false
                    }
                }
            };

            // 2. If graceful worked, wait briefly for completion. Otherwise, set kill_flag.
            if graceful {
                if let Some(rx) = state.completion_rx.take() {
                    match tokio::time::timeout(std::time::Duration::from_secs(2), rx).await {
                        Ok(Ok(exit_info)) => {
                            tracing::info!(
                                vm_id = id,
                                exit_code = exit_info.exit_code,
                                reason = %exit_info.reason,
                                "VM exited after graceful shutdown"
                            );
                        }
                        Ok(Err(_)) => {
                            tracing::warn!(vm_id = id, "completion channel dropped during stop");
                        }
                        Err(_) => {
                            tracing::warn!(
                                vm_id = id,
                                "VM did not exit within 2s, setting kill_flag"
                            );
                            state
                                .kill_flag
                                .store(true, std::sync::atomic::Ordering::Release);
                        }
                    }
                }
            } else {
                // Force kill: set kill_flag so run_loop exits at next timer interrupt.
                state
                    .kill_flag
                    .store(true, std::sync::atomic::Ordering::Release);
            }

            // 3. Capture serial output before joining thread.
            let serial_bytes = state.serial_output.as_bytes();
            if !serial_bytes.is_empty() {
                let stdout = crate::vm::extract_stdout(&serial_bytes);
                let exit_code = crate::vm::parse_exit_code(&serial_bytes);
                let mut vms = self.vms.write().await;
                if let Some(vm) = vms.get_mut(id) {
                    if !stdout.is_empty() {
                        vm.stdout = Some(stdout);
                    }
                    vm.exit_code = Some(exit_code);
                }
            }

            // 4. Wait for vCPU thread to finish (2s).
            if let Some(thread) = state.thread.take() {
                let join_result = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    tokio::task::spawn_blocking(move || thread.join()),
                )
                .await;
                match join_result {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        tracing::warn!(vm_id = id, "vCPU thread join failed: {e}");
                    }
                    Err(_) => {
                        tracing::warn!(vm_id = id, "vCPU thread did not exit within 2s, detaching");
                    }
                }
            }

            // 5. Clean up port-forwarding rules (RAII drop).
            if let Some(ref pf) = state.port_forward_handle {
                tracing::debug!(
                    vm_id = id,
                    mappings = pf.mapping_count(),
                    "dropping port-forward rules"
                );
            }
            drop(state.port_forward_handle.take());

            // 5. Clean up temp directory.
            let _ = std::fs::remove_dir_all(&state.tmp_dir);
        }

        // Update state.
        let mut vms = self.vms.write().await;
        if let Some(vm) = vms.get_mut(id) {
            vm.state = VmState::Stopped;
        }
        Ok(())
    }

    /// Start a previously stopped or failed VM using its stored creation config.
    async fn start(&self, id: &str) -> anyhow::Result<VmInfo> {
        let id = self.resolve_id(id).await?;
        let existing = {
            let vms = self.vms.read().await;
            vms.get(&id)
                .cloned()
                .context(format!("vm not found: {id}"))?
        };
        anyhow::ensure!(
            existing.state == VmState::Stopped || existing.state == VmState::Failed,
            "vm {id} cannot be started from state {:?}",
            existing.state
        );

        let mut config = {
            let configs = self.vm_configs.read().await;
            configs
                .get(&id)
                .cloned()
                .context(format!("vm start config not found: {id}"))?
        };
        config.detach = true;
        if config.name.is_none() {
            config.name.clone_from(&existing.name);
        }
        if config.ports.is_empty() {
            config.ports.clone_from(&existing.ports);
        }

        let cid = self.allocate_cid();
        {
            let mut live_vms = self.live_vms.write().await;
            live_vms.remove(&id);
        }
        {
            let mut vms = self.vms.write().await;
            let vm = vms.get_mut(&id).context(format!("vm not found: {id}"))?;
            vm.state = VmState::Creating;
            vm.memory_mib = config.memory_mib;
            vm.vcpus = config.vcpus;
            vm.name.clone_from(&config.name);
            vm.ports.clone_from(&config.ports);
            vm.exit_code = None;
            vm.stdout = None;
            vm.stderr = None;
            vm.cid = Some(cid);
        }
        self.vm_configs
            .write()
            .await
            .insert(id.clone(), config.clone());

        match self.boot_pipeline(&id, &config, cid).await {
            Ok(live_state) => {
                self.live_vms.write().await.insert(id.clone(), live_state);
                let result = {
                    let mut vms = self.vms.write().await;
                    let vm = vms
                        .get_mut(&id)
                        .context(format!("vm disappeared after start: {id}"))?;
                    vm.state = VmState::Running;
                    vm.cid = Some(cid);
                    vm.clone()
                };

                let backend = self.clone();
                let id_clone = id.clone();
                tokio::spawn(async move {
                    backend.monitor_vm_completion(&id_clone).await;
                });

                Ok(result)
            }
            Err(error) => {
                let mut vms = self.vms.write().await;
                if let Some(vm) = vms.get_mut(&id) {
                    vm.state = VmState::Failed;
                }
                Err(error)
            }
        }
    }

    /// Force-kill a running VM immediately via `kill_flag` (no vsock shutdown).
    async fn kill(&self, id: &str) -> anyhow::Result<()> {
        let id = &self.resolve_id(id).await?;
        // Already stopped is a no-op.
        {
            let vms = self.vms.read().await;
            let vm = vms.get(id).context(format!("vm not found: {id}"))?;
            if vm.state == VmState::Stopped || vm.state == VmState::Failed {
                return Ok(());
            }
        }

        // Set kill_flag and join thread — no vsock, no grace period.
        let live = self.live_vms.write().await.remove(id);
        if let Some(mut state) = live {
            state
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);

            // Capture serial output before joining thread.
            let serial_bytes = state.serial_output.as_bytes();
            if !serial_bytes.is_empty() {
                let stdout = crate::vm::extract_stdout(&serial_bytes);
                let exit_code = crate::vm::parse_exit_code(&serial_bytes);
                let mut vms = self.vms.write().await;
                if let Some(vm) = vms.get_mut(id) {
                    if !stdout.is_empty() {
                        vm.stdout = Some(stdout);
                    }
                    vm.exit_code = Some(exit_code);
                }
            }

            // Wait for vCPU thread (2s — should be near-instant with kill_flag).
            if let Some(thread) = state.thread.take() {
                let join_result = tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    tokio::task::spawn_blocking(move || thread.join()),
                )
                .await;
                match join_result {
                    Ok(Ok(_)) => {}
                    Ok(Err(e)) => {
                        tracing::warn!(vm_id = id, "vCPU thread join failed: {e}");
                    }
                    Err(_) => {
                        tracing::warn!(vm_id = id, "vCPU thread did not exit within 2s, detaching");
                    }
                }
            }

            // Clean up port-forwarding rules (RAII drop).
            if let Some(ref pf) = state.port_forward_handle {
                tracing::debug!(
                    vm_id = id,
                    mappings = pf.mapping_count(),
                    "dropping port-forward rules"
                );
            }
            drop(state.port_forward_handle.take());

            let _ = std::fs::remove_dir_all(&state.tmp_dir);
        }

        // Update state.
        let mut vms = self.vms.write().await;
        if let Some(vm) = vms.get_mut(id) {
            vm.state = VmState::Stopped;
        }
        Ok(())
    }

    /// Destroy a VM, removing it from the backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found.
    async fn destroy(&self, id: &str) -> anyhow::Result<()> {
        let id = &self.resolve_id(id).await?;
        // Stop first if running.
        self.stop(id, 10).await.ok();
        let mut vms = self.vms.write().await;
        vms.remove(id).context(format!("vm not found: {id}"))?;
        self.live_vms.write().await.remove(id);
        self.vm_configs.write().await.remove(id);
        Ok(())
    }

    async fn console_output(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let id = &self.resolve_id(id).await?;
        let live_vms = self.live_vms.read().await;
        let state = live_vms
            .get(id)
            .context(format!("vm '{id}' is not running"))?;
        Ok(state.serial_output.as_bytes())
    }
}

impl VmmBackend {
    /// Build `RunConfig` from `VmConfig` and image config.
    async fn build_run_config(
        &self,
        id: &str,
        config: &VmConfig,
        cid: u32,
    ) -> anyhow::Result<(
        visor_init::config::RunConfig,
        std::path::PathBuf,
        crate::oci::config::ImageConfig,
        std::path::PathBuf,
        ResolvedVmStorage,
    )> {
        // Phase 1: OCI pull + rootfs build
        let (rootfs_path, image_config, tmp_dir) = self.pull_and_build_rootfs(id, config).await?;

        // Phase 2: Build RunConfig for visor-init
        let cmd = resolve_run_command(config, &image_config);

        let mut env = image_config.env.clone();
        env.extend(config.env.iter().cloned());

        let workdir = config
            .working_dir
            .clone()
            .or(image_config.working_dir.clone())
            .unwrap_or_else(|| "/".to_owned());
        let storage =
            resolve_vm_storage(&config.volumes, &tmp_dir).context("resolve VM storage devices")?;

        let mut run_config = visor_init::config::RunConfig::default();
        run_config.cmd = cmd;
        run_config.env = env;
        run_config.workdir = workdir;
        let guest_networks = guest_network_configs_for_vm(config, cid);
        if guest_networks.len() <= 1 {
            run_config.network = guest_networks.first().cloned();
        } else {
            run_config.networks = guest_networks;
        }
        run_config.extra_hosts = guest_extra_hosts(config);
        run_config.volumes.clone_from(&storage.guest_volumes);
        if let Some(ref mode) = config.mode {
            run_config.mode.clone_from(mode);
        }
        run_config.exec_listener = config.detach && run_config.mode != "agent";

        Ok((run_config, rootfs_path, image_config, tmp_dir, storage))
    }

    /// Boot pipeline for detached VMs: OCI pull → build rootfs → boot → return handle.
    ///
    /// Returns [`VmLiveState`] with the running VM's handle and CID.
    ///
    /// # Errors
    ///
    /// Returns an error if any step fails.
    async fn boot_pipeline(
        &self,
        id: &str,
        config: &VmConfig,
        cid: u32,
    ) -> anyhow::Result<VmLiveState> {
        use crate::vm;

        let pipeline_start = std::time::Instant::now();

        let t0 = std::time::Instant::now();
        let (run_config, rootfs_path, _image_config, tmp_dir, storage) =
            self.build_run_config(id, config, cid).await?;
        let oci_ms = t0.elapsed().as_millis();

        // macOS: HVF is one-VM-per-process — boot via process-per-VM lifecycle.
        #[cfg(target_os = "macos")]
        {
            let boot_config = crate::lifecycle::VmBootConfig {
                vm_id: id,
                run_config: &run_config,
                rootfs_path: &rootfs_path,
                memory_mib: config.memory_mib,
                vcpus: config.vcpus,
                cid,
                shared_dirs: &storage.shared_dirs,
                port_config: config,
                tmp_dir,
            };
            let live = self
                .lifecycle
                .boot(boot_config)
                .await
                .context("boot microVM via lifecycle")?;
            tracing::info!(
                vm_id = id,
                cid,
                oci_ms,
                total_ms = pipeline_start.elapsed().as_millis(),
                "VM booted (lifecycle, detached)"
            );
            return Ok(live);
        }

        #[cfg(not(target_os = "macos"))]
        {
            let t1 = std::time::Instant::now();
            let mut handle = if crate::pool::snapshot_cache::supports_snapshot_fast_path(config) {
                let snapshot_dir =
                    snapshot_dir_for_config(config).context("resolve snapshot cache path")?;
                vm::boot_vm_with_snapshot(
                    id,
                    &run_config,
                    &rootfs_path,
                    vm::VmBootSpec::new(config.memory_mib, config.vcpus, cid)
                        .with_guest_virtualization(config.guest_virtualization),
                    vm::BootStorage::new(&storage.shared_dirs, &storage.data_disks),
                    &snapshot_dir,
                )
                .context("boot microVM with snapshot save")?
            } else {
                vm::boot_vm(
                    id,
                    &run_config,
                    &rootfs_path,
                    vm::VmBootSpec::new(config.memory_mib, config.vcpus, cid)
                        .with_guest_virtualization(config.guest_virtualization),
                    vm::BootStorage::new(&storage.shared_dirs, &storage.data_disks),
                )
                .context("boot microVM without snapshot save")?
            };
            let boot_ms = t1.elapsed().as_millis();

            let t2 = std::time::Instant::now();
            let port_forward_handle =
                setup_port_forwards(config, &run_config.effective_networks())?;
            let pf_ms = t2.elapsed().as_millis();

            let total_ms = pipeline_start.elapsed().as_millis();
            tracing::info!(
                vm_id = id,
                cid,
                oci_ms,
                boot_ms,
                port_forward_ms = pf_ms,
                total_ms,
                "VM booted (detached)"
            );

            let parts = handle.take_parts();
            Ok(VmLiveState {
                cid,
                thread: parts.thread,
                kill_flag: parts.kill_flag,
                completion_rx: parts.completion_rx,
                serial_output: parts.serial_output,
                tmp_dir,
                port_forward_handle,
            })
        }
    }

    /// Background monitor for detached VMs.
    ///
    /// Awaits the VM's `completion_rx` oneshot and transitions the VM from
    /// `Running` → `Stopped`, capturing serial output and cleaning up
    /// resources. Without this, short-lived containers (e.g. `docker run
    /// alpine echo hello`) would never report exit to the Docker
    /// `attach`/`wait` polling loops.
    async fn monitor_vm_completion(&self, id: &str) {
        // Take completion_rx without removing the entry — stop()/kill()
        // may still need the live state for other fields.
        let rx = {
            let mut live_vms = self.live_vms.write().await;
            live_vms.get_mut(id).and_then(|s| s.completion_rx.take())
        };

        let Some(rx) = rx else {
            tracing::warn!(vm_id = id, "monitor: no completion_rx for detached VM");
            return;
        };

        // Await VM exit — no locks held while blocking.
        let exit_info = if let Ok(info) = rx.await {
            info
        } else {
            tracing::warn!(vm_id = id, "monitor: completion channel dropped");
            crate::vm::VmExitInfo {
                exit_code: 1,
                reason: crate::vm::VmExitReason::Error("completion channel dropped".to_owned()),
            }
        };

        tracing::info!(
            vm_id = id,
            exit_code = exit_info.exit_code,
            reason = %exit_info.reason,
            "detached VM exited"
        );

        // Claim live state for cleanup.  If stop()/kill() already
        // removed it, we just update VmInfo and move on.
        let live = self.live_vms.write().await.remove(id);
        if let Some(mut state) = live {
            let serial_bytes = state.serial_output.as_bytes();
            let stdout = crate::vm::extract_stdout(&serial_bytes);
            let exit_code = crate::vm::parse_exit_code(&serial_bytes);

            {
                let mut vms = self.vms.write().await;
                if let Some(vm) = vms.get_mut(id) {
                    vm.state = VmState::Stopped;
                    vm.exit_code = Some(exit_code);
                    if !stdout.is_empty() {
                        vm.stdout = Some(stdout);
                    }
                }
            }

            // Join vCPU thread.
            if let Some(thread) = state.thread.take() {
                let _ = tokio::task::spawn_blocking(move || thread.join()).await;
            }

            // Clean up port-forwarding rules (RAII drop).
            if let Some(ref pf) = state.port_forward_handle {
                tracing::debug!(
                    vm_id = id,
                    mappings = pf.mapping_count(),
                    "monitor: dropping port-forward rules"
                );
            }
            drop(state.port_forward_handle.take());

            // Clean up temp directory.
            let _ = std::fs::remove_dir_all(&state.tmp_dir);
        } else {
            // stop()/kill() already handled cleanup — just ensure state.
            let mut vms = self.vms.write().await;
            if let Some(vm) = vms.get_mut(id) {
                if vm.state == VmState::Running {
                    vm.state = VmState::Stopped;
                }
            }
        }
    }

    /// Sync pipeline: OCI pull → build rootfs → boot → wait → capture output.
    ///
    /// Returns a [`VmInfo`] with exit code and stdout populated.
    ///
    /// # Errors
    ///
    /// Returns an error if any step fails.
    async fn run_pipeline(&self, id: &str, config: &VmConfig, cid: u32) -> anyhow::Result<VmInfo> {
        use crate::vm;

        let pipeline_start = std::time::Instant::now();

        let t0 = std::time::Instant::now();
        let (run_config, rootfs_path, _image_config, tmp_dir, storage) =
            self.build_run_config(id, config, cid).await?;
        let oci_ms = t0.elapsed().as_millis();

        // Update state to Running before boot.
        {
            let mut vms = self.vms.write().await;
            if let Some(vm) = vms.get_mut(id) {
                vm.state = VmState::Running;
            }
        }

        let t1 = std::time::Instant::now();
        let mut handle = if crate::pool::snapshot_cache::supports_snapshot_fast_path(config) {
            let snapshot_dir =
                snapshot_dir_for_config(config).context("resolve snapshot cache path")?;
            vm::boot_vm_with_snapshot(
                id,
                &run_config,
                &rootfs_path,
                vm::VmBootSpec::new(config.memory_mib, config.vcpus, cid)
                    .with_guest_virtualization(config.guest_virtualization),
                vm::BootStorage::new(&storage.shared_dirs, &storage.data_disks),
                &snapshot_dir,
            )
            .context("boot microVM with snapshot save")?
        } else {
            vm::boot_vm(
                id,
                &run_config,
                &rootfs_path,
                vm::VmBootSpec::new(config.memory_mib, config.vcpus, cid)
                    .with_guest_virtualization(config.guest_virtualization),
                vm::BootStorage::new(&storage.shared_dirs, &storage.data_disks),
            )
            .context("boot microVM without snapshot save")?
        };
        let boot_ms = t1.elapsed().as_millis();

        // Set up port forwarding — handle stays alive until function returns.
        let _port_forward_handle = setup_port_forwards(config, &run_config.effective_networks())?;

        // Take parts so Drop does not join while we await completion.
        let parts = handle.take_parts();
        let exit_info = if let Some(rx) = parts.completion_rx {
            rx.await.unwrap_or(vm::VmExitInfo {
                exit_code: 1,
                reason: vm::VmExitReason::Error("completion channel dropped".to_owned()),
            })
        } else {
            vm::VmExitInfo {
                exit_code: 1,
                reason: vm::VmExitReason::Error("no completion receiver".to_owned()),
            }
        };

        if let Some(thread) = parts.thread {
            let _ = thread.join();
        }

        // Capture output
        let serial_bytes = parts.serial_output.as_bytes();
        let exit_code = vm::parse_exit_code(&serial_bytes);
        let stdout = vm::extract_stdout(&serial_bytes);

        let _ = std::fs::remove_dir_all(&tmp_dir);

        let total_ms = pipeline_start.elapsed().as_millis();
        tracing::info!(
            vm_id = id,
            exit_code,
            reason = %exit_info.reason,
            stdout_len = stdout.len(),
            oci_ms,
            boot_ms,
            total_ms,
            "VM completed"
        );

        let mut info = VmInfo::new(
            id.to_owned(),
            config.image.clone(),
            VmState::Stopped,
            crate::timeutil::utc_now_iso8601(),
            config.memory_mib,
            config.vcpus,
        );
        info.name.clone_from(&config.name);
        info.ports.clone_from(&config.ports);
        info.exit_code = Some(exit_code);
        info.stdout = Some(stdout);
        Ok(info)
    }

    /// OCI pull pipeline: parse ref → pull manifest → download layers → merge → rootfs.
    ///
    /// Returns `(rootfs_path, image_config, tmp_dir)` for the boot phase.
    async fn pull_and_build_rootfs(
        &self,
        id: &str,
        config: &VmConfig,
    ) -> anyhow::Result<(
        std::path::PathBuf,
        crate::oci::config::ImageConfig,
        std::path::PathBuf,
    )> {
        use crate::oci::cache::LayerCache;
        use crate::oci::config::ImageConfig;
        use crate::oci::reference::ImageReference;
        use crate::oci::registry::RegistryClient;

        let pull_start = std::time::Instant::now();
        // 1. Parse image reference
        tracing::debug!(vm_id = id, image = %config.image, "pull: parsing image reference");
        let image_ref = ImageReference::parse(&config.image).context("parse image reference")?;

        let repository = image_ref.repository().as_ref();
        let tag = image_ref.tag().map_or("latest", |t| t.as_ref());

        // 2. Create cache early — needed for manifests, config, and layers
        let cache = LayerCache::new(LayerCache::default_path().context("determine cache path")?)
            .context("create layer cache")?;

        let registry = image_ref.registry().as_ref();

        if let Some(local_image) = load_local_image_into_cache(
            &config.image,
            &self.image_store_path,
            &cache,
            registry,
            repository,
            tag,
        )
        .with_context(|| format!("load local image '{}'", config.image))?
        {
            tracing::debug!(vm_id = id, image = %config.image, "pull: local image store hit");

            let t_rootfs = std::time::Instant::now();
            let (rootfs_path, tmp_dir) = download_and_build_rootfs(
                id,
                &cache,
                registry,
                repository,
                &local_image.manifest,
                &local_image.image_config,
            )
            .await?;
            let rootfs_ms = t_rootfs.elapsed().as_millis();
            let pull_total_ms = pull_start.elapsed().as_millis();
            tracing::info!(
                vm_id = id,
                rootfs_ms,
                pull_total_ms,
                "local OCI image pipeline complete"
            );

            return Ok((rootfs_path, local_image.image_config, tmp_dir));
        }

        // 3. Resolve manifest (cache hit → skip network entirely)
        let t_manifest = std::time::Instant::now();
        let manifest = if let Some(cached) = cache
            .get_manifest(registry, repository, tag)
            .context("check manifest cache")?
        {
            tracing::debug!(vm_id = id, "pull: manifest cache hit");
            serde_json::from_slice(&cached).context("parse cached manifest")?
        } else {
            tracing::debug!(
                vm_id = id,
                "pull: manifest cache miss, fetching from registry"
            );
            let mut client = RegistryClient::new(registry).context("create registry client")?;
            tracing::debug!(vm_id = id, "pull: authenticating with registry");
            client
                .authenticate(repository)
                .await
                .context("authenticate with registry")?;
            tracing::debug!(vm_id = id, "pull: pulling manifest");
            let m = client
                .pull_manifest(repository, tag)
                .await
                .context(format!("pull manifest for '{}'", config.image))?;
            let bytes = serde_json::to_vec(&m).context("serialize manifest for cache")?;
            cache
                .put_manifest(registry, repository, tag, &bytes)
                .context("cache manifest")?;
            m
        };
        let manifest_ms = t_manifest.elapsed().as_millis();

        // 4. Resolve config blob (reuse blob cache — config is content-addressed)
        let t_config = std::time::Instant::now();
        let config_blob = if let Some(cached_path) = cache
            .get(&manifest.config.digest)
            .context("check config blob cache")?
        {
            std::fs::read(&cached_path).context("read cached config blob")?
        } else {
            // Need a client for network fetch (may already exist from manifest miss)
            let mut client =
                RegistryClient::new(registry).context("create registry client for config blob")?;
            client
                .authenticate(repository)
                .await
                .context("authenticate with registry for config blob")?;
            let blob = client
                .pull_blob(repository, &manifest.config.digest)
                .await
                .context("pull image config blob")?;
            cache
                .put(&manifest.config.digest, &blob)
                .context("cache config blob")?;
            blob
        };
        let image_config = ImageConfig::from_json(&config_blob).context("parse image config")?;

        let config_ms = t_config.elapsed().as_millis();

        // 5. Download layers, merge, and build rootfs
        let t_rootfs = std::time::Instant::now();
        let (rootfs_path, tmp_dir) =
            download_and_build_rootfs(id, &cache, registry, repository, &manifest, &image_config)
                .await?;
        let rootfs_ms = t_rootfs.elapsed().as_millis();

        let pull_total_ms = pull_start.elapsed().as_millis();
        tracing::info!(
            vm_id = id,
            manifest_ms,
            config_ms,
            rootfs_ms,
            pull_total_ms,
            "OCI pull pipeline complete"
        );

        Ok((rootfs_path, image_config, tmp_dir))
    }
}

#[derive(Debug)]
struct LocalResolvedImage {
    manifest: crate::oci::registry::Manifest,
    image_config: crate::oci::config::ImageConfig,
}

fn default_image_store_path() -> PathBuf {
    crate::paths::best_effort_persistent_subdir("images")
}

fn load_local_image_into_cache(
    image: &str,
    store_dir: &Path,
    cache: &crate::oci::cache::LayerCache,
    registry: &str,
    repository: &str,
    tag: &str,
) -> anyhow::Result<Option<LocalResolvedImage>> {
    let store = visor_build::ImageStore::new(store_dir.to_path_buf());
    let manifest_digest = store
        .get_by_tag(image)
        .with_context(|| format!("read local image tag {image}"))?;

    if let Some(manifest_digest) = manifest_digest {
        match load_store_layout_image_into_cache(
            store_dir,
            cache,
            registry,
            repository,
            tag,
            &manifest_digest,
        ) {
            Ok(resolved) => return Ok(Some(resolved)),
            Err(error) => {
                if let Some(cached) =
                    load_cached_image_from_cache(cache, registry, repository, tag)?
                {
                    tracing::debug!(
                        image,
                        error = %error,
                        "falling back to cached manifest for local image"
                    );
                    return Ok(Some(cached));
                }
                return Err(error);
            }
        }
    }

    if let Some(cached) = load_cached_image_from_cache(cache, registry, repository, tag)? {
        return Ok(Some(cached));
    }

    Ok(None)
}

fn load_store_layout_image_into_cache(
    store_dir: &Path,
    cache: &crate::oci::cache::LayerCache,
    registry: &str,
    repository: &str,
    tag: &str,
    manifest_digest: &str,
) -> anyhow::Result<LocalResolvedImage> {
    let image_dir = store_dir.join(
        manifest_digest
            .strip_prefix("sha256:")
            .unwrap_or(manifest_digest),
    );
    let manifest_path = oci_blob_path(&image_dir, manifest_digest);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read local manifest {}", manifest_path.display()))?;
    let manifest: crate::oci::registry::Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse local OCI manifest")?;
    cache
        .put_manifest(registry, repository, tag, &manifest_bytes)
        .context("cache local manifest")?;

    let config_path = oci_blob_path(&image_dir, &manifest.config.digest);
    let config_bytes = std::fs::read(&config_path)
        .with_context(|| format!("read local image config {}", config_path.display()))?;
    cache
        .put_from_file(&manifest.config.digest, &config_path)
        .with_context(|| format!("cache local config blob {}", manifest.config.digest))?;

    for layer in &manifest.layers {
        let layer_path = oci_blob_path(&image_dir, &layer.digest);
        cache
            .put_from_file(&layer.digest, &layer_path)
            .with_context(|| format!("cache local layer {}", layer.digest))?;
    }

    let image_config = crate::oci::config::ImageConfig::from_json(&config_bytes)
        .context("parse local image config")?;

    Ok(LocalResolvedImage {
        manifest,
        image_config,
    })
}

fn load_cached_image_from_cache(
    cache: &crate::oci::cache::LayerCache,
    registry: &str,
    repository: &str,
    tag: &str,
) -> anyhow::Result<Option<LocalResolvedImage>> {
    let Some(manifest_bytes) = cache
        .get_manifest(registry, repository, tag)
        .with_context(|| format!("read cached manifest for {registry}/{repository}:{tag}"))?
    else {
        return Ok(None);
    };

    let manifest: crate::oci::registry::Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse cached OCI manifest")?;

    let config_path = cache
        .get(&manifest.config.digest)
        .with_context(|| format!("read cached config blob {}", manifest.config.digest))?
        .with_context(|| format!("cached config blob {} is missing", manifest.config.digest))?;
    let config_bytes = std::fs::read(&config_path)
        .with_context(|| format!("read cached image config {}", config_path.display()))?;
    let image_config = crate::oci::config::ImageConfig::from_json(&config_bytes)
        .context("parse cached image config")?;

    Ok(Some(LocalResolvedImage {
        manifest,
        image_config,
    }))
}

fn oci_blob_path(image_dir: &Path, digest: &str) -> PathBuf {
    image_dir
        .join("blobs")
        .join("sha256")
        .join(digest.strip_prefix("sha256:").unwrap_or(digest))
}

fn guest_network_configs_for_vm(
    config: &VmConfig,
    cid: u32,
) -> Vec<visor_init::config::NetworkConfig> {
    if !config.network_enabled && config.ports.is_empty() && config.service_ports.is_empty() {
        return Vec::new();
    }

    let needs_host_access_network = !config.ports.is_empty() || !config.service_ports.is_empty();

    if config.networks.is_empty() {
        return vec![default_guest_network_for_cid(cid)];
    }

    let mut networks = Vec::with_capacity(config.networks.len() + usize::from(needs_host_access_network));

    if needs_host_access_network {
        networks.push(default_guest_network_for_cid(cid));
    }

    networks.extend(config.networks.iter().enumerate().map(|(index, network_name)| {
        let interface_index = index + usize::from(needs_host_access_network);
        named_guest_network_for_name(network_name, cid, interface_index)
    }));

    networks
}

fn guest_dns_servers() -> Vec<String> {
    guest_dns_servers_from_paths(&[
        std::path::Path::new("/etc/resolv.conf"),
        std::path::Path::new("/run/systemd/resolve/resolv.conf"),
        std::path::Path::new("/lib/systemd/resolv.conf"),
    ])
}

fn guest_dns_servers_from_paths(paths: &[&std::path::Path]) -> Vec<String> {
    for path in paths {
        let Some(contents) = std::fs::read_to_string(path).ok() else {
            continue;
        };
        let servers = parse_guest_dns_servers(&contents);
        if !servers.is_empty() {
            return servers;
        }
    }

    fallback_guest_dns_servers()
}

fn parse_guest_dns_servers(contents: &str) -> Vec<String> {
    let mut servers = Vec::new();

    for line in contents.lines() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("nameserver") {
            continue;
        }

        let Some(raw_ip) = parts.next() else {
            continue;
        };

        let Ok(ip) = raw_ip.parse::<std::net::IpAddr>() else {
            continue;
        };

        let std::net::IpAddr::V4(ipv4) = ip else {
            continue;
        };

        if ipv4.is_loopback() || ipv4 == std::net::Ipv4Addr::UNSPECIFIED {
            continue;
        }

        let server = ipv4.to_string();
        if !servers.contains(&server) {
            servers.push(server);
        }
    }

    servers
}

fn fallback_guest_dns_servers() -> Vec<String> {
    vec!["1.1.1.1".to_owned(), "8.8.8.8".to_owned()]
}

fn guest_dns_servers_for_gateway(gateway: Ipv4Addr) -> Vec<String> {
    let gateway = gateway.to_string();
    let mut servers = vec![gateway.clone()];
    for server in guest_dns_servers() {
        if server != gateway {
            servers.push(server);
        }
    }
    servers
}

fn encode_guest_archive(archive: &[u8]) -> anyhow::Result<String> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder
        .write_all(archive)
        .context("write tar archive into gzip encoder")?;
    let compressed = encoder
        .finish()
        .context("finalize guest archive gzip stream")?;
    Ok(base64::engine::general_purpose::STANDARD.encode(compressed))
}

fn resolve_run_command(
    config: &VmConfig,
    image_config: &crate::oci::config::ImageConfig,
) -> Vec<String> {
    let entrypoint = if config.entrypoint.is_empty() {
        image_config.entrypoint.as_ref()
    } else {
        Some(&config.entrypoint)
    };

    let cmd = if config.cmd.is_empty() {
        image_config.cmd.as_ref()
    } else {
        Some(&config.cmd)
    };

    match (entrypoint, cmd) {
        (Some(entrypoint), Some(cmd)) => {
            let mut effective = entrypoint.clone();
            effective.extend(cmd.iter().cloned());
            effective
        }
        (Some(entrypoint), None) => entrypoint.clone(),
        (None, Some(cmd)) => cmd.clone(),
        (None, None) => Vec::new(),
    }
}

fn default_guest_network_for_cid(cid: u32) -> visor_init::config::NetworkConfig {
    let link = GuestNetworkLink::for_cid(cid);

    let mut network = visor_init::config::NetworkConfig::default();
    network.interface = Some("eth0".to_owned());
    network.address = link.guest_ip.to_string();
    network.netmask = link.netmask.to_string();
    network.gateway = link.gateway_ip.to_string();
    network.dns_servers = guest_dns_servers_for_gateway(link.gateway_ip);
    network.default_route = true;
    network
}

fn named_guest_network_for_name(
    network_name: &str,
    cid: u32,
    index: usize,
) -> visor_init::config::NetworkConfig {
    let link = GuestNetworkLink::for_named_network(network_name, cid);

    let mut network = visor_init::config::NetworkConfig::default();
    network.name = Some(network_name.to_owned());
    network.interface = Some(format!("eth{index}"));
    network.address = link.guest_ip.to_string();
    network.netmask = link.netmask.to_string();
    network.gateway = link.gateway_ip.to_string();
    network.dns_servers = guest_dns_servers_for_gateway(link.gateway_ip);
    network.default_route = index == 0;
    network
}

fn guest_extra_hosts(config: &VmConfig) -> Vec<visor_init::config::HostEntry> {
    config
        .extra_hosts
        .iter()
        .map(|entry| visor_init::config::HostEntry::new(&entry.hostname, &entry.address))
        .collect()
}

fn snapshot_dir_for_config(config: &VmConfig) -> anyhow::Result<PathBuf> {
    let snapshot_key = crate::pool::snapshot_cache::snapshot_key_for_config(config)
        .context("build snapshot cache key")?;
    let snapshot_cache_dir = crate::pool::snapshot_cache::SnapshotCache::default_dir()
        .context("determine snapshot cache directory")?;
    Ok(
        crate::pool::snapshot_cache::SnapshotCache::new(snapshot_cache_dir)
            .snapshot_dir(&snapshot_key),
    )
}

fn fallback_vm_config_for_info(info: &VmInfo) -> VmConfig {
    let mut config = VmConfig::new(info.image.clone());
    config.memory_mib = info.memory_mib;
    config.vcpus = info.vcpus;
    config.name.clone_from(&info.name);
    config.ports.clone_from(&info.ports);
    config.detach = true;
    config
}

fn resolve_vm_storage(
    volumes: &[VolumeMount],
    staging_root: &Path,
) -> anyhow::Result<ResolvedVmStorage> {
    let mut storage = ResolvedVmStorage::default();
    let staged_volume_dir = staging_root.join("volumes");
    std::fs::create_dir_all(&staged_volume_dir).with_context(|| {
        format!(
            "create staged volume directory {}",
            staged_volume_dir.display()
        )
    })?;
    let mut data_disk_index = 0usize;

    for volume in volumes {
        let host_path = PathBuf::from(&volume.host_path);
        let metadata = std::fs::metadata(&host_path)
            .with_context(|| format!("inspect volume host path {}", host_path.display()))?;

        let mut guest_volume = visor_init::config::VolumeConfig::default();
        guest_volume.host_path.clone_from(&volume.host_path);
        guest_volume.guest_path.clone_from(&volume.guest_path);
        guest_volume.read_only = volume.read_only;

        if metadata.is_dir() {
            anyhow::ensure!(
                volume.read_only,
                "directory volume {} must be mounted read-only until direct shared-fs support lands",
                volume.host_path
            );
            let staged_path =
                stage_directory_volume(&host_path, &staged_volume_dir, data_disk_index)
                    .with_context(|| format!("stage directory volume {}", host_path.display()))?;
            guest_volume.device_path = guest_block_device_path(data_disk_index)?;
            "ext4".clone_into(&mut guest_volume.fs_type);
            storage
                .data_disks
                .push(visor_vmm::vm::DataDiskConfig::new(staged_path, true));
            data_disk_index += 1;
        } else if metadata.is_file() {
            guest_volume.device_path = guest_block_device_path(data_disk_index)?;
            "ext4".clone_into(&mut guest_volume.fs_type);
            storage.data_disks.push(visor_vmm::vm::DataDiskConfig::new(
                host_path,
                volume.read_only,
            ));
            data_disk_index += 1;
        } else {
            anyhow::bail!(
                "volume host path must be a regular file or directory: {}",
                volume.host_path
            );
        }

        storage.guest_volumes.push(guest_volume);
    }

    Ok(storage)
}

fn stage_directory_volume(
    host_path: &Path,
    staged_volume_dir: &Path,
    data_disk_index: usize,
) -> anyhow::Result<PathBuf> {
    let staged_path = staged_volume_dir.join(format!("volume-{data_disk_index}.ext4"));
    crate::oci::rootfs::RootfsBuilder::new(host_path, &staged_path)
        .build()
        .with_context(|| {
            format!(
                "build staged volume image {} from {}",
                staged_path.display(),
                host_path.display()
            )
        })?;
    Ok(staged_path)
}

fn guest_block_device_path(index: usize) -> anyhow::Result<String> {
    let device_index = index
        .checked_add(1)
        .context("guest block device index overflow")?;
    let suffix = u8::try_from(device_index).context("too many data disks for virtio-blk naming")?;
    anyhow::ensure!(suffix <= 25, "too many data disks for virtio-blk naming");
    Ok(format!("/dev/vd{}", char::from(b'b' + suffix - 1)))
}

fn create_vm_temp_dir(id: &str) -> anyhow::Result<PathBuf> {
    let temp_root = visor_temp_root();
    std::fs::create_dir_all(&temp_root)
        .with_context(|| format!("create visor temp root {}", temp_root.display()))?;
    let tmp_dir = temp_root.join(format!("visor-{id}"));
    std::fs::create_dir_all(&tmp_dir)
        .with_context(|| format!("create temp directory {}", tmp_dir.display()))?;
    Ok(tmp_dir)
}

fn visor_temp_root() -> PathBuf {
    let home_dir = std::env::var_os("HOME").map(PathBuf::from);
    let override_dir = std::env::var_os("VISOR_TMPDIR").map(PathBuf::from);
    visor_temp_root_from_env(home_dir.as_deref(), override_dir.as_deref())
}

fn visor_temp_root_from_env(home_dir: Option<&Path>, override_dir: Option<&Path>) -> PathBuf {
    if let Some(override_dir) = override_dir {
        return override_dir.to_path_buf();
    }
    if let Some(home_dir) = home_dir {
        return home_dir.join(".visor").join("tmp");
    }
    std::env::temp_dir()
}

fn build_port_forward_mappings(
    config: &VmConfig,
    guest_ip: Ipv4Addr,
    gateway_ip: Ipv4Addr,
    include_service_ports: bool,
) -> anyhow::Result<Vec<visor_vmm::net::PortMapping>> {
    let mut mappings = config
        .ports
        .iter()
        .map(|port| {
            visor_vmm::net::PortMapping::from_spec(
                &format!("{}:{}/{}", port.host_port, port.guest_port, port.protocol),
                guest_ip,
            )
        })
        .collect::<Result<Vec<_>, _>>()
        .context("invalid external port mapping")?;

    if include_service_ports {
        let service_mappings = config
            .service_ports
            .iter()
            .map(|port| {
                visor_vmm::net::PortMapping::from_spec(
                    &format!("{}:{}/{}", port.port, port.port, port.protocol),
                    guest_ip,
                )
                .map(|mapping| mapping.with_host_ip(gateway_ip))
            })
            .collect::<Result<Vec<_>, _>>()
            .context("invalid internal service route mapping")?;
        mappings.extend(service_mappings);
    }

    Ok(mappings)
}

/// Sets up port-forwarding rules for the given config ports.
///
/// Converts runtime [`PortMapping`] (from visor-types) to VMM `PortMapping`
/// (from `visor_vmm::net`) and calls the platform network backend.
///
/// Returns `None` if no ports are configured.
///
/// # Errors
///
/// Returns an error if a port mapping spec is invalid or the platform
/// backend fails to apply the rules.
pub(crate) fn setup_port_forwards(
    config: &VmConfig,
    guest_networks: &[visor_init::config::NetworkConfig],
) -> anyhow::Result<Option<Box<dyn visor_vmm::net::PortForwardHandle>>> {
    use visor_vmm::net::{NetworkBackend as _, PlatformNetworkBackend};

    if config.ports.is_empty() && config.service_ports.is_empty() {
        return Ok(None);
    }

    let guest_network = guest_networks
        .iter()
        .find(|network| network.default_route)
        .or_else(|| guest_networks.first())
        .context("missing guest network config for port-forwarded VM")?;
    let guest_ip = guest_network
        .address
        .parse()
        .context("parse guest IP for port forwarding")?;
    let gateway_ip = guest_network
        .gateway
        .parse()
        .context("parse gateway IP for port forwarding")?;
    let include_service_ports = guest_network.name.is_none();
    let net_backend = PlatformNetworkBackend::new();
    let vmm_mappings =
        build_port_forward_mappings(config, guest_ip, gateway_ip, include_service_ports)?;
    let handle = net_backend
        .setup_port_forward(&vmm_mappings)
        .context("setup port forwarding")?;
    Ok(Some(Box::new(handle)))
}

/// Downloads OCI layers, merges them, injects visor-init, and builds the ext4 rootfs.
///
/// # Errors
///
/// Returns an error if layer download, merge, or rootfs build fails.
async fn download_and_build_rootfs(
    id: &str,
    cache: &crate::oci::cache::LayerCache,
    registry: &str,
    repository: &str,
    manifest: &crate::oci::registry::Manifest,
    _image_config: &crate::oci::config::ImageConfig,
) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf)> {
    use crate::oci::layers::LayerMerger;
    use crate::oci::registry::RegistryClient;
    use crate::oci::rootfs::RootfsBuilder;

    tracing::debug!(
        vm_id = id,
        layer_count = manifest.layers.len(),
        "pull: downloading layers"
    );
    let tmp_dir = create_vm_temp_dir(id).context("create temp directory for layer merge")?;

    let merged_dir = tmp_dir.join("merged");
    let merger = LayerMerger::new(&merged_dir).context("create layer merger")?;

    // Lazily-created client — only needed when a layer is not cached.
    let mut layer_client: Option<RegistryClient> = None;

    for layer_desc in &manifest.layers {
        if let Some(cached_path) = cache.get(&layer_desc.digest).context("check layer cache")? {
            merger
                .unpack_layer(&cached_path)
                .with_context(|| format!("unpack cached layer {}", layer_desc.digest))?;
            continue;
        }

        let client = if let Some(c) = &mut layer_client {
            c
        } else {
            let mut c =
                RegistryClient::new(registry).context("create registry client for layer pull")?;
            c.authenticate(repository)
                .await
                .context("authenticate with registry for layer pull")?;
            layer_client.insert(c)
        };

        let blob = client
            .pull_blob(repository, &layer_desc.digest)
            .await
            .with_context(|| format!("pull layer {}", layer_desc.digest))?;
        cache
            .put(&layer_desc.digest, &blob)
            .with_context(|| format!("cache layer {}", layer_desc.digest))?;

        let cached_path = cache
            .get(&layer_desc.digest)
            .context("check layer cache after put")?
            .context("layer not found in cache after put")?;
        merger
            .unpack_layer(&cached_path)
            .with_context(|| format!("unpack layer {}", layer_desc.digest))?;
    }

    inject_visor_init(&merged_dir)?;

    // Build ext4 rootfs
    tracing::debug!(vm_id = id, "pull: building ext4 rootfs");
    let rootfs_path = tmp_dir.join("rootfs.ext4");
    RootfsBuilder::new(&merged_dir, &rootfs_path)
        .build()
        .context("build ext4 rootfs image")?;
    tracing::debug!(vm_id = id, rootfs = %rootfs_path.display(), "pull: rootfs complete");

    Ok((rootfs_path, tmp_dir))
}

/// Copy the visor-init binary into the merged rootfs at `/sbin/visor-init`.
///
/// # Errors
///
/// Returns an error if the visor-init binary cannot be located or copied.
fn inject_visor_init(merged_dir: &std::path::Path) -> anyhow::Result<()> {
    let init_src = crate::vm::visor_init_path().context("locate visor-init binary")?;
    let init_dest = merged_dir.join("sbin").join("visor-init");
    if let Some(parent) = init_dest.parent() {
        std::fs::create_dir_all(parent).context("create /sbin in merged rootfs")?;
    }
    std::fs::copy(&init_src, &init_dest).with_context(|| {
        format!(
            "copy visor-init from {} to {}",
            init_src.display(),
            init_dest.display()
        )
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(&init_dest, std::fs::Permissions::from_mode(0o755))
            .context("set visor-init permissions")?;
    }

    Ok(())
}
