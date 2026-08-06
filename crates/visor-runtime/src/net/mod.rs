//! Virtual networking stack: IP allocation, switching, DNS, and network backend.
//!
//! This module provides the portable networking infrastructure for visor networks:
//!
//! - [`ip_alloc`] — Thread-safe IPv4 subnet allocator (allocate/release IPs from a /24 subnet)
//! - [`switch`] — Virtual switch managing network membership and MAC-based forwarding tables
//! - [`dns`] — Embedded DNS resolver using hickory-server (VM name resolution + upstream forwarding)
//!
//!
//! Platform-specific networking (TAP devices, NAT, port forwarding) lives in
//! [`visor_vmm::net`] and is accessed via the [`NetworkBackend`] trait.
//!
//! # Default Network
//!
//! The default network is `visor0` with subnet `172.20.0.0/24` and gateway `172.20.0.1`.

pub mod dns;
pub mod ip_alloc;
pub mod server;
pub mod switch;

use std::net::Ipv4Addr;

// Re-export the network backend trait and types from visor-vmm for convenience.
pub use visor_vmm::net::{
    InterfaceConfig, NatConfig, NatHandle, NetError, NetworkBackend, NetworkInterface,
    PortForwardHandle, PortMapping,
};

pub use visor_vmm::net::PlatformNetworkBackend;

/// Default network name.
pub const DEFAULT_NETWORK_NAME: &str = "visor0";

/// Default subnet base address.
pub const DEFAULT_SUBNET_BASE: Ipv4Addr = Ipv4Addr::new(172, 20, 0, 0);

/// Default subnet prefix length.
pub const DEFAULT_SUBNET_PREFIX: u8 = 24;

/// Default gateway address.
pub const DEFAULT_GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 20, 0, 1);
