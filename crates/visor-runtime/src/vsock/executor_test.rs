//! Tests for [`VsockBuildExecutor`].
//!
//! Uses `tokio::io::duplex` mock streams following the same pattern as
//! `client_test.rs`.

use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};
use visor_build::dockerfile::MountType;
use visor_build::engine::{BuildExecutor, ResolvedMount};
use visor_vmm::comms::AsyncStream;

use crate::vsock::client::VsockClient;
use crate::vsock::executor::VsockBuildExecutor;
use crate::vsock::protocol::{
    CopyFilesResult, ExecResult, INTERNAL_ERROR, JsonRpcError, JsonRpcRequest, JsonRpcResponse,
    SnapshotLayerResult,
};

// ── Helper: mock agent ───────────────────────────────────────────────────────────

/// Create a `VsockBuildExecutor` backed by a duplex stream for testing.
/// Returns `(executor, server_half)` where `server_half` simulates visor-init.
fn mock_executor() -> (VsockBuildExecutor, DuplexStream) {
    let (client_stream, server_stream) = tokio::io::duplex(8192);
    let boxed: Box<dyn AsyncStream> = Box::new(client_stream);
    let client = VsockClient::from_stream(boxed);
    let executor = VsockBuildExecutor::new(client);
    (executor, server_stream)
}

/// Read a JSON-RPC request from the server side of a duplex stream.
async fn read_request(server: &mut DuplexStream) -> JsonRpcRequest {
    let mut buf = vec![0u8; 8192];
    let n = server.read(&mut buf).await.unwrap();
    let json = std::str::from_utf8(&buf[..n]).unwrap();
    serde_json::from_str(json.trim()).unwrap()
}

/// Write a JSON-RPC success response to the server side.
async fn send_ok(server: &mut DuplexStream, id: serde_json::Value, result: serde_json::Value) {
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: Some(result),
        error: None,
        id,
    };
    let json = serde_json::to_string(&resp).unwrap();
    server.write_all(json.as_bytes()).await.unwrap();
    server.write_all(b"\n").await.unwrap();
    server.flush().await.unwrap();
}

/// Write a JSON-RPC error response to the server side.
async fn send_error(server: &mut DuplexStream, id: serde_json::Value, code: i32, message: &str) {
    let resp = JsonRpcResponse {
        jsonrpc: "2.0".to_owned(),
        result: None,
        error: Some(JsonRpcError {
            code,
            message: message.to_owned(),
            data: None,
        }),
        id,
    };
    let json = serde_json::to_string(&resp).unwrap();
    server.write_all(json.as_bytes()).await.unwrap();
    server.write_all(b"\n").await.unwrap();
    server.flush().await.unwrap();
}

// ── overlay_init tests ───────────────────────────────────────────────────────

#[tokio::test]
async fn overlay_init_delegates_to_client() {
    let (executor, mut server) = mock_executor();

    let exec_task =
        tokio::spawn(async move { executor.overlay_init(Some("/rootfs".to_owned())).await });

    let req = read_request(&mut server).await;
    assert_eq!(req.method, "overlay_init");
    send_ok(&mut server, req.id, serde_json::json!("ok")).await;

    exec_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn overlay_init_with_none_lower_dir() {
    let (executor, mut server) = mock_executor();

    let exec_task = tokio::spawn(async move { executor.overlay_init(None).await });

    let req = read_request(&mut server).await;
    assert_eq!(req.method, "overlay_init");
    let params = req.params.unwrap();
    assert!(params["lower_dir"].is_null());
    send_ok(&mut server, req.id, serde_json::json!("ok")).await;

    exec_task.await.unwrap().unwrap();
}

// ── exec tests ───────────────────────────────────────────────────────────────

#[tokio::test]
async fn exec_returns_exit_code_stdout_stderr() {
    let (executor, mut server) = mock_executor();

    let exec_task = tokio::spawn(async move {
        executor
            .exec(
                &["echo".to_owned(), "hello".to_owned()],
                &["HOME=/root".to_owned()],
                "/tmp",
            )
            .await
    });

    let req = read_request(&mut server).await;
    assert_eq!(req.method, "exec");

    let result = ExecResult {
        exit_code: 0,
        stdout: "hello\n".to_owned(),
        stderr: String::new(),
    };
    send_ok(&mut server, req.id, serde_json::to_value(&result).unwrap()).await;

    let (code, stdout, stderr) = exec_task.await.unwrap().unwrap();
    assert_eq!(code, 0);
    assert_eq!(stdout, "hello\n");
    assert!(stderr.is_empty());
}

#[tokio::test]
async fn exec_returns_nonzero_exit_code() {
    let (executor, mut server) = mock_executor();

    let exec_task =
        tokio::spawn(async move { executor.exec(&["false".to_owned()], &[], "/").await });

    let req = read_request(&mut server).await;
    let result = ExecResult {
        exit_code: 1,
        stdout: String::new(),
        stderr: "error\n".to_owned(),
    };
    send_ok(&mut server, req.id, serde_json::to_value(&result).unwrap()).await;

    let (code, _stdout, stderr) = exec_task.await.unwrap().unwrap();
    assert_eq!(code, 1);
    assert_eq!(stderr, "error\n");
}

// ── snapshot_layer tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn snapshot_layer_maps_to_layer_snapshot() {
    let (executor, mut server) = mock_executor();

    let exec_task = tokio::spawn(async move { executor.snapshot_layer().await });

    let req = read_request(&mut server).await;
    assert_eq!(req.method, "snapshot_layer");

    let layer = SnapshotLayerResult {
        data: "dGVzdA==".to_owned(),
        compressed_digest: "sha256:abc123".to_owned(),
        uncompressed_digest: "sha256:def456".to_owned(),
        compressed_size: 2048,
    };
    send_ok(&mut server, req.id, serde_json::to_value(&layer).unwrap()).await;

    let snap = exec_task.await.unwrap().unwrap();
    assert_eq!(snap.data, "dGVzdA==");
    assert_eq!(snap.compressed_digest, "sha256:abc123");
    assert_eq!(snap.uncompressed_digest, "sha256:def456");
    assert_eq!(snap.compressed_size, 2048);
}

// ── flatten_overlay tests ────────────────────────────────────────────────────

#[tokio::test]
async fn flatten_overlay_delegates_to_client() {
    let (executor, mut server) = mock_executor();

    let exec_task = tokio::spawn(async move { executor.flatten_overlay().await });

    let req = read_request(&mut server).await;
    assert_eq!(req.method, "flatten_overlay");
    send_ok(&mut server, req.id, serde_json::json!("ok")).await;

    exec_task.await.unwrap().unwrap();
}

// ── copy_to_guest tests ──────────────────────────────────────────────────

#[tokio::test]
async fn copy_to_guest_sends_tar_gz_via_copy_files() {
    let (executor, mut server) = mock_executor();

    // Create a temp file to copy
    let dir = crate::testutil::tempdir("visor-runtime-vsock-").unwrap();
    let file_path = dir.path().join("hello.txt");
    std::fs::write(&file_path, "hello world").unwrap();

    let exec_task = {
        let path = file_path.clone();
        tokio::spawn(async move { executor.copy_to_guest(&[path], "/app").await })
    };

    // Read the copy_files request from mock server
    let req = read_request(&mut server).await;
    assert_eq!(req.method, "copy_files");
    let params = req.params.unwrap();
    assert_eq!(params["dest"], "/app");

    // Verify the data field is valid base64-encoded tar.gz
    let data = params["data"].as_str().unwrap();
    use base64::Engine as _;
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(data)
        .unwrap();
    let decoder = flate2::read::GzDecoder::new(&compressed[..]);
    let mut archive = tar::Archive::new(decoder);
    let mut found_file = false;
    for entry in archive.entries().unwrap() {
        let entry = entry.unwrap();
        let path = entry.path().unwrap();
        if path.to_str().unwrap().contains("hello.txt") {
            found_file = true;
        }
    }
    assert!(found_file, "tar archive should contain hello.txt");

    // Send success response
    let result = CopyFilesResult { files_written: 1 };
    send_ok(&mut server, req.id, serde_json::to_value(&result).unwrap()).await;

    exec_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn copy_to_guest_with_directory_sends_tar_gz() {
    let (executor, mut server) = mock_executor();

    // Create a temp directory with files to copy
    let dir = crate::testutil::tempdir("visor-runtime-vsock-").unwrap();
    let sub = dir.path().join("subdir");
    std::fs::create_dir(&sub).unwrap();
    std::fs::write(dir.path().join("a.txt"), "aaa").unwrap();
    std::fs::write(sub.join("b.txt"), "bbb").unwrap();

    let src_dir = dir.path().to_path_buf();
    let exec_task = tokio::spawn(async move { executor.copy_to_guest(&[src_dir], "/opt").await });

    let req = read_request(&mut server).await;
    assert_eq!(req.method, "copy_files");
    assert_eq!(req.params.as_ref().unwrap()["dest"], "/opt");

    // Send success response
    let result = CopyFilesResult { files_written: 2 };
    send_ok(&mut server, req.id, serde_json::to_value(&result).unwrap()).await;

    exec_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn copy_to_guest_error_propagates() {
    let (executor, mut server) = mock_executor();

    let dir = crate::testutil::tempdir("visor-runtime-vsock-").unwrap();
    let file_path = dir.path().join("test.txt");
    std::fs::write(&file_path, "content").unwrap();

    let exec_task = {
        let path = file_path.clone();
        tokio::spawn(async move { executor.copy_to_guest(&[path], "/app").await })
    };

    let req = read_request(&mut server).await;
    send_error(&mut server, req.id, INTERNAL_ERROR, "disk full").await;

    let err = exec_task.await.unwrap().unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("disk full") || msg.contains("copy_files"),
        "error should mention cause: {msg}"
    );
}

// ── setup_mount tests ────────────────────────────────────────────────────────

#[tokio::test]
async fn setup_mount_tmpfs_sends_mount_command() {
    let (executor, mut server) = mock_executor();

    let mount = ResolvedMount::new(MountType::Tmpfs, "/tmp/cache".to_owned(), None, false, None);

    let exec_task = tokio::spawn(async move { executor.setup_mount(&mount).await });

    let req = read_request(&mut server).await;
    assert_eq!(req.method, "exec");
    let params = req.params.unwrap();
    let cmd: Vec<String> = serde_json::from_value(params["cmd"].clone()).unwrap();
    assert_eq!(cmd, vec!["mount", "-t", "tmpfs", "tmpfs", "/tmp/cache"]);

    let result = ExecResult {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
    };
    send_ok(&mut server, req.id, serde_json::to_value(&result).unwrap()).await;

    exec_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn setup_mount_cache_sends_mkdir() {
    let (executor, mut server) = mock_executor();

    let mount = ResolvedMount::new(
        MountType::Cache,
        "/var/cache/apt".to_owned(),
        None,
        false,
        Some("apt".to_owned()),
    );

    let exec_task = tokio::spawn(async move { executor.setup_mount(&mount).await });

    let req = read_request(&mut server).await;
    assert_eq!(req.method, "exec");
    let params = req.params.unwrap();
    let cmd: Vec<String> = serde_json::from_value(params["cmd"].clone()).unwrap();
    assert_eq!(cmd, vec!["mkdir", "-p", "/var/cache/apt"]);

    let result = ExecResult {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
    };
    send_ok(&mut server, req.id, serde_json::to_value(&result).unwrap()).await;

    exec_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn setup_mount_bind_returns_ok() {
    let (executor, _server) = mock_executor();

    let mount = ResolvedMount::new(
        MountType::Bind,
        "/data".to_owned(),
        Some("/host/data".to_owned()),
        true,
        None,
    );

    let result = executor.setup_mount(&mount).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn setup_mount_secret_returns_ok() {
    let (executor, _server) = mock_executor();

    let mount = ResolvedMount::new(
        MountType::Secret,
        "/run/secrets/token".to_owned(),
        None,
        true,
        Some("token".to_owned()),
    );

    let result = executor.setup_mount(&mount).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn setup_mount_ssh_returns_ok() {
    let (executor, _server) = mock_executor();

    let mount = ResolvedMount::new(MountType::Ssh, "/run/ssh".to_owned(), None, true, None);

    let result = executor.setup_mount(&mount).await;
    assert!(result.is_ok());
}

// ── teardown_mount tests ─────────────────────────────────────────────────────

#[tokio::test]
async fn teardown_mount_tmpfs_sends_umount() {
    let (executor, mut server) = mock_executor();

    let mount = ResolvedMount::new(MountType::Tmpfs, "/tmp/cache".to_owned(), None, false, None);

    let exec_task = tokio::spawn(async move { executor.teardown_mount(&mount).await });

    let req = read_request(&mut server).await;
    assert_eq!(req.method, "exec");
    let params = req.params.unwrap();
    let cmd: Vec<String> = serde_json::from_value(params["cmd"].clone()).unwrap();
    assert_eq!(cmd, vec!["umount", "/tmp/cache"]);

    let result = ExecResult {
        exit_code: 0,
        stdout: String::new(),
        stderr: String::new(),
    };
    send_ok(&mut server, req.id, serde_json::to_value(&result).unwrap()).await;

    exec_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn teardown_mount_cache_returns_ok() {
    let (executor, _server) = mock_executor();

    let mount = ResolvedMount::new(
        MountType::Cache,
        "/var/cache/apt".to_owned(),
        None,
        false,
        None,
    );

    let result = executor.teardown_mount(&mount).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn teardown_mount_bind_returns_ok() {
    let (executor, _server) = mock_executor();

    let mount = ResolvedMount::new(MountType::Bind, "/data".to_owned(), None, false, None);

    let result = executor.teardown_mount(&mount).await;
    assert!(result.is_ok());
}

// ── error propagation tests ──────────────────────────────────────────────────

#[tokio::test]
async fn overlay_init_error_propagates_as_anyhow() {
    let (executor, mut server) = mock_executor();

    let exec_task = tokio::spawn(async move { executor.overlay_init(None).await });

    let req = read_request(&mut server).await;
    send_error(&mut server, req.id, INTERNAL_ERROR, "disk full").await;

    let err = exec_task.await.unwrap().unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("disk full") || msg.contains("overlay_init"),
        "error should mention cause: {msg}"
    );
}

#[tokio::test]
async fn exec_error_propagates_as_anyhow() {
    let (executor, mut server) = mock_executor();

    let exec_task = tokio::spawn(async move { executor.exec(&["ls".to_owned()], &[], "/").await });

    let req = read_request(&mut server).await;
    send_error(&mut server, req.id, INTERNAL_ERROR, "exec failed").await;

    let err = exec_task.await.unwrap().unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("exec failed") || msg.contains("exec"),
        "error should mention cause: {msg}"
    );
}

#[tokio::test]
async fn snapshot_layer_error_propagates_as_anyhow() {
    let (executor, mut server) = mock_executor();

    let exec_task = tokio::spawn(async move { executor.snapshot_layer().await });

    let req = read_request(&mut server).await;
    send_error(&mut server, req.id, INTERNAL_ERROR, "no overlay").await;

    let err = exec_task.await.unwrap().unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("no overlay") || msg.contains("snapshot"),
        "error should mention cause: {msg}"
    );
}
