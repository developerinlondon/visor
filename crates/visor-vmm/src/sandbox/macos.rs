//! macOS sandbox backend using the [`nono`] crate (Seatbelt).
//!
//! Wraps [`nono::Sandbox`] to provide kernel-enforced process sandboxing
//! on macOS via Apple's Seatbelt mechanism. Once applied, restrictions
//! are **irreversible** — they persist for the lifetime of the process
//! and are inherited by child processes.

use nono::{AccessMode, CapabilitySet, Sandbox};

use super::backend::{SandboxBackend, SandboxError};

/// macOS sandbox backend backed by [`nono`] (Seatbelt).
///
/// Constructs a [`CapabilitySet`] granting minimal filesystem access
/// required by the VMM process, then applies it via `sandbox_init`.
pub struct MacosSandbox {
    capabilities: CapabilitySet,
}

impl MacosSandbox {
    /// Creates a new sandbox with default VMM capabilities.
    ///
    /// The default policy grants:
    /// - Read access to system libraries and frameworks
    /// - Read-write access to `/tmp` and `/var/tmp`
    /// - Network access (required for vmnet)
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Compile`] if any capability path is invalid.
    pub fn new() -> Result<Self, SandboxError> {
        let caps = default_vmm_capabilities().map_err(|e| SandboxError::Compile(e.to_string()))?;
        Ok(Self { capabilities: caps })
    }

    /// Creates a sandbox with a custom capability set.
    #[must_use]
    pub fn with_capabilities(capabilities: CapabilitySet) -> Self {
        Self { capabilities }
    }
}

impl SandboxBackend for MacosSandbox {
    /// Applies the Seatbelt sandbox to the current process.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Install`] if the sandbox cannot be applied
    /// (e.g. sandbox profile compilation fails, or `sandbox_init` fails).
    fn apply(&self) -> Result<(), SandboxError> {
        Sandbox::apply(&self.capabilities).map_err(|e| SandboxError::Install(e.to_string()))
    }
}

/// Builds the default capability set for the VMM process.
fn default_vmm_capabilities() -> Result<CapabilitySet, nono::NonoError> {
    // Network is allowed by default in nono — no explicit call needed.
    CapabilitySet::new()
        // System libraries and frameworks (read-only).
        .allow_path("/usr/lib", AccessMode::Read)?
        .allow_path("/usr/share", AccessMode::Read)?
        .allow_path("/System/Library", AccessMode::Read)?
        .allow_path("/Library/Frameworks", AccessMode::Read)?
        // Temporary directories (read-write for VM scratch files).
        .allow_path("/tmp", AccessMode::ReadWrite)?
        .allow_path("/var/tmp", AccessMode::ReadWrite)
}

#[cfg(test)]
#[path = "macos_test.rs"]
mod tests;
