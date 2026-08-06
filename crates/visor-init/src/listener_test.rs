use super::*;

use crate::agent::{INTERNAL_ERROR, JsonRpcResponse, METHOD_NOT_FOUND, PARSE_ERROR};
use crate::config::RunConfig;
use crate::testutil::tempdir;

/// Create a default [`AgentState`] for testing.
fn test_state() -> AgentState {
    AgentState::new(RunConfig::default())
}

// ── Ping tests ──────────────────────────────────────────────────────────────

#[test]
fn handle_ping_returns_pong() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"ping","id":1}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert_eq!(parsed.jsonrpc, "2.0");
    assert_eq!(parsed.result, Some(serde_json::json!("pong")));
    assert!(parsed.error.is_none());
    assert_eq!(parsed.id, serde_json::json!(1));
}

// ── Exec tests ──────────────────────────────────────────────────────────────

#[test]
fn handle_exec_echo_returns_stdout() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"exec","params":{"cmd":["echo","hello"],"env":[],"workdir":"/"},"id":2}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_none(),
        "unexpected error: {:?}",
        parsed.error
    );
    let result = parsed.result.unwrap();
    assert_eq!(result["exit_code"], 0);
    assert!(
        result["stdout"].as_str().unwrap().contains("hello"),
        "stdout should contain 'hello': {}",
        result["stdout"]
    );
}

#[test]
fn handle_exec_failing_command_returns_nonzero_exit() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"exec","params":{"cmd":["false"],"env":[],"workdir":"/"},"id":3}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_none(),
        "unexpected error: {:?}",
        parsed.error
    );
    let result = parsed.result.unwrap();
    assert_ne!(
        result["exit_code"], 0,
        "false should return non-zero exit code"
    );
}

#[test]
fn handle_exec_invalid_command_returns_error() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"exec","params":{"cmd":["/nonexistent/binary"],"env":[],"workdir":"/"},"id":4}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_some(),
        "should return error for invalid command"
    );
    assert_eq!(parsed.error.as_ref().unwrap().code, INTERNAL_ERROR);
}

#[test]
fn handle_exec_with_env_vars() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"exec","params":{"cmd":["env"],"env":["MY_VAR=test_value"],"workdir":"/"},"id":5}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_none(),
        "unexpected error: {:?}",
        parsed.error
    );
    let result = parsed.result.unwrap();
    assert_eq!(result["exit_code"], 0);
    assert!(
        result["stdout"]
            .as_str()
            .unwrap()
            .contains("MY_VAR=test_value"),
        "stdout should contain env var: {}",
        result["stdout"]
    );
}

// ── GetConfig tests ─────────────────────────────────────────────────────────

#[test]
fn handle_get_config_returns_config() {
    let config = RunConfig {
        cmd: vec!["/bin/test".to_owned()],
        workdir: "/app".to_owned(),
        ..RunConfig::default()
    };
    let mut state = AgentState::new(config);
    let line = r#"{"jsonrpc":"2.0","method":"get_config","id":6}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_none(),
        "unexpected error: {:?}",
        parsed.error
    );
    let result = parsed.result.unwrap();
    let restored: RunConfig = serde_json::from_value(result).unwrap();
    assert_eq!(restored.cmd, vec!["/bin/test"]);
    assert_eq!(restored.workdir, "/app");
}

// ── Shutdown tests ──────────────────────────────────────────────────────────

#[test]
fn handle_shutdown_sets_flag_and_returns_ok() {
    let mut state = test_state();
    assert!(!state.shutdown_requested);
    let line = r#"{"jsonrpc":"2.0","method":"shutdown","id":7}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert_eq!(parsed.result, Some(serde_json::json!("ok")));
    assert!(parsed.error.is_none());
    assert!(
        state.shutdown_requested,
        "shutdown_requested should be true after shutdown"
    );
}

// ── Kill tests ──────────────────────────────────────────────────────────────

#[test]
fn handle_kill_without_child_returns_error() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"kill","params":{"signal":9},"id":8}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_some(),
        "kill without child should return error"
    );
    assert_eq!(parsed.error.as_ref().unwrap().code, INTERNAL_ERROR);
    assert!(
        parsed
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("no child process"),
        "error message should mention no child: {}",
        parsed.error.as_ref().unwrap().message
    );
}

// ── Parse error tests ───────────────────────────────────────────────────────

#[test]
fn handle_invalid_json_returns_parse_error() {
    let mut state = test_state();
    let line = "not valid json {{{";
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(parsed.error.is_some());
    assert_eq!(parsed.error.as_ref().unwrap().code, PARSE_ERROR);
    assert_eq!(parsed.id, serde_json::Value::Null);
}

#[test]
fn handle_empty_json_object_returns_parse_error() {
    let mut state = test_state();
    let line = "{}";
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(parsed.error.is_some());
    assert_eq!(parsed.error.as_ref().unwrap().code, PARSE_ERROR);
}

// ── Unknown method tests ────────────────────────────────────────────────────

#[test]
fn handle_unknown_method_returns_method_not_found() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"nonexistent","id":9}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(parsed.error.is_some());
    assert_eq!(parsed.error.as_ref().unwrap().code, METHOD_NOT_FOUND);
    assert_eq!(parsed.id, serde_json::json!(9));
}

// ── Overlay wiring tests ────────────────────────────────────────────────────

#[test]
fn handle_snapshot_without_overlay_returns_error() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"snapshot_layer","id":10}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(parsed.error.is_some());
    assert_eq!(parsed.error.as_ref().unwrap().code, INTERNAL_ERROR);
    assert!(
        parsed
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("not initialized"),
        "error should mention overlay not initialized: {}",
        parsed.error.as_ref().unwrap().message
    );
}

#[test]
fn handle_flatten_without_overlay_returns_error() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"flatten_overlay","id":11}"#;
    let response = handle_request_line(line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(parsed.error.is_some());
    assert_eq!(parsed.error.as_ref().unwrap().code, INTERNAL_ERROR);
    assert!(
        parsed
            .error
            .as_ref()
            .unwrap()
            .message
            .contains("not initialized"),
        "error should mention overlay not initialized: {}",
        parsed.error.as_ref().unwrap().message
    );
}

// ── AgentState tests ────────────────────────────────────────────────────────

#[test]
fn agent_state_new_has_correct_defaults() {
    let config = RunConfig::default();
    let state = AgentState::new(config);
    assert!(state.overlay.is_none());
    assert!(state.child_pid.is_none());
    assert!(!state.shutdown_requested);
    assert_eq!(state.config.cmd, vec!["/bin/sh"]);
}

// ── Config mode tests ───────────────────────────────────────────────────────

#[test]
fn config_mode_defaults_to_run() {
    let config = RunConfig::default();
    assert_eq!(config.mode, "run");
}

#[test]
fn config_mode_parses_agent() {
    let json = r#"{"cmd": ["/bin/sh"], "mode": "agent"}"#;
    let config = RunConfig::from_json(json).unwrap();
    assert_eq!(config.mode, "agent");
}

// ── Response wire format tests ──────────────────────────────────────────────

#[test]
fn response_is_single_line_json() {
    let mut state = test_state();
    let line = r#"{"jsonrpc":"2.0","method":"ping","id":1}"#;
    let response = handle_request_line(line, &mut state);
    assert!(
        !response.contains('\n'),
        "response should be single-line JSON"
    );
    // Must be valid JSON
    let _: serde_json::Value = serde_json::from_str(&response).unwrap();
}

#[test]
fn multiple_requests_produce_independent_responses() {
    let mut state = test_state();

    let line1 = r#"{"jsonrpc":"2.0","method":"ping","id":1}"#;
    let resp1 = handle_request_line(line1, &mut state);
    let parsed1: JsonRpcResponse = serde_json::from_str(&resp1).unwrap();
    assert_eq!(parsed1.id, serde_json::json!(1));

    let line2 = r#"{"jsonrpc":"2.0","method":"ping","id":2}"#;
    let resp2 = handle_request_line(line2, &mut state);
    let parsed2: JsonRpcResponse = serde_json::from_str(&resp2).unwrap();
    assert_eq!(parsed2.id, serde_json::json!(2));
}

#[test]
fn write_docker_stream_frame_prefixes_payload_with_header() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("frame.bin");
    let mut file = std::fs::File::create(&path).unwrap();

    write_docker_stream_frame(&mut file, 1, b"abc").unwrap();
    drop(file);

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes[..8], &[1, 0, 0, 0, 0, 0, 0, 3]);
    assert_eq!(&bytes[8..], b"abc");
}

#[test]
fn write_stream_output_raw_writes_plain_payload() {
    let tempdir = tempdir().unwrap();
    let path = tempdir.path().join("stream.bin");
    let file = std::fs::File::create(&path).unwrap();
    let writer = Arc::new(Mutex::new(file));

    write_stream_output(
        std::io::Cursor::new(b"tty-ok".to_vec()),
        &writer,
        StreamOutputMode::Raw,
    )
    .unwrap();

    drop(writer);

    let bytes = std::fs::read(&path).unwrap();
    assert_eq!(&bytes, b"tty-ok");
}

#[test]
fn handle_method_ping_directly() {
    let mut state = test_state();
    let resp = handle_method(
        crate::agent::AgentMethod::Ping,
        &mut state,
        serde_json::json!(42),
    );
    assert_eq!(resp.result, Some(serde_json::json!("pong")));
    assert_eq!(resp.id, serde_json::json!(42));
}

#[test]
fn handle_method_shutdown_directly() {
    let mut state = test_state();
    let resp = handle_method(
        crate::agent::AgentMethod::Shutdown,
        &mut state,
        serde_json::json!(99),
    );
    assert_eq!(resp.result, Some(serde_json::json!("ok")));
    assert!(state.shutdown_requested);
}

#[test]
fn handle_method_get_config_directly() {
    let config = RunConfig {
        cmd: vec!["/usr/bin/python".to_owned()],
        ..RunConfig::default()
    };
    let mut state = AgentState::new(config);
    let resp = handle_method(
        crate::agent::AgentMethod::GetConfig,
        &mut state,
        serde_json::json!(1),
    );
    assert!(resp.error.is_none());
    let result = resp.result.unwrap();
    let restored: RunConfig = serde_json::from_value(result).unwrap();
    assert_eq!(restored.cmd, vec!["/usr/bin/python"]);
}

// ── CopyFiles tests ──────────────────────────────────────────────────────────

#[test]
fn handle_copy_files_extracts_archive() {
    // Build a tar.gz with a single file
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    {
        let mut tar_builder = tar::Builder::new(&mut encoder);
        let content = b"hello from copy_files";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "test.txt", &content[..])
            .unwrap();
        tar_builder.finish().unwrap();
    }
    let compressed = encoder.finish().unwrap();

    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&compressed);

    let dest = tempdir().unwrap();
    let dest_path = dest.path().to_str().unwrap().to_owned();

    let line = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "copy_files",
        "params": {
            "data": encoded,
            "dest": dest_path
        },
        "id": 20
    })
    .to_string();

    let mut state = test_state();
    let response = handle_request_line(&line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_none(),
        "unexpected error: {:?}",
        parsed.error
    );
    let result = parsed.result.unwrap();
    assert_eq!(result["files_written"], 1);

    // Verify the file was actually extracted
    let extracted = std::fs::read_to_string(dest.path().join("test.txt")).unwrap();
    assert_eq!(extracted, "hello from copy_files");
}

#[test]
fn handle_copy_files_creates_dest_dirs() {
    // Build a tar.gz with a file in a subdirectory
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    {
        let mut tar_builder = tar::Builder::new(&mut encoder);
        let content = b"nested content";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "sub/dir/file.txt", &content[..])
            .unwrap();
        tar_builder.finish().unwrap();
    }
    let compressed = encoder.finish().unwrap();

    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&compressed);

    let dest = tempdir().unwrap();
    let dest_path = dest.path().to_str().unwrap().to_owned();

    let line = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "copy_files",
        "params": {
            "data": encoded,
            "dest": dest_path
        },
        "id": 21
    })
    .to_string();

    let mut state = test_state();
    let response = handle_request_line(&line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_none(),
        "unexpected error: {:?}",
        parsed.error
    );

    let extracted = std::fs::read_to_string(dest.path().join("sub/dir/file.txt")).unwrap();
    assert_eq!(extracted, "nested content");
}

#[test]
fn handle_copy_files_rewrites_loopback_only_resolv_conf() {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    {
        let mut tar_builder = tar::Builder::new(&mut encoder);
        let content = b"nameserver ::1\n";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "resolv.conf", &content[..])
            .unwrap();
        tar_builder.finish().unwrap();
    }
    let compressed = encoder.finish().unwrap();

    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&compressed);

    let dest = tempdir().unwrap();
    let dest_path = dest.path().to_str().unwrap().to_owned();

    let line = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "copy_files",
        "params": {
            "data": encoded,
            "dest": dest_path
        },
        "id": 23
    })
    .to_string();

    let mut state = test_state();
    let response = handle_request_line(&line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_none(),
        "unexpected error: {:?}",
        parsed.error
    );

    let extracted = std::fs::read_to_string(dest.path().join("resolv.conf")).unwrap();
    assert_eq!(extracted, "nameserver 1.1.1.1\nnameserver 8.8.8.8\n");
}

#[test]
fn handle_copy_files_invalid_base64_returns_error() {
    let line = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "copy_files",
        "params": {
            "data": "not-valid-base64!!!",
            "dest": "/tmp/test"
        },
        "id": 22
    })
    .to_string();

    let mut state = test_state();
    let response = handle_request_line(&line, &mut state);
    let parsed: JsonRpcResponse = serde_json::from_str(&response).unwrap();
    assert!(
        parsed.error.is_some(),
        "should return error for invalid base64"
    );
    assert_eq!(parsed.error.as_ref().unwrap().code, INTERNAL_ERROR);
}

#[test]
fn handle_method_copy_files_directly() {
    use crate::agent::CopyFilesParams;

    // Build a minimal tar.gz
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    {
        let mut tar_builder = tar::Builder::new(&mut encoder);
        let content = b"direct test";
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        tar_builder
            .append_data(&mut header, "direct.txt", &content[..])
            .unwrap();
        tar_builder.finish().unwrap();
    }
    let compressed = encoder.finish().unwrap();

    use base64::Engine as _;
    let encoded = base64::engine::general_purpose::STANDARD.encode(&compressed);

    let dest = tempdir().unwrap();
    let params = CopyFilesParams {
        data: encoded,
        dest: dest.path().to_str().unwrap().to_owned(),
    };

    let mut state = test_state();
    let resp = handle_method(
        crate::agent::AgentMethod::CopyFiles(params),
        &mut state,
        serde_json::json!(23),
    );
    assert!(resp.error.is_none(), "unexpected error: {:?}", resp.error);
    let result = resp.result.unwrap();
    assert_eq!(result["files_written"], 1);
    assert_eq!(resp.id, serde_json::json!(23));
}
