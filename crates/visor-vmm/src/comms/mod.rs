//! Communication backend abstraction.
//!
//! Defines the [`CommsBackend`] trait and portable types for establishing
//! async communication channels to guest VMs. Platform-specific
//! implementations are selected at compile time via [`PlatformCommsBackend`].
//!
//! Consumers should use [`create_comms_backend`] to obtain the correct
//! backend for the current platform without any `cfg` gates.

pub mod backend;
pub mod muxer;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

pub use backend::{AsyncStream, CommsBackend, CommsError, DEFAULT_CONNECT_TIMEOUT};

#[cfg(target_os = "linux")]
pub use linux::LinuxCommsBackend;

#[cfg(target_os = "macos")]
pub use macos::MacosCommsBackend;

/// Platform-appropriate [`CommsBackend`] implementation.
///
/// Resolves to the correct backend for the current OS at compile time:
/// - Linux: [`LinuxCommsBackend`] (Unix domain sockets via the vsock muxer)
/// - macOS: [`MacosCommsBackend`] (Unix domain sockets via the vsock muxer)
#[cfg(target_os = "linux")]
pub type PlatformCommsBackend = LinuxCommsBackend;

/// Platform-appropriate [`CommsBackend`] implementation.
///
/// Resolves to the correct backend for the current OS at compile time:
/// - Linux: [`LinuxCommsBackend`] (Unix domain sockets via the vsock muxer)
/// - macOS: [`MacosCommsBackend`] (Unix domain sockets via the vsock muxer)
#[cfg(target_os = "macos")]
pub type PlatformCommsBackend = MacosCommsBackend;

/// Creates the platform-appropriate comms backend.
///
/// Returns the correct [`CommsBackend`] implementation for the current OS.
/// Consumers call this once and pass the backend to connection-establishing
/// code — no `cfg` gates needed outside of `visor-vmm`.
#[must_use]
pub fn create_comms_backend() -> PlatformCommsBackend {
    PlatformCommsBackend::new()
}
