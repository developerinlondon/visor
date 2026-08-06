//! Network backend trait and portable configuration types.
//!
//! Defines the [`NetworkBackend`] trait and associated sub-traits for
//! platform-agnostic network operations. Configuration structs
//! ([`InterfaceConfig`], [`NatConfig`], [`PortMapping`]) are portable
//! and carry no platform-specific logic.

use std::fmt;
use std::net::Ipv4Addr;

/// Default gateway address used when no IP is specified.
const DEFAULT_GATEWAY: Ipv4Addr = Ipv4Addr::new(172, 20, 0, 1);

/// Maximum length of a network interface name (IFNAMSIZ - 1 for null terminator).
const MAX_IFACE_NAME_LEN: usize = 15;

// ── Errors ───────────────────────────────────────────────────────────

/// Errors from network backend operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NetError {
    /// A network interface operation failed.
    #[error("network interface operation failed: {0}")]
    Interface(String),

    /// A NAT operation failed.
    #[error("NAT operation failed: {0}")]
    Nat(String),

    /// A port-forwarding operation failed.
    #[error("port forwarding operation failed: {0}")]
    PortForward(String),

    /// The supplied configuration is invalid.
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// A shell command failed to execute.
    #[error("command execution failed: {0}")]
    Command(#[from] std::io::Error),

    /// Networking is not supported on this platform.
    #[error("networking not supported on this platform")]
    Unsupported,
}

// ── Configuration structs ────────────────────────────────────────────

/// Configuration for creating a network interface (TAP device).
///
/// Use [`InterfaceConfig::new`] to create a config with default settings,
/// then customize with builder methods.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct InterfaceConfig {
    name: String,
    ip: Ipv4Addr,
    netmask: Ipv4Addr,
    bridge_name: Option<String>,
}

impl InterfaceConfig {
    /// Create a new interface configuration with the given name.
    ///
    /// Defaults to IP 172.20.0.1 and netmask 255.255.255.0.
    #[must_use]
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_owned(),
            ip: DEFAULT_GATEWAY,
            netmask: Ipv4Addr::new(255, 255, 255, 0),
            bridge_name: None,
        }
    }

    /// Set the IP address for the interface.
    #[must_use]
    pub fn with_ip(mut self, ip: Ipv4Addr) -> Self {
        self.ip = ip;
        self
    }

    /// Set the netmask for the interface.
    #[must_use]
    pub fn with_netmask(mut self, netmask: Ipv4Addr) -> Self {
        self.netmask = netmask;
        self
    }

    /// Attach the TAP device to an existing or managed Linux bridge.
    #[must_use]
    pub fn with_bridge(mut self, bridge_name: &str) -> Self {
        self.bridge_name = Some(bridge_name.to_owned());
        self
    }

    /// Returns the interface name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the IP address.
    #[must_use]
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }

    /// Returns the netmask.
    #[must_use]
    pub fn netmask(&self) -> Ipv4Addr {
        self.netmask
    }

    /// Returns the optional bridge name the interface should join.
    #[must_use]
    pub fn bridge_name(&self) -> Option<&str> {
        self.bridge_name.as_deref()
    }

    /// Validate the configuration.
    ///
    /// # Errors
    ///
    /// Returns [`NetError::InvalidConfig`] if the interface name is empty
    /// or exceeds 15 characters.
    pub fn validate(&self) -> Result<(), NetError> {
        if self.name.is_empty() {
            return Err(NetError::InvalidConfig(
                "interface name cannot be empty".to_owned(),
            ));
        }
        if self.name.len() > MAX_IFACE_NAME_LEN {
            return Err(NetError::InvalidConfig(format!(
                "interface name '{}' exceeds maximum length of {MAX_IFACE_NAME_LEN} characters",
                self.name
            )));
        }
        Ok(())
    }
}

impl fmt::Display for InterfaceConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Interface({} ip={} mask={} bridge={})",
            self.name,
            self.ip,
            self.netmask,
            self.bridge_name.as_deref().unwrap_or("-")
        )
    }
}

/// Configuration for NAT rules.
///
/// Describes which subnet should be masqueraded for outbound traffic.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct NatConfig {
    /// TAP interface name (e.g., "visor0").
    interface: String,
    /// Subnet in CIDR notation (e.g., "172.20.0.0/24").
    subnet: String,
    /// Outbound interface for MASQUERADE (e.g., "eth0").
    outbound_interface: String,
}

impl NatConfig {
    /// Create a new NAT configuration.
    #[must_use]
    pub fn new(interface: &str, subnet: &str) -> Self {
        Self {
            interface: interface.to_owned(),
            subnet: subnet.to_owned(),
            outbound_interface: "eth0".to_owned(),
        }
    }

    /// Set the outbound network interface.
    #[must_use]
    pub fn with_outbound_interface(mut self, iface: &str) -> Self {
        iface.clone_into(&mut self.outbound_interface);
        self
    }

    /// Returns the TAP interface name.
    #[must_use]
    pub fn interface(&self) -> &str {
        &self.interface
    }

    /// Returns the subnet in CIDR notation.
    #[must_use]
    pub fn subnet(&self) -> &str {
        &self.subnet
    }

    /// Returns the outbound interface name.
    #[must_use]
    pub fn outbound_interface(&self) -> &str {
        &self.outbound_interface
    }
}

/// A host:port → guest:port forwarding mapping.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct PortMapping {
    /// Host port to listen on.
    host_port: u16,
    /// Optional host-side destination IP to match before forwarding.
    host_ip: Option<Ipv4Addr>,
    /// Guest port to forward to.
    guest_port: u16,
    /// Guest IP address.
    guest_ip: Ipv4Addr,
    /// Protocol (tcp or udp).
    protocol: String,
}

impl PortMapping {
    /// Parse a port mapping from a spec string like "8080:80" or "53:53/udp".
    ///
    /// # Errors
    ///
    /// Returns [`NetError::InvalidConfig`] if the spec is malformed or ports are invalid.
    pub fn from_spec(spec: &str, guest_ip: Ipv4Addr) -> Result<Self, NetError> {
        let (ports_part, protocol) = if let Some((p, proto)) = spec.rsplit_once('/') {
            (p, proto.to_owned())
        } else {
            (spec, "tcp".to_owned())
        };

        let (host_str, guest_str) = ports_part.split_once(':').ok_or_else(|| {
            NetError::InvalidConfig(format!(
                "invalid port mapping format: expected 'host:guest', got '{spec}'"
            ))
        })?;

        let host_port: u16 = host_str
            .parse()
            .map_err(|_| NetError::InvalidConfig(format!("invalid host port: '{host_str}'")))?;
        let guest_port: u16 = guest_str
            .parse()
            .map_err(|_| NetError::InvalidConfig(format!("invalid guest port: '{guest_str}'")))?;

        if host_port == 0 {
            return Err(NetError::InvalidConfig("host port must be > 0".to_owned()));
        }
        if guest_port == 0 {
            return Err(NetError::InvalidConfig("guest port must be > 0".to_owned()));
        }

        Ok(Self {
            host_port,
            host_ip: None,
            guest_port,
            guest_ip,
            protocol,
        })
    }

    /// Restrict this mapping to a specific host-side destination IP.
    #[must_use]
    pub fn with_host_ip(mut self, host_ip: Ipv4Addr) -> Self {
        self.host_ip = Some(host_ip);
        self
    }

    /// Returns the host port.
    #[must_use]
    pub fn host_port(&self) -> u16 {
        self.host_port
    }

    /// Returns the matched host-side destination IP, if one is configured.
    #[must_use]
    pub fn host_ip(&self) -> Option<Ipv4Addr> {
        self.host_ip
    }

    /// Returns the guest port.
    #[must_use]
    pub fn guest_port(&self) -> u16 {
        self.guest_port
    }

    /// Returns the guest IP address.
    #[must_use]
    pub fn guest_ip(&self) -> Ipv4Addr {
        self.guest_ip
    }

    /// Returns the protocol.
    #[must_use]
    pub fn protocol(&self) -> &str {
        &self.protocol
    }
}

impl fmt::Display for PortMapping {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host_ip = self
            .host_ip
            .map_or_else(|| "0.0.0.0".to_owned(), |ip| ip.to_string());
        write!(
            f,
            "{}:{}/{} → {}:{}",
            host_ip, self.host_port, self.protocol, self.guest_ip, self.guest_port
        )
    }
}

// ── Traits ───────────────────────────────────────────────────────────

/// Abstraction over platform-specific network operations.
///
/// Implementations create TAP interfaces, configure NAT, and set up
/// port forwarding using OS-specific mechanisms.
pub trait NetworkBackend: Sized + Send + Sync {
    /// Handle for a created network interface.
    type Interface: NetworkInterface;
    /// Handle for applied NAT rules.
    type Nat: NatHandle;
    /// Handle for applied port-forwarding rules.
    type PortForward: PortForwardHandle;

    /// Create and configure a network interface (e.g., TAP device).
    ///
    /// # Errors
    ///
    /// Returns [`NetError`] if the interface cannot be created or configured.
    fn create_interface(&self, config: &InterfaceConfig) -> Result<Self::Interface, NetError>;

    /// Apply NAT rules for the given configuration.
    ///
    /// # Errors
    ///
    /// Returns [`NetError`] if the NAT rules cannot be applied.
    fn setup_nat(&self, config: &NatConfig) -> Result<Self::Nat, NetError>;

    /// Apply port-forwarding rules for the given mappings.
    ///
    /// # Errors
    ///
    /// Returns [`NetError`] if the port-forward rules cannot be applied.
    fn setup_port_forward(&self, mappings: &[PortMapping]) -> Result<Self::PortForward, NetError>;
}

/// Handle to a created network interface.
///
/// The interface is automatically torn down when the handle is dropped.
pub trait NetworkInterface {
    /// Returns the interface name.
    fn name(&self) -> &str;
}

/// Handle to applied NAT rules.
///
/// Rules are automatically cleaned up when the handle is dropped.
pub trait NatHandle {
    /// Returns the number of currently applied rules.
    fn rule_count(&self) -> usize;

    /// Manually tear down all applied rules.
    ///
    /// # Errors
    ///
    /// Returns [`NetError`] if any rule cannot be removed.
    fn teardown(&mut self) -> Result<(), NetError>;
}

/// Handle to applied port-forwarding rules.
///
/// Rules are automatically cleaned up when the handle is dropped.
pub trait PortForwardHandle: Send + Sync {
    /// Returns the number of configured port mappings.
    fn mapping_count(&self) -> usize;

    /// Manually tear down all applied rules.
    ///
    /// # Errors
    ///
    /// Returns [`NetError`] if any rule cannot be removed.
    fn teardown(&mut self) -> Result<(), NetError>;
}

#[cfg(test)]
#[path = "backend_test.rs"]
mod tests;
