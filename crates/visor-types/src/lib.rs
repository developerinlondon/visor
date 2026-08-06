//! Shared types for the visor microVM runtime.
//!
//! This crate contains platform-agnostic data types and the [`ExecutionBackend`]
//! trait used across `visor-runtime` modules (CLI, API, TUI, compose, pool)
//! without requiring a transitive dependency on `visor-vmm`.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};
use utoipa::ToSchema;

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;

// ── Default value helpers ──────────────────────────────────────────

/// Default memory allocation in MiB for a new VM.
const fn default_memory() -> u32 {
    512
}

/// Default number of virtual CPUs for a new VM.
const fn default_vcpus() -> u32 {
    1
}

/// Default networking policy for new VMs.
const fn default_network_enabled() -> bool {
    true
}

/// Default network protocol for port mappings.
fn default_protocol() -> String {
    "tcp".to_owned()
}

/// Default first guest CID assigned by the runtime.
pub const FIRST_GUEST_CID: u32 = 3;

// ── Shared types ───────────────────────────────────────────────────

/// How much hardware virtualization support the guest should see.
///
/// `Standard` is the default portable mode for ordinary workload VMs.
/// `Nested` opt-ins to guest-visible virtualization extensions for builder
/// guests that need `/dev/kvm` on supported platforms.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum GuestVirtualizationMode {
    /// Default guest profile with nested virtualization disabled.
    #[default]
    Standard,
    /// Expose nested virtualization support to the guest where available.
    Nested,
}

/// Configuration for creating a new VM.
///
/// Only `image` is required — all other fields have sensible defaults.
///
/// # Examples
///
/// ```
/// # use serde_json;
/// # use visor_types::VmConfig;
/// let config: VmConfig = serde_json::from_str(r#"{"image": "alpine:latest"}"#).unwrap();
/// assert_eq!(config.memory_mib, 512);
/// assert_eq!(config.vcpus, 1);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct VmConfig {
    /// OCI image reference (e.g. `"alpine:latest"`).
    pub image: String,
    /// Entrypoint override to run inside the VM.
    #[serde(default)]
    pub entrypoint: Vec<String>,
    /// Command to run inside the VM. Defaults to the image's entrypoint.
    #[serde(default)]
    pub cmd: Vec<String>,
    /// Environment variables in `KEY=VALUE` format.
    #[serde(default)]
    pub env: Vec<String>,
    /// Working directory inside the VM.
    pub working_dir: Option<String>,
    /// Memory allocation in MiB (default: 512).
    #[serde(default = "default_memory")]
    pub memory_mib: u32,
    /// Number of virtual CPUs (default: 1).
    #[serde(default = "default_vcpus")]
    pub vcpus: u32,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// Arbitrary metadata labels associated with the VM.
    #[serde(default)]
    pub labels: std::collections::HashMap<String, String>,
    /// Port mappings from host to guest.
    #[serde(default)]
    pub ports: Vec<PortMapping>,
    /// Volume mounts from host to guest.
    #[serde(default)]
    pub volumes: Vec<VolumeMount>,
    /// Additional hostname mappings that should be written inside the guest.
    #[serde(default)]
    pub extra_hosts: Vec<HostEntry>,
    /// Logical network memberships used by Compose and Docker workflows.
    ///
    /// This is control-plane metadata today. It scopes service discovery and
    /// alias injection, even though the guest dataplane is still a single NIC.
    #[serde(default)]
    pub networks: Vec<String>,
    /// DNS names and aliases that should resolve to this VM on virtual networks.
    #[serde(default)]
    pub service_names: Vec<String>,
    /// Guest service ports that should be reachable by peer VMs on virtual networks.
    #[serde(default)]
    pub service_ports: Vec<ServicePort>,
    /// Whether guest networking should be configured for this VM.
    #[serde(default = "default_network_enabled")]
    pub network_enabled: bool,
    /// Guest virtualization profile (`standard` or `nested`).
    #[serde(default)]
    pub guest_virtualization: GuestVirtualizationMode,
    /// When `true`, `create()` returns after boot with the VM still running.
    /// When `false` (default), `create()` runs to completion and returns output.
    #[serde(default)]
    pub detach: bool,
    /// Guest operating mode: `"run"` (default) executes a command,
    /// `"agent"` starts the vsock build agent listener.
    #[serde(default)]
    pub mode: Option<String>,
}

impl VmConfig {
    /// Create a new VM config with just an image reference.
    ///
    /// All other fields use sensible defaults.
    #[must_use]
    pub fn new(image: impl Into<String>) -> Self {
        Self {
            image: image.into(),
            entrypoint: Vec::new(),
            cmd: Vec::new(),
            env: Vec::new(),
            working_dir: None,
            memory_mib: default_memory(),
            vcpus: default_vcpus(),
            name: None,
            labels: std::collections::HashMap::new(),
            ports: Vec::new(),
            volumes: Vec::new(),
            extra_hosts: Vec::new(),
            networks: Vec::new(),
            service_names: Vec::new(),
            service_ports: Vec::new(),
            network_enabled: default_network_enabled(),
            guest_virtualization: GuestVirtualizationMode::Standard,
            detach: false,
            mode: None,
        }
    }
}

/// Static hostname mapping injected into the guest.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostEntry {
    /// Hostname that should resolve inside the guest.
    pub hostname: String,
    /// IPv4 address for the hostname.
    pub address: String,
}

impl HostEntry {
    /// Create a new static host entry.
    #[must_use]
    pub fn new(hostname: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            address: address.into(),
        }
    }
}

/// Guest service port reachable on the VM's virtual network.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema, PartialEq, Eq, PartialOrd, Ord)]
#[non_exhaustive]
pub struct ServicePort {
    /// Guest port number.
    pub port: u16,
    /// Transport protocol (`"tcp"` or `"udp"`).
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

impl ServicePort {
    /// Create a new service port descriptor.
    #[must_use]
    pub fn new(port: u16, protocol: impl Into<String>) -> Self {
        Self {
            port,
            protocol: protocol.into(),
        }
    }
}

/// Maps a host port to a guest port with a protocol.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct PortMapping {
    /// Port on the host.
    pub host_port: u16,
    /// Port inside the guest VM.
    pub guest_port: u16,
    /// Network protocol (default: `"tcp"`).
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

impl PortMapping {
    /// Create a new TCP port mapping.
    #[must_use]
    pub fn new(host_port: u16, guest_port: u16) -> Self {
        Self {
            host_port,
            guest_port,
            protocol: default_protocol(),
        }
    }

    /// Create a port mapping with a specific protocol.
    #[must_use]
    pub fn with_protocol(host_port: u16, guest_port: u16, protocol: impl Into<String>) -> Self {
        Self {
            host_port,
            guest_port,
            protocol: protocol.into(),
        }
    }
}

/// Mounts a host directory into the guest VM.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct VolumeMount {
    /// Absolute path on the host.
    pub host_path: String,
    /// Mount point inside the guest VM.
    pub guest_path: String,
    /// Whether the mount is read-only (default: `false`).
    #[serde(default)]
    pub read_only: bool,
}

impl VolumeMount {
    /// Create a read-write volume mount.
    #[must_use]
    pub fn new(host_path: impl Into<String>, guest_path: impl Into<String>) -> Self {
        Self {
            host_path: host_path.into(),
            guest_path: guest_path.into(),
            read_only: false,
        }
    }

    /// Create a read-only volume mount.
    #[must_use]
    pub fn read_only(host_path: impl Into<String>, guest_path: impl Into<String>) -> Self {
        Self {
            host_path: host_path.into(),
            guest_path: guest_path.into(),
            read_only: true,
        }
    }
}

/// Deterministic point-to-point guest network allocation for a VM CID.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct GuestNetworkLink {
    /// Guest IPv4 address assigned inside the VM.
    pub guest_ip: std::net::Ipv4Addr,
    /// Host-side IPv4 gateway exposed to the guest.
    pub gateway_ip: std::net::Ipv4Addr,
    /// Guest interface netmask.
    pub netmask: std::net::Ipv4Addr,
}

impl GuestNetworkLink {
    /// Derive the default point-to-point link for a guest CID.
    #[must_use]
    pub fn for_cid(cid: u32) -> Self {
        let index = cid.saturating_sub(FIRST_GUEST_CID);
        let third_octet = (index / 64).to_le_bytes()[0];
        let fourth_octet = ((index % 64) * 4).to_le_bytes()[0];
        Self {
            guest_ip: std::net::Ipv4Addr::new(172, 20, third_octet, fourth_octet + 2),
            gateway_ip: std::net::Ipv4Addr::new(172, 20, third_octet, fourth_octet + 1),
            netmask: std::net::Ipv4Addr::new(255, 255, 255, 252),
        }
    }

    /// Derive a deterministic shared-network link for a logical network name.
    ///
    /// Logical networks use a routed `/24` subnet shared across guests that
    /// declare the same network name, while the guest host octet remains tied
    /// to the active guest CID.
    #[must_use]
    pub fn for_named_network(network_name: &str, cid: u32) -> Self {
        let hash = fnv1a64(network_name.as_bytes());
        let second_octet = 64_u8.saturating_add(((hash >> 8) & 0x3f) as u8);
        let third_octet = (hash & 0xff) as u8;
        let host_octet = 2_u8.saturating_add((cid.saturating_sub(FIRST_GUEST_CID) % 253) as u8);

        Self {
            guest_ip: std::net::Ipv4Addr::new(100, second_octet, third_octet, host_octet),
            gateway_ip: std::net::Ipv4Addr::new(100, second_octet, third_octet, 1),
            netmask: std::net::Ipv4Addr::new(255, 255, 255, 0),
        }
    }
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

/// Runtime information about a VM instance.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct VmInfo {
    /// Unique VM identifier (UUID v4).
    pub id: String,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// OCI image the VM was created from.
    pub image: String,
    /// Current lifecycle state.
    pub state: VmState,
    /// ISO 8601 timestamp of when the VM was created.
    pub created_at: String,
    /// Memory allocation in MiB.
    pub memory_mib: u32,
    /// Number of virtual CPUs.
    pub vcpus: u32,
    /// Active port mappings.
    pub ports: Vec<PortMapping>,
    /// Exit code if the VM has stopped.
    pub exit_code: Option<i32>,
    /// Captured stdout from the VM (only populated for completed VMs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stdout: Option<String>,
    /// Captured stderr from the VM (only populated for completed VMs).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stderr: Option<String>,
    /// Vsock context ID (CID) assigned to this VM, if running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cid: Option<u32>,
    /// Initial command to execute when the container starts.
    /// Consumed by the first attach or start call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_cmd: Option<Vec<String>>,
    /// Initial environment for the initial command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_env: Option<Vec<String>>,
    /// Initial working directory for the initial command.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initial_working_dir: Option<String>,
}

impl VmInfo {
    /// Create a new `VmInfo` with required fields. Optional fields default to `None`.
    #[must_use]
    pub fn new(
        id: String,
        image: String,
        state: VmState,
        created_at: String,
        memory_mib: u32,
        vcpus: u32,
    ) -> Self {
        Self {
            id,
            name: None,
            image,
            state,
            created_at,
            memory_mib,
            vcpus,
            ports: Vec::new(),
            exit_code: None,
            stdout: None,
            stderr: None,
            cid: None,
            initial_cmd: None,
            initial_env: None,
            initial_working_dir: None,
        }
    }
}
/// Lifecycle state of a VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum VmState {
    /// VM is being created (pulling image, building rootfs, etc.).
    #[default]
    Creating,
    /// VM is running and accepting exec requests.
    Running,
    /// VM has been stopped (gracefully or by request).
    Stopped,
    /// VM failed to start or crashed.
    Failed,
}

/// Request to execute a command inside a running VM.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct ExecRequest {
    /// Command and arguments to execute.
    pub cmd: Vec<String>,
    /// Additional environment variables in `KEY=VALUE` format.
    #[serde(default)]
    pub env: Vec<String>,
    /// Working directory for the command.
    pub working_dir: Option<String>,
    /// Whether the command should run with terminal semantics.
    #[serde(default)]
    pub tty: bool,
}

impl ExecRequest {
    /// Create a new exec request.
    #[must_use]
    pub fn new(cmd: Vec<String>) -> Self {
        Self {
            cmd,
            env: Vec::new(),
            working_dir: None,
            tty: false,
        }
    }
}

/// Result of executing a command inside a VM.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct ExecResult {
    /// Process exit code.
    pub exit_code: i32,
    /// Captured standard output.
    pub stdout: String,
    /// Captured standard error.
    pub stderr: String,
}

impl ExecResult {
    /// Create a new exec result.
    #[must_use]
    pub fn new(exit_code: i32, stdout: String, stderr: String) -> Self {
        Self {
            exit_code,
            stdout,
            stderr,
        }
    }
}

/// Async bidirectional byte stream used for interactive backend operations.
///
/// This trait keeps stream-capable features, such as Docker-style hijacked
/// exec sessions, behind the shared backend abstraction without coupling
/// higher layers to a specific transport implementation.
pub trait AsyncIoStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> AsyncIoStream for T {}

// ── ExecutionBackend trait ──────────────────────────────────────────

/// Trait for VM lifecycle management.
///
/// Implementations handle creating, listing, inspecting, executing commands in,
/// stopping, and destroying VMs.
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// Create a new VM from the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM cannot be created (e.g. invalid config,
    /// resource exhaustion).
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo>;

    /// Create a VM from a pre-saved snapshot for fast restore.
    ///
    /// Implementations that support snapshot-based restore can skip the
    /// OCI pull pipeline and boot from a memory/CPU snapshot instead.
    ///
    /// The default implementation ignores the snapshot directory and
    /// falls back to [`create`](Self::create).
    ///
    /// # Errors
    ///
    /// Returns an error if snapshot restore or VM creation fails.
    async fn create_from_snapshot(
        &self,
        config: VmConfig,
        snapshot_dir: &std::path::Path,
    ) -> anyhow::Result<VmInfo> {
        let _ = snapshot_dir;
        self.create(config).await
    }

    /// List all known VMs.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend state cannot be read.
    async fn list(&self) -> anyhow::Result<Vec<VmInfo>>;

    /// Get information about a specific VM.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found.
    async fn get(&self, id: &str) -> anyhow::Result<VmInfo>;

    /// Execute a command inside a running VM.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found, not running, or the command fails.
    async fn exec(&self, id: &str, req: ExecRequest) -> anyhow::Result<ExecResult>;

    /// Start a streaming exec session inside a running VM.
    ///
    /// Backends that support interactive or hijacked exec can override this to
    /// return a bidirectional byte stream connected to the guest command's
    /// stdin/stdout/stderr transport. The default implementation reports that
    /// streaming exec is unsupported.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend does not support streaming exec, if the
    /// VM is not found, or if the streaming session cannot be established.
    async fn exec_stream(
        &self,
        id: &str,
        req: ExecRequest,
    ) -> anyhow::Result<Box<dyn AsyncIoStream>> {
        let _ = (id, req);
        anyhow::bail!("streaming exec not supported by this backend");
    }

    /// Copy a tar archive into a running VM at the given destination path.
    ///
    /// This is the shared backend seam for Docker-compatible archive upload
    /// flows such as `PUT /containers/{id}/archive`.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend does not support guest file copy, if the
    /// VM is not found, or if the archive cannot be transferred to the guest.
    async fn copy_to_guest(&self, id: &str, archive: Vec<u8>, dest: &str) -> anyhow::Result<()> {
        let _ = (id, archive, dest);
        anyhow::bail!("guest file copy not supported by this backend");
    }

    /// Stop a running VM with a grace period.
    ///
    /// Attempts graceful shutdown via vsock, then waits up to `timeout_secs`
    /// for the VM to exit. If the grace period expires (or is 0), sets the
    /// kill flag to force-stop the vCPU.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found or cannot be stopped.
    async fn stop(&self, id: &str, timeout_secs: u64) -> anyhow::Result<()>;

    /// Start a previously stopped or failed VM again.
    ///
    /// Backends that keep full VM configuration may override this to reboot an
    /// existing VM identity and return its updated runtime info. The default
    /// implementation reports that lifecycle restart is unsupported.
    ///
    /// # Errors
    ///
    /// Returns an error if the backend does not support restarting existing
    /// VMs, if the VM is not found, or if the boot path fails.
    async fn start(&self, id: &str) -> anyhow::Result<VmInfo> {
        let _ = id;
        anyhow::bail!("starting existing VMs is not supported by this backend");
    }

    /// Force-kill a running VM immediately (no graceful shutdown).
    ///
    /// Sets the vCPU kill flag and waits for the thread to exit.
    /// Unlike [`stop`], this skips the vsock shutdown attempt entirely.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found.
    async fn kill(&self, id: &str) -> anyhow::Result<()>;

    /// Destroy a VM, removing all associated resources.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found or cannot be destroyed.
    async fn destroy(&self, id: &str) -> anyhow::Result<()>;

    /// Returns the current console (serial) output for a running VM.
    ///
    /// The returned bytes include all serial output accumulated since boot.
    /// Callers can track their read position and request new data by
    /// comparing with previous lengths.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found or has no live state.
    async fn console_output(&self, id: &str) -> anyhow::Result<Vec<u8>>;

    /// Take the initial command from a VM (consumes it so it's only sent once).
    ///
    /// Returns `None` if the VM has no initial command or it was already consumed.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM is not found.
    async fn take_initial_cmd(&self, _id: &str) -> anyhow::Result<Option<ExecRequest>> {
        Ok(None)
    }
}

// ── Image Management ──────────────────────────────────────────────

/// Metadata about a cached or pulled image.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ImageInfo {
    /// Image ID, typically a `sha256:` digest.
    pub id: String,
    /// Tags associated with the image.
    pub repo_tags: Vec<String>,
    /// Creation timestamp as a Unix epoch in seconds.
    pub created: i64,
    /// Total size in bytes.
    pub size: u64,
    /// Arbitrary image labels.
    pub labels: std::collections::HashMap<String, String>,
    /// Target operating system.
    pub os: String,
    /// Target CPU architecture.
    pub architecture: String,
}

impl ImageInfo {
    /// Create a new image record with Docker-compatible defaults.
    #[must_use]
    pub fn new(id: impl Into<String>, repo_tags: Vec<String>) -> Self {
        Self {
            id: id.into(),
            repo_tags,
            created: 0,
            size: 0,
            labels: std::collections::HashMap::new(),
            os: "linux".to_owned(),
            architecture: std::env::consts::ARCH.to_owned(),
        }
    }
}

/// Trait for image lifecycle operations used by Docker-compatible endpoints.
#[async_trait]
pub trait ImageManager: Send + Sync {
    /// List all known images.
    ///
    /// # Errors
    ///
    /// Returns an error if image metadata cannot be read.
    async fn list_images(&self) -> anyhow::Result<Vec<ImageInfo>>;

    /// Pull or otherwise make an image reference available.
    ///
    /// # Errors
    ///
    /// Returns an error if the image cannot be fetched or cached.
    async fn pull_image(&self, reference: &str) -> anyhow::Result<ImageInfo>;

    /// Inspect a single image by reference.
    ///
    /// # Errors
    ///
    /// Returns an error if the image is unknown.
    async fn inspect_image(&self, reference: &str) -> anyhow::Result<ImageInfo>;

    /// Remove an image reference.
    ///
    /// # Errors
    ///
    /// Returns an error if the image cannot be removed.
    async fn remove_image(&self, reference: &str) -> anyhow::Result<()>;
}

// ── Build Service ─────────────────────────────────────────────────

/// Request to build an OCI image from a Dockerfile.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BuildRequest {
    /// Raw Dockerfile content.
    pub dockerfile_content: String,
    /// Build context directory on the host.
    pub context_dir: std::path::PathBuf,
    /// Build arguments.
    pub build_args: std::collections::HashMap<String, String>,
    /// Target stage name (`--target`).
    pub target: Option<String>,
    /// Skip cache.
    pub no_cache: bool,
    /// Image tag to apply.
    pub tag: Option<String>,
}

impl BuildRequest {
    /// Create a new build request with just the Dockerfile content.
    ///
    /// All other fields use sensible defaults.
    #[must_use]
    pub fn new(dockerfile_content: String) -> Self {
        Self {
            dockerfile_content,
            context_dir: std::path::PathBuf::new(),
            build_args: std::collections::HashMap::new(),
            target: None,
            no_cache: false,
            tag: None,
        }
    }
}

/// Progress update from a build step.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BuildProgress {
    /// Step number (1-indexed).
    pub step: usize,
    /// Total steps.
    pub total: usize,
    /// Instruction text.
    pub instruction: String,
    /// Whether this step was cached.
    pub cached: bool,
    /// Command output (stdout) if any.
    pub output: Option<String>,
}

impl BuildProgress {
    /// Create a new build progress update.
    #[must_use]
    pub fn new(step: usize, total: usize, instruction: String) -> Self {
        Self {
            step,
            total,
            instruction,
            cached: false,
            output: None,
        }
    }
}

/// Result of a successful image build.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BuildOutput {
    /// Image ID (digest).
    pub image_id: String,
    /// Build progress steps.
    pub steps: Vec<BuildProgress>,
}

impl BuildOutput {
    /// Create a new build output.
    #[must_use]
    pub fn new(image_id: String, steps: Vec<BuildProgress>) -> Self {
        Self { image_id, steps }
    }
}

/// Service for building OCI images from Dockerfiles.
///
/// # Errors
///
/// Implementations return errors if the build fails at any step
/// (parsing, VM boot, instruction execution, image assembly).
#[async_trait]
pub trait BuildService: Send + Sync {
    /// Build an OCI image from a Dockerfile and context.
    ///
    /// # Errors
    ///
    /// Returns an error if the build cannot be completed.
    async fn build_image(&self, request: BuildRequest) -> anyhow::Result<BuildOutput>;
}
