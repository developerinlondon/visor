use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

use super::*;
use crate::vsock::protocol::{
    ExecResult, INTERNAL_ERROR, JsonRpcError, JsonRpcRequest, JsonRpcResponse, METHOD_NOT_FOUND,
};

// ── Helper: create a mock transport pair ────────────────────────────────────

/// Creates a `VsockClient` backed by a `tokio::io::DuplexStream` for testing.
/// Returns `(client, server_half)` where `server_half` can simulate the guest.
fn mock_client() -> (VsockClient<DuplexStream>, DuplexStream) {
    let (client_stream, server_stream) = tokio::io::duplex(8192);
    let client = VsockClient::from_stream(client_stream);
    (client, server_stream)
}

/// Write a JSON-RPC response line to the server half of the duplex stream.
async fn send_response(server: &mut DuplexStream, resp: &JsonRpcResponse) {
    let json = serde_json::to_string(resp).unwrap();
    server.write_all(json.as_bytes()).await.unwrap();
    server.write_all(b"\n").await.unwrap();
    server.flush().await.unwrap();
}

async fn read_request_line(server: &mut DuplexStream) -> String {
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0u8; 1];
        let bytes_read = server.read(&mut buf).await.unwrap();
        assert_ne!(bytes_read, 0, "server stream closed before newline");
        if buf[0] == b'\n' {
            break;
        }
        bytes.push(buf[0]);
    }
    String::from_utf8(bytes).unwrap()
}

// ── Default timeout tests ───────────────────────────────────────────────────

#[test]
fn default_connect_timeout_is_10s() {
    assert_eq!(DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(10));
}

#[test]
fn default_request_timeout_is_30s() {
    assert_eq!(DEFAULT_REQUEST_TIMEOUT, Duration::from_secs(30));
}

// ── Ping tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn ping_sends_correct_request_and_returns_pong() {
    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move { client.ping().await });

    // Read the request from the server side
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    assert_eq!(req.method, "ping");
    assert!(req.params.is_none());

    // Send pong response
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::json!("pong")),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    let result = client_task.await.unwrap().unwrap();
    assert_eq!(result, "pong");
}

// ── Exec tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn exec_sends_correct_params_and_parses_result() {
    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move {
        client
            .exec(
                vec!["echo".to_owned(), "hello".to_owned()],
                vec!["HOME=/root".to_owned()],
                "/tmp".to_owned(),
            )
            .await
    });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    assert_eq!(req.method, "exec");
    let params = req.params.unwrap();
    assert_eq!(params["cmd"], serde_json::json!(["echo", "hello"]));
    assert_eq!(params["env"], serde_json::json!(["HOME=/root"]));
    assert_eq!(params["workdir"], "/tmp");

    // Send exec result response
    let exec_result = ExecResult {
        exit_code: 0,
        stdout: "hello\n".to_owned(),
        stderr: String::new(),
    };
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::to_value(&exec_result).unwrap()),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    let result = client_task.await.unwrap().unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "hello\n");
    assert!(result.stderr.is_empty());
}

#[tokio::test]
async fn negotiate_exec_stream_sends_request_and_returns_raw_stream() {
    let (client_stream, mut server) = tokio::io::duplex(8192);

    let client_task = tokio::spawn(async move {
        negotiate_exec_stream(
            client_stream,
            vec!["buildctl".to_owned(), "dial-stdio".to_owned()],
            vec!["BUILDKIT_PROGRESS=plain".to_owned()],
            "/".to_owned(),
            true,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    });

    let request_json = read_request_line(&mut server).await;
    let req: JsonRpcRequest = serde_json::from_str(&request_json).unwrap();
    assert_eq!(req.method, "exec_stream");
    let params = req.params.unwrap();
    assert_eq!(params["cmd"], serde_json::json!(["buildctl", "dial-stdio"]));
    assert_eq!(
        params["env"],
        serde_json::json!(["BUILDKIT_PROGRESS=plain"])
    );
    assert_eq!(params["workdir"], "/");
    assert_eq!(params["tty"], true);

    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::json!("ok")),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    let mut stream = client_task.await.unwrap().unwrap();
    stream.write_all(b"ping").await.unwrap();

    let mut inbound = [0u8; 4];
    server.read_exact(&mut inbound).await.unwrap();
    assert_eq!(&inbound, b"ping");

    server.write_all(b"pong").await.unwrap();
    server.flush().await.unwrap();

    let mut outbound = [0u8; 4];
    stream.read_exact(&mut outbound).await.unwrap();
    assert_eq!(&outbound, b"pong");
}

#[tokio::test]
async fn negotiate_exec_stream_returns_rpc_error_when_guest_rejects() {
    let (client_stream, mut server) = tokio::io::duplex(8192);

    let client_task = tokio::spawn(async move {
        negotiate_exec_stream(
            client_stream,
            vec!["buildctl".to_owned(), "dial-stdio".to_owned()],
            vec![],
            "/".to_owned(),
            false,
            DEFAULT_REQUEST_TIMEOUT,
        )
        .await
    });

    let request_json = read_request_line(&mut server).await;
    let req: JsonRpcRequest = serde_json::from_str(&request_json).unwrap();

    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: None,
        error: Some(JsonRpcError {
            code: INTERNAL_ERROR,
            message: "streaming exec not available".to_owned(),
            data: None,
        }),
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    let err = client_task.await.unwrap().unwrap_err();
    match err {
        VsockError::Rpc { code, message, .. } => {
            assert_eq!(code, INTERNAL_ERROR);
            assert_eq!(message, "streaming exec not available");
        }
        other => panic!("expected RpcError, got {other:?}"),
    }
}

// ── Kill tests ──────────────────────────────────────────────────────────────

#[tokio::test]
async fn kill_sends_signal_param() {
    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move { client.kill(9).await });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    assert_eq!(req.method, "kill");
    let params = req.params.unwrap();
    assert_eq!(params["signal"], 9);

    // Send ok response
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::json!("ok")),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    client_task.await.unwrap().unwrap();
}

// ── Shutdown tests ──────────────────────────────────────────────────────────

#[tokio::test]
async fn shutdown_sends_correct_request() {
    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move { client.shutdown().await });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    assert_eq!(req.method, "shutdown");
    assert!(req.params.is_none());

    // Send ok response
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::json!("ok")),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    client_task.await.unwrap().unwrap();
}

// ── Error handling tests ────────────────────────────────────────────────────

#[tokio::test]
async fn rpc_error_response_returns_typed_error() {
    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move { client.ping().await });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    // Send error response
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: None,
        error: Some(JsonRpcError {
            code: METHOD_NOT_FOUND,
            message: "method not found".to_owned(),
            data: None,
        }),
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    let err = client_task.await.unwrap().unwrap_err();
    match err {
        VsockError::Rpc { code, message, .. } => {
            assert_eq!(code, METHOD_NOT_FOUND);
            assert_eq!(message, "method not found");
        }
        other => panic!("expected RpcError, got {other:?}"),
    }
}

#[tokio::test]
async fn server_disconnect_returns_error() {
    let (mut client, server) = mock_client();

    // Drop server to simulate disconnect
    drop(server);

    let err = client.ping().await.unwrap_err();
    // Should get some IO or protocol error
    assert!(
        format!("{err:?}").contains("read")
            || format!("{err:?}").contains("empty")
            || format!("{err:?}").contains("EOF")
            || format!("{err:?}").contains("response")
            || format!("{err:?}").contains("BrokenPipe")
            || format!("{err:?}").contains("Io"),
        "error should mention read/connection issue: {err:?}"
    );
}

#[tokio::test]
async fn request_timeout_returns_error() {
    let (mut client, _server) = mock_client();

    // Set a very short timeout
    client.set_request_timeout(Duration::from_millis(50));

    let err = client.ping().await.unwrap_err();
    assert!(
        matches!(err, VsockError::Timeout { .. }),
        "expected Timeout error, got: {err:?}"
    );
}

// ── VsockError display tests ────────────────────────────────────────────────

#[test]
fn vsock_error_display_connect() {
    let err = VsockError::Connect {
        cid: 3,
        port: 52,
        source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"),
    };
    let msg = format!("{err}");
    assert!(msg.contains('3'), "should contain CID: {msg}");
    assert!(msg.contains("52"), "should contain port: {msg}");
}

#[test]
fn vsock_error_display_timeout() {
    let err = VsockError::Timeout {
        operation: "ping".to_owned(),
        duration: Duration::from_secs(30),
    };
    let msg = format!("{err}");
    assert!(msg.contains("ping"), "should contain operation: {msg}");
    assert!(msg.contains("30s"), "should contain duration: {msg}");
}

#[test]
fn vsock_error_display_rpc() {
    let err = VsockError::Rpc {
        code: INTERNAL_ERROR,
        message: "something broke".to_owned(),
        data: None,
    };
    let msg = format!("{err}");
    assert!(msg.contains("-32603"), "should contain code: {msg}");
    assert!(
        msg.contains("something broke"),
        "should contain message: {msg}"
    );
}

#[test]
fn vsock_error_display_io() {
    let err = VsockError::Io(std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "pipe broke",
    ));
    let msg = format!("{err}");
    assert!(msg.contains("pipe broke"), "should contain IO error: {msg}");
}

#[test]
fn vsock_error_display_protocol() {
    let err = VsockError::Protocol("bad version".to_owned());
    let msg = format!("{err}");
    assert!(msg.contains("bad version"), "should contain details: {msg}");
}

// ── Overlay init tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn overlay_init_sends_correct_method_and_params() {
    let (mut client, mut server) = mock_client();

    let client_task =
        tokio::spawn(async move { client.overlay_init(Some("/rootfs".to_owned())).await });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    assert_eq!(req.method, "overlay_init");
    let params = req.params.unwrap();
    assert_eq!(params["lower_dir"], "/rootfs");

    // Send ok response
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::json!("ok")),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    client_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn overlay_init_with_none_sends_null_lower_dir() {
    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move { client.overlay_init(None).await });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    assert_eq!(req.method, "overlay_init");
    let params = req.params.unwrap();
    assert!(params["lower_dir"].is_null());

    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::json!("ok")),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    client_task.await.unwrap().unwrap();
}

// ── Snapshot layer tests ───────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_layer_parses_result_correctly() {
    use crate::vsock::protocol::SnapshotLayerResult;

    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move { client.snapshot_layer().await });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    assert_eq!(req.method, "snapshot_layer");
    assert!(req.params.is_none());

    // Send snapshot result response
    let layer_result = SnapshotLayerResult {
        data: "dGVzdA==".to_owned(),
        compressed_digest: "sha256:abc".to_owned(),
        uncompressed_digest: "sha256:def".to_owned(),
        compressed_size: 1024,
    };
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::to_value(&layer_result).unwrap()),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    let result = client_task.await.unwrap().unwrap();
    assert_eq!(result.data, "dGVzdA==");
    assert_eq!(result.compressed_digest, "sha256:abc");
    assert_eq!(result.uncompressed_digest, "sha256:def");
    assert_eq!(result.compressed_size, 1024);
}

// ── Flatten overlay tests ──────────────────────────────────────────────────

#[tokio::test]
async fn flatten_overlay_sends_correct_method() {
    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move { client.flatten_overlay().await });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    assert_eq!(req.method, "flatten_overlay");
    assert!(req.params.is_none());

    // Send ok response
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::json!("ok")),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    client_task.await.unwrap().unwrap();
}

// ── Copy files tests ──────────────────────────────────────────────────────

#[tokio::test]
async fn copy_files_sends_correct_params_and_parses_result() {
    use crate::vsock::protocol::CopyFilesResult;

    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move {
        client
            .copy_files("dGVzdA==".to_owned(), "/app".to_owned())
            .await
    });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    assert_eq!(req.method, "copy_files");
    let params = req.params.unwrap();
    assert_eq!(params["data"], "dGVzdA==");
    assert_eq!(params["dest"], "/app");

    // Send copy_files result response
    let copy_result = CopyFilesResult { files_written: 3 };
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(serde_json::to_value(&copy_result).unwrap()),
        error: None,
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    let result = client_task.await.unwrap().unwrap();
    assert_eq!(result.files_written, 3);
}

#[tokio::test]
async fn copy_files_rpc_error_returns_error() {
    let (mut client, mut server) = mock_client();

    let client_task = tokio::spawn(async move {
        client
            .copy_files("data".to_owned(), "/app".to_owned())
            .await
    });

    // Read request
    let mut buf = vec![0u8; 4096];
    let n = server.read(&mut buf).await.unwrap();
    let request_json = std::str::from_utf8(&buf[..n]).unwrap();
    let req: JsonRpcRequest = serde_json::from_str(request_json.trim()).unwrap();

    // Send error response
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: None,
        error: Some(JsonRpcError {
            code: INTERNAL_ERROR,
            message: "extraction failed".to_owned(),
            data: None,
        }),
        id: req.id,
    };
    send_response(&mut server, &resp).await;

    let err = client_task.await.unwrap().unwrap_err();
    match err {
        VsockError::Rpc { code, message, .. } => {
            assert_eq!(code, INTERNAL_ERROR);
            assert_eq!(message, "extraction failed");
        }
        other => panic!("expected RpcError, got {other:?}"),
    }
}
