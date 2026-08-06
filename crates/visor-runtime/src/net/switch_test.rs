use std::net::Ipv4Addr;

use super::*;

// ── Construction ──────────────────────────────────────────────────────

#[test]
fn new_switch_has_no_ports() {
    let sw = VirtualSwitch::new(
        "visor0",
        Ipv4Addr::new(172, 20, 0, 0),
        24,
        Ipv4Addr::new(172, 20, 0, 1),
    );
    assert_eq!(sw.name(), "visor0");
    assert_eq!(sw.port_count(), 0);
    assert!(sw.ports().is_empty());
}

#[test]
fn default_switch_uses_visor0_settings() {
    let sw = VirtualSwitch::default_network();
    assert_eq!(sw.name(), "visor0");
    assert_eq!(sw.subnet_base(), Ipv4Addr::new(172, 20, 0, 0));
    assert_eq!(sw.subnet_prefix(), 24);
    assert_eq!(sw.gateway(), Ipv4Addr::new(172, 20, 0, 1));
}

// ── Port Registration ────────────────────────────────────────────────

#[test]
fn register_port_adds_to_switch() {
    let mut sw = VirtualSwitch::default_network();
    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let ip = Ipv4Addr::new(172, 20, 0, 2);

    sw.register_port("vm-1", mac, ip).unwrap();
    assert_eq!(sw.port_count(), 1);
    assert!(sw.has_port(&mac));
}

#[test]
fn register_duplicate_mac_fails() {
    let mut sw = VirtualSwitch::default_network();
    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let ip1 = Ipv4Addr::new(172, 20, 0, 2);
    let ip2 = Ipv4Addr::new(172, 20, 0, 3);

    sw.register_port("vm-1", mac, ip1).unwrap();
    let result = sw.register_port("vm-2", mac, ip2);
    assert!(result.is_err(), "duplicate MAC should fail");
}

#[test]
fn register_duplicate_ip_fails() {
    let mut sw = VirtualSwitch::default_network();
    let mac1 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mac2 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    let ip = Ipv4Addr::new(172, 20, 0, 2);

    sw.register_port("vm-1", mac1, ip).unwrap();
    let result = sw.register_port("vm-2", mac2, ip);
    assert!(result.is_err(), "duplicate IP should fail");
}

// ── Port Removal ─────────────────────────────────────────────────────

#[test]
fn unregister_port_removes_from_switch() {
    let mut sw = VirtualSwitch::default_network();
    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let ip = Ipv4Addr::new(172, 20, 0, 2);

    sw.register_port("vm-1", mac, ip).unwrap();
    assert_eq!(sw.port_count(), 1);

    sw.unregister_port(&mac).unwrap();
    assert_eq!(sw.port_count(), 0);
    assert!(!sw.has_port(&mac));
}

#[test]
fn unregister_nonexistent_port_fails() {
    let mut sw = VirtualSwitch::default_network();
    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0xFF]);
    let result = sw.unregister_port(&mac);
    assert!(result.is_err(), "unregistering nonexistent MAC should fail");
}

// ── Lookup ───────────────────────────────────────────────────────────

#[test]
fn lookup_by_mac_returns_port_info() {
    let mut sw = VirtualSwitch::default_network();
    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let ip = Ipv4Addr::new(172, 20, 0, 2);

    sw.register_port("vm-1", mac, ip).unwrap();

    let port = sw.lookup_mac(&mac).unwrap();
    assert_eq!(port.vm_id(), "vm-1");
    assert_eq!(port.ip(), ip);
    assert_eq!(port.mac(), mac);
}

#[test]
fn lookup_by_ip_returns_port_info() {
    let mut sw = VirtualSwitch::default_network();
    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let ip = Ipv4Addr::new(172, 20, 0, 2);

    sw.register_port("vm-1", mac, ip).unwrap();

    let port = sw.lookup_ip(ip).unwrap();
    assert_eq!(port.vm_id(), "vm-1");
    assert_eq!(port.mac(), mac);
}

#[test]
fn lookup_by_vm_id_returns_port_info() {
    let mut sw = VirtualSwitch::default_network();
    let mac = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let ip = Ipv4Addr::new(172, 20, 0, 2);

    sw.register_port("vm-1", mac, ip).unwrap();

    let port = sw.lookup_vm("vm-1").unwrap();
    assert_eq!(port.mac(), mac);
    assert_eq!(port.ip(), ip);
}

// ── MAC Address ──────────────────────────────────────────────────────

#[test]
fn mac_addr_display_format() {
    let mac = MacAddr::new([0x02, 0xAB, 0xCD, 0xEF, 0x01, 0x23]);
    assert_eq!(format!("{mac}"), "02:ab:cd:ef:01:23");
}

#[test]
fn mac_addr_equality() {
    let mac1 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mac2 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mac3 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);
    assert_eq!(mac1, mac2);
    assert_ne!(mac1, mac3);
}

#[test]
fn generate_mac_is_deterministic() {
    let mac1 = MacAddr::generate("visor0", 2);
    let mac2 = MacAddr::generate("visor0", 2);
    assert_eq!(mac1, mac2);
}

#[test]
fn generate_mac_differs_for_different_inputs() {
    let mac1 = MacAddr::generate("visor0", 2);
    let mac2 = MacAddr::generate("visor0", 3);
    let mac3 = MacAddr::generate("visor1", 2);
    assert_ne!(mac1, mac2);
    assert_ne!(mac1, mac3);
}

#[test]
fn generate_mac_sets_locally_administered_bit() {
    let mac = MacAddr::generate("visor0", 2);
    // Bit 1 of first octet should be set (locally administered)
    assert_eq!(mac.octets()[0] & 0x02, 0x02);
    // Bit 0 of first octet should be clear (unicast)
    assert_eq!(mac.octets()[0] & 0x01, 0x00);
}

// ── Ports listing ────────────────────────────────────────────────────

#[test]
fn ports_returns_all_registered_ports() {
    let mut sw = VirtualSwitch::default_network();
    let mac1 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
    let mac2 = MacAddr::new([0x02, 0x00, 0x00, 0x00, 0x00, 0x02]);

    sw.register_port("vm-1", mac1, Ipv4Addr::new(172, 20, 0, 2))
        .unwrap();
    sw.register_port("vm-2", mac2, Ipv4Addr::new(172, 20, 0, 3))
        .unwrap();

    let ports = sw.ports();
    assert_eq!(ports.len(), 2);
}
