use std::net::Ipv4Addr;

use super::*;
use crate::config::NetworkConfig;

#[test]
fn from_config_parses_valid_config() {
    let config = NetworkConfig {
        address: "10.0.0.2".to_owned(),
        netmask: "255.255.255.0".to_owned(),
        gateway: "10.0.0.1".to_owned(),
        dns_servers: Vec::new(),
        ..NetworkConfig::default()
    };
    let setup = NetworkSetup::from_config(&config, 0).expect("valid config should parse");
    assert_eq!(setup.address, Ipv4Addr::new(10, 0, 0, 2));
    assert_eq!(setup.netmask, Ipv4Addr::new(255, 255, 255, 0));
    assert_eq!(setup.gateway, Ipv4Addr::new(10, 0, 0, 1));
    assert!(setup.default_route);
}

#[test]
fn from_config_rejects_invalid_address() {
    let config = NetworkConfig {
        address: "not-an-ip".to_owned(),
        netmask: "255.255.255.0".to_owned(),
        gateway: "10.0.0.1".to_owned(),
        dns_servers: Vec::new(),
        ..NetworkConfig::default()
    };
    let err = NetworkSetup::from_config(&config, 0).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("invalid"), "expected 'invalid' in: {msg}");
}

#[test]
fn from_config_rejects_invalid_netmask() {
    let config = NetworkConfig {
        address: "10.0.0.2".to_owned(),
        netmask: "bad-mask".to_owned(),
        gateway: "10.0.0.1".to_owned(),
        dns_servers: Vec::new(),
        ..NetworkConfig::default()
    };
    let err = NetworkSetup::from_config(&config, 0).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("netmask"), "expected 'netmask' in: {msg}");
}

#[test]
fn from_config_rejects_invalid_gateway() {
    let config = NetworkConfig {
        address: "10.0.0.2".to_owned(),
        netmask: "255.255.255.0".to_owned(),
        gateway: "999.999.999.999".to_owned(),
        dns_servers: Vec::new(),
        ..NetworkConfig::default()
    };
    let err = NetworkSetup::from_config(&config, 0).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("gateway"), "expected 'gateway' in: {msg}");
}

#[test]
fn from_config_defaults_interface_to_eth0() {
    let config = NetworkConfig {
        address: "192.168.1.10".to_owned(),
        netmask: "255.255.255.0".to_owned(),
        gateway: "192.168.1.1".to_owned(),
        dns_servers: Vec::new(),
        ..NetworkConfig::default()
    };
    let setup = NetworkSetup::from_config(&config, 0).expect("valid config");
    assert_eq!(setup.interface, "eth0");
}

#[test]
fn from_config_uses_attachment_index_for_default_interface_names() {
    let config = NetworkConfig::default();
    let setup = NetworkSetup::from_config(&config, 2).expect("valid config");
    assert_eq!(setup.interface, "eth2");
}

#[test]
fn parse_ipv4_valid_address() {
    let addr = parse_ipv4("10.0.0.1").expect("valid IPv4");
    assert_eq!(addr, Ipv4Addr::new(10, 0, 0, 1));
}

#[test]
fn parse_ipv4_invalid_returns_error() {
    let err = parse_ipv4("not.an.ip.address").unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("invalid IPv4 address"),
        "expected 'invalid IPv4 address' in: {msg}"
    );
}

#[test]
fn build_ifreq_sets_interface_name_correctly() {
    let ifr = build_ifreq("eth0");
    #[allow(clippy::cast_possible_wrap)]
    let expected: [libc::c_char; 5] = [
        b'e' as libc::c_char,
        b't' as libc::c_char,
        b'h' as libc::c_char,
        b'0' as libc::c_char,
        0,
    ];
    assert_eq!(&ifr.ifr_name[..5], &expected);
}

#[test]
fn build_ifreq_truncates_long_interface_names() {
    let long_name = "a".repeat(libc::IFNAMSIZ + 10);
    let ifr = build_ifreq(&long_name);

    // Last byte must be NUL terminator (from zeroing)
    assert_eq!(ifr.ifr_name[libc::IFNAMSIZ - 1], 0);

    // All bytes before the terminator should be 'a'
    for byte in &ifr.ifr_name[..libc::IFNAMSIZ - 1] {
        #[allow(clippy::cast_possible_wrap)]
        {
            assert_eq!(*byte, b'a' as libc::c_char);
        }
    }
}

#[test]
fn sockaddr_in_from_ip_sets_correct_address_bytes() {
    let ip = Ipv4Addr::new(10, 0, 0, 2);
    let sa = sockaddr_in_from_ip(ip);
    let addr_bytes = sa.sin_addr.s_addr.to_ne_bytes();
    assert_eq!(addr_bytes, [10, 0, 0, 2]);
}

#[test]
fn sockaddr_in_from_ip_sets_af_inet_family() {
    let sa = sockaddr_in_from_ip(Ipv4Addr::LOCALHOST);
    #[allow(clippy::cast_possible_truncation)]
    {
        assert_eq!(sa.sin_family, libc::AF_INET as libc::sa_family_t);
    }
}

#[test]
fn loopback_address_is_127_0_0_1() {
    let sa = sockaddr_in_from_ip(Ipv4Addr::LOCALHOST);
    let addr_bytes = sa.sin_addr.s_addr.to_ne_bytes();
    assert_eq!(addr_bytes, [127, 0, 0, 1]);
}

#[test]
fn resolv_conf_contents_defaults_to_gateway_when_no_dns_servers_are_set() {
    let config = NetworkConfig {
        address: "10.0.0.2".to_owned(),
        netmask: "255.255.255.0".to_owned(),
        gateway: "10.0.0.1".to_owned(),
        dns_servers: Vec::new(),
        ..NetworkConfig::default()
    };

    let resolv_conf = resolv_conf_contents(&[config]);
    assert_eq!(resolv_conf, "nameserver 10.0.0.1\n");
}

#[test]
fn resolv_conf_contents_uses_all_explicit_dns_servers() {
    let config = NetworkConfig {
        address: "10.0.0.2".to_owned(),
        netmask: "255.255.255.0".to_owned(),
        gateway: "10.0.0.1".to_owned(),
        dns_servers: vec!["1.1.1.1".to_owned(), "8.8.8.8".to_owned()],
        ..NetworkConfig::default()
    };

    let resolv_conf = resolv_conf_contents(&[config]);
    assert_eq!(resolv_conf, "nameserver 1.1.1.1\nnameserver 8.8.8.8\n");
}

#[test]
fn hosts_file_contents_includes_default_localhost_and_extra_hosts() {
    let hosts = hosts_file_contents(&[
        crate::config::HostEntry {
            hostname: "api".to_owned(),
            address: "172.20.0.1".to_owned(),
        },
        crate::config::HostEntry {
            hostname: "db".to_owned(),
            address: "172.20.0.5".to_owned(),
        },
    ]);

    assert!(hosts.contains("127.0.0.1 localhost\n"));
    assert!(hosts.contains("::1 localhost ip6-localhost ip6-loopback\n"));
    assert!(hosts.contains("172.20.0.1 api\n"));
    assert!(hosts.contains("172.20.0.5 db\n"));
}
