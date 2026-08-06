//! Windows network backend stub.
//!
//! This is a placeholder implementation. All methods return
//! [`NetError::Unsupported`] until Windows networking support is implemented.

use super::backend::{
    InterfaceConfig, NatConfig, NatHandle, NetError, NetworkBackend, NetworkInterface,
    PortForwardHandle, PortMapping,
};

/// Windows network backend (stub).
pub struct WindowsNetworkBackend;

/// Windows network interface (stub).
pub struct WindowsNetworkInterface;

/// Windows NAT handle (stub).
pub struct WindowsNatHandle;

/// Windows port-forward handle (stub).
pub struct WindowsPortForwardHandle;

impl NetworkBackend for WindowsNetworkBackend {
    type Interface = WindowsNetworkInterface;
    type Nat = WindowsNatHandle;
    type PortForward = WindowsPortForwardHandle;

    fn create_interface(&self, _config: &InterfaceConfig) -> Result<Self::Interface, NetError> {
        Err(NetError::Unsupported)
    }

    fn setup_nat(&self, _config: &NatConfig) -> Result<Self::Nat, NetError> {
        Err(NetError::Unsupported)
    }

    fn setup_port_forward(&self, _mappings: &[PortMapping]) -> Result<Self::PortForward, NetError> {
        Err(NetError::Unsupported)
    }
}

impl NetworkInterface for WindowsNetworkInterface {
    fn name(&self) -> &str {
        ""
    }
}

impl NatHandle for WindowsNatHandle {
    fn rule_count(&self) -> usize {
        0
    }

    fn teardown(&mut self) -> Result<(), NetError> {
        Err(NetError::Unsupported)
    }
}

impl PortForwardHandle for WindowsPortForwardHandle {
    fn mapping_count(&self) -> usize {
        0
    }

    fn teardown(&mut self) -> Result<(), NetError> {
        Err(NetError::Unsupported)
    }
}

#[cfg(test)]
#[path = "windows_test.rs"]
mod tests;
