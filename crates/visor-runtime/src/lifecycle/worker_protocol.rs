//! Control protocol for parent ↔ VM worker IPC.
//!
//! The parent daemon spawns `visor --vm-worker` child processes, one per VM.
//! Communication happens over a Unix socketpair using newline-delimited JSON.
//!
//! # Protocol Flow
//!
//! ```text
//! Parent                    Worker
//!   │                         │
//!   │─── VmWorkerConfig ────→ │  (via stdin, at startup)
//!   │                         │
//!   │                         │── boot VM ──
//!   │                         │
//!   │←── WorkerMessage::Ready │  (VM booted, vCPU running)
//!   │                         │
//!   │── ParentMessage::Stop ─→│  (graceful shutdown)
//!   │                         │
//!   │←── WorkerMessage::VmExit│  (VM exited)
//!   │                         │
//! ```

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

#[cfg(test)]
#[path = "worker_protocol_test.rs"]
mod tests;

/// Configuration sent from parent to worker on stdin at startup.
///
/// Contains everything the worker needs to boot exactly one VM.
/// The parent serializes this as JSON followed by a newline.
///
/// # Wire Format
///
/// Single JSON object on stdin, newline-terminated:
/// ```json
/// {"vm_id":"abc-123","cid":3,"memory_mib":512,...}\n
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub(crate) struct VmWorkerConfig {
    /// Unique VM identifier (UUID from parent).
    pub vm_id: String,
    /// Vsock context ID assigned by the parent.
    pub cid: u32,
    /// Guest memory in MiB.
    pub memory_mib: u32,
    /// Number of virtual CPUs.
    pub vcpus: u32,
    /// Path to the ext4 rootfs image.
    pub rootfs_path: PathBuf,
    /// Base64-encoded `RunConfig` JSON (passed via kernel cmdline).
    pub run_config_json: String,
    /// Host directories to share with the guest via virtio-fs.
    #[serde(default)]
    pub shared_dirs: Vec<PathBuf>,
    /// Unix socket path for the control channel (parent ↔ worker).
    pub control_socket: PathBuf,
    /// Port mappings for this VM.
    #[serde(default)]
    pub ports: Vec<WorkerPortMapping>,
    /// Temp directory for rootfs cleanup on exit.
    pub tmp_dir: PathBuf,
    /// POSIX shared memory name (e.g., "/visor-vm-abc123").
    /// When set, the worker maps this shm region as guest RAM.
    #[serde(default)]
    pub shm_name: Option<String>,
}

/// Minimal port mapping for worker (avoids depending on visor-types in protocol).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub(crate) struct WorkerPortMapping {
    /// Host port number.
    pub host_port: u16,
    /// Guest port number.
    pub guest_port: u16,
    /// Protocol (`"tcp"` or `"udp"`).
    #[serde(default = "default_protocol")]
    pub protocol: String,
}

fn default_protocol() -> String {
    "tcp".to_owned()
}

/// Messages sent from parent → worker over the control socket.
///
/// Each message is a newline-delimited JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub(crate) enum ParentMessage {
    /// Request graceful VM shutdown.
    Stop {
        /// Seconds to wait before force-killing.
        #[serde(default = "default_timeout")]
        timeout_secs: u64,
    },
    /// Force-kill the VM immediately.
    Kill,
    /// Execute a command in the guest.
    Exec {
        /// Command and arguments.
        cmd: Vec<String>,
        /// Environment variables (`KEY=VALUE`).
        #[serde(default)]
        env: Vec<String>,
        /// Working directory inside the guest.
        #[serde(default = "default_workdir")]
        working_dir: String,
    },
    /// Assign a VM to a pooled worker (pool mode only).
    AssignVm(Box<VmWorkerConfig>),
}

fn default_timeout() -> u64 {
    10
}

fn default_workdir() -> String {
    "/".to_owned()
}

/// Messages sent from worker → parent over the control socket.
///
/// Each message is a newline-delimited JSON object.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub(crate) enum WorkerMessage {
    /// Worker has booted the VM and vCPU is running.
    Ready {
        /// Worker process PID.
        pid: u32,
    },
    /// VM exited (naturally or via stop/kill).
    VmExit {
        /// Exit code from the guest (parsed from serial output).
        exit_code: i32,
        /// Human-readable reason for the exit.
        reason: String,
    },
    /// Result of an exec command.
    ExecResult {
        /// Exit code from the executed command.
        exit_code: i32,
        /// Captured stdout.
        stdout: String,
        /// Captured stderr.
        stderr: String,
    },
    /// Worker encountered an unrecoverable error.
    Error {
        /// Error description.
        message: String,
    },
    /// Pooled worker is ready to accept VM assignments (pool mode only).
    PoolReady {
        /// Worker process PID.
        pid: u32,
    },
}

/// Encode a message as newline-delimited JSON bytes.
///
/// # Errors
///
/// Returns an error if JSON serialization fails.
pub(crate) fn encode_message<T: Serialize>(msg: &T) -> anyhow::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(msg)
        .map_err(|e| anyhow::anyhow!("failed to serialize message: {e}"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Decode a newline-delimited JSON message from bytes.
///
/// Trims trailing whitespace/newlines before parsing.
///
/// # Errors
///
/// Returns an error if the bytes are not valid JSON for the target type.
pub(crate) fn decode_message<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> anyhow::Result<T> {
    let trimmed = std::str::from_utf8(bytes)
        .map_err(|e| anyhow::anyhow!("invalid UTF-8 in message: {e}"))?
        .trim();
    serde_json::from_str(trimmed)
        .map_err(|e| anyhow::anyhow!("failed to deserialize message: {e}"))
}
