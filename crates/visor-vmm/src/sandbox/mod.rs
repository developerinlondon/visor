//! Sandbox backend abstraction.
//!
//! Defines the [`SandboxBackend`] trait for platform-agnostic process-level
//! security enforcement. Platform-specific implementations are selected at
//! compile time:
//!
//! - **Linux**: [`linux::LinuxSandbox`] — seccomp BPF syscall filtering
//! - **macOS**: stub returning [`SandboxError::Unsupported`]
//! - **Windows**: stub returning [`SandboxError::Unsupported`]

pub mod backend;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

pub use backend::{SandboxBackend, SandboxError};

#[cfg(target_os = "linux")]
pub use linux::LinuxSandbox;
