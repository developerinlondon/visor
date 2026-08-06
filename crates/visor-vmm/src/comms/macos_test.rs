use std::path::PathBuf;

use super::*;
use crate::comms::backend::{CommsBackend, CommsError};

// ── MacosCommsBackend construction ──────────────────────────────────

#[test]
fn macos_comms_backend_new() {
    let backend = MacosCommsBackend::new();
    assert_eq!(
        backend.socket_dir().to_str().unwrap(),
        MacosCommsBackend::DEFAULT_SOCKET_DIR
    );
}

#[test]
fn macos_comms_backend_default() {
    let backend = MacosCommsBackend::default();
    assert_eq!(
        backend.socket_dir().to_str().unwrap(),
        MacosCommsBackend::DEFAULT_SOCKET_DIR
    );
}

#[test]
fn macos_comms_backend_custom_socket_dir() {
    let backend = MacosCommsBackend::with_socket_dir(PathBuf::from("/tmp/visor-test"));
    assert_eq!(backend.socket_dir().to_str().unwrap(), "/tmp/visor-test");
}

#[test]
fn macos_comms_backend_debug() {
    let backend = MacosCommsBackend::new();
    let debug = format!("{backend:?}");
    assert!(debug.contains("MacosCommsBackend"));
    assert!(debug.contains("socket_dir"));
}

// ── Socket path generation ──────────────────────────────────────────

#[test]
fn socket_path_format() {
    let backend = MacosCommsBackend::with_socket_dir(PathBuf::from("/var/run/visor/vsock"));
    let path = backend.socket_path(3, 52);
    assert_eq!(path, PathBuf::from("/var/run/visor/vsock/3/52.sock"));
}

#[test]
fn socket_path_different_cid_and_port() {
    let backend = MacosCommsBackend::with_socket_dir(PathBuf::from("/tmp/socks"));
    assert_eq!(
        backend.socket_path(100, 1024),
        PathBuf::from("/tmp/socks/100/1024.sock")
    );
}

#[test]
fn socket_path_cid_zero() {
    let backend = MacosCommsBackend::new();
    let path = backend.socket_path(0, 0);
    assert!(path.to_str().unwrap().contains("0/0.sock"));
}

#[test]
fn muxer_socket_path_format() {
    let backend = MacosCommsBackend::with_socket_dir(PathBuf::from("/var/run/visor/vsock"));
    let path = backend.muxer_socket_path(3);
    assert_eq!(path, PathBuf::from("/var/run/visor/vsock/3.sock"));
}

#[test]
fn muxer_socket_path_different_cid() {
    let backend = MacosCommsBackend::with_socket_dir(PathBuf::from("/tmp/socks"));
    assert_eq!(
        backend.muxer_socket_path(42),
        PathBuf::from("/tmp/socks/42.sock")
    );
}

// ── Connect to missing socket ────────────────────────────────────────

#[tokio::test]
async fn connect_to_missing_socket_returns_error() {
    let dir = crate::testutil::tempdir("visor-vmm-macos-comms-").unwrap();
    let backend = MacosCommsBackend::with_socket_dir(dir.path().to_path_buf());

    let result = backend.connect(3, 52).await;
    let Err(err) = result else {
        panic!("connecting to missing socket should fail");
    };
    assert!(
        matches!(
            err,
            CommsError::Connect {
                cid: 3,
                port: 52,
                ..
            }
        ),
        "expected Connect error for cid=3 port=52, got: {err:?}"
    );
}

#[tokio::test]
async fn connect_error_contains_cid_and_port() {
    let dir = crate::testutil::tempdir("visor-vmm-macos-comms-").unwrap();
    let backend = MacosCommsBackend::with_socket_dir(dir.path().to_path_buf());

    let Err(err) = backend.connect(42, 9999).await else {
        panic!("connecting to missing socket should fail");
    };
    let msg = format!("{err}");
    assert!(msg.contains("42"), "error should mention CID: {msg}");
    assert!(msg.contains("9999"), "error should mention port: {msg}");
}

// ── Connect via muxer protocol (integration-style) ──────────────────

#[tokio::test]
async fn connect_to_listening_socket_succeeds() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let dir = crate::testutil::tempdir("visor-vmm-macos-comms-").unwrap();
    let cid = 5_u32;
    let port = 1024_u32;

    // Bind a muxer-style socket at {dir}/{cid}.sock
    let muxer_path = dir.path().join(format!("{cid}.sock"));
    let listener = tokio::net::UnixListener::bind(&muxer_path).unwrap();

    let backend = MacosCommsBackend::with_socket_dir(dir.path().to_path_buf());

    // Spawn a mock muxer that accepts the CONNECT handshake, then echoes data.
    let echo_handle = tokio::spawn(async move {
        let (mut stream, _addr) = listener.accept().await.unwrap();

        // Read CONNECT line.
        let mut reader = tokio::io::BufReader::new(&mut stream);
        let mut line = String::new();
        reader.read_line(&mut line).await.unwrap();
        assert!(
            line.starts_with("CONNECT "),
            "expected CONNECT, got: {line}"
        );

        // Respond with OK.
        stream.write_all(b"OK 1073741824\n").await.unwrap();

        // Echo data.
        let mut buf = [0u8; 64];
        let n = stream.read(&mut buf).await.unwrap();
        stream.write_all(&buf[..n]).await.unwrap();
    });

    // Connect via the backend.
    let mut stream = backend
        .connect(cid, port)
        .await
        .expect("connect should succeed");

    // Write and read back.
    let msg = b"hello visor";
    stream.write_all(msg).await.unwrap();
    let mut response = vec![0u8; msg.len()];
    stream.read_exact(&mut response).await.unwrap();
    assert_eq!(&response, msg);

    echo_handle.await.unwrap();
}

#[tokio::test]
async fn connect_returns_async_stream() {
    // Verify the returned type satisfies AsyncStream bounds.
    let dir = crate::testutil::tempdir("visor-vmm-macos-comms-").unwrap();

    // Bind a muxer socket at {dir}/7.sock
    let muxer_path = dir.path().join("7.sock");
    let listener = tokio::net::UnixListener::bind(&muxer_path).unwrap();

    // Spawn mock muxer that responds to CONNECT.
    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut reader = tokio::io::BufReader::new(&mut stream);
        let mut line = String::new();
        let _ = reader.read_line(&mut line).await;
        stream.write_all(b"OK 42\n").await.unwrap();
    });

    let backend = MacosCommsBackend::with_socket_dir(dir.path().to_path_buf());
    let stream = backend.connect(7, 80).await.unwrap();

    // The stream is Box<dyn AsyncStream> — verify it's usable.
    // AsyncStream: AsyncRead + AsyncWrite + Unpin + Send
    fn assert_async_stream(_s: &dyn crate::comms::backend::AsyncStream) {}
    assert_async_stream(&*stream);
}
