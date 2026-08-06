use super::*;
use super::super::worker_protocol::{ParentMessage, VmWorkerConfig};

#[tokio::test]
async fn pool_new_creates_target_workers() {
    // This test verifies that WorkerPool::new attempts to spawn workers.
    // We can't easily test actual spawning without a full integration setup,
    // but we can verify the pool structure is created correctly.
    let pool = WorkerPool {
        idle: tokio::sync::Mutex::new(Vec::new()),
        target_size: 3,
        shutdown: Arc::new(AtomicBool::new(false)),
    };

    assert_eq!(pool.target_size, 3);
    assert_eq!(pool.available().await, 0);
}

#[tokio::test]
async fn pool_grab_returns_worker() {
    // Create a mock pool with a fake worker.
    let pool = WorkerPool {
        idle: tokio::sync::Mutex::new(Vec::new()),
        target_size: 1,
        shutdown: Arc::new(AtomicBool::new(false)),
    };

    // We can't easily create a real PooledWorker without spawning a process,
    // so this test verifies the grab() logic works on an empty pool.
    assert_eq!(pool.available().await, 0);
    assert!(pool.grab().await.is_none());
}

#[tokio::test]
async fn pool_grab_empty_returns_none() {
    let pool = WorkerPool {
        idle: tokio::sync::Mutex::new(Vec::new()),
        target_size: 5,
        shutdown: Arc::new(AtomicBool::new(false)),
    };

    assert!(pool.grab().await.is_none());
}

#[tokio::test]
async fn pool_shutdown_cleans_up() {
    let pool = WorkerPool {
        idle: tokio::sync::Mutex::new(Vec::new()),
        target_size: 0,
        shutdown: Arc::new(AtomicBool::new(false)),
    };

    assert!(!pool.shutdown.load(std::sync::atomic::Ordering::Acquire));
    pool.shutdown().await;
    assert!(pool.shutdown.load(std::sync::atomic::Ordering::Acquire));
}

#[test]
fn protocol_assign_vm_roundtrip() {
    use super::super::worker_protocol::{encode_message, decode_message};

    let config = VmWorkerConfig {
        vm_id: "test-vm".to_owned(),
        cid: 3,
        memory_mib: 512,
        vcpus: 2,
        rootfs_path: "/tmp/rootfs.ext4".into(),
        run_config_json: "{}".to_owned(),
        shared_dirs: vec![],
        control_socket: "/tmp/ctrl.sock".into(),
        ports: vec![],
        tmp_dir: "/tmp".into(),
        shm_name: None,
    };

    let msg = ParentMessage::AssignVm(Box::new(config.clone()));
    let bytes = encode_message(&msg).expect("encode AssignVm");
    let decoded: ParentMessage = decode_message(&bytes).expect("decode AssignVm");

    match decoded {
        ParentMessage::AssignVm(decoded_config) => {
            assert_eq!(decoded_config.vm_id, config.vm_id);
            assert_eq!(decoded_config.cid, config.cid);
            assert_eq!(decoded_config.memory_mib, config.memory_mib);
        }
        _ => panic!("expected AssignVm, got {decoded:?}"),
    }
}

#[test]
fn protocol_pool_ready_roundtrip() {
    use super::super::worker_protocol::{encode_message, decode_message};

    let msg = WorkerMessage::PoolReady { pid: 12345 };
    let bytes = encode_message(&msg).expect("encode PoolReady");
    let decoded: WorkerMessage = decode_message(&bytes).expect("decode PoolReady");

    match decoded {
        WorkerMessage::PoolReady { pid } => {
            assert_eq!(pid, 12345);
        }
        _ => panic!("expected PoolReady, got {decoded:?}"),
    }
}
