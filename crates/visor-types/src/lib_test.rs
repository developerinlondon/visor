use super::*;

#[test]
fn vm_config_defaults() {
    let config: VmConfig = serde_json::from_str(r#"{"image": "alpine:latest"}"#).unwrap();
    assert_eq!(config.memory_mib, 512);
    assert_eq!(config.vcpus, 1);
    assert!(config.rootfs_extra_size_mib.is_none());
    assert!(config.process_limit.is_none());
    assert!(config.entrypoint.is_empty());
    assert!(config.cmd.is_empty());
    assert!(config.env.is_empty());
    assert!(config.ports.is_empty());
    assert!(config.volumes.is_empty());
    assert!(config.extra_hosts.is_empty());
    assert!(config.networks.is_empty());
    assert!(config.service_names.is_empty());
    assert!(config.service_ports.is_empty());
    assert!(config.network_enabled);
    assert_eq!(
        config.guest_virtualization,
        GuestVirtualizationMode::Standard
    );
    assert!(!config.detach);
    assert!(config.mode.is_none());
}

#[test]
fn host_entry_new_sets_hostname_and_address() {
    let entry = HostEntry::new("api", "172.20.0.1");

    assert_eq!(entry.hostname, "api");
    assert_eq!(entry.address, "172.20.0.1");
}

#[test]
fn guest_network_link_for_first_guest_cid_uses_expected_point_to_point_range() {
    let link = GuestNetworkLink::for_cid(3);

    assert_eq!(link.guest_ip, std::net::Ipv4Addr::new(172, 20, 0, 2));
    assert_eq!(link.gateway_ip, std::net::Ipv4Addr::new(172, 20, 0, 1));
    assert_eq!(link.netmask, std::net::Ipv4Addr::new(255, 255, 255, 252));
}

#[test]
fn guest_network_link_for_named_network_is_stable() {
    let link = GuestNetworkLink::for_named_network("alpha_frontend", 3);

    assert_eq!(link.guest_ip, std::net::Ipv4Addr::new(100, 107, 56, 2));
    assert_eq!(link.gateway_ip, std::net::Ipv4Addr::new(100, 107, 56, 1));
    assert_eq!(link.netmask, std::net::Ipv4Addr::new(255, 255, 255, 0));
}

#[test]
fn guest_network_link_for_named_network_changes_subnet_by_network() {
    let frontend = GuestNetworkLink::for_named_network("alpha_frontend", 3);
    let backend = GuestNetworkLink::for_named_network("alpha_backend", 3);

    assert_ne!(frontend.gateway_ip, backend.gateway_ip);
    assert_ne!(frontend.guest_ip, backend.guest_ip);
}

#[test]
fn named_network_supernet_maps_names_into_configured_range() {
    let supernet = NamedNetworkSupernet::parse("10.200.0.0/16").unwrap();
    let link = supernet.link_for_named_network("runner-bridge", 3);

    assert_eq!(supernet.cidr(), "10.200.0.0/16");
    assert_eq!(link.guest_ip, std::net::Ipv4Addr::new(10, 200, 243, 2));
    assert_eq!(link.gateway_ip, std::net::Ipv4Addr::new(10, 200, 243, 1));
    assert_eq!(link.netmask, std::net::Ipv4Addr::new(255, 255, 255, 0));
}

#[test]
fn named_network_supernet_rejects_ranges_smaller_than_one_ipv4_subnet() {
    let error = NamedNetworkSupernet::parse("10.200.0.0/25").unwrap_err();

    assert!(error.to_string().contains("prefix must be between /8 and /24"));
}

#[test]
fn vm_state_default_is_creating() {
    assert_eq!(VmState::default(), VmState::Creating);
}

#[test]
fn port_mapping_defaults() {
    let mapping: PortMapping =
        serde_json::from_str(r#"{"host_port": 8080, "guest_port": 80}"#).unwrap();
    assert_eq!(mapping.protocol, "tcp");
}

#[test]
fn volume_mount_defaults() {
    let mount: VolumeMount =
        serde_json::from_str(r#"{"host_path": "/tmp", "guest_path": "/mnt"}"#).unwrap();
    assert!(!mount.read_only);
}

#[test]
fn vm_state_serde_roundtrip() {
    let state = VmState::Running;
    let json = serde_json::to_string(&state).unwrap();
    assert_eq!(json, r#""running""#);
    let parsed: VmState = serde_json::from_str(&json).unwrap();
    assert_eq!(parsed, state);
}

#[test]
fn exec_result_fields() {
    let result = ExecResult {
        exit_code: 0,
        stdout: "hello".to_owned(),
        stderr: String::new(),
    };
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "hello");
}

#[test]
fn build_request_new_sets_defaults() {
    let req = BuildRequest::new("FROM alpine\nRUN echo hello".to_owned());
    assert_eq!(req.dockerfile_content, "FROM alpine\nRUN echo hello");
    assert!(req.context_dir.as_os_str().is_empty());
    assert!(req.build_args.is_empty());
    assert!(req.target.is_none());
    assert!(!req.no_cache);
    assert!(req.tag.is_none());
}

#[test]
fn build_progress_new_fields() {
    let progress = BuildProgress::new(1, 3, "RUN echo hello".to_owned());
    assert_eq!(progress.step, 1);
    assert_eq!(progress.total, 3);
    assert_eq!(progress.instruction, "RUN echo hello");
    assert!(!progress.cached);
    assert!(progress.output.is_none());
}

#[test]
fn build_output_new_fields() {
    let output = BuildOutput::new(
        "sha256:abc123".to_owned(),
        vec![BuildProgress::new(1, 1, "FROM alpine".to_owned())],
    );
    assert_eq!(output.image_id, "sha256:abc123");
    assert_eq!(output.steps.len(), 1);
}

#[test]
fn vm_config_mode_field_deserialization() {
    let config: VmConfig =
        serde_json::from_str(r#"{"image": "scratch", "mode": "agent"}"#).unwrap();
    assert_eq!(config.mode.as_deref(), Some("agent"));
}

#[test]
fn vm_config_mode_defaults_to_none() {
    let config = VmConfig::new("alpine");
    assert!(config.mode.is_none());
}

#[test]
fn guest_virtualization_mode_defaults_to_standard() {
    let mode: GuestVirtualizationMode = serde_json::from_str(r#""standard""#).unwrap();
    assert_eq!(mode, GuestVirtualizationMode::Standard);
}

#[test]
fn vm_config_guest_virtualization_deserializes_nested() {
    let config: VmConfig =
        serde_json::from_str(r#"{"image": "alpine:latest", "guest_virtualization": "nested"}"#)
            .unwrap();
    assert_eq!(config.guest_virtualization, GuestVirtualizationMode::Nested);
}

#[test]
fn image_info_new_sets_docker_defaults() {
    let info = ImageInfo::new("sha256:test", vec!["alpine:latest".to_owned()]);
    assert_eq!(info.id, "sha256:test");
    assert_eq!(info.repo_tags, vec!["alpine:latest"]);
    assert_eq!(info.created, 0);
    assert_eq!(info.size, 0);
    assert!(info.labels.is_empty());
    assert_eq!(info.os, "linux");
    assert!(!info.architecture.is_empty());
}
