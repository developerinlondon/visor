//! Communication backend trait and portable types.
//!
//! Defines the [`CommsBackend`] trait for platform-agnostic guest
//! communication channel setup and the [`CommsError`] error type.

use std::future::Future;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncWrite};

// ── Stream trait ──────────────────────────────────────────────────

/// A trait combining [`AsyncRead`], [`AsyncWrite`], [`Unpin`], and [`Send`]
/// for use as an async communication stream.
///
/// This blanket-implemented trait allows passing any compatible async I/O
/// type (e.g., `TcpStream`, `DuplexStream`) as a boxed trait object.
pub trait AsyncStream: AsyncRead + AsyncWrite + Unpin + Send {}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> AsyncStream for T {}

// ── Errors ────────────────────────────────────────────────────────

/// Default timeout for establishing a communication connection.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Errors from communication backend operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CommsError {
    /// Failed to establish a connection to the guest.
    #[error("connection to CID {cid} port {port} failed: {source}")]
    Connect {
        /// Guest VM context ID.
        cid: u32,
        /// Port number on the guest.
        port: u32,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// Connection attempt timed out.
    #[error("connection to CID {cid} port {port} timed out after {timeout:?}")]
    Timeout {
        /// Guest VM context ID.
        cid: u32,
        /// Port number on the guest.
        port: u32,
        /// How long we waited before giving up.
        timeout: Duration,
    },

    /// Communication is not supported on this platform.
    #[error("communication not supported on this platform")]
    Unsupported,
}

// ── Trait ──────────────────────────────────────────────────────────

/// Abstraction over platform-specific guest communication channel setup.
///
/// Implementations create a connected async stream to a guest VM given
/// a context ID (CID) and port number. The returned stream can be used
/// with [`tokio::io`] read/write operations.
///
/// # Platform implementations
///
/// - **Linux**: [`crate::comms::linux::LinuxCommsBackend`] — Unix sockets via the vsock muxer
/// - **macOS**: [`crate::comms::macos::MacosCommsBackend`] — Unix sockets via the vsock muxer
/// - **Windows**: stub returning [`CommsError::Unsupported`]
pub trait CommsBackend: Send + Sync {
    /// Establish a connected async stream to a guest VM.
    ///
    /// Connects to the guest at the given `cid` and `port`, returning
    /// a boxed async stream ready for reading and writing.
    ///
    /// # Errors
    ///
    /// Returns [`CommsError::Connect`] if the connection cannot be established,
    /// [`CommsError::Timeout`] if the connection attempt exceeds the backend's
    /// configured timeout, or [`CommsError::Unsupported`] if the platform
    /// does not support this communication channel.
    fn connect(
        &self,
        cid: u32,
        port: u32,
    ) -> impl Future<Output = Result<Box<dyn AsyncStream>, CommsError>> + Send;
}

#[cfg(test)]
#[path = "backend_test.rs"]
mod tests;
