//! VM worker entry point for process-per-VM architecture.
//!
//! When visor spawns a child process via `visor --vm-worker`, this module's
//! [`run_worker()`] function is the entry point. The worker:
//!
//! 1. Reads [`VmWorkerConfig`] from stdin (newline-delimited JSON)
//! 2. Connects to the parent's control socket
//! 3. Boots the VM via [`crate::vm::boot_vm()`]
//! 4. Sends [`WorkerMessage::Ready`] to the parent
//! 5. Enters an event loop handling [`ParentMessage`]s and VM completion
//!
//! # Protocol
//!
//! ```text
//! Parent (daemon)              Worker (this process)
//!   │                            │
//!   │── VmWorkerConfig ────────→ │  (stdin, at startup)
//!   │                            │
//!   │                            │── boot VM ──
//!   │                            │
//!   │←── WorkerMessage::Ready ── │  (control socket)
//!   │                            │
//!   │── ParentMessage::Stop ───→ │  (control socket)
//!   │                            │
//!   │←── WorkerMessage::VmExit ─ │  (control socket)
//! ```

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread::JoinHandle;
use anyhow::Context;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWriteExt, BufReader};

use super::worker_protocol::{
    ParentMessage, VmWorkerConfig, WorkerMessage, decode_message, encode_message,
};

#[cfg(test)]
#[path = "vm_worker_test.rs"]
mod tests;

// ── Worker Action ────────────────────────────────────────────────

/// Action returned by [`handle_parent_message()`] to drive the worker event loop.
///
/// This intermediate type decouples message parsing from side-effect execution,
/// making the message dispatch logic independently testable.
#[derive(Debug)]
#[non_exhaustive]
pub(crate) enum WorkerAction {
    /// Graceful shutdown with a timeout.
    Stop {
        /// Seconds to wait before force-killing.
        timeout_secs: u64,
    },
    /// Immediate force-kill.
    Kill,
    /// Execute a command in the guest.
    Exec {
        /// Command and arguments.
        cmd: Vec<String>,
        /// Environment variables (`KEY=VALUE`).
        env: Vec<String>,
        /// Working directory inside the guest.
        working_dir: String,
    },
}

// ── Config Reading ───────────────────────────────────────────────

/// Reads and parses a [`VmWorkerConfig`] from the given async reader.
///
/// Expects exactly one newline-delimited JSON object. This is typically
/// stdin, but accepts any `AsyncRead` for testability.
///
/// # Errors
///
/// Returns an error if the input is empty, not valid UTF-8, or not valid
/// JSON for [`VmWorkerConfig`].
pub(crate) async fn read_worker_config<R: AsyncRead + Unpin>(
    reader: R,
) -> anyhow::Result<VmWorkerConfig> {
    let mut buf_reader = BufReader::new(reader);
    let mut line = String::new();
    let bytes_read = buf_reader
        .read_line(&mut line)
        .await
        .context("read VmWorkerConfig from stdin")?;

    if bytes_read == 0 {
        anyhow::bail!("empty stdin: no VmWorkerConfig received");
    }

    decode_message(line.as_bytes()).context("deserialize VmWorkerConfig from stdin")
}

// ── Message Dispatch ─────────────────────────────────────────────

/// Maps a [`ParentMessage`] to a [`WorkerAction`] for the event loop.
///
/// Pure function — no I/O or side effects — making it trivially testable.
#[must_use]
pub(crate) fn handle_parent_message(msg: &ParentMessage) -> WorkerAction {
    match msg {
        ParentMessage::Stop { timeout_secs } => WorkerAction::Stop {
            timeout_secs: *timeout_secs,
        },
        ParentMessage::Kill => WorkerAction::Kill,
        ParentMessage::Exec {
            cmd,
            env,
            working_dir,
        } => WorkerAction::Exec {
            cmd: cmd.clone(),
            env: env.clone(),
            working_dir: working_dir.clone(),
        },
        ParentMessage::AssignVm(_) => {
            // AssignVm is only used in pool mode, not in normal worker event loop.
            // This should never be reached in normal operation.
            tracing::warn!("received AssignVm in normal worker event loop (unexpected)");
            WorkerAction::Kill
        }
    }
}

// ── Message Sending ──────────────────────────────────────────────

/// Sends a [`WorkerMessage`] over the given writer as newline-delimited JSON.
///
/// # Errors
///
/// Returns an error if serialization or the write fails.
pub(crate) async fn send_worker_message<W: tokio::io::AsyncWrite + Unpin>(
    msg: &WorkerMessage,
    mut writer: W,
) -> anyhow::Result<()> {
    let bytes = encode_message(msg).context("encode worker message")?;
    writer
        .write_all(&bytes)
        .await
        .context("write worker message to control socket")?;
    writer
        .flush()
        .await
        .context("flush worker message to control socket")?;
    Ok(())
}

/// Best-effort: send an error message to the parent via the control socket.
///
/// Silently ignores any I/O errors (the parent may have already disconnected).
async fn send_error_message<W: tokio::io::AsyncWrite + Unpin>(writer: &mut W, message: &str) {
    let err_msg = WorkerMessage::Error {
        message: message.to_owned(),
    };
    if let Ok(bytes) = encode_message(&err_msg) {
        let _ = writer.write_all(&bytes).await;
        let _ = writer.flush().await;
    }
}

// ── VM Lifecycle Helpers ─────────────────────────────────────────

/// Performs a graceful VM stop via vsock shutdown, falling back to `kill_flag`.
///
/// # Errors
///
/// Returns an error only if sending the `VmExit` message fails.
async fn handle_stop<W: tokio::io::AsyncWrite + Unpin>(
    config: &VmWorkerConfig,
    timeout_secs: u64,
    kill_flag: &Arc<std::sync::atomic::AtomicBool>,
    thread: &mut Option<JoinHandle<()>>,
    serial_output: &crate::vm::SerialOutput,
    ctrl_write: &mut W,
) -> anyhow::Result<()> {
    tracing::info!(
        vm_id = %config.vm_id,
        timeout_secs,
        "received Stop, initiating graceful shutdown"
    );

    // Try vsock shutdown with timeout.
    let backend = crate::backend::comms_backend();
    let shutdown_result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        async {
            let mut client = crate::vsock::client::VsockClient::connect(
                &backend,
                config.cid,
                crate::vsock::client::VSOCK_AGENT_PORT,
            )
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))?;
            client
                .shutdown()
                .await
                .map_err(|e| anyhow::anyhow!("{e}"))
        },
    )
    .await;

    match shutdown_result {
        Ok(Ok(())) => {
            tracing::info!(vm_id = %config.vm_id, "vsock shutdown sent");
        }
        Ok(Err(e)) => {
            tracing::warn!(
                vm_id = %config.vm_id,
                error = %e,
                "vsock shutdown failed, forcing kill"
            );
            kill_flag.store(true, Ordering::Release);
        }
        Err(_) => {
            tracing::warn!(
                vm_id = %config.vm_id,
                timeout_secs,
                "vsock shutdown timed out, forcing kill"
            );
            kill_flag.store(true, Ordering::Release);
        }
    }

    join_vcpu_thread(thread).await;

    let exit_code = crate::vm::parse_exit_code(&serial_output.as_bytes());
    let exit_msg = WorkerMessage::VmExit {
        exit_code,
        reason: "stopped".to_owned(),
    };
    send_worker_message(&exit_msg, ctrl_write)
        .await
        .context("send VmExit after stop")
}

/// Force-kills the VM via `kill_flag` and sends `VmExit` to the parent.
///
/// # Errors
///
/// Returns an error only if sending the `VmExit` message fails.
async fn handle_kill<W: tokio::io::AsyncWrite + Unpin>(
    config: &VmWorkerConfig,
    kill_flag: &Arc<std::sync::atomic::AtomicBool>,
    thread: &mut Option<JoinHandle<()>>,
    serial_output: &crate::vm::SerialOutput,
    ctrl_write: &mut W,
) -> anyhow::Result<()> {
    tracing::info!(vm_id = %config.vm_id, "received Kill, forcing shutdown");
    kill_flag.store(true, Ordering::Release);

    join_vcpu_thread(thread).await;

    let exit_code = crate::vm::parse_exit_code(&serial_output.as_bytes());
    let exit_msg = WorkerMessage::VmExit {
        exit_code,
        reason: "killed".to_owned(),
    };
    send_worker_message(&exit_msg, ctrl_write)
        .await
        .context("send VmExit after kill")
}

/// Executes a command in the guest via vsock and sends the result to the parent.
///
/// # Errors
///
/// Returns an error only if sending the response message fails.
async fn handle_exec<W: tokio::io::AsyncWrite + Unpin>(
    config: &VmWorkerConfig,
    cmd: Vec<String>,
    env: Vec<String>,
    working_dir: String,
    ctrl_write: &mut W,
) -> anyhow::Result<()> {
    tracing::info!(
        vm_id = %config.vm_id,
        cmd = ?cmd,
        "received Exec request"
    );

    let backend = crate::backend::comms_backend();
    let exec_result = async {
        let mut client = crate::vsock::client::VsockClient::connect(
            &backend,
            config.cid,
            crate::vsock::client::VSOCK_AGENT_PORT,
        )
        .await
        .map_err(|e| anyhow::anyhow!("{e}"))?;
        client
            .exec(cmd, env, working_dir)
            .await
            .map_err(|e| anyhow::anyhow!("{e}"))
    }
    .await;

    let response = match exec_result {
        Ok(result) => WorkerMessage::ExecResult {
            exit_code: result.exit_code,
            stdout: result.stdout,
            stderr: result.stderr,
        },
        Err(e) => WorkerMessage::Error {
            message: format!("exec failed: {e}"),
        },
    };

    send_worker_message(&response, ctrl_write)
        .await
        .context("send exec result to parent")
}

/// Joins the vCPU thread, waiting for it to finish.
async fn join_vcpu_thread(thread: &mut Option<JoinHandle<()>>) {
    if let Some(t) = thread.take() {
        let _ = tokio::task::spawn_blocking(move || t.join()).await;
    }
}

// ── Worker Entry Point ───────────────────────────────────────────

/// Main entry point for a VM worker process.
///
/// Called when the binary is invoked as `visor vm-worker`. Reads config
/// from stdin, boots the VM, and enters the control socket event loop.
///
/// # Errors
///
/// Returns an error if config reading, VM boot, or the event loop fails.
/// On any unrecoverable error, sends [`WorkerMessage::Error`] to the parent
/// before returning.
pub(crate) async fn run_worker() -> anyhow::Result<()> {
    // 0. Verify HVF entitlement (macOS). Fail early before touching HVF.
    crate::codesign::verify_current_binary()?;

    // 1. Read config from stdin.
    let stdin = tokio::io::stdin();
    let config = read_worker_config(stdin)
        .await
        .context("read worker config from stdin")?;

    tracing::info!(
        vm_id = %config.vm_id,
        cid = config.cid,
        memory_mib = config.memory_mib,
        vcpus = config.vcpus,
        "worker starting"
    );

    // 2. Connect to parent's control socket.
    let control_stream = tokio::net::UnixStream::connect(&config.control_socket)
        .await
        .context("connect to parent control socket")?;
    let (ctrl_read, mut ctrl_write) = tokio::io::split(control_stream);
    let mut ctrl_reader = BufReader::new(ctrl_read);

    // 3. Parse RunConfig and boot VM.
    let (kill_flag, mut completion_rx, serial_output, mut thread) =
        boot_worker_vm(&config, &mut ctrl_write).await?;

    // 4. Send Ready to parent.
    let ready_msg = WorkerMessage::Ready {
        pid: std::process::id(),
    };
    send_worker_message(&ready_msg, &mut ctrl_write)
        .await
        .context("send Ready to parent")?;

    tracing::info!(
        vm_id = %config.vm_id,
        pid = std::process::id(),
        "worker ready, entering event loop"
    );

    // 5. Event loop.
    worker_event_loop(
        &config,
        &mut ctrl_reader,
        &mut ctrl_write,
        &kill_flag,
        &mut completion_rx,
        &serial_output,
        &mut thread,
    )
    .await?;

    tracing::info!(vm_id = %config.vm_id, "worker exiting");
    Ok(())
}

/// Entry point for a pooled worker process (pool mode).
///
/// Called when the binary is invoked as `visor vm-worker --pool <socket_path>`.
/// The worker waits idle for VM assignment via `ParentMessage::AssignVm`.
///
/// # Errors
///
/// Returns an error if socket connection or the event loop fails.
pub(crate) async fn run_pool_worker(socket_path: &std::path::Path) -> anyhow::Result<()> {
    // 0. Verify HVF entitlement (macOS).
    crate::codesign::verify_current_binary()?;

    // 1. Connect to parent's control socket.
    let control_stream = tokio::net::UnixStream::connect(socket_path)
        .await
        .context("connect to parent control socket")?;
    let (ctrl_read, mut ctrl_write) = tokio::io::split(control_stream);
    let mut ctrl_reader = BufReader::new(ctrl_read);

    // 2. Send PoolReady to parent.
    let pool_ready_msg = WorkerMessage::PoolReady {
        pid: std::process::id(),
    };
    send_worker_message(&pool_ready_msg, &mut ctrl_write)
        .await
        .context("send PoolReady to parent")?;

    tracing::info!(
        pid = std::process::id(),
        "pool worker ready, waiting for VM assignment"
    );

    // 3. Wait for AssignVm message from parent.
    let mut line = String::new();
    let bytes_read = ctrl_reader
        .read_line(&mut line)
        .await
        .context("read AssignVm from parent")?;

    if bytes_read == 0 {
        anyhow::bail!("parent closed connection before sending AssignVm");
    }

    let parent_msg: ParentMessage = decode_message(line.as_bytes())
        .context("decode AssignVm message")?;

    let config = match parent_msg {
        ParentMessage::AssignVm(cfg) => *cfg,
        _ => anyhow::bail!("expected AssignVm, got {parent_msg:?}"),
    };

    tracing::info!(
        vm_id = %config.vm_id,
        cid = config.cid,
        "pool worker assigned VM, booting"
    );

    // 4. Parse RunConfig and boot VM (same as normal worker).
    let (kill_flag, mut completion_rx, serial_output, mut thread) =
        boot_worker_vm(&config, &mut ctrl_write).await?;

    // 5. Send Ready to parent.
    let ready_msg = WorkerMessage::Ready {
        pid: std::process::id(),
    };
    send_worker_message(&ready_msg, &mut ctrl_write)
        .await
        .context("send Ready to parent")?;

    tracing::info!(
        vm_id = %config.vm_id,
        pid = std::process::id(),
        "pool worker ready, entering event loop"
    );

    // 6. Event loop (same as normal worker).
    worker_event_loop(
        &config,
        &mut ctrl_reader,
        &mut ctrl_write,
        &kill_flag,
        &mut completion_rx,
        &serial_output,
        &mut thread,
    )
    .await?;

    tracing::info!(vm_id = %config.vm_id, "pool worker exiting");
    Ok(())
}

/// Parses `RunConfig` and boots the VM, returning the live handle parts.
///
/// On failure, sends an error message to the parent before returning.
///
/// # Errors
///
/// Returns an error if the `RunConfig` JSON is invalid or the VM fails to boot.
async fn boot_worker_vm<W: tokio::io::AsyncWrite + Unpin>(
    config: &VmWorkerConfig,
    ctrl_write: &mut W,
) -> anyhow::Result<(
    Arc<std::sync::atomic::AtomicBool>,
    Option<tokio::sync::oneshot::Receiver<crate::vm::VmExitInfo>>,
    crate::vm::SerialOutput,
    Option<JoinHandle<()>>,
)> {
    let run_config: visor_init::config::RunConfig =
        match serde_json::from_str(&config.run_config_json) {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("parse RunConfig: {e}");
                tracing::error!(vm_id = %config.vm_id, "{msg}");
                send_error_message(ctrl_write, &msg).await;
                anyhow::bail!("{msg}");
            }
        };

    // Re-open shared memory by name (fd is NOT inherited across posix_spawn/exec).
    let guest_memory = if let Some(shm_name) = config.shm_name.as_ref() {
        let memory_size = config.memory_mib.max(64) as usize * 1024 * 1024;
        let shm_region = visor_vmm::shared_memory::SharedMemoryRegion::open_existing(
            shm_name,
            memory_size,
        )
        .context("open shared memory by name in worker")?;
        let mem = visor_vmm::memory::GuestMemory::from_shared_fd(
            shm_region.fd(),
            memory_size,
            visor_vmm::boot::GUEST_RAM_START,
        )
        .context("map shared memory as guest RAM")?;
        tracing::info!(
            vm_id = %config.vm_id,
            shm_name,
            memory_size,
            fd = shm_region.fd(),
            "re-opened shared memory by name for guest RAM"
        );
        // Keep shm_region alive so the fd stays valid for the VM's lifetime.
        // Leak it intentionally — the process exits when the VM exits.
        std::mem::forget(shm_region);
        Some(Arc::new(mem))
    } else {
        None
    };

    let shared_dirs = config.shared_dirs.clone();
    let mut storage = crate::vm::BootStorage::new(&shared_dirs, &[]);
    #[cfg(target_os = "macos")]
    {
        storage = storage.with_guest_memory(guest_memory);
    }
    let mut handle = match crate::vm::boot_vm(
        &config.vm_id,
        &run_config,
        &config.rootfs_path,
        crate::vm::VmBootSpec::new(config.memory_mib, config.vcpus, config.cid),
        storage,
    ) {
        Ok(h) => h,
        Err(e) => {
            let msg = format!("boot VM: {e}");
            tracing::error!(vm_id = %config.vm_id, "{msg}");
            send_error_message(ctrl_write, &msg).await;
            anyhow::bail!("{msg}");
        }
    };

    // Take parts so VmHandle::Drop is a no-op.
    let parts = handle.take_parts();
    Ok((
        parts.kill_flag,
        parts.completion_rx,
        parts.serial_output,
        parts.thread,
    ))
}

/// Main event loop: listens for parent messages and VM completion.
///
/// # Errors
///
/// Returns an error if reading from the control socket or sending a
/// response message fails.
async fn worker_event_loop<W: tokio::io::AsyncWrite + Unpin>(
    config: &VmWorkerConfig,
    ctrl_reader: &mut BufReader<tokio::io::ReadHalf<tokio::net::UnixStream>>,
    ctrl_write: &mut W,
    kill_flag: &Arc<std::sync::atomic::AtomicBool>,
    completion_rx: &mut Option<tokio::sync::oneshot::Receiver<crate::vm::VmExitInfo>>,
    serial_output: &crate::vm::SerialOutput,
    thread: &mut Option<JoinHandle<()>>,
) -> anyhow::Result<()> {
    loop {
        let mut line = String::new();

        tokio::select! {
            // Branch 1: Parent sends a control message.
            read_result = ctrl_reader.read_line(&mut line) => {
                let bytes_read = read_result.context("read from control socket")?;
                if bytes_read == 0 {
                    tracing::warn!(vm_id = %config.vm_id, "control socket closed by parent");
                    kill_flag.store(true, Ordering::Release);
                    join_vcpu_thread(thread).await;
                    break;
                }

                let parent_msg: ParentMessage = match decode_message(line.as_bytes()) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(
                            vm_id = %config.vm_id,
                            error = %e,
                            "malformed parent message, ignoring"
                        );
                        continue;
                    }
                };

                match handle_parent_message(&parent_msg) {
                    WorkerAction::Stop { timeout_secs } => {
                        handle_stop(config, timeout_secs, kill_flag, thread, serial_output, ctrl_write).await?;
                        break;
                    }
                    WorkerAction::Kill => {
                        handle_kill(config, kill_flag, thread, serial_output, ctrl_write).await?;
                        break;
                    }
                    WorkerAction::Exec { cmd, env, working_dir } => {
                        handle_exec(config, cmd, env, working_dir, ctrl_write).await?;
                    }
                }
            }

            // Branch 2: VM completed naturally.
            exit_info = async {
                if let Some(rx) = completion_rx.take() {
                    rx.await.ok()
                } else {
                    std::future::pending().await
                }
            } => {
                let exit_code = exit_info.as_ref().map_or(1, |info| info.exit_code);
                let reason = exit_info.as_ref().map_or_else(
                    || "completion channel dropped".to_owned(),
                    |info| info.reason.to_string(),
                );

                tracing::info!(
                    vm_id = %config.vm_id,
                    exit_code,
                    reason = %reason,
                    "VM exited naturally"
                );

                join_vcpu_thread(thread).await;

                let exit_msg = WorkerMessage::VmExit { exit_code, reason };
                send_worker_message(&exit_msg, ctrl_write)
                    .await
                    .context("send VmExit on natural completion")?;
                break;
            }
        }
    }
    Ok(())
}
