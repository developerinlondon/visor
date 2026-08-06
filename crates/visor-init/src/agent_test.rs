use super::*;

// ── Parse tests ─────────────────────────────────────────────────────────────

#[test]
fn parse_valid_ping_request() {
    let json = r#"{"jsonrpc":"2.0","method":"ping","id":1}"#;
    let req = parse_request(json).unwrap();
    assert_eq!(req.jsonrpc, "2.0");
    assert_eq!(req.method, "ping");
    assert!(req.params.is_none());
    assert_eq!(req.id, serde_json::json!(1));
}

#[test]
fn parse_valid_exec_request_with_params() {
    let json = r#"{
        "jsonrpc": "2.0",
        "method": "exec",
        "params": {"cmd": ["ls", "-la"], "env": ["HOME=/root"], "workdir": "/tmp"},
        "id": 42
    }"#;
    let req = parse_request(json).unwrap();
    assert_eq!(req.method, "exec");
    assert!(req.params.is_some());
    assert_eq!(req.id, serde_json::json!(42));
}

#[test]
fn parse_valid_kill_request() {
    let json = r#"{"jsonrpc":"2.0","method":"kill","params":{"signal":9},"id":"abc"}"#;
    let req = parse_request(json).unwrap();
    assert_eq!(req.method, "kill");
    let params = req.params.unwrap();
    assert_eq!(params["signal"], 9);
    assert_eq!(req.id, serde_json::json!("abc"));
}

#[test]
fn parse_shutdown_request() {
    let json = r#"{"jsonrpc":"2.0","method":"shutdown","id":99}"#;
    let req = parse_request(json).unwrap();
    assert_eq!(req.method, "shutdown");
    assert!(req.params.is_none());
}

#[test]
fn parse_invalid_json_returns_error() {
    let json = "not valid json {{{";
    let err = parse_request(json).unwrap_err();
    assert!(
        format!("{err:?}").contains("failed to parse"),
        "error should mention parsing: {err:?}"
    );
}

#[test]
fn parse_missing_jsonrpc_field() {
    let json = r#"{"method":"ping","id":1}"#;
    let err = parse_request(json).unwrap_err();
    assert!(
        format!("{err:?}").contains("failed to parse")
            || format!("{err:?}").contains("missing field"),
        "error should mention missing field: {err:?}"
    );
}

#[test]
fn parse_wrong_jsonrpc_version() {
    let json = r#"{"jsonrpc":"1.0","method":"ping","id":1}"#;
    let err = parse_request(json).unwrap_err();
    assert!(
        format!("{err:?}").contains("invalid JSON-RPC version"),
        "error should mention version: {err:?}"
    );
}

#[test]
fn parse_request_with_null_params() {
    let json = r#"{"jsonrpc":"2.0","method":"ping","params":null,"id":1}"#;
    let req = parse_request(json).unwrap();
    assert_eq!(req.method, "ping");
    assert!(req.params.is_none());
}

// ── Dispatch tests ──────────────────────────────────────────────────────────

#[test]
fn dispatch_ping_method() {
    let method = dispatch_method("ping", None).unwrap();
    assert!(matches!(method, AgentMethod::Ping));
}

#[test]
fn dispatch_exec_method_with_valid_params() {
    let params = serde_json::json!({
        "cmd": ["echo", "hello"],
        "env": [],
        "workdir": "/tmp"
    });
    let method = dispatch_method("exec", Some(&params)).unwrap();
    match method {
        AgentMethod::Exec(p) => {
            assert_eq!(p.cmd, vec!["echo", "hello"]);
            assert!(p.env.is_empty());
            assert_eq!(p.workdir, "/tmp");
        }
        other => panic!("expected Exec, got {other:?}"),
    }
}

#[test]
fn dispatch_exec_stream_method_with_valid_params() {
    let params = serde_json::json!({
        "cmd": ["buildctl", "dial-stdio"],
        "env": ["BUILDKIT_PROGRESS=plain"],
        "workdir": "/"
    });
    let method = dispatch_method("exec_stream", Some(&params)).unwrap();
    match method {
        AgentMethod::ExecStream(p) => {
            assert_eq!(p.cmd, vec!["buildctl", "dial-stdio"]);
            assert_eq!(p.env, vec!["BUILDKIT_PROGRESS=plain"]);
            assert_eq!(p.workdir, "/");
            assert!(!p.tty);
        }
        other => panic!("expected ExecStream, got {other:?}"),
    }
}

#[test]
fn dispatch_unknown_method_returns_error() {
    let err = dispatch_method("nonexistent", None).unwrap_err();
    assert!(
        format!("{err:?}").contains("unknown method"),
        "error should mention unknown method: {err:?}"
    );
}

#[test]
fn dispatch_exec_with_missing_cmd_returns_error() {
    let params = serde_json::json!({"env": [], "workdir": "/"});
    let err = dispatch_method("exec", Some(&params)).unwrap_err();
    assert!(
        format!("{err:?}").contains("invalid exec params")
            || format!("{err:?}").contains("missing field"),
        "error should mention invalid params: {err:?}"
    );
}

#[test]
fn dispatch_exec_with_empty_cmd_returns_error() {
    let params = serde_json::json!({"cmd": [], "env": [], "workdir": "/"});
    let err = dispatch_method("exec", Some(&params)).unwrap_err();
    assert!(
        format!("{err:?}").contains("cmd must not be empty"),
        "error should mention empty cmd: {err:?}"
    );
}

#[test]
fn dispatch_kill_method_with_valid_params() {
    let params = serde_json::json!({"signal": 15});
    let method = dispatch_method("kill", Some(&params)).unwrap();
    match method {
        AgentMethod::Kill(p) => assert_eq!(p.signal, 15),
        other => panic!("expected Kill, got {other:?}"),
    }
}

// ── Response builder tests ──────────────────────────────────────────────────

#[test]
fn build_success_response() {
    let resp = JsonRpcResponse::success(serde_json::json!(1), serde_json::json!("pong"));
    assert_eq!(resp.jsonrpc, "2.0");
    assert_eq!(resp.result, Some(serde_json::json!("pong")));
    assert!(resp.error.is_none());
    assert_eq!(resp.id, serde_json::json!(1));
}

#[test]
fn build_error_response() {
    let resp = JsonRpcResponse::error(serde_json::json!(2), METHOD_NOT_FOUND, "method not found");
    assert_eq!(resp.jsonrpc, "2.0");
    assert!(resp.result.is_none());
    let err = resp.error.unwrap();
    assert_eq!(err.code, METHOD_NOT_FOUND);
    assert_eq!(err.message, "method not found");
    assert!(err.data.is_none());
    assert_eq!(resp.id, serde_json::json!(2));
}

#[test]
fn response_serialization_round_trip() {
    let resp = JsonRpcResponse::success(serde_json::json!(7), serde_json::json!({"status": "ok"}));
    let json_str = resp.to_json().unwrap();
    let parsed: JsonRpcResponse = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.jsonrpc, "2.0");
    assert_eq!(parsed.result, Some(serde_json::json!({"status": "ok"})));
    assert!(parsed.error.is_none());
    assert_eq!(parsed.id, serde_json::json!(7));
}

#[test]
fn error_response_omits_result_field_in_json() {
    let resp = JsonRpcResponse::error(serde_json::json!(1), PARSE_ERROR, "bad json");
    let json_str = resp.to_json().unwrap();
    let raw: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert!(
        raw.get("result").is_none(),
        "error response should not contain result field"
    );
    assert!(raw.get("error").is_some());
}

#[test]
fn success_response_omits_error_field_in_json() {
    let resp = JsonRpcResponse::success(serde_json::json!(1), serde_json::json!("ok"));
    let json_str = resp.to_json().unwrap();
    let raw: serde_json::Value = serde_json::from_str(&json_str).unwrap();
    assert!(
        raw.get("error").is_none(),
        "success response should not contain error field"
    );
    assert!(raw.get("result").is_some());
}

// ── Overlay dispatch tests ─────────────────────────────────────────────────

#[test]
fn dispatch_overlay_init_with_params() {
    let params = serde_json::json!({"lower_dir": "/rootfs"});
    let method = dispatch_method("overlay_init", Some(&params)).unwrap();
    match method {
        AgentMethod::OverlayInit(p) => {
            assert_eq!(p.lower_dir, Some("/rootfs".to_owned()));
        }
        other => panic!("expected OverlayInit, got {other:?}"),
    }
}

#[test]
fn dispatch_overlay_init_with_null_params_defaults() {
    let params = serde_json::json!({});
    let method = dispatch_method("overlay_init", Some(&params)).unwrap();
    match method {
        AgentMethod::OverlayInit(p) => {
            assert!(p.lower_dir.is_none());
        }
        other => panic!("expected OverlayInit, got {other:?}"),
    }
}

#[test]
fn dispatch_overlay_init_with_no_params() {
    let method = dispatch_method("overlay_init", None).unwrap();
    match method {
        AgentMethod::OverlayInit(p) => {
            assert!(p.lower_dir.is_none());
        }
        other => panic!("expected OverlayInit, got {other:?}"),
    }
}

#[test]
fn dispatch_snapshot_layer() {
    let method = dispatch_method("snapshot_layer", None).unwrap();
    assert!(matches!(method, AgentMethod::SnapshotLayer));
}

#[test]
fn dispatch_flatten_overlay() {
    let method = dispatch_method("flatten_overlay", None).unwrap();
    assert!(matches!(method, AgentMethod::FlattenOverlay));
}

// ── Overlay params serialization tests ──────────────────────────────────────

#[test]
fn overlay_init_params_serialization_round_trip() {
    let params = OverlayInitParams {
        lower_dir: Some("/mnt/rootfs".to_owned()),
    };
    let json_str = serde_json::to_string(&params).unwrap();
    let parsed: OverlayInitParams = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.lower_dir, Some("/mnt/rootfs".to_owned()));
}

#[test]
fn overlay_init_params_with_none_lower_dir() {
    let params = OverlayInitParams { lower_dir: None };
    let value = serde_json::to_value(&params).unwrap();
    assert!(value["lower_dir"].is_null());
    let parsed: OverlayInitParams = serde_json::from_value(value).unwrap();
    assert!(parsed.lower_dir.is_none());
}

#[test]
fn snapshot_layer_result_serialization_round_trip() {
    let result = SnapshotLayerResult {
        data: "dGVzdA==".to_owned(),
        compressed_digest: "sha256:abc123".to_owned(),
        uncompressed_digest: "sha256:def456".to_owned(),
        compressed_size: 1024,
    };
    let json_str = serde_json::to_string(&result).unwrap();
    let parsed: SnapshotLayerResult = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.data, "dGVzdA==");
    assert_eq!(parsed.compressed_digest, "sha256:abc123");
    assert_eq!(parsed.uncompressed_digest, "sha256:def456");
    assert_eq!(parsed.compressed_size, 1024);
}

// ── Overlay response builder tests ──────────────────────────────────────────

#[test]
fn overlay_init_response_is_success() {
    let resp = overlay_init_response(serde_json::json!(10));
    assert_eq!(resp.jsonrpc, "2.0");
    assert_eq!(resp.result, Some(serde_json::json!("ok")));
    assert!(resp.error.is_none());
    assert_eq!(resp.id, serde_json::json!(10));
}

#[test]
fn snapshot_layer_response_contains_result() {
    let result = SnapshotLayerResult {
        data: "dGVzdA==".to_owned(),
        compressed_digest: "sha256:abc".to_owned(),
        uncompressed_digest: "sha256:def".to_owned(),
        compressed_size: 512,
    };
    let resp = snapshot_layer_response(serde_json::json!(11), &result).unwrap();
    assert_eq!(resp.jsonrpc, "2.0");
    let value = resp.result.unwrap();
    assert_eq!(value["data"], "dGVzdA==");
    assert_eq!(value["compressed_size"], 512);
    assert!(resp.error.is_none());
}

#[test]
fn flatten_overlay_response_is_success() {
    let resp = flatten_overlay_response(serde_json::json!(12));
    assert_eq!(resp.jsonrpc, "2.0");
    assert_eq!(resp.result, Some(serde_json::json!("ok")));
    assert!(resp.error.is_none());
    assert_eq!(resp.id, serde_json::json!(12));
}

// ── CopyFiles dispatch tests ──────────────────────────────────────────────

#[test]
fn dispatch_copy_files_with_valid_params() {
    let params = serde_json::json!({
        "data": "dGVzdA==",
        "dest": "/app"
    });
    let method = dispatch_method("copy_files", Some(&params)).unwrap();
    match method {
        AgentMethod::CopyFiles(p) => {
            assert_eq!(p.data, "dGVzdA==");
            assert_eq!(p.dest, "/app");
        }
        other => panic!("expected CopyFiles, got {other:?}"),
    }
}

#[test]
fn dispatch_copy_files_without_params_returns_error() {
    let err = dispatch_method("copy_files", None).unwrap_err();
    assert!(
        format!("{err:?}").contains("requires params"),
        "error should mention requires params: {err:?}"
    );
}

#[test]
fn dispatch_copy_files_with_invalid_params_returns_error() {
    let params = serde_json::json!({"data": 123});
    let err = dispatch_method("copy_files", Some(&params)).unwrap_err();
    assert!(
        format!("{err:?}").contains("invalid copy_files params")
            || format!("{err:?}").contains("missing field"),
        "error should mention invalid params: {err:?}"
    );
}

// ── CopyFiles params serialization tests ──────────────────────────────────

#[test]
fn copy_files_params_serialization_round_trip() {
    let params = CopyFilesParams {
        data: "dGVzdA==".to_owned(),
        dest: "/app".to_owned(),
    };
    let json_str = serde_json::to_string(&params).unwrap();
    let parsed: CopyFilesParams = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.data, "dGVzdA==");
    assert_eq!(parsed.dest, "/app");
}

#[test]
fn copy_files_result_serialization_round_trip() {
    let result = CopyFilesResult { files_written: 5 };
    let json_str = serde_json::to_string(&result).unwrap();
    let parsed: CopyFilesResult = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.files_written, 5);
}

// ── CopyFiles response builder tests ──────────────────────────────────────

#[test]
fn copy_files_response_contains_result() {
    let result = CopyFilesResult { files_written: 3 };
    let resp = copy_files_response(serde_json::json!(13), &result).unwrap();
    assert_eq!(resp.jsonrpc, "2.0");
    let value = resp.result.unwrap();
    assert_eq!(value["files_written"], 3);
    assert!(resp.error.is_none());
    assert_eq!(resp.id, serde_json::json!(13));
}
