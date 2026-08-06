//! JSON-RPC 2.0 protocol types for host→guest vsock communication.
//!
//! These types mirror the server-side definitions in `visor-init`'s agent module,
//! but are oriented for the **client** (host) side: building requests and parsing
//! responses.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

// ── Standard JSON-RPC 2.0 error codes ──────────────────────────────────────

/// Parse error: invalid JSON was received.
pub const PARSE_ERROR: i32 = -32700;
/// Invalid request: the JSON sent is not a valid Request object.
pub const INVALID_REQUEST: i32 = -32600;
/// Method not found: the method does not exist or is not available.
pub const METHOD_NOT_FOUND: i32 = -32601;
/// Invalid params: invalid method parameter(s).
pub const INVALID_PARAMS: i32 = -32602;
/// Internal error: internal JSON-RPC error.
pub const INTERNAL_ERROR: i32 = -32603;

/// Monotonically increasing request ID counter.
///
/// Each [`JsonRpcRequest::new`] call gets a unique ID, making it safe to
/// correlate responses even if multiple requests are in flight.
pub static REQUEST_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

// ── JSON-RPC 2.0 message types ─────────────────────────────────────────────

/// A JSON-RPC 2.0 request object (client-side builder).
///
/// See <https://www.jsonrpc.org/specification#request_object>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct JsonRpcRequest {
    /// Protocol version — always `"2.0"`.
    pub jsonrpc: String,
    /// Method name to invoke.
    pub method: String,
    /// Structured parameter values for the method (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
    /// Request identifier — echoed back in the response.
    pub id: serde_json::Value,
}

/// A JSON-RPC 2.0 response object.
///
/// See <https://www.jsonrpc.org/specification#response_object>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct JsonRpcResponse {
    /// Protocol version — always `"2.0"`.
    pub jsonrpc: String,
    /// Result value on success (mutually exclusive with `error`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    /// Error object on failure (mutually exclusive with `result`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    /// Request identifier — matches the request this responds to.
    pub id: serde_json::Value,
}

/// A JSON-RPC 2.0 error object.
///
/// See <https://www.jsonrpc.org/specification#error_object>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct JsonRpcError {
    /// Numeric error code.
    pub code: i32,
    /// Short human-readable description.
    pub message: String,
    /// Additional error data (optional).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

// ── Method parameter types ──────────────────────────────────────────────────

/// Parameters for the `exec` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ExecParams {
    /// Command and arguments (e.g., `["ls", "-la"]`).
    pub cmd: Vec<String>,
    /// Environment variables as `KEY=VALUE` pairs.
    pub env: Vec<String>,
    /// Working directory for the command.
    pub workdir: String,
    /// Whether the command should run with terminal semantics.
    #[serde(default)]
    pub tty: bool,
}

/// Parameters for the `kill` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct KillParams {
    /// Signal number to send (e.g., 9 for `SIGKILL`).
    pub signal: i32,
}

/// Result of an `exec` method call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ExecResult {
    /// Process exit code.
    pub exit_code: i32,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
}

/// Parameters for the `overlay_init` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct OverlayInitParams {
    /// Path to the lower directory (base rootfs).
    /// Defaults to `"/"` if not provided.
    pub lower_dir: Option<String>,
}

/// Result of `snapshot_layer` --- returns the layer data.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct SnapshotLayerResult {
    /// Base64-encoded tar.gz of the overlay upper directory.
    pub data: String,
    /// SHA-256 digest of the compressed tar.gz (`"sha256:..."`).
    pub compressed_digest: String,
    /// SHA-256 digest of the uncompressed tar (`"sha256:..."`).
    pub uncompressed_digest: String,
    /// Size of the compressed tar.gz in bytes.
    pub compressed_size: u64,
}

/// Parameters for the `copy_files` method.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CopyFilesParams {
    /// Base64-encoded tar.gz archive containing files to copy.
    pub data: String,
    /// Destination directory inside the guest.
    pub dest: String,
}

/// Result of a `copy_files` method call.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CopyFilesResult {
    /// Number of files written.
    pub files_written: u64,
}

// ── Request builder ─────────────────────────────────────────────────────────

impl JsonRpcRequest {
    /// Build a new JSON-RPC 2.0 request with an auto-incrementing ID.
    #[must_use]
    pub fn new(method: &str, params: Option<serde_json::Value>) -> Self {
        let id = REQUEST_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        Self {
            jsonrpc: "2.0".to_owned(),
            method: method.to_owned(),
            params,
            id: serde_json::json!(id),
        }
    }

    /// Serialize this request to a compact JSON string (no internal newlines).
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string(self)
    }
}

// ── Response parser ─────────────────────────────────────────────────────────

/// Parse a JSON string into a [`JsonRpcResponse`], validating the protocol version.
///
/// # Errors
///
/// Returns an error if the JSON is malformed or the `jsonrpc` field is not `"2.0"`.
pub fn parse_response(json: &str) -> anyhow::Result<JsonRpcResponse> {
    let response: JsonRpcResponse =
        serde_json::from_str(json).context("failed to parse JSON-RPC response")?;

    if response.jsonrpc != "2.0" {
        anyhow::bail!(
            "invalid JSON-RPC version: expected \"2.0\", got {:?}",
            response.jsonrpc
        );
    }

    Ok(response)
}

use anyhow::Context;

#[cfg(test)]
#[path = "protocol_test.rs"]
mod tests;
