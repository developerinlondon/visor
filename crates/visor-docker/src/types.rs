//! Docker Engine API request and response types.
//!
//! These structs match the Docker Engine API v1.45 JSON schema.
//! Only fields that visor actually uses are included — unknown fields
//! from clients are silently ignored via `#[serde(default)]`.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;

// ── Version & System ────────────────────────────────────────────

/// Response for `GET /_ping`.
///
/// Docker returns a plain text "OK" with version headers.
/// The actual version info is in the response headers, not body.
pub const PING_RESPONSE: &str = "OK";

/// Response for `GET /version`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct VersionResponse {
    /// Docker Engine API version (e.g. `"1.45"`).
    pub api_version: String,
    /// Server version string.
    pub version: String,
    /// Minimum API version the server supports.
    pub min_a_p_i_version: String,
    /// Git commit hash.
    pub git_commit: String,
    /// Go version (we report Rust version).
    pub go_version: String,
    /// Host OS.
    pub os: String,
    /// Host architecture.
    pub arch: String,
    /// Kernel version.
    pub kernel_version: String,
    /// Build time.
    pub build_time: String,
}

/// Response for `GET /info`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct InfoResponse {
    /// Number of containers total.
    pub containers: u64,
    /// Number of running containers.
    pub containers_running: u64,
    /// Number of stopped containers.
    pub containers_stopped: u64,
    /// Number of images.
    pub images: u64,
    /// Server version.
    pub server_version: String,
    /// Host OS type (always `"linux"` for Docker compat).
    pub os_type: String,
    /// Host architecture.
    pub architecture: String,
    /// Name of the server.
    pub name: String,
    /// Runtime driver.
    pub driver: String,
    /// Memory limit support.
    pub mem_total: u64,
}

// ── Container Types ─────────────────────────────────────────────

/// Request body for `POST /containers/create`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ContainerCreateRequest {
    /// OCI image reference.
    pub image: String,
    /// Command to run.
    pub cmd: Option<Vec<String>>,
    /// Entrypoint override.
    pub entrypoint: Option<Vec<String>>,
    /// Environment variables as `["KEY=VALUE"]`.
    pub env: Option<Vec<String>>,
    /// Working directory inside the container.
    pub working_dir: Option<String>,
    /// Allocate a pseudo-TTY.
    pub tty: Option<bool>,
    /// Attach to stdin.
    pub open_stdin: Option<bool>,
    /// Attach to stdout.
    pub attach_stdout: Option<bool>,
    /// Attach to stderr.
    pub attach_stderr: Option<bool>,
    /// User inside the container.
    pub user: Option<String>,
    /// Labels as key-value pairs.
    pub labels: Option<HashMap<String, String>>,
    /// Exposed ports (Docker format: `{"80/tcp": {}}`).
    pub exposed_ports: Option<HashMap<String, serde_json::Value>>,
    /// Host configuration (port bindings, volumes, etc.).
    pub host_config: Option<HostConfig>,
    /// Network configuration.
    pub networking_config: Option<NetworkingConfig>,
    /// Stop timeout in seconds.
    pub stop_timeout: Option<u64>,
    /// Hostname for the container.
    pub hostname: Option<String>,
    /// Domain name.
    pub domainname: Option<String>,
}

/// Host-side configuration for a container.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct HostConfig {
    /// Port bindings: `{"80/tcp": [{"HostPort": "8080"}]}`.
    pub port_bindings: Option<HashMap<String, Vec<PortBinding>>>,
    /// Bind mounts: `["/host/path:/container/path:ro"]`.
    pub binds: Option<Vec<String>>,
    /// Memory limit in bytes.
    pub memory: Option<u64>,
    /// CPU count.
    pub nano_cpus: Option<u64>,
    /// Restart policy.
    pub restart_policy: Option<RestartPolicy>,
    /// Network mode (e.g. `"bridge"`, `"host"`).
    pub network_mode: Option<String>,
    /// Auto-remove container on exit.
    pub auto_remove: Option<bool>,
}

/// A single port binding entry.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct PortBinding {
    /// Host IP to bind (e.g. `"0.0.0.0"`).
    pub host_ip: Option<String>,
    /// Host port as string (e.g. `"8080"`).
    pub host_port: Option<String>,
}

/// Container restart policy.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct RestartPolicy {
    /// Policy name: `""`, `"always"`, `"on-failure"`, `"unless-stopped"`.
    pub name: String,
    /// Max retry count for `"on-failure"`.
    pub maximum_retry_count: u32,
}

/// Network configuration for container create.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct NetworkingConfig {
    /// Per-network endpoint settings.
    pub endpoints_config: Option<HashMap<String, EndpointConfig>>,
}

/// Endpoint configuration for a specific network.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct EndpointConfig {
    /// Network aliases for DNS.
    pub aliases: Option<Vec<String>>,
}

/// Response for `POST /containers/create`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerCreateResponse {
    /// Created container ID.
    pub id: String,
    /// Warnings from creation.
    pub warnings: Vec<String>,
}

/// One entry in the `GET /containers/json` response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerListEntry {
    /// Container ID.
    pub id: String,
    /// Container names (Docker prefixes with `/`).
    pub names: Vec<String>,
    /// Image name.
    pub image: String,
    /// Image ID.
    pub image_i_d: String,
    /// Human-readable command.
    pub command: String,
    /// Unix timestamp of creation.
    pub created: i64,
    /// Container state (`"running"`, `"exited"`, etc.).
    pub state: String,
    /// Human-readable status (e.g. `"Up 5 minutes"`).
    pub status: String,
    /// Port mappings.
    pub ports: Vec<ContainerPort>,
    /// Labels.
    pub labels: HashMap<String, String>,
}

/// Port entry in container listing.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerPort {
    /// Private (container) port.
    pub private_port: u16,
    /// Public (host) port.
    pub public_port: Option<u16>,
    /// Protocol (`"tcp"` or `"udp"`).
    #[serde(rename = "Type")]
    pub port_type: String,
}

/// Response for `GET /containers/{id}/json`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerInspectResponse {
    /// Container ID.
    pub id: String,
    /// Container name (prefixed with `/`).
    pub name: String,
    /// Creation timestamp (ISO 8601).
    pub created: String,
    /// Container state.
    pub state: ContainerState,
    /// Container config (image, cmd, env, etc.).
    pub config: ContainerConfig,
    /// Host config.
    pub host_config: HostConfigResponse,
    /// Network settings.
    pub network_settings: NetworkSettings,
    /// Mounts.
    pub mounts: Vec<MountPoint>,
}

/// Container lifecycle state.
#[allow(clippy::struct_excessive_bools)] // Docker API requires these boolean fields
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerState {
    /// Status string: `"created"`, `"running"`, `"exited"`, etc.
    pub status: String,
    /// Whether the container is running.
    pub running: bool,
    /// Whether the container is paused.
    pub paused: bool,
    /// Whether the container is restarting.
    pub restarting: bool,
    /// Whether the container process has exited.
    #[serde(rename = "OOMKilled")]
    pub oom_killed: bool,
    /// Whether the container is dead.
    pub dead: bool,
    /// PID of the main process (0 if not running).
    pub pid: u64,
    /// Exit code of the main process.
    pub exit_code: i32,
    /// Error message if the container failed.
    pub error: String,
    /// When the container started (ISO 8601).
    pub started_at: String,
    /// When the container finished (ISO 8601).
    pub finished_at: String,
    /// Health check state (omitted when no health check is configured).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub health: Option<HealthState>,
}

/// Health check state for a container.
///
/// Matches Docker Engine API `Health` object. Populated when the
/// container is running or starting; `None` when stopped/exited.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct HealthState {
    /// Status: `"none"`, `"starting"`, `"healthy"`, or `"unhealthy"`.
    pub status: String,
    /// Number of consecutive health check failures.
    pub failing_streak: u32,
    /// Recent health check results.
    pub log: Vec<HealthLogEntry>,
}

/// A single health check log entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct HealthLogEntry {
    /// When the check started (ISO 8601).
    pub start: String,
    /// When the check ended (ISO 8601).
    pub end: String,
    /// Exit code of the health check command.
    pub exit_code: i32,
    /// Output from the health check.
    pub output: String,
}

/// Container configuration stored at creation time.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerConfig {
    /// Image reference.
    pub image: String,
    /// Command.
    pub cmd: Option<Vec<String>>,
    /// Environment variables.
    pub env: Option<Vec<String>>,
    /// Working directory.
    pub working_dir: String,
    /// Labels.
    pub labels: HashMap<String, String>,
}

/// Host config portion of inspect response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct HostConfigResponse {
    /// Port bindings.
    pub port_bindings: HashMap<String, Vec<PortBinding>>,
    /// Bind mounts as strings.
    pub binds: Vec<String>,
}

/// Network settings for a container.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkSettings {
    /// Per-network settings.
    pub networks: HashMap<String, NetworkEntry>,
}

/// A single network entry.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkEntry {
    /// Network ID.
    pub network_i_d: String,
    /// IP address.
    #[serde(rename = "IPAddress")]
    pub ip_address: String,
    /// Gateway.
    pub gateway: String,
}

/// A mount point in the container.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct MountPoint {
    /// Mount type: `"bind"` or `"volume"`.
    #[serde(rename = "Type")]
    pub mount_type: String,
    /// Source path on the host.
    pub source: String,
    /// Destination path in the container.
    pub destination: String,
    /// Read-write mode.
    #[serde(rename = "RW")]
    pub rw: bool,
}

/// Response for `POST /containers/{id}/wait`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ContainerWaitResponse {
    /// Exit code.
    pub status_code: i32,
}

// ── Exec Types ──────────────────────────────────────────────────

/// Request body for `POST /containers/{id}/exec`.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ExecCreateRequest {
    /// Command to execute.
    pub cmd: Vec<String>,
    /// Environment variables.
    pub env: Option<Vec<String>>,
    /// Working directory.
    pub working_dir: Option<String>,
    /// Attach to stdin.
    pub attach_stdin: Option<bool>,
    /// Attach to stdout.
    pub attach_stdout: Option<bool>,
    /// Attach to stderr.
    pub attach_stderr: Option<bool>,
    /// Allocate TTY.
    pub tty: Option<bool>,
    /// Run in detached mode.
    pub detach: Option<bool>,
}

impl Default for ExecCreateRequest {
    fn default() -> Self {
        Self {
            cmd: Vec::new(),
            env: None,
            working_dir: None,
            attach_stdin: Some(false),
            attach_stdout: Some(true),
            attach_stderr: Some(true),
            tty: None,
            detach: None,
        }
    }
}

/// Response for `POST /containers/{id}/exec`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExecCreateResponse {
    /// Exec instance ID.
    pub id: String,
}

/// Request body for `POST /exec/{id}/start`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct ExecStartRequest {
    /// Run in detached mode.
    pub detach: Option<bool>,
    /// Allocate TTY.
    pub tty: Option<bool>,
}

/// Response for `GET /exec/{id}/json`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ExecInspectResponse {
    /// Exec instance ID.
    #[serde(rename = "ID")]
    pub id: String,
    /// Whether exec is still running.
    pub running: bool,
    /// Exit code (null if still running).
    pub exit_code: Option<i32>,
    /// Container ID this exec belongs to.
    pub container_i_d: String,
}

// ── Image Types ─────────────────────────────────────────────────

/// One entry in `GET /images/json` response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct ImageListEntry {
    /// Image ID (sha256 digest).
    pub id: String,
    /// Repository tags (e.g. `["alpine:latest"]`).
    pub repo_tags: Vec<String>,
    /// Creation timestamp (Unix).
    pub created: i64,
    /// Image size in bytes.
    pub size: u64,
    /// Labels.
    pub labels: HashMap<String, String>,
}

// ── Network Types ───────────────────────────────────────────────

/// Request for `POST /networks/create`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct NetworkCreateRequest {
    /// Network name.
    pub name: String,
    /// Driver (e.g. `"bridge"`).
    pub driver: Option<String>,
    /// Labels.
    pub labels: Option<HashMap<String, String>>,
}

/// Response for `POST /networks/create`.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkCreateResponse {
    /// Network ID.
    pub id: String,
    /// Warning message.
    pub warning: String,
}

/// One entry in `GET /networks` response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct NetworkListEntry {
    /// Network ID.
    pub id: String,
    /// Network name.
    pub name: String,
    /// Driver.
    pub driver: String,
    /// Scope.
    pub scope: String,
}

// ── Volume Types ────────────────────────────────────────────────

/// Request for `POST /volumes/create`.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct VolumeCreateRequest {
    /// Volume name.
    pub name: Option<String>,
    /// Driver (e.g. `"local"`).
    pub driver: Option<String>,
    /// Labels.
    pub labels: Option<HashMap<String, String>>,
}

/// One entry in `GET /volumes` response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct VolumeEntry {
    /// Volume name.
    pub name: String,
    /// Driver.
    pub driver: String,
    /// Mount point.
    pub mountpoint: String,
    /// Labels.
    pub labels: HashMap<String, String>,
    /// Scope.
    pub scope: String,
}

/// Wrapper for `GET /volumes` response.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct VolumeListResponse {
    /// List of volumes.
    pub volumes: Vec<VolumeEntry>,
    /// Warnings.
    pub warnings: Vec<String>,
}

// ── Error Types ─────────────────────────────────────────────────

/// Standard Docker API error response.
#[derive(Debug, Clone, Serialize)]
pub struct DockerError {
    /// Error message.
    pub message: String,
}
