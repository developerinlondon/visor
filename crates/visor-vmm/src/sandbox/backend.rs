//! Sandbox backend trait and portable error types.
//!
//! Defines the [`SandboxBackend`] trait for platform-agnostic process-level
//! security enforcement and the [`SandboxError`] error type.

// ── Errors ───────────────────────────────────────────────────────────

/// Errors from sandbox operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SandboxError {
    /// Filter compilation to BPF bytecode failed.
    #[error("sandbox filter compilation failed: {0}")]
    Compile(String),

    /// Installing the sandbox filter failed.
    #[error("sandbox filter installation failed: {0}")]
    Install(String),

    /// Sandboxing is not supported on this platform.
    #[error("sandboxing not supported on this platform")]
    Unsupported,
}

// ── Trait ─────────────────────────────────────────────────────────────

/// Abstraction over platform-specific process-level security enforcement.
///
/// Implementations apply OS-specific sandboxing mechanisms:
///
/// - **Linux**: seccomp BPF syscall filtering
/// - **macOS**: App Sandbox (stub, not yet implemented)
/// - **Windows**: Job Objects (stub, not yet implemented)
///
/// Once applied, sandboxing is typically **irreversible** — the restrictions
/// persist for the lifetime of the process.
pub trait SandboxBackend: Send + Sync {
    /// Apply the sandbox restrictions to the current process.
    ///
    /// After this call, the process is restricted according to the backend's
    /// configured policy. This operation is typically **irreversible**.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError`] if the sandbox cannot be applied.
    fn apply(&self) -> Result<(), SandboxError>;
}

#[cfg(test)]
#[path = "backend_test.rs"]
mod tests;
