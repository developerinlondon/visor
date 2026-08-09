//! Unix-domain-socket communication backend using the vsock muxer.
//!
//! Connects to guest VMs via a muxer socket at `{socket_dir}/{cid}.sock`.
//! The client sends `CONNECT {port}\n` and waits for an `OK` response
//! before using the stream for bidirectional traffic.

use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

use super::backend::{AsyncStream, CommsBackend, CommsError, DEFAULT_CONNECT_TIMEOUT};

/// Communication backend backed by the portable vsock muxer protocol.
pub struct MuxerCommsBackend {
    socket_dir: PathBuf,
}

impl MuxerCommsBackend {
    /// Default socket directory for guest communication.
    pub const DEFAULT_SOCKET_DIR: &str = "/var/run/visor/vsock";

    /// Environment variable that overrides the guest communication socket
    /// directory.
    pub const SOCKET_DIR_ENV: &str = "VISOR_VSOCK_SOCKET_DIR";

    /// Create a new backend with the configured socket directory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            socket_dir: Self::configured_socket_dir(),
        }
    }

    /// Resolve the socket directory from the runtime override or the default.
    #[must_use]
    pub fn configured_socket_dir() -> PathBuf {
        configured_socket_dir_from(std::env::var_os(Self::SOCKET_DIR_ENV))
    }

    /// Create a new backend with a custom socket directory.
    #[must_use]
    pub fn with_socket_dir(socket_dir: PathBuf) -> Self {
        Self { socket_dir }
    }

    /// Returns the socket directory path.
    #[must_use]
    pub fn socket_dir(&self) -> &Path {
        &self.socket_dir
    }

    /// Build the muxer socket path for a given CID.
    #[must_use]
    pub fn muxer_socket_path(&self, cid: u32) -> PathBuf {
        self.socket_dir.join(format!("{cid}.sock"))
    }

    /// Build the legacy per-port socket path for a given CID and port.
    #[must_use]
    pub fn socket_path(&self, cid: u32, port: u32) -> PathBuf {
        self.socket_dir
            .join(cid.to_string())
            .join(format!("{port}.sock"))
    }
}

fn configured_socket_dir_from(override_path: Option<std::ffi::OsString>) -> PathBuf {
    override_path.filter(|path| !path.is_empty()).map_or_else(
        || PathBuf::from(MuxerCommsBackend::DEFAULT_SOCKET_DIR),
        PathBuf::from,
    )
}

impl Default for MuxerCommsBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for MuxerCommsBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MuxerCommsBackend")
            .field("socket_dir", &self.socket_dir)
            .finish()
    }
}

impl CommsBackend for MuxerCommsBackend {
    async fn connect(&self, cid: u32, port: u32) -> Result<Box<dyn AsyncStream>, CommsError> {
        let muxer_path = self.muxer_socket_path(cid);

        let connect_and_handshake = async {
            let mut stream = tokio::net::UnixStream::connect(&muxer_path)
                .await
                .map_err(|source| CommsError::Connect { cid, port, source })?;

            stream
                .write_all(format!("CONNECT {port}\n").as_bytes())
                .await
                .map_err(|source| CommsError::Connect { cid, port, source })?;

            let mut reader = tokio::io::BufReader::new(&mut stream);
            let mut response = String::new();
            reader
                .read_line(&mut response)
                .await
                .map_err(|source| CommsError::Connect { cid, port, source })?;

            if !response.starts_with("OK") {
                return Err(CommsError::Connect {
                    cid,
                    port,
                    source: std::io::Error::new(
                        std::io::ErrorKind::ConnectionRefused,
                        format!("muxer rejected: {}", response.trim()),
                    ),
                });
            }

            Ok(stream)
        };

        match tokio::time::timeout(DEFAULT_CONNECT_TIMEOUT, connect_and_handshake).await {
            Ok(Ok(stream)) => Ok(Box::new(stream)),
            Ok(Err(error)) => Err(error),
            Err(_) => Err(CommsError::Timeout {
                cid,
                port,
                timeout: DEFAULT_CONNECT_TIMEOUT,
            }),
        }
    }
}

#[cfg(test)]
#[path = "muxer_test.rs"]
mod tests;
