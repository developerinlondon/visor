use std::io::Cursor;

use super::*;

#[test]
fn parse_valid_minimal_config() {
    let json = r#"{"cmd": ["/bin/echo", "hello"]}"#;
    let config = RunConfig::from_json(json).unwrap();
    assert_eq!(config.cmd, vec!["/bin/echo", "hello"]);
    assert!(config.env.is_empty());
    assert_eq!(config.workdir, "/");
    assert!(config.network.is_none());
    assert!(config.networks.is_empty());
    assert!(config.extra_hosts.is_empty());
    assert!(config.volumes.is_empty());
}

#[test]
fn parse_config_with_all_fields() {
    let json = r#"{
        "cmd": ["/usr/bin/python", "-c", "print('hi')"],
        "env": ["HOME=/root", "PATH=/usr/bin"],
        "workdir": "/app",
        "network": {
            "address": "10.0.0.5",
            "netmask": "255.255.255.0",
            "gateway": "10.0.0.1"
        },
        "volumes": [
            {"host_path": "/data", "guest_path": "/mnt/data", "read_only": false},
            {"host_path": "/config", "guest_path": "/etc/app", "read_only": true}
        ]
    }"#;
    let config = RunConfig::from_json(json).unwrap();
    assert_eq!(config.cmd.len(), 3);
    assert_eq!(config.env, vec!["HOME=/root", "PATH=/usr/bin"]);
    assert_eq!(config.workdir, "/app");

    let net = config.network.as_ref().unwrap();
    assert_eq!(net.address, "10.0.0.5");
    assert_eq!(net.netmask, "255.255.255.0");
    assert_eq!(net.gateway, "10.0.0.1");
    assert!(config.networks.is_empty());

    assert_eq!(config.volumes.len(), 2);
    assert!(!config.volumes[0].read_only);
    assert!(config.volumes[1].read_only);
    assert_eq!(config.volumes[1].guest_path, "/etc/app");
}

#[test]
fn parse_config_with_defaults_for_optional_fields() {
    let json = r#"{"cmd": ["ls"]}"#;
    let config = RunConfig::from_json(json).unwrap();
    assert_eq!(config.cmd, vec!["ls"]);
    assert!(config.env.is_empty());
    assert_eq!(config.workdir, "/");
    assert!(config.network.is_none());
    assert!(config.networks.is_empty());
    assert!(config.volumes.is_empty());
}

#[test]
fn round_trip_serialize_deserialize() {
    let original = RunConfig {
        cmd: vec!["/bin/cat".to_owned(), "/etc/hosts".to_owned()],
        env: vec!["LANG=C".to_owned()],
        workdir: "/tmp".to_owned(),
        network: Some(NetworkConfig {
            name: None,
            interface: None,
            address: "192.168.1.10".to_owned(),
            netmask: "255.255.255.0".to_owned(),
            gateway: "192.168.1.1".to_owned(),
            dns_servers: vec![],
            default_route: true,
        }),
        networks: Vec::new(),
        extra_hosts: vec![HostEntry {
            hostname: "api".to_owned(),
            address: "172.20.0.1".to_owned(),
        }],
        volumes: vec![VolumeConfig {
            host_path: "/src".to_owned(),
            guest_path: "/mnt/src".to_owned(),
            read_only: true,
            mount_tag: String::new(),
            device_path: String::new(),
            fs_type: String::new(),
        }],
        mode: "run".to_owned(),
        exec_listener: true,
    };

    let json = original.to_json().unwrap();
    let restored = RunConfig::from_json(&json).unwrap();

    assert_eq!(original.cmd, restored.cmd);
    assert_eq!(original.env, restored.env);
    assert_eq!(original.workdir, restored.workdir);
    assert_eq!(
        original.network.as_ref().unwrap().address,
        restored.network.as_ref().unwrap().address
    );
    assert_eq!(restored.extra_hosts.len(), 1);
    assert_eq!(restored.extra_hosts[0].hostname, "api");
    assert_eq!(restored.extra_hosts[0].address, "172.20.0.1");
    assert_eq!(original.volumes.len(), restored.volumes.len());
    assert_eq!(original.volumes[0].host_path, restored.volumes[0].host_path);
    assert_eq!(original.volumes[0].read_only, restored.volumes[0].read_only);
    assert_eq!(original.exec_listener, restored.exec_listener);
}

#[test]
fn serialize_omits_default_fields_when_multi_networks_are_present() {
    let mut config = RunConfig::default();
    config.cmd = vec![
        "sh".to_owned(),
        "-lc".to_owned(),
        "trap 'exit 0' TERM INT; while true; do sleep 1; done".to_owned(),
    ];
    config.env =
        vec!["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned()];
    config.networks = vec![
        NetworkConfig {
            name: Some("delta_frontend".to_owned()),
            interface: Some("eth0".to_owned()),
            address: "100.70.1.5".to_owned(),
            netmask: "255.255.255.0".to_owned(),
            gateway: "100.70.1.1".to_owned(),
            dns_servers: vec!["100.70.1.1".to_owned()],
            default_route: true,
        },
        NetworkConfig {
            name: Some("delta_backend".to_owned()),
            interface: Some("eth1".to_owned()),
            address: "100.71.1.5".to_owned(),
            netmask: "255.255.255.0".to_owned(),
            gateway: "100.71.1.1".to_owned(),
            dns_servers: vec!["100.71.1.1".to_owned()],
            default_route: false,
        },
    ];
    config.extra_hosts = vec![
        HostEntry::new("api", "100.70.1.3"),
        HostEntry::new("api.delta", "100.70.1.3"),
        HostEntry::new("delta-api-1", "100.70.1.3"),
        HostEntry::new("delta-api-1.delta", "100.70.1.3"),
        HostEntry::new("db", "100.71.1.4"),
        HostEntry::new("db.delta", "100.71.1.4"),
        HostEntry::new("delta-db-1", "100.71.1.4"),
        HostEntry::new("delta-db-1.delta", "100.71.1.4"),
    ];
    config.exec_listener = true;

    let json = config.to_json().unwrap();
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();

    assert!(value.get("network").is_none());
    assert!(value.get("volumes").is_none());
    assert!(value.get("mode").is_none());
    assert!(value.get("n").is_none());
    assert!(value.get("v").is_none());
    assert!(value.get("m").is_none());
    assert_eq!(value["ns"].as_array().unwrap().len(), 2);
    assert_eq!(value["h"].as_array().unwrap().len(), 8);
}

#[test]
fn parse_from_reader() {
    let json = r#"{"cmd": ["whoami"], "workdir": "/home"}"#;
    let cursor = Cursor::new(json.as_bytes());
    let config = RunConfig::from_reader(cursor).unwrap();
    assert_eq!(config.cmd, vec!["whoami"]);
    assert_eq!(config.workdir, "/home");
}

#[test]
fn reject_empty_command() {
    let config = RunConfig {
        cmd: vec![],
        ..RunConfig::default()
    };
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("cmd"),
        "error should mention cmd: {err}"
    );
}

#[test]
fn reject_workdir_not_starting_with_slash() {
    let config = RunConfig {
        cmd: vec!["ls".to_owned()],
        workdir: "relative/path".to_owned(),
        ..RunConfig::default()
    };
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("workdir"),
        "error should mention workdir: {err}"
    );
}

#[test]
fn validate_accepts_valid_config() {
    let config = RunConfig {
        cmd: vec!["/bin/sh".to_owned()],
        workdir: "/".to_owned(),
        network: Some(NetworkConfig::default()),
        ..RunConfig::default()
    };
    config.validate().unwrap();
}

#[test]
fn validate_network_rejects_empty_gateway() {
    let config = RunConfig {
        cmd: vec!["/bin/sh".to_owned()],
        network: Some(NetworkConfig {
            address: "10.0.0.2".to_owned(),
            netmask: "255.255.255.0".to_owned(),
            gateway: String::new(),
            dns_servers: vec![],
            ..NetworkConfig::default()
        }),
        ..RunConfig::default()
    };
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("gateway"),
        "error should mention gateway: {err}"
    );
}

#[test]
fn validate_network_rejects_empty_address() {
    let config = RunConfig {
        cmd: vec!["/bin/sh".to_owned()],
        network: Some(NetworkConfig {
            address: String::new(),
            netmask: "255.255.255.0".to_owned(),
            gateway: "10.0.0.1".to_owned(),
            dns_servers: vec![],
            ..NetworkConfig::default()
        }),
        ..RunConfig::default()
    };
    let err = config.validate().unwrap_err();
    assert!(
        err.to_string().contains("address"),
        "error should mention address: {err}"
    );
}

#[test]
fn default_run_config_has_bin_sh() {
    let config = RunConfig::default();
    assert_eq!(config.cmd, vec!["/bin/sh"]);
    assert_eq!(config.workdir, "/");
    assert!(config.env.is_empty());
    assert!(config.network.is_none());
    assert!(config.networks.is_empty());
    assert!(config.volumes.is_empty());
    assert!(!config.exec_listener);
}

#[test]
fn default_network_config_has_expected_values() {
    let net = NetworkConfig::default();
    assert!(net.name.is_none());
    assert!(net.interface.is_none());
    assert_eq!(net.address, "10.0.0.2");
    assert_eq!(net.netmask, "255.255.255.0");
    assert_eq!(net.gateway, "10.0.0.1");
    assert!(net.default_route);
}

#[test]
fn parse_json_with_unknown_fields_succeeds() {
    let json = r#"{"cmd": ["ls"], "unknown_field": 42, "extra": true}"#;
    let config = RunConfig::from_json(json).unwrap();
    assert_eq!(config.cmd, vec!["ls"]);
}

#[test]
fn parse_invalid_json_returns_error() {
    let result = RunConfig::from_json("not valid json {{{");
    assert!(result.is_err());
}

#[test]
fn volumes_parse_with_read_only_flag() {
    let json = r#"{
        "cmd": ["ls"],
        "volumes": [
            {"host_path": "/a", "guest_path": "/b", "read_only": true},
            {"host_path": "/c", "guest_path": "/d", "read_only": false}
        ]
    }"#;
    let config = RunConfig::from_json(json).unwrap();
    assert_eq!(config.volumes.len(), 2);
    assert!(config.volumes[0].read_only);
    assert!(!config.volumes[1].read_only);
    assert_eq!(config.volumes[0].host_path, "/a");
    assert_eq!(config.volumes[1].guest_path, "/d");
}

#[test]
fn volumes_parse_with_explicit_mount_tag() {
    let json = r#"{
        "cmd": ["ls"],
        "volumes": [
            {
                "guest_path": "/workspace",
                "mount_tag": "visor-fs-0",
                "read_only": true
            }
        ]
    }"#;
    let config = RunConfig::from_json(json).unwrap();
    assert_eq!(config.volumes.len(), 1);
    assert_eq!(config.volumes[0].mount_tag, "visor-fs-0");
    assert!(config.volumes[0].device_path.is_empty());
}

#[test]
fn volumes_parse_with_explicit_device_path() {
    let json = r#"{
        "cmd": ["ls"],
        "volumes": [
            {
                "guest_path": "/var/lib/data",
                "device_path": "/dev/vdb",
                "fs_type": "ext4"
            }
        ]
    }"#;
    let config = RunConfig::from_json(json).unwrap();
    assert_eq!(config.volumes.len(), 1);
    assert_eq!(config.volumes[0].device_path, "/dev/vdb");
    assert_eq!(config.volumes[0].fs_type, "ext4");
    assert!(config.volumes[0].mount_tag.is_empty());
}

#[test]
fn default_volume_config_values() {
    let vol = VolumeConfig::default();
    assert!(vol.host_path.is_empty());
    assert!(vol.guest_path.is_empty());
    assert!(!vol.read_only);
}

#[test]
fn parse_cmdline_with_visor_config() {
    use base64::Engine as _;
    let json = r#"{"cmd":["/bin/echo","hello"]}"#;
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(json);
    let cmdline = format!("console=ttyS0 visor.config={b64} quiet");
    let config = RunConfig::parse_cmdline(&cmdline).unwrap();
    assert_eq!(config.cmd, vec!["/bin/echo", "hello"]);
}

#[test]
fn parse_cmdline_without_visor_config_returns_none() {
    let cmdline = "console=ttyS0 quiet";
    assert!(RunConfig::parse_cmdline(cmdline).is_none());
}

#[test]
fn parse_cmdline_with_empty_string_returns_none() {
    assert!(RunConfig::parse_cmdline("").is_none());
}

#[test]
fn parse_cmdline_with_invalid_base64_returns_none() {
    let cmdline = "visor.config=!!!not-base64!!!";
    assert!(RunConfig::parse_cmdline(cmdline).is_none());
}

#[test]
fn parse_cmdline_preserves_all_fields() {
    use base64::Engine as _;
    let json = r#"{"cmd":["/usr/bin/python"],"env":["HOME=/root"],"workdir":"/app"}"#;
    let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(json);
    let cmdline = format!("visor.config={b64}");
    let config = RunConfig::parse_cmdline(&cmdline).unwrap();
    assert_eq!(config.cmd, vec!["/usr/bin/python"]);
    assert_eq!(config.env, vec!["HOME=/root"]);
    assert_eq!(config.workdir, "/app");
}
