//! Network backend abstraction.
//!
//! Defines the [`NetworkBackend`] trait and portable configuration types.
//! Platform-specific implementations are selected at compile time via
//! [`PlatformNetworkBackend`].
//!
//! Consumers should use [`PlatformNetworkBackend`] to obtain the correct
//! backend type for the current platform without any `cfg` gates.

pub mod backend;
pub mod shared_ring;

#[cfg(target_os = "linux")]
pub mod linux;

#[cfg(target_os = "macos")]
pub mod macos;

#[cfg(target_os = "windows")]
pub mod windows;

pub use backend::{
    InterfaceConfig, NatConfig, NatHandle, NetError, NetworkBackend, NetworkInterface,
    PortForwardHandle, PortMapping,
};

#[cfg(target_os = "linux")]
pub use linux::{LinuxNetworkBackend, cleanup_visor_iptables_rules};

#[cfg(target_os = "macos")]
pub use macos::MacosNetworkBackend;

/// Platform-appropriate [`NetworkBackend`] implementation.
///
/// Resolves to the correct backend for the current OS at compile time:
/// - Linux: [`LinuxNetworkBackend`] (TAP devices, iptables NAT/port-forward)
/// - macOS: [`MacosNetworkBackend`] (vmnet.framework, pfctl port-forward)
#[cfg(target_os = "linux")]
pub type PlatformNetworkBackend = LinuxNetworkBackend;

/// Platform-appropriate [`NetworkBackend`] implementation.
///
/// Resolves to the correct backend for the current OS at compile time:
/// - Linux: [`LinuxNetworkBackend`] (TAP devices, iptables NAT/port-forward)
/// - macOS: [`MacosNetworkBackend`] (vmnet.framework, pfctl port-forward)
#[cfg(target_os = "macos")]
pub type PlatformNetworkBackend = MacosNetworkBackend;
