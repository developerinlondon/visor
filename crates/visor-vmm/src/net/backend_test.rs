use std::net::Ipv4Addr;

use super::*;

// ── Mock implementation for trait testing ─────────────────────────────

/// Mock network interface that just stores the name.
struct MockInterface {
    name: String,
}

impl NetworkInterface for MockInterface {
    fn name(&self) -> &str {
        &self.name
    }
}

/// Mock NAT handle tracking rule count.
struct MockNatHandle {
    rule_count: usize,
}

impl NatHandle for MockNatHandle {
    fn rule_count(&self) -> usize {
        self.rule_count
    }

    fn teardown(&mut self) -> Result<(), NetError> {
        self.rule_count = 0;
        Ok(())
    }
}

/// Mock port-forward handle.
struct MockPortForwardHandle {
    mapping_count: usize,
}

impl PortForwardHandle for MockPortForwardHandle {
    fn mapping_count(&self) -> usize {
        self.mapping_count
    }

    fn teardown(&mut self) -> Result<(), NetError> {
        self.mapping_count = 0;
        Ok(())
    }
}

/// Mock backend that creates in-memory handles without system calls.
struct MockBackend;

impl NetworkBackend for MockBackend {
    type Interface = MockInterface;
    type Nat = MockNatHandle;
    type PortForward = MockPortForwardHandle;

    fn create_interface(&self, config: &InterfaceConfig) -> Result<Self::Interface, NetError> {
        config.validate()?;
        Ok(MockInterface {
            name: config.name().to_owned(),
        })
    }

    fn setup_nat(&self, config: &NatConfig) -> Result<Self::Nat, NetError> {
        // Pretend 3 rules per NAT setup (matches real iptables rule count)
        let _ = config.interface();
        Ok(MockNatHandle { rule_count: 3 })
    }

    fn setup_port_forward(&self, mappings: &[PortMapping]) -> Result<Self::PortForward, NetError> {
        Ok(MockPortForwardHandle {
            mapping_count: mappings.len(),
        })
    }
}

// ── InterfaceConfig tests ─────────────────────────────────────────────

#[test]
fn interface_config_has_correct_defaults() {
    let config = InterfaceConfig::new("visor0");
    assert_eq!(config.name(), "visor0");
    assert_eq!(config.ip(), Ipv4Addr::new(172, 20, 0, 1));
    assert_eq!(config.netmask(), Ipv4Addr::new(255, 255, 255, 0));
}

#[test]
fn interface_config_with_custom_ip() {
    let config = InterfaceConfig::new("test0")
        .with_ip(Ipv4Addr::new(10, 0, 0, 1))
        .with_netmask(Ipv4Addr::new(255, 255, 0, 0));
    assert_eq!(config.name(), "test0");
    assert_eq!(config.ip(), Ipv4Addr::new(10, 0, 0, 1));
    assert_eq!(config.netmask(), Ipv4Addr::new(255, 255, 0, 0));
}

#[test]
fn interface_config_name_too_long_is_detected() {
    let long_name = "a".repeat(20);
    let config = InterfaceConfig::new(&long_name);
    let result = config.validate();
    assert!(
        result.is_err(),
        "interface name > 15 chars should fail validation"
    );
}

#[test]
fn interface_config_empty_name_is_detected() {
    let config = InterfaceConfig::new("");
    let result = config.validate();
    assert!(
        result.is_err(),
        "empty interface name should fail validation"
    );
}

#[test]
fn interface_config_valid_name_passes() {
    let config = InterfaceConfig::new("visor0");
    assert!(config.validate().is_ok());
}

#[test]
fn interface_config_display() {
    let config = InterfaceConfig::new("visor0");
    let display = format!("{config}");
    assert!(display.contains("visor0"), "display should contain name");
    assert!(display.contains("172.20.0.1"), "display should contain IP");
}

// ── NatConfig tests ───────────────────────────────────────────────────

#[test]
fn nat_config_has_correct_defaults() {
    let config = NatConfig::new("visor0", "172.20.0.0/24");
    assert_eq!(config.interface(), "visor0");
    assert_eq!(config.subnet(), "172.20.0.0/24");
    assert_eq!(config.outbound_interface(), "eth0");
}

#[test]
fn nat_config_with_custom_outbound() {
    let config = NatConfig::new("visor0", "172.20.0.0/24").with_outbound_interface("ens3");
    assert_eq!(config.outbound_interface(), "ens3");
}

// ── PortMapping tests ─────────────────────────────────────────────────

#[test]
fn port_mapping_from_spec_valid() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap();
    assert_eq!(mapping.host_port(), 8080);
    assert_eq!(mapping.guest_port(), 80);
    assert_eq!(mapping.guest_ip(), Ipv4Addr::new(172, 20, 0, 2));
    assert_eq!(mapping.protocol(), "tcp");
}

#[test]
fn port_mapping_from_spec_with_protocol() {
    let mapping = PortMapping::from_spec("53:53/udp", Ipv4Addr::new(172, 20, 0, 2)).unwrap();
    assert_eq!(mapping.host_port(), 53);
    assert_eq!(mapping.guest_port(), 53);
    assert_eq!(mapping.protocol(), "udp");
}

#[test]
fn port_mapping_with_host_ip_scopes_destination_ip() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2))
        .unwrap()
        .with_host_ip(Ipv4Addr::new(172, 20, 0, 1));

    assert_eq!(mapping.host_ip(), Some(Ipv4Addr::new(172, 20, 0, 1)));
}

#[test]
fn port_mapping_from_spec_invalid_format() {
    let result = PortMapping::from_spec("invalid", Ipv4Addr::new(172, 20, 0, 2));
    assert!(result.is_err(), "invalid format should fail");
}

#[test]
fn port_mapping_from_spec_invalid_port() {
    let result = PortMapping::from_spec("99999:80", Ipv4Addr::new(172, 20, 0, 2));
    assert!(result.is_err(), "port > 65535 should fail");
}

#[test]
fn port_mapping_from_spec_zero_port() {
    let result = PortMapping::from_spec("0:80", Ipv4Addr::new(172, 20, 0, 2));
    assert!(result.is_err(), "port 0 should fail");
}

#[test]
fn port_mapping_display() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap();
    let display = format!("{mapping}");
    assert!(display.contains("8080"), "should contain host port");
    assert!(display.contains("80"), "should contain guest port");
    assert!(display.contains("172.20.0.2"), "should contain guest IP");
}

// ── NetworkBackend trait via mock ─────────────────────────────────────

#[test]
fn mock_backend_creates_interface() {
    let backend = MockBackend;
    let config = InterfaceConfig::new("mock0");
    let iface = backend.create_interface(&config).unwrap();
    assert_eq!(iface.name(), "mock0");
}

#[test]
fn mock_backend_rejects_invalid_interface() {
    let backend = MockBackend;
    let config = InterfaceConfig::new(&"x".repeat(20));
    let result = backend.create_interface(&config);
    assert!(result.is_err());
}

#[test]
fn mock_backend_sets_up_nat() {
    let backend = MockBackend;
    let config = NatConfig::new("mock0", "10.0.0.0/24");
    let nat = backend.setup_nat(&config).unwrap();
    assert_eq!(nat.rule_count(), 3);
}

#[test]
fn mock_backend_nat_teardown() {
    let backend = MockBackend;
    let config = NatConfig::new("mock0", "10.0.0.0/24");
    let mut nat = backend.setup_nat(&config).unwrap();
    nat.teardown().unwrap();
    assert_eq!(nat.rule_count(), 0);
}

#[test]
fn mock_backend_sets_up_port_forward() {
    let backend = MockBackend;
    let mappings = vec![
        PortMapping::from_spec("8080:80", Ipv4Addr::new(10, 0, 0, 2)).unwrap(),
        PortMapping::from_spec("443:443", Ipv4Addr::new(10, 0, 0, 2)).unwrap(),
    ];
    let pf = backend.setup_port_forward(&mappings).unwrap();
    assert_eq!(pf.mapping_count(), 2);
}

#[test]
fn mock_backend_port_forward_teardown() {
    let backend = MockBackend;
    let mappings = vec![PortMapping::from_spec("8080:80", Ipv4Addr::new(10, 0, 0, 2)).unwrap()];
    let mut pf = backend.setup_port_forward(&mappings).unwrap();
    pf.teardown().unwrap();
    assert_eq!(pf.mapping_count(), 0);
}

// ── NetError tests ────────────────────────────────────────────────────

#[test]
fn net_error_unsupported_display() {
    let err = NetError::Unsupported;
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("unsupported") || msg.to_lowercase().contains("not supported"),
        "Unsupported error should mention 'unsupported': {msg}"
    );
}

#[test]
fn net_error_invalid_config_display() {
    let err = NetError::InvalidConfig("bad name".to_owned());
    let msg = format!("{err}");
    assert!(msg.contains("bad name"), "should contain the detail: {msg}");
}
