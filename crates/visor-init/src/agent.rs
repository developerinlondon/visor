//! JSON-RPC 2.0 agent protocol for host↔guest vsock communication.
//!
//! Defines the message types, parsing, and dispatch table for the vsock agent.
//! This module handles **protocol only** — no actual vsock I/O or command execution.

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::config::RunConfig;

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

// ── JSON-RPC 2.0 message types ─────────────────────────────────────────────

/// A JSON-RPC 2.0 request object.
///
/// See <https://www.jsonrpc.org/specification#request_object>.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct JsonRpcRequest {
    /// Protocol version — must be exactly `"2.0"`.
    pub jsonrpc: String,
    /// Method name to invoke.
    pub method: String,
    /// Structured parameter values for the method (optional).
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

// ── Agent method types ──────────────────────────────────────────────────────

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
    /// Signal number to send (e.g., 9 for SIGKILL).
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
/// Parsed agent method with its parameters.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum AgentMethod {
    /// Health check — returns `"pong"`.
    Ping,
    /// Execute a command in the guest.
    Exec(ExecParams),
    /// Execute a command in the guest over a raw streaming transport.
    ExecStream(ExecParams),
    /// Send a signal to the running process.
    Kill(KillParams),
    /// Return the current [`RunConfig`].
    GetConfig,
    /// Gracefully shut down the guest.
    Shutdown,
    /// Set up overlayfs for build layer snapshotting.
    OverlayInit(OverlayInitParams),
    /// Snapshot the overlay upper directory as a tar.gz layer.
    SnapshotLayer,
    /// Flatten the overlay upper into lower, reset for next instruction.
    FlattenOverlay,
    /// Copy files (as a tar.gz archive) into the guest filesystem.
    CopyFiles(CopyFilesParams),
}

// ── Parsing ─────────────────────────────────────────────────────────────────

/// Parse a JSON string into a [`JsonRpcRequest`], validating the protocol version.
///
/// # Errors
///
/// Returns an error if the JSON is malformed or the `jsonrpc` field is not `"2.0"`.
#[must_use = "parsed request should be dispatched"]
pub fn parse_request(json: &str) -> anyhow::Result<JsonRpcRequest> {
    let request: JsonRpcRequest =
        serde_json::from_str(json).context("failed to parse JSON-RPC request")?;

    if request.jsonrpc != "2.0" {
        anyhow::bail!(
            "invalid JSON-RPC version: expected \"2.0\", got {:?}",
            request.jsonrpc
        );
    }

    Ok(request)
}

// ── Dispatch ────────────────────────────────────────────────────────────────

/// Dispatch a method name and optional parameters to an [`AgentMethod`].
///
/// # Errors
///
/// Returns an error if the method is unknown or parameters are invalid.
pub fn dispatch_method(
    method: &str,
    params: Option<&serde_json::Value>,
) -> anyhow::Result<AgentMethod> {
    match method {
        "ping" => Ok(AgentMethod::Ping),
        "exec" => {
            let params_value = params.context("exec method requires params")?;
            let exec_params: ExecParams =
                serde_json::from_value(params_value.clone()).context("invalid exec params")?;
            if exec_params.cmd.is_empty() {
                anyhow::bail!("exec params: cmd must not be empty");
            }
            Ok(AgentMethod::Exec(exec_params))
        }
        "exec_stream" => {
            let params_value = params.context("exec_stream method requires params")?;
            let exec_params: ExecParams = serde_json::from_value(params_value.clone())
                .context("invalid exec_stream params")?;
            if exec_params.cmd.is_empty() {
                anyhow::bail!("exec_stream params: cmd must not be empty");
            }
            Ok(AgentMethod::ExecStream(exec_params))
        }
        "kill" => {
            let params_value = params.context("kill method requires params")?;
            let kill_params: KillParams =
                serde_json::from_value(params_value.clone()).context("invalid kill params")?;
            Ok(AgentMethod::Kill(kill_params))
        }
        "get_config" => Ok(AgentMethod::GetConfig),
        "shutdown" => Ok(AgentMethod::Shutdown),
        "overlay_init" => {
            let overlay_params: OverlayInitParams = params
                .map(|v| serde_json::from_value(v.clone()))
                .transpose()
                .context("invalid overlay_init params")?
                .unwrap_or(OverlayInitParams { lower_dir: None });
            Ok(AgentMethod::OverlayInit(overlay_params))
        }
        "snapshot_layer" => Ok(AgentMethod::SnapshotLayer),
        "flatten_overlay" => Ok(AgentMethod::FlattenOverlay),
        "copy_files" => {
            let params_value = params.context("copy_files method requires params")?;
            let copy_params: CopyFilesParams = serde_json::from_value(params_value.clone())
                .context("invalid copy_files params")?;
            Ok(AgentMethod::CopyFiles(copy_params))
        }
        unknown => anyhow::bail!("unknown method: {unknown}"),
    }
}

// ── Response builders ───────────────────────────────────────────────────────

impl JsonRpcResponse {
    /// Build a success response with the given result value.
    #[must_use]
    pub fn success(id: serde_json::Value, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            result: Some(result),
            error: None,
            id,
        }
    }

    /// Build an error response with the given code and message.
    #[must_use]
    pub fn error(id: serde_json::Value, code: i32, message: impl Into<String>) -> Self {
        Self {
            jsonrpc: "2.0".to_owned(),
            result: None,
            error: Some(JsonRpcError {
                code,
                message: message.into(),
                data: None,
            }),
            id,
        }
    }

    /// Serialize this response to a JSON string.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails.
    pub fn to_json(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).context("failed to serialize JSON-RPC response")
    }
}

/// Build a success response for a `ping` request.
#[must_use]
pub fn ping_response(id: serde_json::Value) -> JsonRpcResponse {
    JsonRpcResponse::success(id, serde_json::Value::String("pong".to_owned()))
}

/// Build a success response for an `exec` request.
///
/// # Errors
///
/// Returns an error if the result cannot be serialized.
pub fn exec_response(
    id: serde_json::Value,
    result: &ExecResult,
) -> anyhow::Result<JsonRpcResponse> {
    let value = serde_json::to_value(result).context("failed to serialize exec result")?;
    Ok(JsonRpcResponse::success(id, value))
}

/// Build a success response for a `kill` request.
#[must_use]
pub fn kill_response(id: serde_json::Value) -> JsonRpcResponse {
    JsonRpcResponse::success(id, serde_json::Value::String("ok".to_owned()))
}

/// Build a success response for a `get_config` request.
///
/// # Errors
///
/// Returns an error if the config cannot be serialized.
pub fn get_config_response(
    id: serde_json::Value,
    config: &RunConfig,
) -> anyhow::Result<JsonRpcResponse> {
    let value = serde_json::to_value(config).context("failed to serialize RunConfig")?;
    Ok(JsonRpcResponse::success(id, value))
}

/// Build a success response for a `shutdown` request.
#[must_use]
pub fn shutdown_response(id: serde_json::Value) -> JsonRpcResponse {
    JsonRpcResponse::success(id, serde_json::Value::String("ok".to_owned()))
}

/// Build a success response for an `overlay_init` request.
#[must_use]
pub fn overlay_init_response(id: serde_json::Value) -> JsonRpcResponse {
    JsonRpcResponse::success(id, serde_json::Value::String("ok".to_owned()))
}

/// Build a success response for a `snapshot_layer` request.
///
/// # Errors
///
/// Returns an error if the result cannot be serialized.
pub fn snapshot_layer_response(
    id: serde_json::Value,
    result: &SnapshotLayerResult,
) -> anyhow::Result<JsonRpcResponse> {
    let value =
        serde_json::to_value(result).context("failed to serialize snapshot layer result")?;
    Ok(JsonRpcResponse::success(id, value))
}

/// Build a success response for a `flatten_overlay` request.
#[must_use]
pub fn flatten_overlay_response(id: serde_json::Value) -> JsonRpcResponse {
    JsonRpcResponse::success(id, serde_json::Value::String("ok".to_owned()))
}

/// Build a success response for a `copy_files` request.
///
/// # Errors
///
/// Returns an error if the result cannot be serialized.
pub fn copy_files_response(
    id: serde_json::Value,
    result: &CopyFilesResult,
) -> anyhow::Result<JsonRpcResponse> {
    let value = serde_json::to_value(result).context("failed to serialize copy_files result")?;
    Ok(JsonRpcResponse::success(id, value))
}

#[cfg(test)]
#[path = "agent_test.rs"]
mod tests;
