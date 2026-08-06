//! Process-per-VM lifecycle for macOS (HVF one-VM-per-process constraint).
//!
//! [`WorkerProcessLifecycle`] implements [`VmLifecycle`] by spawning a child
//! process (`visor vm-worker`) for each VM. The parent communicates with the
//! worker over a Unix control socket using newline-delimited JSON messages
//! defined in [`super::worker_protocol`].
//!
//! # Architecture
//!
//! ```text
//! Parent (daemon)                  Worker (child process)
//!   │                                │
//!   │── spawn visor vm-worker ─────→ │
//!   │── VmWorkerConfig (stdin) ────→ │
//!   │                                │── boot VM (HVF)
//!   │←── WorkerMessage::Ready ────── │
//!   │                                │
//!   │── ParentMessage::Stop ───────→ │
//!   │←── WorkerMessage::VmExit ──── │
//!   │                                │── exit
//! ```

use std::collections::HashMap;
use std::process::Stdio;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::RwLock;

use crate::backend::{self, VmLiveState, VsockConnector};
use crate::vm;

use super::worker_protocol::{
    ParentMessage, VmWorkerConfig, WorkerMessage, WorkerPortMapping, encode_message,
};
use super::{VmBootConfig, VmLifecycle, VmRunResult, VmSnapshotConfig, VmStopResult};
use visor_vmm::shared_memory::SharedMemoryRegion;

#[cfg(test)]
#[path = "worker_process_test.rs"]
mod tests;

// ── Constants ───────────────────────────────────────────────────

/// Timeout for waiting for the worker to send `Ready` after spawning.
const WORKER_READY_TIMEOUT_SECS: u64 = 30;

/// Extra buffer time added to the requested stop timeout when waiting
/// for the worker to send `VmExit` after a `Stop` message.
const STOP_TIMEOUT_BUFFER_SECS: u64 = 5;

// ── Worker Handle ───────────────────────────────────────────────

/// Handle to a running worker process.
///
/// Holds the child process, control socket halves, and the worker PID.
/// Stored in `WorkerProcessLifecycle.workers` keyed by CID.
struct WorkerHandle {
    /// Write half of the control socket (for sending `ParentMessage`).
    ctrl_write: tokio::io::WriteHalf<tokio::net::UnixStream>,
    /// Read half of the control socket (for receiving `WorkerMessage`).
    ctrl_read: BufReader<tokio::io::ReadHalf<tokio::net::UnixStream>>,
    /// Child process handle (for kill/waitpid).
    child: tokio::process::Child,
    /// Worker PID (from `Ready` message). Used for logging during stop/kill.
    worker_pid: u32,
    /// Shared memory region for guest RAM (kept alive for parent-side introspection).
    _shm_region: Option<SharedMemoryRegion>,
}

// ── WorkerProcessLifecycle ──────────────────────────────────────

/// Runs each VM in a separate child process (`visor vm-worker`).
///
/// Required on macOS where Apple's Hypervisor.framework (HVF) limits
/// each process to a single VM. The parent daemon spawns one worker
/// process per VM and communicates via a Unix control socket.
///
/// # Thread Safety
///
/// This struct is safe to share across async tasks via `Arc`.
pub(crate) struct WorkerProcessLifecycle {
    /// Vsock connector (stored for future exec support via parent-side vsock).
    _connector: Arc<dyn VsockConnector>,
    /// Maps CID → active worker handle. Protected by async `RwLock`.
    workers: Arc<RwLock<HashMap<u32, WorkerHandle>>>,
}

impl WorkerProcessLifecycle {
    /// Creates a new process-per-VM lifecycle with the given vsock connector.
    #[must_use]
    pub(crate) fn new(connector: Arc<dyn VsockConnector>) -> Self {
        Self {
            _connector: connector,
            workers: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Returns the number of active worker processes (for testing).
    #[cfg(test)]
    pub(crate) async fn worker_count(&self) -> usize {
        self.workers.read().await.len()
    }
}

// ── Config Building ─────────────────────────────────────────────

/// Builds a [`VmWorkerConfig`] from a [`VmBootConfig`] and control socket path.
///
/// Generates a UUID for the `vm_id` and serializes the `RunConfig` to JSON.
pub(crate) fn build_worker_config(
    config: &VmBootConfig<'_>,
    control_socket: &std::path::Path,
) -> VmWorkerConfig {
    let run_config_json =
        serde_json::to_string(config.run_config).unwrap_or_else(|_| "{}".to_owned());

    let ports: Vec<WorkerPortMapping> = config
        .port_config
        .ports
        .iter()
        .map(|p| WorkerPortMapping {
            host_port: p.host_port,
            guest_port: p.guest_port,
            protocol: p.protocol.clone(),
        })
        .collect();

    VmWorkerConfig {
        vm_id: uuid::Uuid::new_v4().to_string(),
        cid: config.cid,
        memory_mib: config.memory_mib,
        vcpus: config.vcpus,
        rootfs_path: config.rootfs_path.to_path_buf(),
        run_config_json,
        shared_dirs: config.shared_dirs.to_vec(),
        control_socket: control_socket.to_path_buf(),
        ports,
        tmp_dir: config.tmp_dir.clone(),
        shm_name: None,
    }
}

// ── Helpers ─────────────────────────────────────────────────────

/// Reads a single `WorkerMessage` from the control socket reader.
///
/// # Errors
///
/// Returns an error if the socket is closed or the message is malformed.
async fn read_worker_message(
    reader: &mut BufReader<tokio::io::ReadHalf<tokio::net::UnixStream>>,
) -> anyhow::Result<WorkerMessage> {
    let mut line = String::new();
    let bytes_read = reader
        .read_line(&mut line)
        .await
        .context("read from worker control socket")?;
    if bytes_read == 0 {
        anyhow::bail!("worker control socket closed unexpectedly");
    }
    super::worker_protocol::decode_message(line.as_bytes())
        .context("decode worker message from control socket")
}

/// Sends a `ParentMessage` over the control socket write half.
///
/// # Errors
///
/// Returns an error if encoding or writing fails.
async fn send_parent_message(
    msg: &ParentMessage,
    writer: &mut tokio::io::WriteHalf<tokio::net::UnixStream>,
) -> anyhow::Result<()> {
    let bytes = encode_message(msg).context("encode parent message")?;
    writer
        .write_all(&bytes)
        .await
        .context("write parent message to control socket")?;
    writer
        .flush()
        .await
        .context("flush parent message to control socket")?;
    Ok(())
}

/// Spawns a worker process and returns the child, writing config to stdin.
///
/// # Errors
///
/// Returns an error if the current exe cannot be resolved or the child
/// fails to spawn.
async fn spawn_worker(worker_config: &VmWorkerConfig) -> anyhow::Result<tokio::process::Child> {
    let exe_path =
        std::env::current_exe().context("resolve current executable path for worker spawn")?;

    let mut cmd = tokio::process::Command::new(&exe_path);
    cmd.arg("vm-worker")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    // Forward RUST_LOG so worker inherits the same log level as the parent.
    if let Ok(rust_log) = std::env::var("RUST_LOG") {
        cmd.env("RUST_LOG", rust_log);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("spawn worker process: {}", exe_path.display()))?;

    // Write config to child's stdin, then close it.
    let config_bytes =
        encode_message(worker_config).context("serialize VmWorkerConfig for worker stdin")?;
    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(&config_bytes)
            .await
            .context("write VmWorkerConfig to worker stdin")?;
        stdin
            .flush()
            .await
            .context("flush worker stdin")?;
        // Drop stdin to signal EOF.
    }

    Ok(child)
}

/// Creates the Unix control socket for parent ↔ worker communication.
///
/// Returns the socket path and bound listener. Cleans up any stale socket.
///
/// # Errors
///
/// Returns an error if the directory or socket cannot be created.
fn create_control_socket(
    cid: u32,
) -> anyhow::Result<(std::path::PathBuf, std::path::PathBuf, tokio::net::UnixListener)> {
    let socket_dir = std::env::temp_dir().join(format!("visor-worker-{cid}"));
    std::fs::create_dir_all(&socket_dir).context("create worker socket directory")?;
    let socket_path = socket_dir.join("ctrl.sock");

    // Remove stale socket if it exists.
    let _ = std::fs::remove_file(&socket_path);

    let listener =
        tokio::net::UnixListener::bind(&socket_path).context("bind worker control socket")?;

    Ok((socket_dir, socket_path, listener))
}

/// Accepts a worker connection and waits for the `Ready` message.
///
/// # Errors
///
/// Returns an error if the worker doesn't connect or send `Ready` within
/// the timeout, or if the worker sends an error message.
async fn accept_and_wait_for_ready(
    listener: tokio::net::UnixListener,
) -> anyhow::Result<(
    tokio::io::WriteHalf<tokio::net::UnixStream>,
    BufReader<tokio::io::ReadHalf<tokio::net::UnixStream>>,
    u32,
)> {
    // Accept connection (with timeout).
    let (stream, _addr) = tokio::time::timeout(
        std::time::Duration::from_secs(WORKER_READY_TIMEOUT_SECS),
        listener.accept(),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!(
            "worker did not connect to control socket within {WORKER_READY_TIMEOUT_SECS}s"
        )
    })?
    .context("accept worker control socket connection")?;

    let (read_half, write_half) = tokio::io::split(stream);
    let mut ctrl_read = BufReader::new(read_half);

    // Wait for Ready message (with timeout).
    let ready_msg = tokio::time::timeout(
        std::time::Duration::from_secs(WORKER_READY_TIMEOUT_SECS),
        read_worker_message(&mut ctrl_read),
    )
    .await
    .map_err(|_| {
        anyhow::anyhow!("worker did not send Ready within {WORKER_READY_TIMEOUT_SECS}s")
    })?
    .context("read Ready from worker")?;

    let worker_pid = match ready_msg {
        WorkerMessage::Ready { pid } => pid,
        WorkerMessage::Error { message } => {
            anyhow::bail!("worker reported error during boot: {message}");
        }
        other => {
            anyhow::bail!("expected Ready from worker, got: {other:?}");
        }
    };

    Ok((write_half, ctrl_read, worker_pid))
}

/// Spawns a background task that monitors the worker child process.
///
/// If the worker exits unexpectedly (not via stop/kill), sends `VmExitInfo`
/// through the oneshot channel and removes the handle from the workers map.
fn spawn_worker_monitor(
    cid: u32,
    workers: Arc<RwLock<HashMap<u32, WorkerHandle>>>,
    completion_tx: tokio::sync::oneshot::Sender<vm::VmExitInfo>,
) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(std::time::Duration::from_millis(500));
        loop {
            interval.tick().await;
            let mut workers_guard = workers.write().await;
            if let Some(handle) = workers_guard.get_mut(&cid) {
                match handle.child.try_wait() {
                    Ok(Some(status)) => {
                        let exit_code = status.code().unwrap_or(1);
                        tracing::info!(cid, exit_code, "worker process exited unexpectedly");
                        workers_guard.remove(&cid);
                        let _ = completion_tx.send(vm::VmExitInfo {
                            exit_code,
                            reason: vm::VmExitReason::Shutdown,
                        });
                        break;
                    }
                    Ok(None) => { /* Still running. */ }
                    Err(e) => {
                        tracing::warn!(cid, error = %e, "failed to check worker status");
                        workers_guard.remove(&cid);
                        let _ = completion_tx.send(vm::VmExitInfo {
                            exit_code: 1,
                            reason: vm::VmExitReason::Error(format!(
                                "failed to check worker status: {e}"
                            )),
                        });
                        break;
                    }
                }
            } else {
                // Worker was removed by stop/kill — they handle the completion.
                break;
            }
        }
    });
}

/// Cleans up port-forwarding rules and temp directory after VM stop/kill.
fn cleanup_vm_resources(state: &VmLiveState) {
    if let Some(ref pf) = state.port_forward_handle {
        tracing::debug!(
            cid = state.cid,
            mappings = pf.mapping_count(),
            "dropping port-forward rules"
        );
    }
    let _ = std::fs::remove_dir_all(&state.tmp_dir);
}

// ── VmLifecycle Implementation ──────────────────────────────────

#[async_trait]
impl VmLifecycle for WorkerProcessLifecycle {
    /// Boot a VM by spawning a child worker process.
    ///
    /// # Errors
    ///
    /// Returns an error if spawning, socket setup, or the ready handshake fails.
    async fn boot(&self, config: VmBootConfig<'_>) -> anyhow::Result<VmLiveState> {
        let boot_start = std::time::Instant::now();
        let cid = config.cid;

        // 1. Create control socket.
        let (socket_dir, socket_path, listener) = create_control_socket(cid)?;

        // 2. Build worker config and create shared memory for guest RAM.
        let mut worker_config = build_worker_config(&config, &socket_path);
        let vm_id = worker_config.vm_id.clone();
        let port_forward_handle = backend::setup_port_forwards(config.port_config, &config.run_config.effective_networks())
            .context("setup port forwards for worker VM")?;

        // 2b. Create shared memory region for guest RAM.
        let memory_size = config.memory_mib.max(64) as usize * 1024 * 1024;
        let shm_name = format!(
            "/vsr-{}-{}",
            cid,
            &worker_config.vm_id[..8]
        );
        let shm_region = SharedMemoryRegion::create(
            &shm_name,
            memory_size,
        )
        .context("create shared memory for guest RAM")?;
        worker_config.shm_name = Some(shm_region.name().to_owned());

        // 3. Spawn worker process.
        let child = spawn_worker(&worker_config)
            .await
            .context("spawn worker process")?;

        // 4. Wait for control socket connection + Ready message.
        //    Worker re-opens the shm by name, so we must NOT unlink until Ready.
        let (write_half, ctrl_read, worker_pid) = accept_and_wait_for_ready(listener).await?;

        // 4b. Unlink shm now that the worker has opened it.
        //     Existing mappings remain valid; no new processes can open by name.
        if let Err(e) = shm_region.unlink() {
            tracing::warn!(error = %e, "failed to unlink shared memory (non-fatal)");
        }

        tracing::info!(
            cid,
            worker_pid,
            vm_id = %vm_id,
            boot_ms = boot_start.elapsed().as_millis(),
            "VM booted (worker process, detached)"
        );

        // 5. Set up completion monitoring.
        let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
        let kill_flag = Arc::new(AtomicBool::new(false));

        let worker_handle = WorkerHandle {
            ctrl_write: write_half,
            ctrl_read,
            child,
            worker_pid,
            _shm_region: Some(shm_region),
        };
        self.workers.write().await.insert(cid, worker_handle);

        spawn_worker_monitor(cid, Arc::clone(&self.workers), completion_tx);

        // Clean up socket file — worker already connected.
        let _ = std::fs::remove_file(&socket_path);
        let _ = std::fs::remove_dir(&socket_dir);

        Ok(VmLiveState {
            cid,
            thread: None,
            kill_flag,
            completion_rx: Some(completion_rx),
            serial_output: vm::SerialOutput::default(),
            tmp_dir: config.tmp_dir,
            port_forward_handle,
        })
    }

    /// Snapshot restore is not yet supported in process-per-VM mode.
    ///
    /// # Errors
    ///
    /// Always returns an error.
    async fn boot_from_snapshot(
        &self,
        _config: VmSnapshotConfig<'_>,
    ) -> anyhow::Result<VmLiveState> {
        anyhow::bail!("snapshot restore not yet supported in process-per-VM mode")
    }

    /// Run a VM synchronously to completion via a worker process.
    ///
    /// Spawns the worker, waits for `Ready`, then waits for `VmExit`.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker fails to boot or the run loop errors.
    async fn run_to_completion(
        &self,
        config: VmBootConfig<'_>,
    ) -> anyhow::Result<VmRunResult> {
        let pipeline_start = std::time::Instant::now();
        let cid = config.cid;

        // Boot the worker (reuse boot() logic).
        let state = self.boot(config).await?;

        // Wait for the worker to send VmExit.
        let mut workers_guard = self.workers.write().await;
        let mut handle = workers_guard
            .remove(&cid)
            .context("worker handle not found after boot")?;
        drop(workers_guard);

        let exit_msg = read_worker_message(&mut handle.ctrl_read)
            .await
            .context("read VmExit from worker in run_to_completion")?;

        let (exit_code, reason) = match exit_msg {
            WorkerMessage::VmExit { exit_code, reason } => (exit_code, reason),
            WorkerMessage::Error { message } => {
                anyhow::bail!("worker error during run: {message}");
            }
            other => {
                anyhow::bail!("expected VmExit from worker, got: {other:?}");
            }
        };

        // Wait for child process to exit.
        let _ = handle.child.wait().await;

        let serial_bytes = state.serial_output.as_bytes();

        tracing::info!(
            cid,
            exit_code,
            reason = %reason,
            total_ms = pipeline_start.elapsed().as_millis(),
            "VM completed (worker process)"
        );

        // Clean up temp directory.
        let _ = std::fs::remove_dir_all(&state.tmp_dir);

        Ok(VmRunResult {
            exit_info: vm::VmExitInfo {
                exit_code,
                reason: vm::VmExitReason::Shutdown,
            },
            serial_bytes,
        })
    }

    /// Stop a VM gracefully by sending `Stop` to the worker.
    ///
    /// If no worker handle is found (e.g., state was created by a different
    /// lifecycle or injected directly in tests), falls back to in-process
    /// cleanup using the `VmLiveState` fields.
    ///
    /// # Errors
    ///
    /// Returns an error if communication with the worker fails.
    async fn stop(
        &self,
        mut state: VmLiveState,
        timeout_secs: u64,
    ) -> anyhow::Result<VmStopResult> {
        let cid = state.cid;

        // Try to look up and remove worker handle.
        let maybe_handle = self.workers.write().await.remove(&cid);

        if let Some(mut handle) = maybe_handle {
            tracing::info!(cid, worker_pid = handle.worker_pid, "stopping worker VM");

            // Send Stop message.
            send_parent_message(
                &ParentMessage::Stop { timeout_secs },
                &mut handle.ctrl_write,
            )
            .await
            .context("send Stop to worker")?;

            // Wait for VmExit (with timeout).
            let total_timeout = timeout_secs + STOP_TIMEOUT_BUFFER_SECS;
            let exit_result = tokio::time::timeout(
                std::time::Duration::from_secs(total_timeout),
                read_worker_message(&mut handle.ctrl_read),
            )
            .await;

            match exit_result {
                Ok(Ok(WorkerMessage::VmExit { exit_code, reason })) => {
                tracing::info!(cid, exit_code, reason = %reason, "worker VM stopped gracefully");
                }
                Ok(Ok(WorkerMessage::Error { message })) => {
                    tracing::warn!(cid, "worker error during stop: {message}");
                }
                Ok(Ok(other)) => {
                tracing::warn!(cid, "unexpected message from worker during stop: {other:?}");
                }
                Ok(Err(e)) => {
                    tracing::warn!(cid, error = %e, "failed to read VmExit from worker");
                }
                Err(_) => {
                    tracing::warn!(
                        cid, total_timeout,
                        "worker did not send VmExit within timeout, killing"
                    );
                    let _ = handle.child.kill().await;
                }
            }

            let _ = handle.child.wait().await;
        } else {
            // No worker process — fall back to in-process cleanup.
            tracing::debug!(cid, "no worker handle found, performing in-process cleanup");
            if let Some(thread) = state.thread.take() {
                state
                    .kill_flag
                    .store(true, std::sync::atomic::Ordering::Release);
                let _ = tokio::task::spawn_blocking(move || thread.join()).await;
            }
        }

        let serial_bytes = state.serial_output.as_bytes();
        cleanup_vm_resources(&state);
        drop(state.port_forward_handle);

        Ok(VmStopResult { serial_bytes })
    }

    /// Force-kill a VM by killing the worker process.
    ///
    /// If no worker handle is found, falls back to in-process cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error if cleanup fails.
    async fn kill(&self, mut state: VmLiveState) -> anyhow::Result<VmStopResult> {
        let cid = state.cid;

        // Try to look up and remove worker handle.
        let maybe_handle = self.workers.write().await.remove(&cid);

        if let Some(mut handle) = maybe_handle {
            tracing::info!(cid, worker_pid = handle.worker_pid, "killing worker VM");

            // Send Kill message (best-effort).
            let _ = send_parent_message(&ParentMessage::Kill, &mut handle.ctrl_write).await;

            // Kill the child process as backup.
            let _ = handle.child.kill().await;

            // Wait for child process to exit.
            let _ = handle.child.wait().await;
        } else {
            // No worker process — fall back to in-process cleanup.
            tracing::debug!(cid, "no worker handle found, performing in-process cleanup");
            if let Some(thread) = state.thread.take() {
                state
                    .kill_flag
                    .store(true, std::sync::atomic::Ordering::Release);
                let _ = tokio::task::spawn_blocking(move || thread.join()).await;
            }
        }

        let serial_bytes = state.serial_output.as_bytes();
        cleanup_vm_resources(&state);
        drop(state.port_forward_handle);

        Ok(VmStopResult { serial_bytes })
    }
}
