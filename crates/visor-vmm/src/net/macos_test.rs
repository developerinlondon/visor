use std::net::Ipv4Addr;

use super::*;
use crate::net::backend::{
    InterfaceConfig, NatConfig, NatHandle, NetError, NetworkBackend, PortForwardHandle, PortMapping,
};

// ── MacosNetworkBackend construction ─────────────────────────────────

#[test]
fn macos_backend_new() {
    let _backend = MacosNetworkBackend::new();
}

#[test]
fn macos_backend_default() {
    let _backend = MacosNetworkBackend::default();
}

// ── Interface creation ───────────────────────────────────────────────

#[test]
fn create_interface_rejects_invalid_config() {
    let backend = MacosNetworkBackend::new();
    let config = InterfaceConfig::new(&"x".repeat(20));
    let result = backend.create_interface(&config);
    assert!(result.is_err(), "should reject too-long interface name");
}

#[test]
fn create_interface_rejects_empty_name() {
    let backend = MacosNetworkBackend::new();
    let config = InterfaceConfig::new("");
    let result = backend.create_interface(&config);
    assert!(result.is_err(), "should reject empty interface name");
    let err = result.unwrap_err();
    assert!(
        matches!(err, NetError::InvalidConfig(_)),
        "expected InvalidConfig, got: {err:?}"
    );
}

#[test]
fn create_interface_requires_entitlement_or_root() {
    let backend = MacosNetworkBackend::new();
    let config = InterfaceConfig::new("visor0");
    let result = backend.create_interface(&config);

    // vmnet requires either com.apple.vm.networking entitlement or root.
    // In CI / unprivileged environments, this will fail — and that's fine.
    // If we're privileged, it should succeed and return a valid interface.
    match result {
        Ok(iface) => {
            assert_eq!(iface.name(), "visor0");
        }
        Err(e) => {
            let msg = format!("{e}");
            // Expected failures when running without entitlement/root.
            assert!(
                msg.contains("vmnet")
                    || msg.contains("permission")
                    || msg.contains("entitlement")
                    || msg.contains("Operation not permitted")
                    || msg.contains("failed"),
                "unexpected error: {msg}"
            );
        }
    }
}

// ── NAT setup (no-op for vmnet shared mode) ──────────────────────────

#[test]
fn setup_nat_returns_zero_rule_handle() {
    let backend = MacosNetworkBackend::new();
    let config = NatConfig::new("visor0", "172.20.0.0/24");
    let result = backend.setup_nat(&config);
    assert!(result.is_ok(), "NAT setup should always succeed (no-op)");
    let handle = result.unwrap();
    assert_eq!(handle.rule_count(), 0, "vmnet NAT has no explicit rules");
}

#[test]
fn nat_handle_teardown_is_noop() {
    let backend = MacosNetworkBackend::new();
    let config = NatConfig::new("visor0", "172.20.0.0/24");
    let mut handle = backend.setup_nat(&config).unwrap();
    assert!(
        handle.teardown().is_ok(),
        "NAT teardown should always succeed"
    );
}

// ── Port forward rule generation ─────────────────────────────────────

#[test]
fn pf_rules_generates_one_rule_per_mapping() {
    let mappings = vec![
        PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap(),
        PortMapping::from_spec("4443:443", Ipv4Addr::new(172, 20, 0, 2)).unwrap(),
    ];
    let rules = generate_pf_rules(&mappings);
    assert_eq!(rules.len(), 2, "should generate one rule per mapping");
}

#[test]
fn pf_rules_contain_rdr_pass() {
    let mappings = vec![PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap()];
    let rules = generate_pf_rules(&mappings);
    assert!(!rules.is_empty());
    assert!(
        rules[0].rule_text.contains("rdr pass"),
        "rule should contain 'rdr pass': {}",
        rules[0].rule_text
    );
}

#[test]
fn pf_rules_contain_correct_ports() {
    let mappings = vec![PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap()];
    let rules = generate_pf_rules(&mappings);
    let rule = &rules[0];
    assert!(
        rule.rule_text.contains("port 8080"),
        "rule should contain host port 8080: {}",
        rule.rule_text
    );
    assert!(
        rule.rule_text.contains("port 80"),
        "rule should contain guest port 80: {}",
        rule.rule_text
    );
}

#[test]
fn pf_rules_contain_guest_ip() {
    let mappings = vec![PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap()];
    let rules = generate_pf_rules(&mappings);
    assert!(
        rules[0].rule_text.contains("172.20.0.2"),
        "rule should contain guest IP: {}",
        rules[0].rule_text
    );
}

#[test]
fn pf_rules_contain_protocol() {
    let tcp = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap();
    let udp = PortMapping::from_spec("53:53/udp", Ipv4Addr::new(172, 20, 0, 2)).unwrap();

    let tcp_rules = generate_pf_rules(&[tcp]);
    assert!(
        tcp_rules[0].rule_text.contains("proto tcp"),
        "TCP rule should contain 'proto tcp': {}",
        tcp_rules[0].rule_text
    );

    let udp_rules = generate_pf_rules(&[udp]);
    assert!(
        udp_rules[0].rule_text.contains("proto udp"),
        "UDP rule should contain 'proto udp': {}",
        udp_rules[0].rule_text
    );
}

#[test]
fn pf_rules_use_visor_anchor() {
    let mappings = vec![PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap()];
    let rules = generate_pf_rules(&mappings);
    assert!(
        rules[0].anchor.starts_with(PF_ANCHOR),
        "anchor should start with {PF_ANCHOR}: {}",
        rules[0].anchor
    );
    assert!(
        rules[0].anchor.contains("portfwd"),
        "anchor should contain 'portfwd': {}",
        rules[0].anchor
    );
}

#[test]
fn pf_rules_anchor_contains_port_and_protocol() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap();
    let rules = generate_pf_rules(&[mapping]);
    let anchor = &rules[0].anchor;
    assert!(
        anchor.contains("8080"),
        "anchor should contain host port: {anchor}"
    );
    assert!(
        anchor.contains("tcp"),
        "anchor should contain protocol: {anchor}"
    );
}

#[test]
fn pf_rules_empty_mappings_generates_no_rules() {
    let rules = generate_pf_rules(&[]);
    assert!(rules.is_empty(), "empty mappings should generate no rules");
}

// ── PfRule display ───────────────────────────────────────────────────

#[test]
fn pf_rule_display() {
    let rule = PfRule {
        anchor: "com.visor/portfwd-8080-tcp".to_owned(),
        rule_text: "rdr pass on lo0 proto tcp from any to any port 8080 -> 172.20.0.2 port 80"
            .to_owned(),
    };
    let display = format!("{rule}");
    assert!(display.contains("com.visor"), "should contain anchor name");
    assert!(display.contains("rdr pass"), "should contain rule text");
}

// ── Port forward handle ──────────────────────────────────────────────

#[test]
fn port_forward_handle_with_no_rules() {
    let mut handle = MacosPortForwardHandle {
        mapping_count: 0,
        applied_rules: Vec::new(),
    };
    assert_eq!(handle.mapping_count(), 0);
    assert!(handle.teardown().is_ok());
}

#[test]
fn port_forward_handle_mapping_count() {
    let handle = MacosPortForwardHandle {
        mapping_count: 3,
        applied_rules: Vec::new(),
    };
    assert_eq!(handle.mapping_count(), 3);
}

#[test]
fn port_forward_teardown_clears_mapping_count() {
    let mut handle = MacosPortForwardHandle {
        mapping_count: 2,
        applied_rules: Vec::new(),
    };
    handle.teardown().unwrap();
    assert_eq!(handle.mapping_count(), 0);
}

// ── Port forward setup (requires privileges) ─────────────────────────

#[test]
fn setup_port_forward_empty_mappings_succeeds() {
    let backend = MacosNetworkBackend::new();
    let result = backend.setup_port_forward(&[]);
    assert!(result.is_ok(), "empty mappings should succeed");
    let handle = result.unwrap();
    assert_eq!(handle.mapping_count(), 0);
}

#[test]
fn setup_port_forward_requires_privileges() {
    let backend = MacosNetworkBackend::new();
    let mappings = vec![PortMapping::from_spec("19080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap()];
    let result = backend.setup_port_forward(&mappings);

    // pfctl requires root. In unprivileged environments, this will fail.
    match result {
        Ok(mut handle) => {
            assert_eq!(handle.mapping_count(), 1);
            // Clean up
            let _ = handle.teardown();
        }
        Err(e) => {
            let msg = format!("{e}");
            assert!(
                msg.contains("pfctl")
                    || msg.contains("permission")
                    || msg.contains("Operation not permitted")
                    || msg.contains("failed"),
                "unexpected error: {msg}"
            );
        }
    }
}

// ── vmnet-helper: version detection ─────────────────────────────────

#[test]
fn test_version_detection_parses_current_os() {
    let result = is_macos_26_or_later();
    assert!(
        result.is_ok(),
        "version detection should not error: {result:?}"
    );
    // We don't know the exact version, but the result should be a valid bool.
    let _is_26 = result.unwrap();
}

// ── vmnet-helper: info deserialization ───────────────────────────────

#[test]
fn test_vmnet_helper_info_deserializes() {
    let json = r#"{"vmnet_mac_address":"aa:bb:cc:dd:ee:ff","vmnet_mtu":1500,"vmnet_max_packet_size":1514}"#;
    let info: VmnetHelperInfo = serde_json::from_str(json).unwrap();
    assert_eq!(info.mac_address, "aa:bb:cc:dd:ee:ff");
    assert_eq!(info.mtu, 1500);
    assert_eq!(info.max_packet_size, 1514);
}

#[test]
fn test_vmnet_helper_info_handles_missing_fields() {
    let json = r#"{}"#;
    let info: VmnetHelperInfo = serde_json::from_str(json).unwrap();
    assert_eq!(info.mac_address, "");
    assert_eq!(info.mtu, 0);
    assert_eq!(info.max_packet_size, 0);
}

// ── vmnet-helper: socketpair tests ──────────────────────────────────

#[test]
fn test_socketpair_creation() {
    let (a, b) = std::os::unix::net::UnixDatagram::pair().unwrap();
    let msg = b"hello";
    a.send(msg).unwrap();
    let mut buf = [0u8; 64];
    let n = b.recv(&mut buf).unwrap();
    assert_eq!(&buf[..n], msg);
}

#[test]
fn test_socketpair_send_recv_preserves_frame_boundaries() {
    let (a, b) = std::os::unix::net::UnixDatagram::pair().unwrap();
    let frame1 = b"frame_one";
    let frame2 = b"frame_two_longer";
    a.send(frame1).unwrap();
    a.send(frame2).unwrap();

    let mut buf = [0u8; 64];
    let n1 = b.recv(&mut buf).unwrap();
    assert_eq!(
        &buf[..n1],
        frame1,
        "first recv should return first frame exactly"
    );
    let n2 = b.recv(&mut buf).unwrap();
    assert_eq!(
        &buf[..n2],
        frame2,
        "second recv should return second frame exactly"
    );
}
