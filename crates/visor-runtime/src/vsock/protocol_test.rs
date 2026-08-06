use super::*;

// ── Request building tests ──────────────────────────────────────────────────

#[test]
fn build_ping_request() {
    let req = JsonRpcRequest::new("ping", None);
    assert_eq!(req.jsonrpc, "2.0");
    assert_eq!(req.method, "ping");
    assert!(req.params.is_none());
    assert!(req.id.is_number(), "id should be numeric: {:?}", req.id);
}

#[test]
fn build_request_with_params() {
    let params = serde_json::json!({"cmd": ["ls"], "env": [], "workdir": "/tmp"});
    let req = JsonRpcRequest::new("exec", Some(params.clone()));
    assert_eq!(req.method, "exec");
    assert_eq!(req.params, Some(params));
}

#[test]
fn request_id_increments() {
    REQUEST_ID_COUNTER.store(0, std::sync::atomic::Ordering::SeqCst);
    let req1 = JsonRpcRequest::new("ping", None);
    let req2 = JsonRpcRequest::new("ping", None);
    assert_ne!(req1.id, req2.id);
}

// ── Request serialization tests ─────────────────────────────────────────────

#[test]
fn serialize_ping_request() {
    let req = JsonRpcRequest::new("ping", None);
    let json_str = req.to_json().unwrap();
    let raw: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert_eq!(raw["jsonrpc"], "2.0");
    assert_eq!(raw["method"], "ping");
    assert!(raw.get("params").is_none());
}

#[test]
fn serialize_exec_request_with_params() {
    let params = serde_json::json!({"cmd": ["echo", "hi"], "env": [], "workdir": "/"});
    let req = JsonRpcRequest::new("exec", Some(params));
    let json_str = req.to_json().unwrap();
    let raw: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert_eq!(raw["method"], "exec");
    assert!(raw.get("params").is_some());
    assert_eq!(raw["params"]["cmd"][0], "echo");
}

#[test]
fn request_serialization_round_trip() {
    let params = serde_json::json!({"signal": 9});
    let req = JsonRpcRequest::new("kill", Some(params.clone()));
    let json_str = req.to_json().unwrap();
    let parsed: JsonRpcRequest = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.jsonrpc, "2.0");
    assert_eq!(parsed.method, "kill");
    assert_eq!(parsed.params, Some(params));
}

// ── Response parsing tests ──────────────────────────────────────────────────

#[test]
fn parse_success_response() {
    let json = r#"{"jsonrpc":"2.0","result":"pong","id":1}"#;
    let resp = parse_response(json).unwrap();
    assert_eq!(resp.jsonrpc, "2.0");
    assert_eq!(resp.result, Some(serde_json::json!("pong")));
    assert!(resp.error.is_none());
    assert_eq!(resp.id, serde_json::json!(1));
}

#[test]
fn parse_error_response() {
    let json = r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"method not found"},"id":2}"#;
    let resp = parse_response(json).unwrap();
    assert!(resp.result.is_none());
    let err = resp.error.unwrap();
    assert_eq!(err.code, METHOD_NOT_FOUND);
    assert_eq!(err.message, "method not found");
    assert!(err.data.is_none());
}

#[test]
fn parse_error_response_with_data() {
    let json =
        r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"boom","data":"extra"},"id":3}"#;
    let resp = parse_response(json).unwrap();
    let err = resp.error.unwrap();
    assert_eq!(err.code, INTERNAL_ERROR);
    assert_eq!(err.data, Some(serde_json::json!("extra")));
}

#[test]
fn parse_invalid_json_response() {
    let json = "not valid json";
    let err = parse_response(json).unwrap_err();
    assert!(
        format!("{err:?}").contains("failed to parse"),
        "error should mention parsing: {err:?}"
    );
}

#[test]
fn parse_wrong_jsonrpc_version_response() {
    let json = r#"{"jsonrpc":"1.0","result":"ok","id":1}"#;
    let err = parse_response(json).unwrap_err();
    assert!(
        format!("{err:?}").contains("invalid JSON-RPC version"),
        "error should mention version: {err:?}"
    );
}

// ── ExecParams tests ────────────────────────────────────────────────────────

#[test]
fn exec_params_serializes_correctly() {
    let params = ExecParams {
        cmd: vec!["ls".to_owned(), "-la".to_owned()],
        env: vec!["HOME=/root".to_owned()],
        workdir: "/tmp".to_owned(),
        tty: true,
    };
    let value = serde_json::to_value(&params).unwrap();
    assert_eq!(value["cmd"], serde_json::json!(["ls", "-la"]));
    assert_eq!(value["env"], serde_json::json!(["HOME=/root"]));
    assert_eq!(value["workdir"], "/tmp");
    assert_eq!(value["tty"], true);
}

#[test]
fn exec_params_round_trip() {
    let params = ExecParams {
        cmd: vec!["echo".to_owned(), "hello".to_owned()],
        env: vec![],
        workdir: "/".to_owned(),
        tty: true,
    };
    let json_str = serde_json::to_string(&params).unwrap();
    let parsed: ExecParams = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.cmd, params.cmd);
    assert_eq!(parsed.env, params.env);
    assert_eq!(parsed.workdir, params.workdir);
    assert_eq!(parsed.tty, params.tty);
}

// ── KillParams tests ────────────────────────────────────────────────────────

#[test]
fn kill_params_serializes_correctly() {
    let params = KillParams { signal: 9 };
    let value = serde_json::to_value(&params).unwrap();
    assert_eq!(value["signal"], 9);
}

#[test]
fn kill_params_round_trip() {
    let params = KillParams { signal: 15 };
    let json_str = serde_json::to_string(&params).unwrap();
    let parsed: KillParams = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.signal, params.signal);
}

// ── ExecResult tests ────────────────────────────────────────────────────────

#[test]
fn exec_result_deserializes_from_response() {
    let json = r#"{"exit_code":0,"stdout":"hello\n","stderr":""}"#;
    let result: ExecResult = serde_json::from_str(json).unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "hello\n");
    assert!(result.stderr.is_empty());
}

#[test]
fn exec_result_with_nonzero_exit_code() {
    let json = r#"{"exit_code":1,"stdout":"","stderr":"not found"}"#;
    let result: ExecResult = serde_json::from_str(json).unwrap();
    assert_eq!(result.exit_code, 1);
    assert_eq!(result.stderr, "not found");
}

// ── Error code constants tests ──────────────────────────────────────────────

#[test]
fn error_code_constants_match_spec() {
    assert_eq!(PARSE_ERROR, -32700);
    assert_eq!(INVALID_REQUEST, -32600);
    assert_eq!(METHOD_NOT_FOUND, -32601);
    assert_eq!(INVALID_PARAMS, -32602);
    assert_eq!(INTERNAL_ERROR, -32603);
}

// ── Newline-delimited framing tests ─────────────────────────────────────────

#[test]
fn request_json_contains_no_internal_newlines() {
    let params =
        serde_json::json!({"cmd": ["echo", "hello world"], "env": ["A=B\nC=D"], "workdir": "/"});
    let req = JsonRpcRequest::new("exec", Some(params));
    let json_str = req.to_json().unwrap();
    // serde_json compact output should not contain literal newlines
    // (the \n in env value will be escaped as \\n)
    assert!(
        !json_str.contains('\n'),
        "serialized request must not contain literal newlines for framing: {json_str}"
    );
}

// ── OverlayInitParams tests ─────────────────────────────────────────────────

#[test]
fn overlay_init_params_serializes_correctly() {
    let params = OverlayInitParams {
        lower_dir: Some("/rootfs".to_owned()),
    };
    let value = serde_json::to_value(&params).unwrap();
    assert_eq!(value["lower_dir"], "/rootfs");
}

#[test]
fn overlay_init_params_with_none_serializes_null() {
    let params = OverlayInitParams { lower_dir: None };
    let value = serde_json::to_value(&params).unwrap();
    assert!(value["lower_dir"].is_null());
}

#[test]
fn overlay_init_params_round_trip() {
    let params = OverlayInitParams {
        lower_dir: Some("/mnt/base".to_owned()),
    };
    let json_str = serde_json::to_string(&params).unwrap();
    let parsed: OverlayInitParams = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.lower_dir, Some("/mnt/base".to_owned()));
}

// ── SnapshotLayerResult tests ──────────────────────────────────────────────

#[test]
fn snapshot_layer_result_deserializes_from_json() {
    let json = r#"{
        "data": "dGVzdA==",
        "compressed_digest": "sha256:abc",
        "uncompressed_digest": "sha256:def",
        "compressed_size": 2048
    }"#;
    let result: SnapshotLayerResult = serde_json::from_str(json).unwrap();
    assert_eq!(result.data, "dGVzdA==");
    assert_eq!(result.compressed_digest, "sha256:abc");
    assert_eq!(result.uncompressed_digest, "sha256:def");
    assert_eq!(result.compressed_size, 2048);
}

#[test]
fn snapshot_layer_result_round_trip() {
    let result = SnapshotLayerResult {
        data: "Y29udGVudA==".to_owned(),
        compressed_digest: "sha256:111".to_owned(),
        uncompressed_digest: "sha256:222".to_owned(),
        compressed_size: 4096,
    };
    let json_str = serde_json::to_string(&result).unwrap();
    let parsed: SnapshotLayerResult = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.data, result.data);
    assert_eq!(parsed.compressed_digest, result.compressed_digest);
    assert_eq!(parsed.uncompressed_digest, result.uncompressed_digest);
    assert_eq!(parsed.compressed_size, result.compressed_size);
}

// ── CopyFilesParams tests ──────────────────────────────────────────────────

#[test]
fn copy_files_params_serializes_correctly() {
    let params = CopyFilesParams {
        data: "dGVzdA==".to_owned(),
        dest: "/app".to_owned(),
    };
    let value = serde_json::to_value(&params).unwrap();
    assert_eq!(value["data"], "dGVzdA==");
    assert_eq!(value["dest"], "/app");
}

#[test]
fn copy_files_params_round_trip() {
    let params = CopyFilesParams {
        data: "Y29udGVudA==".to_owned(),
        dest: "/opt/build".to_owned(),
    };
    let json_str = serde_json::to_string(&params).unwrap();
    let parsed: CopyFilesParams = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.data, params.data);
    assert_eq!(parsed.dest, params.dest);
}

// ── CopyFilesResult tests ──────────────────────────────────────────────────

#[test]
fn copy_files_result_deserializes_from_json() {
    let json = r#"{"files_written":5}"#;
    let result: CopyFilesResult = serde_json::from_str(json).unwrap();
    assert_eq!(result.files_written, 5);
}

#[test]
fn copy_files_result_round_trip() {
    let result = CopyFilesResult { files_written: 42 };
    let json_str = serde_json::to_string(&result).unwrap();
    let parsed: CopyFilesResult = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.files_written, result.files_written);
}
