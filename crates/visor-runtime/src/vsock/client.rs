//! Host-side vsock client for communicating with visor-init inside guest VMs.
//!
//! Uses virtio-vsock (`AF_VSOCK`) to send JSON-RPC 2.0 requests to the guest
//! agent running on port 52 and receive responses. The wire protocol uses
//! newline-delimited JSON: each message is a single JSON object followed by `\n`.
//!
//! # Architecture
//!
//! ```text
//! visor-runtime (host)          visor-init (guest)
//!   VsockClient ─── AF_VSOCK ───► agent (port 52)
//!     ping()    ─── {"method":"ping"} + \n ──►
//!               ◄── {"result":"pong"} + \n ───
//! ```
//!
//! # Usage
//!
//! ```rust,ignore
//! use visor_runtime::vsock::client::VsockClient;
//! use visor_vmm::comms::create_comms_backend;
//!
//! # async fn example() -> anyhow::Result<()> {
//! let backend = create_comms_backend();
//! let mut client = VsockClient::connect(&backend, 3, 52).await?;
//! let pong = client.ping().await?;
//! assert_eq!(pong, "pong");
//! # Ok(())
//! # }
//! ```

use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use visor_vmm::comms::{AsyncStream, CommsBackend, CommsError};

use super::protocol::{
    CopyFilesParams, CopyFilesResult, ExecParams, ExecResult, JsonRpcRequest, JsonRpcResponse,
    KillParams, OverlayInitParams, SnapshotLayerResult, parse_response,
};

/// Default timeout for establishing a vsock connection.
pub const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Timeout for the initial agent readiness probe during connect.
const CONNECT_PROBE_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_STABILIZATION_DELAY: Duration = Duration::from_millis(250);

/// Default timeout for a single JSON-RPC request/response round trip.
pub const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// The well-known vsock port that visor-init listens on.
pub const VSOCK_AGENT_PORT: u32 = 52;

// ── Error types ─────────────────────────────────────────────────────────────

/// Errors specific to vsock client operations.
///
/// These are typed errors for the vsock module. Call sites in the binary crate
/// wrap them with `anyhow::Context` as needed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VsockError {
    /// Failed to establish a vsock connection to the guest.
    #[error("vsock connect to CID {cid} port {port} failed: {source}")]
    Connect {
        /// Guest VM context ID.
        cid: u32,
        /// Port number on the guest.
        port: u32,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A request/response round trip exceeded the configured timeout.
    #[error("vsock operation '{operation}' timed out after {duration:?}")]
    Timeout {
        /// Name of the operation that timed out.
        operation: String,
        /// How long we waited before giving up.
        duration: Duration,
    },

    /// The guest returned a JSON-RPC error response.
    #[error("JSON-RPC error {code}: {message}")]
    Rpc {
        /// Numeric error code from the JSON-RPC error object.
        code: i32,
        /// Human-readable error description.
        message: String,
        /// Optional additional error data.
        data: Option<serde_json::Value>,
    },

    /// Low-level I/O error during read/write.
    #[error("vsock I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Protocol-level error (malformed response, version mismatch, etc.).
    #[error("vsock protocol error: {0}")]
    Protocol(String),
}

// ── Client ──────────────────────────────────────────────────────────────────

/// A host-side JSON-RPC 2.0 client over virtio-vsock.
///
/// Generic over the transport stream to support both real `AF_VSOCK` sockets
/// (via [`connect`](Self::connect)) and mock streams for testing
/// (via [`from_stream`](Self::from_stream)).
pub struct VsockClient<S: AsyncRead + AsyncWrite + Unpin> {
    reader: BufReader<tokio::io::ReadHalf<S>>,
    writer: tokio::io::WriteHalf<S>,
    request_timeout: Duration,
}

impl VsockClient<Box<dyn AsyncStream>> {
    /// Connect to a guest VM's visor-init agent over vsock.
    ///
    /// Delegates to the provided [`CommsBackend`] to establish the
    /// platform-specific transport, then wraps the resulting stream
    /// in a `VsockClient` for JSON-RPC communication. The returned client
    /// is only considered ready once the guest agent successfully answers
    /// an initial `ping` request.
    ///
    /// # Errors
    ///
    /// Returns [`VsockError::Connect`] if the backend cannot establish
    /// a connection, or [`VsockError::Timeout`] if the connection attempt
    /// exceeds the backend's configured timeout.
    pub async fn connect(
        backend: &impl CommsBackend,
        cid: u32,
        port: u32,
    ) -> Result<Self, VsockError> {
        let stream = backend.connect(cid, port).await.map_err(|e| match e {
            CommsError::Connect { cid, port, source } => VsockError::Connect { cid, port, source },
            CommsError::Timeout { cid, port, timeout } => VsockError::Timeout {
                operation: format!("connect to CID {cid} port {port}"),
                duration: timeout,
            },
            CommsError::Unsupported => VsockError::Connect {
                cid,
                port,
                source: std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "communication not supported on this platform",
                ),
            },
            _ => VsockError::Connect {
                cid,
                port,
                source: std::io::Error::other(format!("{e}")),
            },
        })?;
        let mut client = Self::from_stream(stream);
        tokio::time::sleep(CONNECT_STABILIZATION_DELAY).await;
        client.set_request_timeout(CONNECT_PROBE_TIMEOUT);
        client.ping().await?;
        client.set_request_timeout(DEFAULT_REQUEST_TIMEOUT);
        Ok(client)
    }

    /// Connect to a guest and start a streaming exec session.
    ///
    /// The returned stream is ready for raw bidirectional I/O after the guest
    /// acknowledges the `exec_stream` request. This is used for Docker-style
    /// hijacked exec sessions such as `BuildKit`'s `buildctl dial-stdio`.
    ///
    /// # Errors
    ///
    /// Returns [`VsockError::Connect`] if the transport cannot be established,
    /// [`VsockError::Timeout`] if the handshake exceeds the request timeout, or
    /// [`VsockError::Rpc`] / [`VsockError::Protocol`] if the guest rejects the
    /// streaming exec request.
    pub async fn connect_exec_stream(
        backend: &impl CommsBackend,
        cid: u32,
        port: u32,
        cmd: Vec<String>,
        env: Vec<String>,
        workdir: String,
        tty: bool,
    ) -> Result<Box<dyn AsyncStream>, VsockError> {
        let stream = backend.connect(cid, port).await.map_err(|e| match e {
            CommsError::Connect { cid, port, source } => VsockError::Connect { cid, port, source },
            CommsError::Timeout { cid, port, timeout } => VsockError::Timeout {
                operation: format!("connect to CID {cid} port {port}"),
                duration: timeout,
            },
            CommsError::Unsupported => VsockError::Connect {
                cid,
                port,
                source: std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "communication not supported on this platform",
                ),
            },
            _ => VsockError::Connect {
                cid,
                port,
                source: std::io::Error::other(format!("{e}")),
            },
        })?;
        tokio::time::sleep(CONNECT_STABILIZATION_DELAY).await;
        negotiate_exec_stream(stream, cmd, env, workdir, tty, DEFAULT_REQUEST_TIMEOUT).await
    }
}

impl<S: AsyncRead + AsyncWrite + Unpin> VsockClient<S> {
    /// Create a client from an existing async stream (useful for testing).
    #[must_use]
    pub fn from_stream(stream: S) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
        Self {
            reader: BufReader::new(read_half),
            writer: write_half,
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
        }
    }

    /// Override the request timeout (useful for testing).
    pub fn set_request_timeout(&mut self, timeout: Duration) {
        self.request_timeout = timeout;
    }

    /// Send a ping request and return the response string (should be `"pong"`).
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, times out, or the guest returns
    /// an RPC error.
    pub async fn ping(&mut self) -> Result<String, VsockError> {
        let resp = self.send_request("ping", None).await?;
        let result = resp
            .result
            .ok_or_else(|| VsockError::Protocol("ping response missing result".to_owned()))?;
        serde_json::from_value(result)
            .map_err(|e| VsockError::Protocol(format!("invalid ping result: {e}")))
    }

    /// Execute a command inside the guest VM.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, times out, or the guest returns
    /// an RPC error. Also returns an error if the response cannot be parsed
    /// as an [`ExecResult`].
    pub async fn exec(
        &mut self,
        cmd: Vec<String>,
        env: Vec<String>,
        workdir: String,
    ) -> Result<ExecResult, VsockError> {
        let params = ExecParams {
            cmd,
            env,
            workdir,
            tty: false,
        };
        let params_value = serde_json::to_value(&params)
            .map_err(|e| VsockError::Protocol(format!("failed to serialize exec params: {e}")))?;
        let resp = self.send_request("exec", Some(params_value)).await?;
        let result = resp
            .result
            .ok_or_else(|| VsockError::Protocol("exec response missing result".to_owned()))?;
        serde_json::from_value(result)
            .map_err(|e| VsockError::Protocol(format!("invalid exec result: {e}")))
    }

    /// Send a signal to the running process inside the guest.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, times out, or the guest returns
    /// an RPC error.
    pub async fn kill(&mut self, signal: i32) -> Result<(), VsockError> {
        let params = KillParams { signal };
        let params_value = serde_json::to_value(&params)
            .map_err(|e| VsockError::Protocol(format!("failed to serialize kill params: {e}")))?;
        let resp = self.send_request("kill", Some(params_value)).await?;
        resp.result
            .ok_or_else(|| VsockError::Protocol("kill response missing result".to_owned()))?;
        Ok(())
    }

    /// Request a graceful shutdown of the guest VM.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, times out, or the guest returns
    /// an RPC error.
    pub async fn shutdown(&mut self) -> Result<(), VsockError> {
        let resp = self.send_request("shutdown", None).await?;
        resp.result
            .ok_or_else(|| VsockError::Protocol("shutdown response missing result".to_owned()))?;
        Ok(())
    }

    /// Request the guest to initialize an overlayfs for build operations.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, times out, or the guest returns
    /// an RPC error.
    pub async fn overlay_init(&mut self, lower_dir: Option<String>) -> Result<(), VsockError> {
        let params = OverlayInitParams { lower_dir };
        let params_value = serde_json::to_value(&params).map_err(|e| {
            VsockError::Protocol(format!("failed to serialize overlay_init params: {e}"))
        })?;
        let resp = self
            .send_request("overlay_init", Some(params_value))
            .await?;
        resp.result.ok_or_else(|| {
            VsockError::Protocol("overlay_init response missing result".to_owned())
        })?;
        Ok(())
    }

    /// Request the guest to snapshot the overlay upper directory as a layer.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, times out, or the guest returns
    /// an RPC error. Also returns an error if the response cannot be parsed
    /// as a [`SnapshotLayerResult`].
    pub async fn snapshot_layer(&mut self) -> Result<SnapshotLayerResult, VsockError> {
        let resp = self.send_request("snapshot_layer", None).await?;
        let result = resp.result.ok_or_else(|| {
            VsockError::Protocol("snapshot_layer response missing result".to_owned())
        })?;
        serde_json::from_value(result)
            .map_err(|e| VsockError::Protocol(format!("invalid snapshot_layer result: {e}")))
    }

    /// Request the guest to flatten the overlay and reset for the next instruction.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, times out, or the guest returns
    /// an RPC error.
    pub async fn flatten_overlay(&mut self) -> Result<(), VsockError> {
        let resp = self.send_request("flatten_overlay", None).await?;
        resp.result.ok_or_else(|| {
            VsockError::Protocol("flatten_overlay response missing result".to_owned())
        })?;
        Ok(())
    }

    /// Copy files to the guest by sending a base64-encoded tar.gz archive.
    ///
    /// The guest extracts the archive at the specified destination directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the request fails, times out, or the guest returns
    /// an RPC error. Also returns an error if the response cannot be parsed
    /// as a [`CopyFilesResult`].
    pub async fn copy_files(
        &mut self,
        data: String,
        dest: String,
    ) -> Result<CopyFilesResult, VsockError> {
        let params = CopyFilesParams { data, dest };
        let params_value = serde_json::to_value(&params).map_err(|e| {
            VsockError::Protocol(format!("failed to serialize copy_files params: {e}"))
        })?;
        let resp = self.send_request("copy_files", Some(params_value)).await?;
        let result = resp
            .result
            .ok_or_else(|| VsockError::Protocol("copy_files response missing result".to_owned()))?;
        serde_json::from_value(result)
            .map_err(|e| VsockError::Protocol(format!("invalid copy_files result: {e}")))
    }

    /// Send a JSON-RPC request and wait for the response, applying the request timeout.
    ///
    /// # Errors
    ///
    /// Returns [`VsockError::Timeout`] if no response arrives within the configured
    /// timeout. Returns [`VsockError::Rpc`] if the guest sends an error response.
    /// Returns [`VsockError::Io`] on transport errors.
    async fn send_request(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<JsonRpcResponse, VsockError> {
        let timeout = self.request_timeout;
        let method_name = method.to_owned();
        match tokio::time::timeout(timeout, self.send_request_inner(method, params)).await {
            Ok(result) => result,
            Err(_elapsed) => Err(VsockError::Timeout {
                operation: method_name,
                duration: timeout,
            }),
        }
    }

    /// Inner request/response logic without timeout wrapping.
    async fn send_request_inner(
        &mut self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<JsonRpcResponse, VsockError> {
        // Build and serialize the request
        let request = JsonRpcRequest::new(method, params);
        let json = request
            .to_json()
            .map_err(|e| VsockError::Protocol(format!("failed to serialize request: {e}")))?;

        // Send: JSON line + newline delimiter
        self.writer.write_all(json.as_bytes()).await?;
        self.writer.write_all(b"\n").await?;
        self.writer.flush().await?;

        // Read one newline-delimited response line
        let mut line = String::new();
        let bytes_read = self.reader.read_line(&mut line).await?;
        if bytes_read == 0 {
            return Err(VsockError::Protocol(
                "connection closed: empty response from guest".to_owned(),
            ));
        }

        // Parse the response
        let resp = parse_response(line.trim())
            .map_err(|e| VsockError::Protocol(format!("failed to parse response: {e}")))?;

        // Check for JSON-RPC error
        if let Some(rpc_error) = resp.error {
            return Err(VsockError::Rpc {
                code: rpc_error.code,
                message: rpc_error.message,
                data: rpc_error.data,
            });
        }

        Ok(resp)
    }
}

fn rpc_response_result(resp: JsonRpcResponse) -> Result<JsonRpcResponse, VsockError> {
    if let Some(rpc_error) = resp.error {
        return Err(VsockError::Rpc {
            code: rpc_error.code,
            message: rpc_error.message,
            data: rpc_error.data,
        });
    }

    Ok(resp)
}

async fn read_response_line<S: AsyncRead + Unpin>(stream: &mut S) -> Result<String, VsockError> {
    let mut bytes = Vec::new();
    loop {
        let mut buf = [0u8; 1];
        let bytes_read = stream.read(&mut buf).await?;
        if bytes_read == 0 {
            return Err(VsockError::Protocol(
                "connection closed: empty response from guest".to_owned(),
            ));
        }
        if buf[0] == b'\n' {
            break;
        }
        bytes.push(buf[0]);
    }

    String::from_utf8(bytes)
        .map_err(|e| VsockError::Protocol(format!("response was not valid UTF-8: {e}")))
}

async fn negotiate_exec_stream<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    cmd: Vec<String>,
    env: Vec<String>,
    workdir: String,
    tty: bool,
    timeout: Duration,
) -> Result<S, VsockError> {
    let params = ExecParams {
        cmd,
        env,
        workdir,
        tty,
    };
    let params_value = serde_json::to_value(&params)
        .map_err(|e| VsockError::Protocol(format!("failed to serialize exec params: {e}")))?;
    let request = JsonRpcRequest::new("exec_stream", Some(params_value));
    let json = request
        .to_json()
        .map_err(|e| VsockError::Protocol(format!("failed to serialize request: {e}")))?;

    match tokio::time::timeout(timeout, async {
        stream.write_all(json.as_bytes()).await?;
        stream.write_all(b"\n").await?;
        stream.flush().await?;

        let line = read_response_line(&mut stream).await?;
        let resp = parse_response(line.trim())
            .map_err(|e| VsockError::Protocol(format!("failed to parse response: {e}")))?;
        let _ = rpc_response_result(resp)?;
        Ok(stream)
    })
    .await
    {
        Ok(result) => result,
        Err(_elapsed) => Err(VsockError::Timeout {
            operation: "exec_stream".to_owned(),
            duration: timeout,
        }),
    }
}

#[cfg(test)]
#[path = "client_test.rs"]
mod tests;
