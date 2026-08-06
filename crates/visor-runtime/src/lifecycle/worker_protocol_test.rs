//! Tests for the worker control protocol.

use super::*;

#[test]
fn vm_worker_config_roundtrip() {
    let config = VmWorkerConfig {
        vm_id: "test-vm-123".to_owned(),
        cid: 3,
        memory_mib: 512,
        vcpus: 1,
        rootfs_path: PathBuf::from("/tmp/visor-test/rootfs.ext4"),
        run_config_json: r#"{"cmd":["/bin/echo","hello"]}"#.to_owned(),
        shared_dirs: vec![PathBuf::from("/host/data")],
        control_socket: PathBuf::from("/tmp/visor-worker-test.sock"),
        ports: vec![WorkerPortMapping {
            host_port: 8080,
            guest_port: 80,
            protocol: "tcp".to_owned(),
        }],
        tmp_dir: PathBuf::from("/tmp/visor-test"),
        shm_name: None,
    };

    let json = serde_json::to_string(&config).unwrap();
    let decoded: VmWorkerConfig = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded.vm_id, "test-vm-123");
    assert_eq!(decoded.cid, 3);
    assert_eq!(decoded.memory_mib, 512);
    assert_eq!(decoded.vcpus, 1);
    assert_eq!(
        decoded.rootfs_path,
        PathBuf::from("/tmp/visor-test/rootfs.ext4")
    );
    assert_eq!(decoded.shared_dirs.len(), 1);
    assert_eq!(
        decoded.control_socket,
        PathBuf::from("/tmp/visor-worker-test.sock")
    );
    assert_eq!(decoded.ports.len(), 1);
    assert_eq!(decoded.ports[0].host_port, 8080);
    assert_eq!(decoded.ports[0].guest_port, 80);
}

#[test]
fn vm_worker_config_defaults() {
    let json = r#"{
        "vm_id": "x",
        "cid": 5,
        "memory_mib": 256,
        "vcpus": 2,
        "rootfs_path": "/tmp/r.ext4",
        "run_config_json": "{}",
        "control_socket": "/tmp/ctrl.sock",
        "tmp_dir": "/tmp/t"
    }"#;

    let config: VmWorkerConfig = serde_json::from_str(json).unwrap();
    assert!(config.shared_dirs.is_empty());
    assert!(config.ports.is_empty());
}

#[test]
fn parent_message_stop_serialize() {
    let msg = ParentMessage::Stop { timeout_secs: 5 };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"stop""#));
    assert!(json.contains(r#""timeout_secs":5"#));

    let decoded: ParentMessage = serde_json::from_str(&json).unwrap();
    match decoded {
        ParentMessage::Stop { timeout_secs } => assert_eq!(timeout_secs, 5),
        _ => panic!("expected Stop"),
    }
}

#[test]
fn parent_message_stop_default_timeout() {
    let json = r#"{"type":"stop"}"#;
    let msg: ParentMessage = serde_json::from_str(json).unwrap();
    match msg {
        ParentMessage::Stop { timeout_secs } => assert_eq!(timeout_secs, 10),
        _ => panic!("expected Stop"),
    }
}

#[test]
fn parent_message_kill_serialize() {
    let msg = ParentMessage::Kill;
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"kill""#));

    let decoded: ParentMessage = serde_json::from_str(&json).unwrap();
    assert!(matches!(decoded, ParentMessage::Kill));
}

#[test]
fn parent_message_exec_serialize() {
    let msg = ParentMessage::Exec {
        cmd: vec!["ls".to_owned(), "-la".to_owned()],
        env: vec!["FOO=bar".to_owned()],
        working_dir: "/tmp".to_owned(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"exec""#));

    let decoded: ParentMessage = serde_json::from_str(&json).unwrap();
    match decoded {
        ParentMessage::Exec {
            cmd,
            env,
            working_dir,
        } => {
            assert_eq!(cmd, vec!["ls", "-la"]);
            assert_eq!(env, vec!["FOO=bar"]);
            assert_eq!(working_dir, "/tmp");
        }
        _ => panic!("expected Exec"),
    }
}

#[test]
fn parent_message_exec_defaults() {
    let json = r#"{"type":"exec","cmd":["whoami"]}"#;
    let msg: ParentMessage = serde_json::from_str(json).unwrap();
    match msg {
        ParentMessage::Exec {
            cmd,
            env,
            working_dir,
        } => {
            assert_eq!(cmd, vec!["whoami"]);
            assert!(env.is_empty());
            assert_eq!(working_dir, "/");
        }
        _ => panic!("expected Exec"),
    }
}

#[test]
fn worker_message_ready_serialize() {
    let msg = WorkerMessage::Ready { pid: 1234 };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"ready""#));
    assert!(json.contains(r#""pid":1234"#));

    let decoded: WorkerMessage = serde_json::from_str(&json).unwrap();
    match decoded {
        WorkerMessage::Ready { pid } => assert_eq!(pid, 1234),
        _ => panic!("expected Ready"),
    }
}

#[test]
fn worker_message_vm_exit_serialize() {
    let msg = WorkerMessage::VmExit {
        exit_code: 0,
        reason: "shutdown".to_owned(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"vm_exit""#));

    let decoded: WorkerMessage = serde_json::from_str(&json).unwrap();
    match decoded {
        WorkerMessage::VmExit { exit_code, reason } => {
            assert_eq!(exit_code, 0);
            assert_eq!(reason, "shutdown");
        }
        _ => panic!("expected VmExit"),
    }
}

#[test]
fn worker_message_exec_result_serialize() {
    let msg = WorkerMessage::ExecResult {
        exit_code: 0,
        stdout: "hello\n".to_owned(),
        stderr: String::new(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"exec_result""#));

    let decoded: WorkerMessage = serde_json::from_str(&json).unwrap();
    match decoded {
        WorkerMessage::ExecResult {
            exit_code,
            stdout,
            stderr,
        } => {
            assert_eq!(exit_code, 0);
            assert_eq!(stdout, "hello\n");
            assert!(stderr.is_empty());
        }
        _ => panic!("expected ExecResult"),
    }
}

#[test]
fn worker_message_error_serialize() {
    let msg = WorkerMessage::Error {
        message: "hv_vm_create failed".to_owned(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains(r#""type":"error""#));

    let decoded: WorkerMessage = serde_json::from_str(&json).unwrap();
    match decoded {
        WorkerMessage::Error { message } => {
            assert_eq!(message, "hv_vm_create failed");
        }
        _ => panic!("expected Error"),
    }
}

#[test]
fn encode_decode_roundtrip() {
    let msg = WorkerMessage::Ready { pid: 42 };
    let bytes = encode_message(&msg).unwrap();

    // Verify newline termination.
    assert_eq!(*bytes.last().unwrap(), b'\n');

    let decoded: WorkerMessage = decode_message(&bytes).unwrap();
    match decoded {
        WorkerMessage::Ready { pid } => assert_eq!(pid, 42),
        _ => panic!("expected Ready"),
    }
}

#[test]
fn encode_decode_config_roundtrip() {
    let config = VmWorkerConfig {
        vm_id: "roundtrip".to_owned(),
        cid: 7,
        memory_mib: 1024,
        vcpus: 4,
        rootfs_path: PathBuf::from("/rootfs.ext4"),
        run_config_json: r#"{"cmd":["/bin/sh"]}"#.to_owned(),
        shared_dirs: Vec::new(),
        control_socket: PathBuf::from("/tmp/ctrl.sock"),
        ports: Vec::new(),
        tmp_dir: PathBuf::from("/tmp"),
        shm_name: None,
    };

    let bytes = encode_message(&config).unwrap();
    let decoded: VmWorkerConfig = decode_message(&bytes).unwrap();
    assert_eq!(decoded.vm_id, "roundtrip");
    assert_eq!(decoded.cid, 7);
    assert_eq!(decoded.memory_mib, 1024);
    assert_eq!(decoded.vcpus, 4);
}

#[test]
fn decode_invalid_json_returns_error() {
    let result = decode_message::<WorkerMessage>(b"not json");
    assert!(result.is_err());
}

#[test]
fn decode_invalid_utf8_returns_error() {
    let result = decode_message::<WorkerMessage>(&[0xFF, 0xFE]);
    assert!(result.is_err());
}

#[test]
fn worker_port_mapping_default_protocol() {
    let json = r#"{"host_port":8080,"guest_port":80}"#;
    let pm: WorkerPortMapping = serde_json::from_str(json).unwrap();
    assert_eq!(pm.protocol, "tcp");
}
