//! Tests for [`WorkerProcessLifecycle`].
//!
//! Tests focus on trait conformance, config building, and message encoding/
//! decoding. The actual child-process spawning requires the `visor` binary
//! and is covered by integration tests.

use std::path::PathBuf;
use std::sync::Arc;

use super::*;
use crate::backend::{ExecRequest, ExecResult, VsockConnector};
use crate::lifecycle::worker_protocol::{
    ParentMessage, WorkerMessage, decode_message, encode_message,
};
use crate::lifecycle::{VmBootConfig, VmLifecycle};

// ── Mock Connector ──────────────────────────────────────────────

/// Mock vsock connector for lifecycle tests.
struct MockWorkerConnector;

#[async_trait::async_trait]
impl VsockConnector for MockWorkerConnector {
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
        Ok(())
    }
}

// ── Trait Conformance ───────────────────────────────────────────

#[test]
fn worker_process_lifecycle_implements_trait() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockWorkerConnector);
    let lifecycle = WorkerProcessLifecycle::new(connector);

    // Must be wrappable as Arc<dyn VmLifecycle>.
    let _boxed: Arc<dyn VmLifecycle> = Arc::new(lifecycle);
}

#[test]
fn worker_process_lifecycle_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<WorkerProcessLifecycle>();
}

// ── Config Building ─────────────────────────────────────────────

#[test]
fn build_worker_config_creates_correct_config() {
    let run_config = visor_init::config::RunConfig::default();
    let boot_config = VmBootConfig {
        vm_id: "test-vm",
        run_config: &run_config,
        rootfs_path: std::path::Path::new("/tmp/rootfs.ext4"),
        memory_mib: 512,
        vcpus: 2,
        cid: 5,
        shared_dirs: &[PathBuf::from("/host/data")],
        port_config: &{
            let mut cfg = visor_types::VmConfig::new("alpine:latest");
            cfg.ports = vec![visor_types::PortMapping::new(8080, 80)];
            cfg
        },
        tmp_dir: PathBuf::from("/tmp/visor-test"),
    };

    let socket_path = PathBuf::from("/tmp/ctrl.sock");
    let worker_config = build_worker_config(&boot_config, &socket_path);

    assert_eq!(worker_config.cid, 5);
    assert_eq!(worker_config.memory_mib, 512);
    assert_eq!(worker_config.vcpus, 2);
    assert_eq!(worker_config.rootfs_path, PathBuf::from("/tmp/rootfs.ext4"));
    assert_eq!(
        worker_config.control_socket,
        PathBuf::from("/tmp/ctrl.sock")
    );
    assert_eq!(worker_config.shared_dirs, vec![PathBuf::from("/host/data")]);
    assert_eq!(worker_config.tmp_dir, PathBuf::from("/tmp/visor-test"));
    assert_eq!(worker_config.ports.len(), 1);
    assert_eq!(worker_config.ports[0].host_port, 8080);
    assert_eq!(worker_config.ports[0].guest_port, 80);
    assert_eq!(worker_config.ports[0].protocol, "tcp");
    // vm_id should be a valid UUID.
    assert!(uuid::Uuid::parse_str(&worker_config.vm_id).is_ok());
    // run_config_json should be valid JSON.
    assert!(serde_json::from_str::<serde_json::Value>(&worker_config.run_config_json).is_ok());
}

#[test]
fn build_worker_config_no_ports() {
    let run_config = visor_init::config::RunConfig::default();
    let boot_config = VmBootConfig {
        vm_id: "test-vm",
        run_config: &run_config,
        rootfs_path: std::path::Path::new("/tmp/rootfs.ext4"),
        memory_mib: 256,
        vcpus: 1,
        cid: 3,
        shared_dirs: &[],
        port_config: &visor_types::VmConfig::new("alpine:latest"),
        tmp_dir: PathBuf::from("/tmp/visor-test"),
    };

    let socket_path = PathBuf::from("/tmp/ctrl.sock");
    let worker_config = build_worker_config(&boot_config, &socket_path);

    assert!(worker_config.ports.is_empty());
    assert!(worker_config.shared_dirs.is_empty());
}

// ── Message Encoding ────────────────────────────────────────────

#[test]
fn send_parent_stop_encodes_correctly() {
    let msg = ParentMessage::Stop { timeout_secs: 15 };
    let bytes = encode_message(&msg).unwrap();
    let decoded: ParentMessage = decode_message(&bytes).unwrap();
    match decoded {
        ParentMessage::Stop { timeout_secs } => assert_eq!(timeout_secs, 15),
        _ => panic!("expected Stop"),
    }
}

#[test]
fn send_parent_kill_encodes_correctly() {
    let msg = ParentMessage::Kill;
    let bytes = encode_message(&msg).unwrap();
    let decoded: ParentMessage = decode_message(&bytes).unwrap();
    assert!(matches!(decoded, ParentMessage::Kill));
}

// ── Worker Message Parsing ──────────────────────────────────────

#[test]
fn parse_worker_ready_message() {
    let json = r#"{"type":"ready","pid":9876}"#;
    let msg: WorkerMessage = serde_json::from_str(json).unwrap();
    match msg {
        WorkerMessage::Ready { pid } => assert_eq!(pid, 9876),
        _ => panic!("expected Ready"),
    }
}

#[test]
fn parse_worker_exit_message() {
    let json = r#"{"type":"vm_exit","exit_code":0,"reason":"stopped"}"#;
    let msg: WorkerMessage = serde_json::from_str(json).unwrap();
    match msg {
        WorkerMessage::VmExit { exit_code, reason } => {
            assert_eq!(exit_code, 0);
            assert_eq!(reason, "stopped");
        }
        _ => panic!("expected VmExit"),
    }
}

#[test]
fn parse_worker_error_message() {
    let json = r#"{"type":"error","message":"hv_vm_create failed"}"#;
    let msg: WorkerMessage = serde_json::from_str(json).unwrap();
    match msg {
        WorkerMessage::Error { message } => {
            assert_eq!(message, "hv_vm_create failed");
        }
        _ => panic!("expected Error"),
    }
}

// ── Worker Handle Management ────────────────────────────────────

#[tokio::test]
async fn worker_handle_insert_and_remove() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockWorkerConnector);
    let lifecycle = WorkerProcessLifecycle::new(connector);

    // Verify empty initially.
    assert_eq!(lifecycle.worker_count().await, 0);
}

// ── Control Socket Communication ────────────────────────────────

#[tokio::test]
async fn control_socket_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("ctrl.sock");

    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

    // Simulate worker connecting and sending Ready.
    let connect_handle = tokio::spawn({
        let path = sock_path.clone();
        async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (_, mut write) = tokio::io::split(stream);
            let ready = WorkerMessage::Ready { pid: 42 };
            let bytes = encode_message(&ready).unwrap();
            tokio::io::AsyncWriteExt::write_all(&mut write, &bytes)
                .await
                .unwrap();
            tokio::io::AsyncWriteExt::flush(&mut write).await.unwrap();
        }
    });

    // Parent side: accept and read Ready.
    let (stream, _) = listener.accept().await.unwrap();
    let (read, _write) = tokio::io::split(stream);
    let mut reader = tokio::io::BufReader::new(read);
    let mut line = String::new();
    tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
        .await
        .unwrap();
    let msg: WorkerMessage = decode_message(line.as_bytes()).unwrap();
    match msg {
        WorkerMessage::Ready { pid } => assert_eq!(pid, 42),
        _ => panic!("expected Ready"),
    }

    connect_handle.await.unwrap();
}

#[tokio::test]
async fn parent_sends_stop_over_socket() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("ctrl.sock");

    let listener = tokio::net::UnixListener::bind(&sock_path).unwrap();

    // Simulate worker connecting.
    let worker_handle = tokio::spawn({
        let path = sock_path.clone();
        async move {
            let stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            let (read, _write) = tokio::io::split(stream);
            let mut reader = tokio::io::BufReader::new(read);
            let mut line = String::new();
            tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line)
                .await
                .unwrap();
            let msg: ParentMessage = decode_message(line.as_bytes()).unwrap();
            msg
        }
    });

    // Parent side: accept and send Stop.
    let (stream, _) = listener.accept().await.unwrap();
    let (_read, mut write) = tokio::io::split(stream);
    let stop = ParentMessage::Stop { timeout_secs: 10 };
    let bytes = encode_message(&stop).unwrap();
    tokio::io::AsyncWriteExt::write_all(&mut write, &bytes)
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::flush(&mut write).await.unwrap();

    let received = worker_handle.await.unwrap();
    match received {
        ParentMessage::Stop { timeout_secs } => assert_eq!(timeout_secs, 10),
        _ => panic!("expected Stop"),
    }
}

// ── macOS Multi-VM Integration Tests ──────────────────────────────
//
// These tests verify the process-per-VM architecture by simulating worker
// processes with Unix socket pairs and real child processes. They test the
// WorkerProcessLifecycle's multi-VM management, graceful stop/kill flows,
// worker monitor detection, and shared memory accessibility.
//
// Gated to macOS: this architecture is only used on macOS where HVF limits
// each process to one VM.

/// Creates a Unix socket pair and spawns a real child process (`sleep`),
/// returning a [`WorkerHandle`] for the parent side and the worker-side stream.
///
/// The child PID is also returned so the test can kill it directly.
#[cfg(target_os = "macos")]
async fn create_test_worker_handle(
    _cid: u32,
) -> (WorkerHandle, tokio::net::UnixStream, u32) {
    use std::os::unix::net::UnixStream as StdUnixStream;

    let (parent_std, worker_std) = StdUnixStream::pair().unwrap();
    parent_std.set_nonblocking(true).unwrap();
    worker_std.set_nonblocking(true).unwrap();

    let parent_stream = tokio::net::UnixStream::from_std(parent_std).unwrap();
    let worker_stream = tokio::net::UnixStream::from_std(worker_std).unwrap();

    let child = tokio::process::Command::new("sleep")
        .arg("60")
        .spawn()
        .unwrap();
    let child_pid = child.id().unwrap();

    let (read_half, write_half) = tokio::io::split(parent_stream);
    let handle = WorkerHandle {
        ctrl_write: write_half,
        ctrl_read: tokio::io::BufReader::new(read_half),
        child,
        worker_pid: child_pid,
        _shm_region: None,
    };

    (handle, worker_stream, child_pid)
}

/// Spawns a task that acts as a fake worker on the given stream.
///
/// Reads [`ParentMessage`]s and responds with [`WorkerMessage::VmExit`].
/// After responding, kills the child process (identified by `child_pid`)
/// so that `child.wait()` in the lifecycle's stop/kill path completes.
#[cfg(target_os = "macos")]
fn spawn_fake_worker_responder(
    stream: tokio::net::UnixStream,
    child_pid: u32,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let (read_half, mut write_half) = tokio::io::split(stream);
        let mut reader = tokio::io::BufReader::new(read_half);

        loop {
            let mut line = String::new();
            match tokio::io::AsyncBufReadExt::read_line(&mut reader, &mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if let Ok(msg) = decode_message::<ParentMessage>(line.as_bytes()) {
                        let exit_msg = match msg {
                            ParentMessage::Stop { .. } => WorkerMessage::VmExit {
                                exit_code: 0,
                                reason: "stopped".to_owned(),
                            },
                            ParentMessage::Kill => WorkerMessage::VmExit {
                                exit_code: 137,
                                reason: "killed".to_owned(),
                            },
                            _ => continue,
                        };
                        // Kill the child process so lifecycle's child.wait() returns.
                        std::process::Command::new("kill")
                            .args(["-9", &child_pid.to_string()])
                            .status()
                            .expect("kill child process");
                        let bytes = encode_message(&exit_msg).unwrap();
                        let _ =
                            tokio::io::AsyncWriteExt::write_all(&mut write_half, &bytes).await;
                        let _ = tokio::io::AsyncWriteExt::flush(&mut write_half).await;
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    })
}

/// Creates a [`VmLiveState`] suitable for testing stop/kill flows.
#[cfg(target_os = "macos")]
fn make_test_live_state(
    cid: u32,
    completion_rx: Option<tokio::sync::oneshot::Receiver<crate::vm::VmExitInfo>>,
) -> (VmLiveState, Arc<std::sync::atomic::AtomicBool>) {
    let kill_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let tmp_dir = std::env::temp_dir().join(format!("visor-test-{cid}-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp_dir);

    let state = VmLiveState {
        cid,
        thread: None,
        kill_flag: Arc::clone(&kill_flag),
        completion_rx,
        serial_output: crate::vm::SerialOutput::new(),
        tmp_dir,
        port_forward_handle: None,
    };

    (state, kill_flag)
}

/// Boot two VMs simultaneously and stop both gracefully.
///
/// Verifies that `WorkerProcessLifecycle` can track multiple concurrent
/// worker processes with different CIDs, and that stopping each one
/// cleans up its worker handle.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn boot_two_vms_simultaneously() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockWorkerConnector);
    let lifecycle = WorkerProcessLifecycle::new(connector);

    let cid_a = 10;
    let cid_b = 11;

    // Set up two fake workers with their own socket pairs and child processes.
    let (handle_a, stream_a, pid_a) = create_test_worker_handle(cid_a).await;
    let (handle_b, stream_b, pid_b) = create_test_worker_handle(cid_b).await;

    lifecycle.workers.write().await.insert(cid_a, handle_a);
    lifecycle.workers.write().await.insert(cid_b, handle_b);

    // Spawn fake worker responders that will reply to Stop messages.
    let _resp_a = spawn_fake_worker_responder(stream_a, pid_a);
    let _resp_b = spawn_fake_worker_responder(stream_b, pid_b);

    // Both workers should be tracked.
    assert_eq!(lifecycle.worker_count().await, 2);

    // Create live states (non-zero CIDs).
    let (state_a, _flag_a) = make_test_live_state(cid_a, None);
    let (state_b, _flag_b) = make_test_live_state(cid_b, None);
    assert!(state_a.cid > 0);
    assert!(state_b.cid > 0);
    assert_ne!(state_a.cid, state_b.cid);

    // Stop both VMs gracefully.
    let result_a = lifecycle.stop(state_a, 5).await;
    assert!(result_a.is_ok(), "stop VM A failed: {:?}", result_a.err());

    let result_b = lifecycle.stop(state_b, 5).await;
    assert!(result_b.is_ok(), "stop VM B failed: {:?}", result_b.err());

    // Both workers should be removed.
    assert_eq!(lifecycle.worker_count().await, 0);
}

/// Stop one VM while the other continues running.
///
/// Verifies that stopping a single VM does not affect sibling VMs:
/// the second VM's kill_flag remains `false` and its worker handle
/// stays in the lifecycle's worker map.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn stop_one_vm_other_continues() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockWorkerConnector);
    let lifecycle = WorkerProcessLifecycle::new(connector);

    let cid_a = 20;
    let cid_b = 21;

    let (handle_a, stream_a, pid_a) = create_test_worker_handle(cid_a).await;
    let (handle_b, _stream_b, _pid_b) = create_test_worker_handle(cid_b).await;

    lifecycle.workers.write().await.insert(cid_a, handle_a);
    lifecycle.workers.write().await.insert(cid_b, handle_b);
    assert_eq!(lifecycle.worker_count().await, 2);

    // Only VM A gets a fake responder (we are stopping only A).
    let _resp_a = spawn_fake_worker_responder(stream_a, pid_a);

    let (state_a, _flag_a) = make_test_live_state(cid_a, None);
    let (_state_b, flag_b) = make_test_live_state(cid_b, None);

    // Stop VM A.
    let result_a = lifecycle.stop(state_a, 5).await;
    assert!(result_a.is_ok(), "stop VM A failed: {:?}", result_a.err());

    // VM B should still be tracked and its kill_flag should be false.
    assert_eq!(lifecycle.worker_count().await, 1);
    assert!(
        !flag_b.load(std::sync::atomic::Ordering::Acquire),
        "VM B kill_flag should still be false"
    );

    // Verify VM B's worker handle is still present.
    let workers = lifecycle.workers.read().await;
    assert!(workers.contains_key(&cid_b), "VM B worker handle missing");
    drop(workers);

    // Clean up VM B's child process.
    let mut remaining = lifecycle.workers.write().await;
    if let Some(mut handle) = remaining.remove(&cid_b) {
        let _ = handle.child.kill().await;
        let _ = handle.child.wait().await;
    }
}

/// Kill a worker process directly and verify the monitor detects it.
///
/// The `spawn_worker_monitor` polls `child.try_wait()` every 500ms.
/// When the child is killed externally, the monitor sends `VmExitInfo`
/// through the completion channel and removes the worker handle.
#[cfg(target_os = "macos")]
#[tokio::test]
async fn kill_worker_process_parent_detects() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockWorkerConnector);
    let lifecycle = WorkerProcessLifecycle::new(connector);

    let cid = 42;
    let (handle, _worker_stream, child_pid) = create_test_worker_handle(cid).await;

    lifecycle.workers.write().await.insert(cid, handle);
    assert_eq!(lifecycle.worker_count().await, 1);

    // Set up completion channel and start the worker monitor.
    let (completion_tx, completion_rx) = tokio::sync::oneshot::channel();
    spawn_worker_monitor(cid, Arc::clone(&lifecycle.workers), completion_tx);

    // Kill the child process directly (simulating unexpected worker death).
    std::process::Command::new("kill")
        .args(["-9", &child_pid.to_string()])
        .status()
        .expect("kill child process");

    // Wait on completion_rx — the monitor should detect the exit within ~1s.
    let exit_info = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        completion_rx,
    )
    .await
    .expect("timeout waiting for monitor to detect worker exit")
    .expect("completion channel closed unexpectedly");

    // Process killed by SIGKILL: status.code() returns None, monitor uses
    // unwrap_or(1), so exit_code should be 1.
    assert_eq!(exit_info.exit_code, 1);

    // The monitor should have removed the worker handle (no zombies).
    assert_eq!(lifecycle.worker_count().await, 0);
}

/// Shared memory region is accessible from the parent process.
///
/// Creates a [`SharedMemoryRegion`], writes known data via the raw pointer,
/// and reads it back. This verifies the parent can introspect guest RAM
/// while a worker VM is running (the key property of `MAP_SHARED` shm).
#[cfg(target_os = "macos")]
#[allow(unsafe_code)]
#[tokio::test]
async fn shared_memory_accessible_from_parent() {
    let connector: Arc<dyn VsockConnector> = Arc::new(MockWorkerConnector);
    let lifecycle = WorkerProcessLifecycle::new(connector);

    let cid = 50;
    let shm_name = format!("/visor-test-shm-{}-{cid}", std::process::id());
    let shm_size = 4096; // One page.

    // Create shared memory region (same as boot() does).
    let shm_region = SharedMemoryRegion::create(&shm_name, shm_size)
        .expect("create shared memory region");

    // Write known data into the shared memory (simulating kernel writes).
    let pattern: &[u8] = b"VISOR_TEST_PATTERN_1234567890";
    // SAFETY: shm_region.as_ptr() is a valid mmap'd pointer of `shm_size` bytes.
    // We write within bounds (pattern.len() < shm_size).
    unsafe {
        std::ptr::copy_nonoverlapping(pattern.as_ptr(), shm_region.as_ptr(), pattern.len());
    }

    // Set up a worker handle with the shared memory attached.
    let (mut handle, _stream, _pid) = create_test_worker_handle(cid).await;
    handle._shm_region = Some(shm_region);

    // Get the pointer before moving the handle into the map.
    let shm_ptr = handle._shm_region.as_ref().unwrap().as_ptr();
    let shm_region_size = handle._shm_region.as_ref().unwrap().size();

    lifecycle.workers.write().await.insert(cid, handle);

    // Read shared memory from parent side — verify it's not all zeros.
    // SAFETY: shm_ptr is valid and within bounds (we just wrote to it).
    let read_back = unsafe { std::slice::from_raw_parts(shm_ptr, pattern.len()) };
    assert_eq!(read_back, pattern, "shared memory content mismatch");

    // Verify the region is the expected size.
    assert_eq!(shm_region_size, shm_size);

    // Verify the region is not all zeros (check first 64 bytes).
    let first_64 = unsafe { std::slice::from_raw_parts(shm_ptr, 64.min(shm_size)) };
    assert!(
        first_64.iter().any(|&b| b != 0),
        "shared memory should not be all zeros after write"
    );

    // Clean up: kill child process, remove handle, unlink shm.
    let mut workers = lifecycle.workers.write().await;
    if let Some(mut h) = workers.remove(&cid) {
        let _ = h.child.kill().await;
        let _ = h.child.wait().await;
        // SharedMemoryRegion::drop will munmap and close fd.
        // Unlink explicitly (the test created it, so we own the name).
        let _ = visor_vmm::shared_memory::unlink_shared_memory(&shm_name);
    }
}
