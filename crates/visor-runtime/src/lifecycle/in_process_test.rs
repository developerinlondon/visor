//! Tests for [`InProcessLifecycle`].
//!
//! These tests verify trait conformance and construction. The actual
//! boot/stop/kill paths require a hypervisor and are covered by
//! integration tests on the target machine.

use std::sync::Arc;

use super::*;
use crate::backend::{ExecRequest, ExecResult, VsockConnector};

/// Mock vsock connector for lifecycle tests.
struct MockLifecycleConnector {
    shutdown_ok: bool,
}

impl MockLifecycleConnector {
    fn new() -> Self {
        Self { shutdown_ok: true }
    }
}

#[async_trait::async_trait]
impl VsockConnector for MockLifecycleConnector {
    async fn exec_cmd(&self, _cid: u32, _req: &ExecRequest) -> anyhow::Result<ExecResult> {
        Ok(ExecResult::new(0, String::new(), String::new()))
    }

    async fn exec_stream_cmd(
        &self,
        _cid: u32,
        _req: &ExecRequest,
    ) -> anyhow::Result<Box<dyn crate::backend::AsyncIoStream>> {
        anyhow::bail!("mock does not support exec_stream")
    }

    async fn copy_to_guest(
        &self,
        _cid: u32,
        _archive: &[u8],
        _dest: &str,
    ) -> anyhow::Result<()> {
        Ok(())
    }

    async fn shutdown(&self, _cid: u32) -> anyhow::Result<()> {
        if self.shutdown_ok {
            Ok(())
        } else {
            anyhow::bail!("mock shutdown failure")
        }
    }
}

#[test]
fn in_process_lifecycle_implements_vm_lifecycle_trait() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockLifecycleConnector::new());
    let lifecycle = InProcessLifecycle::new(connector);

    // Verify it can be wrapped in Arc<dyn VmLifecycle>.
    let _boxed: Arc<dyn VmLifecycle> = Arc::new(lifecycle);
}

#[test]
fn create_lifecycle_returns_arc_dyn_vm_lifecycle() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockLifecycleConnector::new());
    let lifecycle = super::super::create_lifecycle(connector);

    // Should be usable as Arc<dyn VmLifecycle>.
    let _: &dyn VmLifecycle = lifecycle.as_ref();
}

#[test]
fn vm_boot_config_is_constructible() {
    let run_config = visor_init::config::RunConfig::default();
    let _config = VmBootConfig {
        vm_id: "test-vm",
        run_config: &run_config,
        rootfs_path: std::path::Path::new("/tmp/rootfs.ext4"),
        memory_mib: 512,
        vcpus: 1,
        cid: 3,
        shared_dirs: &[],
        port_config: &visor_types::VmConfig::new("alpine:latest"),
        tmp_dir: std::path::PathBuf::from("/tmp/visor-test"),
    };
}

#[test]
fn vm_snapshot_config_is_constructible() {
    let _config = VmSnapshotConfig {
        vm_id: "test-vm",
        snapshot_dir: std::path::Path::new("/tmp/snapshot"),
        memory_mib: 512,
        vcpus: 1,
        cid: 3,
        shared_dirs: &[],
        port_config: &visor_types::VmConfig::new("alpine:latest"),
    };
}

#[test]
fn vm_run_result_is_constructible() {
    let _result = VmRunResult {
        exit_info: crate::vm::VmExitInfo {
            exit_code: 0,
            reason: crate::vm::VmExitReason::Shutdown,
        },
        serial_bytes: vec![],
    };
}

#[test]
fn vm_stop_result_is_constructible() {
    let _result = VmStopResult {
        serial_bytes: vec![1, 2, 3],
    };
}

#[tokio::test]
async fn kill_sets_kill_flag_and_returns_serial_output() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockLifecycleConnector::new());
    let lifecycle = InProcessLifecycle::new(connector);

    // Create a VmLiveState with no real thread — simulates a VM that
    // has already exited but whose state hasn't been cleaned up.
    let state = crate::backend::VmLiveState {
        cid: 99,
        thread: None,
        kill_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        completion_rx: None,
        serial_output: crate::vm::SerialOutput::new(),
        tmp_dir: std::path::PathBuf::new(),
        port_forward_handle: None,
    };

    let result = lifecycle.kill(state).await.unwrap();
    assert!(result.serial_bytes.is_empty());
}

#[tokio::test]
async fn stop_with_zero_timeout_sets_kill_flag_immediately() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockLifecycleConnector::new());
    let lifecycle = InProcessLifecycle::new(connector);

    let kill_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let kill_flag_clone = Arc::clone(&kill_flag);

    let state = crate::backend::VmLiveState {
        cid: 99,
        thread: None,
        kill_flag: kill_flag_clone,
        completion_rx: None,
        serial_output: crate::vm::SerialOutput::new(),
        tmp_dir: std::path::PathBuf::new(),
        port_forward_handle: None,
    };

    let _result = lifecycle.stop(state, 0).await.unwrap();

    // Kill flag should have been set because timeout_secs == 0.
    assert!(
        kill_flag.load(std::sync::atomic::Ordering::Acquire),
        "kill_flag should be set when timeout is 0"
    );
}
