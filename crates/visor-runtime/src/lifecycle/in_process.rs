//! In-process VM lifecycle (Linux KVM / macOS HVF single-process model).
//!
//! [`InProcessLifecycle`] runs VMs as threads inside the daemon process.
//! This is the default on Linux (KVM supports unlimited VMs per process)
//! and the temporary implementation on macOS (HVF limits to one VM per
//! process — a future `WorkerProcessLifecycle` will lift this restriction).

use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;

use crate::backend::{self, VmLiveState, VsockConnector};
use crate::vm;

use super::{VmBootConfig, VmLifecycle, VmRunResult, VmSnapshotConfig, VmStopResult};

#[cfg(test)]
#[path = "in_process_test.rs"]
mod tests;

/// Runs VMs as threads within the current process.
///
/// Each `boot()` call spawns a vCPU thread via [`vm::boot_vm()`] and
/// returns a [`VmLiveState`] containing the thread handle, kill flag,
/// and serial output buffer.
///
/// # Thread Safety
///
/// This struct is cheaply cloneable and safe to share across async tasks.
pub(crate) struct InProcessLifecycle {
    connector: Arc<dyn VsockConnector>,
}

impl InProcessLifecycle {
    /// Creates a new in-process lifecycle with the given vsock connector.
    #[must_use]
    pub(crate) fn new(connector: Arc<dyn VsockConnector>) -> Self {
        Self { connector }
    }
}

#[async_trait]
impl VmLifecycle for InProcessLifecycle {
    /// Boot a VM in detached mode (returns immediately with live state).
    ///
    /// Delegates to [`vm::boot_vm()`] for all platform-specific setup
    /// (KVM on Linux, HVF on macOS), then sets up port forwarding.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM fails to boot or port forwarding fails.
    async fn boot(&self, config: VmBootConfig<'_>) -> anyhow::Result<VmLiveState> {
        let boot_start = std::time::Instant::now();
        let guest_networks = config.run_config.effective_networks();

        let t0 = std::time::Instant::now();
        let mut handle = vm::boot_vm(
            config.vm_id,
            config.run_config,
            config.rootfs_path,
            vm::VmBootSpec::new(config.memory_mib, config.vcpus, config.cid),
            vm::BootStorage::new(config.shared_dirs, &[]),
        )
        .context("boot microVM")?;
        let boot_ms = t0.elapsed().as_millis();

        let t1 = std::time::Instant::now();
        let port_forward_handle =
            backend::setup_port_forwards(config.port_config, &guest_networks)?;
        let pf_ms = t1.elapsed().as_millis();

        let total_ms = boot_start.elapsed().as_millis();
        tracing::info!(
            cid = config.cid,
            boot_ms,
            port_forward_ms = pf_ms,
            total_ms,
            "VM booted (in-process, detached)"
        );

        // Take parts so VmHandle::Drop is a no-op — ownership moves to VmLiveState.
        let parts = handle.take_parts();
        Ok(VmLiveState {
            cid: config.cid,
            thread: parts.thread,
            kill_flag: parts.kill_flag,
            completion_rx: parts.completion_rx,
            serial_output: parts.serial_output,
            tmp_dir: config.tmp_dir,
            port_forward_handle,
        })
    }

    /// Boot a VM from a pre-saved snapshot (fast restore path).
    ///
    /// Skips OCI pull entirely. Restores guest memory via
    /// `mmap(MAP_PRIVATE)` COW from `memory.bin` and vCPU registers
    /// from `cpu_state.json`. Provides sub-5ms VM startup.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot is invalid or platform init fails.
    async fn boot_from_snapshot(
        &self,
        config: VmSnapshotConfig<'_>,
    ) -> anyhow::Result<VmLiveState> {
        let pipeline_start = std::time::Instant::now();
        // Snapshot restore currently has no guest-network attach list on the
        // lifecycle config; port forwards still apply from the host VmConfig.
        let guest_networks: &[visor_init::config::NetworkConfig] = &[];

        let t0 = std::time::Instant::now();
        let mut handle = vm::boot_vm_from_snapshot(
            config.vm_id,
            config.snapshot_dir,
            vm::VmBootSpec::new(config.memory_mib, config.vcpus, config.cid),
            vm::BootStorage::new(config.shared_dirs, &[]),
            guest_networks,
        )
        .context("snapshot fast-path restore")?;
        let restore_ms = t0.elapsed().as_millis();

        let t1 = std::time::Instant::now();
        let port_forward_handle =
            backend::setup_port_forwards(config.port_config, guest_networks)
                .context("setup port forwards for snapshot VM")?;
        let pf_ms = t1.elapsed().as_millis();

        let total_ms = pipeline_start.elapsed().as_millis();
        tracing::info!(
            cid = config.cid,
            restore_ms,
            port_forward_ms = pf_ms,
            total_ms,
            "VM restored from snapshot (in-process)"
        );

        let parts = handle.take_parts();
        Ok(VmLiveState {
            cid: config.cid,
            thread: parts.thread,
            kill_flag: parts.kill_flag,
            completion_rx: parts.completion_rx,
            serial_output: parts.serial_output,
            tmp_dir: std::path::PathBuf::new(),
            port_forward_handle,
        })
    }

    /// Run a VM synchronously to completion (blocking until exit).
    ///
    /// Used for non-detached `docker run` / `visor run` flows.
    ///
    /// # Errors
    ///
    /// Returns an error if the VM fails to boot or the run loop errors.
    async fn run_to_completion(
        &self,
        config: VmBootConfig<'_>,
    ) -> anyhow::Result<VmRunResult> {
        let pipeline_start = std::time::Instant::now();
        let guest_networks = config.run_config.effective_networks();

        let mut handle = vm::boot_vm(
            config.vm_id,
            config.run_config,
            config.rootfs_path,
            vm::VmBootSpec::new(config.memory_mib, config.vcpus, config.cid),
            vm::BootStorage::new(config.shared_dirs, &[]),
        )
        .context("boot microVM")?;
        let boot_ms = pipeline_start.elapsed().as_millis();

        // Set up port forwarding — handle stays alive until function returns.
        let _port_forward_handle =
            backend::setup_port_forwards(config.port_config, &guest_networks)?;

        // Take parts so VmHandle::Drop doesn't redundantly join.
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

        // Capture output.
        let serial_bytes = parts.serial_output.as_bytes();

        let total_ms = pipeline_start.elapsed().as_millis();
        tracing::info!(
            cid = config.cid,
            exit_code = exit_info.exit_code,
            reason = %exit_info.reason,
            boot_ms,
            total_ms,
            "VM completed (in-process)"
        );

        // Clean up temp directory.
        let _ = std::fs::remove_dir_all(&config.tmp_dir);

        Ok(VmRunResult {
            exit_info,
            serial_bytes,
        })
    }

    /// Stop a VM gracefully via vsock shutdown, then force-kill if needed.
    ///
    /// 1. Send shutdown via vsock (with timeout)
    /// 2. Wait for completion signal
    /// 3. If graceful fails, set `kill_flag`
    /// 4. Capture serial output
    /// 5. Join vCPU thread
    /// 6. Clean up port-forwarding and temp directory
    ///
    /// # Errors
    ///
    /// Returns an error if the shutdown signal fails.
    async fn stop(
        &self,
        mut state: VmLiveState,
        timeout_secs: u64,
    ) -> anyhow::Result<VmStopResult> {
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
                    tracing::warn!(
                        cid = state.cid,
                        error = %e,
                        "vsock shutdown failed, forcing stop"
                    );
                    false
                }
                Err(_) => {
                    tracing::warn!(
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
                            cid = state.cid,
                            exit_code = exit_info.exit_code,
                            reason = %exit_info.reason,
                            "VM exited after graceful shutdown"
                        );
                    }
                    Ok(Err(_)) => {
                        tracing::warn!(
                            cid = state.cid,
                            "completion channel dropped during stop"
                        );
                    }
                    Err(_) => {
                        tracing::warn!(
                            cid = state.cid,
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
                    tracing::warn!(cid = state.cid, "vCPU thread join failed: {e}");
                }
                Err(_) => {
                    tracing::warn!(
                        cid = state.cid,
                        "vCPU thread did not exit within 2s, detaching"
                    );
                }
            }
        }

        // 5. Clean up port-forwarding rules (RAII drop).
        if let Some(ref pf) = state.port_forward_handle {
            tracing::debug!(
                cid = state.cid,
                mappings = pf.mapping_count(),
                "dropping port-forward rules"
            );
        }
        drop(state.port_forward_handle.take());

        // 6. Clean up temp directory.
        let _ = std::fs::remove_dir_all(&state.tmp_dir);

        Ok(VmStopResult { serial_bytes })
    }

    /// Force-kill a VM immediately (no graceful shutdown).
    ///
    /// Sets the kill flag and joins the vCPU thread.
    ///
    /// # Errors
    ///
    /// Returns an error if the thread join fails.
    async fn kill(&self, mut state: VmLiveState) -> anyhow::Result<VmStopResult> {
        // Set kill_flag immediately.
        state
            .kill_flag
            .store(true, std::sync::atomic::Ordering::Release);

        // Capture serial output before joining thread.
        let serial_bytes = state.serial_output.as_bytes();

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
                    tracing::warn!(cid = state.cid, "vCPU thread join failed: {e}");
                }
                Err(_) => {
                    tracing::warn!(
                        cid = state.cid,
                        "vCPU thread did not exit within 2s, detaching"
                    );
                }
            }
        }

        // Clean up port-forwarding rules (RAII drop).
        if let Some(ref pf) = state.port_forward_handle {
            tracing::debug!(
                cid = state.cid,
                mappings = pf.mapping_count(),
                "dropping port-forward rules"
            );
        }
        drop(state.port_forward_handle.take());

        // Clean up temp directory.
        let _ = std::fs::remove_dir_all(&state.tmp_dir);

        Ok(VmStopResult { serial_bytes })
    }
}
