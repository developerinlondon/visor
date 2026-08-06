use std::time::Duration;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

// ── Mock backend ──────────────────────────────────────────────────

/// Mock backend that returns a `DuplexStream` pair for testing.
struct MockCommsBackend {
    should_fail: bool,
}

impl MockCommsBackend {
    fn new() -> Self {
        Self { should_fail: false }
    }

    fn failing() -> Self {
        Self { should_fail: true }
    }
}

impl CommsBackend for MockCommsBackend {
    async fn connect(&self, cid: u32, port: u32) -> Result<Box<dyn AsyncStream>, CommsError> {
        if self.should_fail {
            return Err(CommsError::Connect {
                cid,
                port,
                source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "mock refused"),
            });
        }
        let (client, _server) = tokio::io::duplex(8192);
        Ok(Box::new(client))
    }
}

// ── CommsBackend trait via mock ───────────────────────────────────

#[tokio::test]
async fn mock_backend_connect_returns_stream() {
    let backend = MockCommsBackend::new();
    let stream = backend.connect(3, 52).await;
    assert!(stream.is_ok(), "mock connect should succeed");
}

#[tokio::test]
async fn mock_backend_connect_failure_returns_error() {
    let backend = MockCommsBackend::failing();
    let Err(err) = backend.connect(3, 52).await else {
        panic!("failing mock should return error");
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
        "should be a Connect error with correct CID/port: {err:?}"
    );
}

#[tokio::test]
async fn mock_backend_stream_is_readable_writable() {
    // Use a duplex where we keep both halves to test read/write.
    let (client, mut server) = tokio::io::duplex(8192);

    // Write from server side
    let write_task = tokio::spawn(async move {
        server.write_all(b"hello").await.unwrap();
        server.flush().await.unwrap();
        server
    });

    // The client side (what the backend would return) should be readable.
    let mut stream: Box<dyn AsyncStream> = Box::new(client);
    let mut buf = [0u8; 5];
    stream.read_exact(&mut buf).await.unwrap();
    assert_eq!(&buf, b"hello");

    let _server = write_task.await.unwrap();
}

// ── CommsError display tests ─────────────────────────────────────

#[test]
fn comms_error_connect_display() {
    let err = CommsError::Connect {
        cid: 3,
        port: 52,
        source: std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "refused"),
    };
    let msg = format!("{err}");
    assert!(msg.contains('3'), "should contain CID: {msg}");
    assert!(msg.contains("52"), "should contain port: {msg}");
    assert!(msg.contains("refused"), "should contain source: {msg}");
}

#[test]
fn comms_error_timeout_display() {
    let err = CommsError::Timeout {
        cid: 3,
        port: 52,
        timeout: Duration::from_secs(10),
    };
    let msg = format!("{err}");
    assert!(msg.contains('3'), "should contain CID: {msg}");
    assert!(msg.contains("52"), "should contain port: {msg}");
    assert!(msg.contains("10"), "should contain timeout: {msg}");
}

#[test]
fn comms_error_unsupported_display() {
    let err = CommsError::Unsupported;
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("not supported") || msg.to_lowercase().contains("unsupported"),
        "Unsupported error should mention unsupported: {msg}"
    );
}

#[test]
fn default_connect_timeout_is_10s() {
    assert_eq!(DEFAULT_CONNECT_TIMEOUT, Duration::from_secs(10));
}
