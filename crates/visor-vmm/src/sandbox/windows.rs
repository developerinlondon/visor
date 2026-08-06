//! Windows sandbox backend stub.
//!
//! This is a placeholder implementation. All methods return
//! [`SandboxError::Unsupported`] until Windows Job Objects support is implemented.

use super::backend::{SandboxBackend, SandboxError};

/// Windows sandbox backend (stub).
pub struct WindowsSandbox;

impl SandboxBackend for WindowsSandbox {
    fn apply(&self) -> Result<(), SandboxError> {
        Err(SandboxError::Unsupported)
    }
}

#[cfg(test)]
#[path = "windows_test.rs"]
mod tests;
