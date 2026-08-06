use std::net::Ipv4Addr;

use super::*;
use crate::net::backend::{InterfaceConfig, NatConfig, NetworkBackend, PortMapping};

// ── LinuxNetworkBackend construction ─────────────────────────────────

#[test]
fn linux_backend_default() {
    let _backend = LinuxNetworkBackend::default();
}

// ── Interface creation ───────────────────────────────────────────────

#[test]
fn create_interface_requires_root() {
    let backend = LinuxNetworkBackend::new();
    let config = InterfaceConfig::new("test_tap0");
    let result = backend.create_interface(&config);

    if nix::unistd::geteuid().is_root() {
        match result {
            Ok(iface) => {
                assert_eq!(iface.name(), "test_tap0");
                // Drop cleans up
            }
            Err(e) => {
                let msg = format!("{e}");
                // Acceptable errors: device busy, already exists
                assert!(
                    msg.contains("EBUSY")
                        || msg.contains("busy")
                        || msg.contains("exists")
                        || msg.contains("permission")
                        || msg.contains("/dev/net/tun")
                        || msg.contains("tuntap"),
                    "unexpected error: {msg}"
                );
            }
        }
    } else {
        assert!(result.is_err(), "non-root should fail to create TAP device");
    }
}

#[test]
fn create_interface_rejects_invalid_config() {
    let backend = LinuxNetworkBackend::new();
    let config = InterfaceConfig::new(&"x".repeat(20));
    let result = backend.create_interface(&config);
    assert!(result.is_err(), "should reject too-long interface name");
}

#[test]
fn tap_packet_io_requires_existing_interface() {
    let result = TapPacketIo::open("visor-missing-iface");
    assert!(
        result.is_err(),
        "opening packet I/O on a missing interface should fail"
    );
}

// ── NAT rule generation ──────────────────────────────────────────────

#[test]
fn nat_rules_generates_masquerade_rule() {
    let config = NatConfig::new("visor0", "172.20.0.0/24");
    let rules = generate_nat_rules(&config);
    assert!(!rules.is_empty(), "should generate at least one rule");

    let has_masquerade = rules
        .iter()
        .any(|r| r.args.iter().any(|a| a == "MASQUERADE"));
    assert!(has_masquerade, "should have a MASQUERADE rule");
}

#[test]
fn nat_rules_contain_forward_accept() {
    let config = NatConfig::new("visor0", "172.20.0.0/24");
    let rules = generate_nat_rules(&config);

    let has_forward = rules.iter().any(|r| r.args.iter().any(|a| a == "FORWARD"));
    assert!(has_forward, "should have FORWARD chain rules");
}

#[test]
fn nat_rules_allow_guest_subnet_forwarding_to_tap() {
    let config = NatConfig::new("visor0", "172.20.0.0/24");
    let rules = generate_nat_rules(&config);

    let guest_forward_rule = rules
        .iter()
        .find(|rule| {
            rule.table == "filter"
                && rule.args.iter().any(|arg| arg == "FORWARD")
                && rule.args.iter().any(|arg| arg == "-o")
                && rule.args.iter().any(|arg| arg == "visor0")
                && rule.args.iter().any(|arg| arg == "-s")
                && rule.args.iter().any(|arg| arg == "172.20.0.0/16")
                && !rule.args.iter().any(|arg| arg == "RELATED,ESTABLISHED")
        })
        .expect("guest subnet forward rule should exist");

    assert!(
        guest_forward_rule.args.iter().any(|arg| arg == "ACCEPT"),
        "guest subnet forward rule should accept forwarded traffic: {guest_forward_rule:?}"
    );
}

#[test]
fn nat_rules_contain_comment() {
    let config = NatConfig::new("visor0", "172.20.0.0/24");
    let rules = generate_nat_rules(&config);

    let has_comment = rules
        .iter()
        .any(|r| r.args.iter().any(|a| a.contains("visor")));
    assert!(
        has_comment,
        "rules should contain visor comment for cleanup"
    );
}

#[test]
fn route_localnet_sysctl_path_points_to_interface_setting() {
    assert_eq!(
        route_localnet_sysctl_path("vsr123"),
        "/proc/sys/net/ipv4/conf/vsr123/route_localnet"
    );
}

#[test]
fn shared_nat_rules_allow_same_bridge_subnet_before_supernet_drop() {
    let rules = generate_shared_nat_rules("vsrbr1234", "100.70.1.0/24");

    let allow_same_subnet = rules
        .iter()
        .find(|rule| {
            rule.table == "filter"
                && rule.args.iter().any(|arg| arg == "FORWARD")
                && rule.args.iter().any(|arg| arg == "-i")
                && rule.args.iter().any(|arg| arg == "vsrbr1234")
                && rule.args.iter().any(|arg| arg == "-d")
                && rule.args.iter().any(|arg| arg == "100.70.1.0/24")
                && rule.args.iter().any(|arg| arg == "ACCEPT")
        })
        .expect("shared bridge should allow traffic within its own subnet");

    let drop_shared_supernet = rules
        .iter()
        .find(|rule| {
            rule.table == "filter"
                && rule.args.iter().any(|arg| arg == "FORWARD")
                && rule.args.iter().any(|arg| arg == "-i")
                && rule.args.iter().any(|arg| arg == "vsrbr1234")
                && rule.args.iter().any(|arg| arg == "-d")
                && rule
                    .args
                    .iter()
                    .any(|arg| arg == VISOR_SHARED_GUEST_SUPERNET)
                && rule.args.iter().any(|arg| arg == "DROP")
        })
        .expect("shared bridge should drop traffic to other shared-network subnets");

    let allow_index = rules
        .iter()
        .position(|rule| std::ptr::eq(rule, allow_same_subnet))
        .expect("same-subnet allow rule should be present");
    let drop_index = rules
        .iter()
        .position(|rule| std::ptr::eq(rule, drop_shared_supernet))
        .expect("shared-supernet drop rule should be present");

    assert!(
        allow_index < drop_index,
        "same-subnet traffic must be accepted before the shared-supernet drop: {rules:?}"
    );
}

#[test]
fn shared_nat_rules_refresh_when_stale_drop_precedes_same_subnet_allow() {
    let desired = generate_shared_nat_rules("vsrbr1234", "100.70.1.0/24");
    let allow_index = desired
        .iter()
        .position(|rule| {
            rule.table == "filter"
                && rule.args.iter().any(|arg| arg == "FORWARD")
                && rule.args.iter().any(|arg| arg == "-d")
                && rule.args.iter().any(|arg| arg == "100.70.1.0/24")
                && rule.args.iter().any(|arg| arg == "ACCEPT")
        })
        .expect("same-subnet allow rule should be present");
    let mut stale = desired.clone();
    let allow_rule = stale.remove(allow_index);
    stale.push(allow_rule);

    assert_ne!(
        stale, desired,
        "stale bridge rules with the same-subnet allow appended later must be refreshed"
    );
}

// ── Port forward rule generation ─────────────────────────────────────

#[test]
fn port_forward_generates_dnat_rule() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap();
    let rules = generate_port_forward_rules(&mapping);

    let has_dnat = rules
        .iter()
        .any(|r| r.args.iter().any(|a| a.contains("DNAT")));
    assert!(has_dnat, "should generate a DNAT rule");
}

#[test]
fn port_forward_generates_forward_rule() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap();
    let rules = generate_port_forward_rules(&mapping);

    let has_forward = rules.iter().any(|r| r.args.iter().any(|a| a == "FORWARD"));
    assert!(has_forward, "should generate a FORWARD rule");
}

#[test]
fn port_forward_rules_contain_comment() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap();
    let rules = generate_port_forward_rules(&mapping);

    let has_comment = rules
        .iter()
        .any(|r| r.args.iter().any(|a| a.contains("visor-portfwd")));
    assert!(has_comment, "rules should contain visor-portfwd comment");
}

#[test]
fn port_forward_rules_masquerade_loopback_clients() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2)).unwrap();
    let rules = generate_port_forward_rules(&mapping);

    let masquerade_rule = rules
        .iter()
        .find(|rule| {
            rule.table == "nat"
                && rule.args.iter().any(|arg| arg == "POSTROUTING")
                && rule.args.iter().any(|arg| arg == "MASQUERADE")
        })
        .expect("loopback source NAT rule should exist");

    assert!(
        masquerade_rule.args.iter().any(|arg| arg == "127.0.0.1/32"),
        "loopback SNAT rule should only affect local clients: {masquerade_rule:?}"
    );
    assert!(
        masquerade_rule
            .args
            .iter()
            .any(|arg| arg == &Ipv4Addr::new(172, 20, 0, 2).to_string()),
        "loopback SNAT rule should target the guest IP: {masquerade_rule:?}"
    );
}

#[test]
fn port_forward_rules_scope_to_specific_host_ip_when_present() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2))
        .unwrap()
        .with_host_ip(Ipv4Addr::new(172, 20, 0, 1));
    let rules = generate_port_forward_rules(&mapping);

    let dnat_rule = rules
        .iter()
        .find(|rule| {
            rule.table == "nat"
                && rule.args.iter().any(|arg| arg == "PREROUTING")
                && rule.args.iter().any(|arg| arg == "DNAT")
        })
        .expect("dnat rule should exist");

    assert!(
        dnat_rule.args.iter().any(|arg| arg == "172.20.0.1"),
        "dnat rule should match the scoped host IP: {dnat_rule:?}"
    );
}

#[test]
fn port_forward_rules_hairpin_masquerade_guest_clients_for_scoped_host_ip() {
    let mapping = PortMapping::from_spec("8080:80", Ipv4Addr::new(172, 20, 0, 2))
        .unwrap()
        .with_host_ip(Ipv4Addr::new(172, 20, 0, 1));
    let rules = generate_port_forward_rules(&mapping);

    let hairpin_rule = rules
        .iter()
        .find(|rule| {
            rule.table == "nat"
                && rule.args.iter().any(|arg| arg == "POSTROUTING")
                && rule.args.iter().any(|arg| arg == "MASQUERADE")
                && !rule.args.iter().any(|arg| arg == "127.0.0.1/32")
        })
        .expect("scoped host IP mappings should masquerade guest hairpin traffic");

    assert!(
        hairpin_rule
            .args
            .iter()
            .any(|arg| arg == &Ipv4Addr::new(172, 20, 0, 2).to_string()),
        "hairpin rule should target the guest IP: {hairpin_rule:?}"
    );
    assert!(
        hairpin_rule.args.iter().any(|arg| arg == "8080")
            || hairpin_rule.args.iter().any(|arg| arg == "80"),
        "hairpin rule should scope to the forwarded port: {hairpin_rule:?}"
    );
}

// ── IptablesRule ─────────────────────────────────────────────────────

#[test]
fn iptables_rule_display() {
    let rule = IptablesRule {
        table: "nat".to_owned(),
        args: vec![
            "-A".to_owned(),
            "POSTROUTING".to_owned(),
            "-s".to_owned(),
            "172.20.0.0/24".to_owned(),
            "-j".to_owned(),
            "MASQUERADE".to_owned(),
        ],
    };
    let display = format!("{rule}");
    assert!(display.contains("nat"), "should contain table name");
    assert!(display.contains("MASQUERADE"), "should contain target");
}

#[test]
fn iptables_rule_delete_args() {
    let rule = IptablesRule {
        table: "nat".to_owned(),
        args: vec![
            "-A".to_owned(),
            "POSTROUTING".to_owned(),
            "-s".to_owned(),
            "172.20.0.0/24".to_owned(),
            "-j".to_owned(),
            "MASQUERADE".to_owned(),
        ],
    };
    let delete_args = rule.delete_args();
    assert!(delete_args.iter().any(|a| a == "-D"));
    assert!(!delete_args.iter().any(|a| a == "-A"));
}

#[test]
fn parse_visor_iptables_rule_parses_tagged_nat_rule() {
    let line = "-A POSTROUTING -s 172.20.0.0/24 ! -o vsr0 -j MASQUERADE -m comment --comment visor-nat-vsr0";

    let rule = parse_visor_iptables_rule("nat", line).expect("expected Visor-tagged rule");

    assert_eq!(rule.table, "nat");
    assert_eq!(rule.args[0], "-A");
    assert!(rule.args.iter().any(|arg| arg == "POSTROUTING"));
    assert!(rule.args.iter().any(|arg| arg == "visor-nat-vsr0"));
}

#[test]
fn parse_visor_iptables_rule_strips_wrapped_comment_quotes() {
    let line = "-A FORWARD -i vsr0 -j ACCEPT -m comment --comment \"visor-portfwd-8080:tcp-172.20.0.2:80\"";

    let rule = parse_visor_iptables_rule("filter", line).expect("expected Visor-tagged rule");

    assert!(
        rule.args
            .iter()
            .any(|arg| arg == "visor-portfwd-8080:tcp-172.20.0.2:80"),
        "quoted comment token should be normalized: {rule:?}"
    );
}

#[test]
fn parse_visor_iptables_rule_ignores_non_visor_rules() {
    let line = "-A FORWARD -j ACCEPT -m comment --comment kube-proxy";
    assert!(parse_visor_iptables_rule("filter", line).is_none());
}

#[test]
fn parse_visor_iptables_rules_collects_only_tagged_entries() {
    let output = "\
-P FORWARD ACCEPT\n\
-A FORWARD -j ACCEPT -m comment --comment kube-proxy\n\
-A FORWARD -i vsr0 -j ACCEPT -m comment --comment visor-nat-vsr0\n\
-A OUTPUT -p tcp --dport 8080 -j DNAT --to-destination 172.20.0.2:80 -m comment --comment visor-portfwd-8080:tcp-172.20.0.2:80\n";

    let rules = parse_visor_iptables_rules("filter", output);

    assert_eq!(rules.len(), 2);
    assert!(rules.iter().all(|rule| {
        rule.args
            .iter()
            .any(|arg| arg.starts_with("visor-nat-") || arg.starts_with("visor-portfwd-"))
    }));
}

// ── Netmask conversion ───────────────────────────────────────────────

#[test]
fn netmask_to_prefix_common_values() {
    assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 255, 255, 0)), 24);
    assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 255, 0, 0)), 16);
    assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 0, 0, 0)), 8);
    assert_eq!(netmask_to_prefix(Ipv4Addr::new(0, 0, 0, 0)), 0);
    assert_eq!(netmask_to_prefix(Ipv4Addr::new(255, 255, 255, 255)), 32);
}
