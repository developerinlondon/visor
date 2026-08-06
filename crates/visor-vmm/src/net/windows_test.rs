use super::*;
use crate::net::backend::{InterfaceConfig, NatConfig, NetError, NetworkBackend};

#[test]
fn windows_backend_create_interface_returns_unsupported() {
    let backend = WindowsNetworkBackend;
    let config = InterfaceConfig::new("test0");
    let result = backend.create_interface(&config);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, NetError::Unsupported),
        "expected Unsupported, got: {err:?}"
    );
}

#[test]
fn windows_backend_setup_nat_returns_unsupported() {
    let backend = WindowsNetworkBackend;
    let config = NatConfig::new("test0", "10.0.0.0/24");
    let result = backend.setup_nat(&config);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, NetError::Unsupported),
        "expected Unsupported, got: {err:?}"
    );
}

#[test]
fn windows_backend_setup_port_forward_returns_unsupported() {
    let backend = WindowsNetworkBackend;
    let result = backend.setup_port_forward(&[]);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, NetError::Unsupported),
        "expected Unsupported, got: {err:?}"
    );
}
