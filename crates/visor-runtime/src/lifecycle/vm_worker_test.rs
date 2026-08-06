//! Tests for the VM worker entry point.
//!
//! Tests focus on protocol handling, config parsing, and control socket
//! message dispatch — the actual VM boot path requires a hypervisor and
//! is covered by integration tests.

use std::path::PathBuf;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;

use crate::lifecycle::worker_protocol::{
    ParentMessage, VmWorkerConfig, WorkerMessage, WorkerPortMapping, decode_message,
    encode_message,
};

use super::{WorkerAction, read_worker_config, handle_parent_message, send_worker_message};

// ── Helpers ──────────────────────────────────────────────────────

/// Creates a minimal `VmWorkerConfig` for tests.
fn test_config(control_socket: PathBuf) -> VmWorkerConfig {
    VmWorkerConfig {
        vm_id: "test-vm-001".to_owned(),
        cid: 3,
        memory_mib: 512,
        vcpus: 1,
        rootfs_path: PathBuf::from("/tmp/visor-test/rootfs.ext4"),
        run_config_json: r#"{"cmd":["/bin/echo","hello"]}"#.to_owned(),
        shared_dirs: vec![],
        control_socket,
        ports: vec![WorkerPortMapping {
            host_port: 8080,
            guest_port: 80,
            protocol: "tcp".to_owned(),
        }],
        tmp_dir: PathBuf::from("/tmp/visor-test"),
        shm_name: None,
    }
}

// ── Config Parsing Tests ─────────────────────────────────────────

#[tokio::test]
async fn read_worker_config_parses_valid_json() {
    let config = test_config(PathBuf::from("/tmp/test.sock"));
    let json_bytes = encode_message(&config).unwrap();

    let parsed = read_worker_config(&json_bytes[..]).await.unwrap();

    assert_eq!(parsed.vm_id, "test-vm-001");
    assert_eq!(parsed.cid, 3);
    assert_eq!(parsed.memory_mib, 512);
    assert_eq!(parsed.vcpus, 1);
    assert_eq!(
        parsed.rootfs_path,
        PathBuf::from("/tmp/visor-test/rootfs.ext4")
    );
    assert_eq!(parsed.ports.len(), 1);
    assert_eq!(parsed.ports[0].host_port, 8080);
}

#[tokio::test]
async fn read_worker_config_rejects_invalid_json() {
    let bad_input = b"not valid json\n";
    let result = read_worker_config(&bad_input[..]).await;
    assert!(result.is_err());
    let err_msg = format!("{:#}", result.unwrap_err());
    assert!(
        err_msg.contains("deserialize"),
        "error should mention deserialization: {err_msg}"
    );
}

#[tokio::test]
async fn read_worker_config_rejects_empty_input() {
    let empty: &[u8] = b"";
    let result = read_worker_config(empty).await;
    assert!(result.is_err());
    let err_msg = format!("{:#}", result.unwrap_err());
    assert!(
        err_msg.contains("empty") || err_msg.contains("stdin"),
        "error should mention empty/stdin: {err_msg}"
    );
}

#[tokio::test]
async fn read_worker_config_with_minimal_fields() {
    let json = r#"{"vm_id":"x","cid":5,"memory_mib":256,"vcpus":2,"rootfs_path":"/r.ext4","run_config_json":"{}","control_socket":"/s.sock","tmp_dir":"/tmp/t"}"#;
    let mut input = json.as_bytes().to_vec();
    input.push(b'\n');

    let parsed = read_worker_config(&input[..]).await.unwrap();
    assert_eq!(parsed.vm_id, "x");
    assert_eq!(parsed.cid, 5);
    assert!(parsed.shared_dirs.is_empty());
    assert!(parsed.ports.is_empty());
}

// ── Run Config Parsing Tests ─────────────────────────────────────

#[test]
fn run_config_json_parses_correctly() {
    let json = r#"{"cmd":["/bin/echo","hello"],"env":["FOO=bar"],"workdir":"/app"}"#;
    let config: visor_init::config::RunConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.cmd, vec!["/bin/echo", "hello"]);
    assert_eq!(config.env, vec!["FOO=bar"]);
    assert_eq!(config.workdir, "/app");
}

#[test]
fn run_config_json_rejects_invalid() {
    let result = serde_json::from_str::<visor_init::config::RunConfig>("not json");
    assert!(result.is_err());
}

// ── Control Socket Message Tests ─────────────────────────────────

#[tokio::test]
async fn send_worker_message_writes_newline_delimited_json() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("test.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();

    let msg = WorkerMessage::Ready { pid: 42 };

    // Connect and send
    let send_task = tokio::spawn({
        let sock_path = sock_path.clone();
        async move {
            let stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
            let (_, write_half) = tokio::io::split(stream);
            send_worker_message(&msg, write_half).await.unwrap();
        }
    });

    // Accept and read
    let (stream, _) = listener.accept().await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    send_task.await.unwrap();

    let decoded: WorkerMessage = decode_message(line.as_bytes()).unwrap();
    match decoded {
        WorkerMessage::Ready { pid } => assert_eq!(pid, 42),
        _ => panic!("expected Ready message"),
    }
}

#[tokio::test]
async fn handle_parent_message_stop_returns_stop_action() {
    let msg = ParentMessage::Stop { timeout_secs: 5 };
    let action = handle_parent_message(&msg);
    match action {
        WorkerAction::Stop { timeout_secs } => assert_eq!(timeout_secs, 5),
        _ => panic!("expected Stop action, got {:?}", action),
    }
}

#[tokio::test]
async fn handle_parent_message_kill_returns_kill_action() {
    let msg = ParentMessage::Kill;
    let action = handle_parent_message(&msg);
    assert!(
        matches!(action, WorkerAction::Kill),
        "expected Kill action"
    );
}

#[tokio::test]
async fn handle_parent_message_exec_returns_exec_action() {
    let msg = ParentMessage::Exec {
        cmd: vec!["ls".to_owned(), "-la".to_owned()],
        env: vec!["FOO=bar".to_owned()],
        working_dir: "/tmp".to_owned(),
    };
    let action = handle_parent_message(&msg);
    match action {
        WorkerAction::Exec {
            cmd,
            env,
            working_dir,
        } => {
            assert_eq!(cmd, vec!["ls", "-la"]);
            assert_eq!(env, vec!["FOO=bar"]);
            assert_eq!(working_dir, "/tmp");
        }
        _ => panic!("expected Exec action, got {:?}", action),
    }
}

// ── Control Socket Roundtrip Tests ───────────────────────────────

#[tokio::test]
async fn control_socket_parent_to_worker_roundtrip() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("ctrl.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();

    let parent_msg = ParentMessage::Stop { timeout_secs: 7 };
    let msg_bytes = encode_message(&parent_msg).unwrap();

    // Parent sends message
    let send_task = tokio::spawn({
        let sock_path = sock_path.clone();
        let msg_bytes = msg_bytes.clone();
        async move {
            let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
            stream.write_all(&msg_bytes).await.unwrap();
            stream.flush().await.unwrap();
        }
    });

    // Worker reads message
    let (stream, _) = listener.accept().await.unwrap();
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    send_task.await.unwrap();

    let decoded: ParentMessage = decode_message(line.as_bytes()).unwrap();
    match decoded {
        ParentMessage::Stop { timeout_secs } => assert_eq!(timeout_secs, 7),
        _ => panic!("expected Stop"),
    }
}

#[tokio::test]
async fn control_socket_multiple_messages() {
    let tmp = tempfile::tempdir().unwrap();
    let sock_path = tmp.path().join("multi.sock");
    let listener = UnixListener::bind(&sock_path).unwrap();

    let messages = vec![
        ParentMessage::Exec {
            cmd: vec!["whoami".to_owned()],
            env: vec![],
            working_dir: "/".to_owned(),
        },
        ParentMessage::Stop { timeout_secs: 3 },
        ParentMessage::Kill,
    ];

    // Send all messages
    let send_task = tokio::spawn({
        let sock_path = sock_path.clone();
        let messages = messages.clone();
        async move {
            let mut stream = tokio::net::UnixStream::connect(&sock_path).await.unwrap();
            for msg in &messages {
                let bytes = encode_message(msg).unwrap();
                stream.write_all(&bytes).await.unwrap();
            }
            stream.flush().await.unwrap();
        }
    });

    // Read all messages
    let (stream, _) = listener.accept().await.unwrap();
    let mut reader = BufReader::new(stream);

    let mut decoded_messages = Vec::new();
    for _ in 0..3 {
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        let msg: ParentMessage = decode_message(line.as_bytes()).unwrap();
        decoded_messages.push(msg);
    }

    send_task.await.unwrap();

    assert!(matches!(&decoded_messages[0], ParentMessage::Exec { .. }));
    assert!(matches!(&decoded_messages[1], ParentMessage::Stop { .. }));
    assert!(matches!(&decoded_messages[2], ParentMessage::Kill));
}

// ── Error Path Tests ─────────────────────────────────────────────

#[tokio::test]
async fn malformed_parent_message_detected() {
    let bad_json = b"{ invalid json }\n";
    let result = decode_message::<ParentMessage>(bad_json);
    assert!(result.is_err());
}

#[test]
fn worker_action_debug_format() {
    let action = WorkerAction::Stop { timeout_secs: 10 };
    let debug = format!("{action:?}");
    assert!(debug.contains("Stop"));
    assert!(debug.contains("10"));
}

#[test]
fn worker_action_all_variants_constructible() {
    let _stop = WorkerAction::Stop { timeout_secs: 5 };
    let _kill = WorkerAction::Kill;
    let _exec = WorkerAction::Exec {
        cmd: vec!["test".to_owned()],
        env: vec![],
        working_dir: "/".to_owned(),
    };
}
