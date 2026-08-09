use std::collections::HashMap;

use visor_types::{ExecResult, PortMapping, VmInfo, VmState};

use super::*;
use crate::types::{
    ContainerCreateRequest, EndpointConfig, ExecCreateRequest, HostConfig, NetworkingConfig,
    PortBinding,
};

#[test]
fn docker_create_to_vm_config_basic() {
    let req = ContainerCreateRequest {
        image: "alpine:latest".to_owned(),
        entrypoint: Some(vec!["/docker-entrypoint.sh".to_owned()]),
        cmd: Some(vec!["echo".to_owned(), "hello".to_owned()]),
        env: Some(vec!["FOO=bar".to_owned()]),
        working_dir: Some("/app".to_owned()),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, Some("mycontainer"), true).unwrap();
    assert_eq!(config.image, "alpine:latest");
    assert_eq!(config.entrypoint, vec!["/docker-entrypoint.sh"]);
    assert_eq!(config.cmd, vec!["echo", "hello"]);
    assert_eq!(config.env, vec!["FOO=bar"]);
    assert_eq!(config.working_dir.as_deref(), Some("/app"));
    assert_eq!(config.name.as_deref(), Some("mycontainer"));
    assert!(config.network_enabled);
    assert!(config.detach);
}

#[test]
fn docker_create_to_vm_config_disables_network_for_network_mode_none() {
    let req = ContainerCreateRequest {
        image: "alpine:latest".to_owned(),
        host_config: Some(HostConfig {
            network_mode: Some("none".to_owned()),
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert!(!config.network_enabled);
}

#[test]
fn docker_create_to_vm_config_preserves_labels() {
    let req = ContainerCreateRequest {
        image: "alpine:latest".to_owned(),
        labels: Some(HashMap::from([
            (
                "com.docker.compose.project".to_owned(),
                "visor-compose".to_owned(),
            ),
            ("com.docker.compose.service".to_owned(), "app".to_owned()),
        ])),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, Some("compose-app"), true).unwrap();
    assert_eq!(
        config.labels.get("com.docker.compose.project"),
        Some(&"visor-compose".to_owned())
    );
    assert_eq!(
        config.labels.get("com.docker.compose.service"),
        Some(&"app".to_owned())
    );
}

#[test]
fn docker_create_to_vm_config_ports() {
    let mut port_bindings = HashMap::new();
    port_bindings.insert(
        "80/tcp".to_owned(),
        vec![PortBinding {
            host_ip: Some("0.0.0.0".to_owned()),
            host_port: Some("8080".to_owned()),
        }],
    );

    let req = ContainerCreateRequest {
        image: "nginx".to_owned(),
        host_config: Some(HostConfig {
            port_bindings: Some(port_bindings),
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert_eq!(config.ports.len(), 1);
    assert_eq!(config.ports[0].host_port, 8080);
    assert_eq!(config.ports[0].guest_port, 80);
}

#[test]
fn docker_create_to_vm_config_volumes() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        host_config: Some(HostConfig {
            binds: Some(vec![
                "/data:/mnt/data".to_owned(),
                "/config:/etc/config:ro".to_owned(),
            ]),
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, false).unwrap();
    assert_eq!(config.volumes.len(), 2);
    assert_eq!(config.volumes[0].host_path, "/data");
    assert_eq!(config.volumes[0].guest_path, "/mnt/data");
    assert!(!config.volumes[0].read_only);
    assert_eq!(config.volumes[1].host_path, "/config");
    assert_eq!(config.volumes[1].guest_path, "/etc/config");
    assert!(config.volumes[1].read_only);
}

#[test]
fn docker_create_to_vm_config_memory() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        host_config: Some(HostConfig {
            memory: Some(268_435_456), // 256 MiB in bytes
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert_eq!(config.memory_mib, 256);
}

#[test]
fn docker_create_to_vm_config_memory_minimum_64mib() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        host_config: Some(HostConfig {
            memory: Some(1024 * 1024), // 1 MiB — below minimum
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert_eq!(config.memory_mib, 64); // Clamped to minimum
}

#[test]
fn docker_create_to_vm_config_nanocpus() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        host_config: Some(HostConfig {
            nano_cpus: Some(2_000_000_000), // 2 CPUs
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert_eq!(config.vcpus, 2);
}

#[test]
fn docker_create_to_vm_config_maps_process_limit() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        host_config: Some(HostConfig {
            pids_limit: Some(256),
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert_eq!(config.process_limit, Some(256));
}

#[test]
fn docker_create_to_vm_config_rejects_invalid_process_limit() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        host_config: Some(HostConfig {
            pids_limit: Some(-2),
            ..Default::default()
        }),
        ..Default::default()
    };

    let error = docker_create_to_vm_config(&req, None, true).unwrap_err();
    assert_eq!(error, DockerConfigError::InvalidProcessLimit(-2));
}

#[test]
fn docker_create_to_vm_config_maps_writable_layer_size() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        host_config: Some(HostConfig {
            storage_opt: Some(HashMap::from([("size".to_owned(), "1024m".to_owned())])),
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert_eq!(config.rootfs_extra_size_mib, Some(1024));
}

#[test]
fn docker_create_to_vm_config_rejects_invalid_writable_layer_size() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        host_config: Some(HostConfig {
            storage_opt: Some(HashMap::from([("size".to_owned(), "many".to_owned())])),
            ..Default::default()
        }),
        ..Default::default()
    };

    let error = docker_create_to_vm_config(&req, None, true).unwrap_err();
    assert_eq!(
        error,
        DockerConfigError::InvalidStorageSize("many".to_owned())
    );
}

#[test]
fn docker_create_to_vm_config_rejects_oversized_writable_layer() {
    let requested_mib = visor_types::MAX_ROOTFS_EXTRA_SIZE_MIB + 1;
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        host_config: Some(HostConfig {
            storage_opt: Some(HashMap::from([(
                "size".to_owned(),
                format!("{requested_mib}m"),
            )])),
            ..Default::default()
        }),
        ..Default::default()
    };

    let error = docker_create_to_vm_config(&req, None, true).unwrap_err();
    assert_eq!(
        error,
        DockerConfigError::StorageSizeTooLarge {
            requested_mib,
            maximum_mib: visor_types::MAX_ROOTFS_EXTRA_SIZE_MIB,
        }
    );
}

#[test]
fn docker_create_to_vm_config_collects_service_names_and_exposed_ports() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        hostname: Some("api-host".to_owned()),
        networking_config: Some(NetworkingConfig {
            endpoints_config: Some(HashMap::from([(
                "visor-compose_default".to_owned(),
                EndpointConfig {
                    aliases: Some(vec!["api".to_owned(), "backend".to_owned()]),
                },
            )])),
        }),
        exposed_ports: Some(HashMap::from([
            ("8080/tcp".to_owned(), serde_json::json!({})),
            ("53/udp".to_owned(), serde_json::json!({})),
        ])),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, Some("visor-compose-api-1"), true).unwrap();

    assert_eq!(config.networks, vec!["visor-compose_default".to_owned()]);
    assert_eq!(
        config.service_names,
        vec![
            "api".to_owned(),
            "api-host".to_owned(),
            "backend".to_owned(),
            "visor-compose-api-1".to_owned(),
        ]
    );
    assert_eq!(
        config.service_ports,
        vec![
            visor_types::ServicePort::new(53, "udp"),
            visor_types::ServicePort::new(8080, "tcp"),
        ]
    );
}

#[test]
fn docker_create_to_vm_config_falls_back_to_compose_default_network_name() {
    let req = ContainerCreateRequest {
        image: "app".to_owned(),
        labels: Some(HashMap::from([
            ("com.docker.compose.project".to_owned(), "alpha".to_owned()),
            ("com.docker.compose.service".to_owned(), "api".to_owned()),
        ])),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, Some("alpha-api-1"), true).unwrap();

    assert_eq!(config.networks, vec!["alpha_default".to_owned()]);
}

#[test]
fn vm_info_to_list_entry_running() {
    let vm = VmInfo::new(
        "vm-123".to_owned(),
        "alpine:latest".to_owned(),
        VmState::Running,
        "2024-01-15T10:30:00Z".to_owned(),
        512,
        1,
    );

    let entry = vm_info_to_list_entry(&vm);
    assert_eq!(entry.id, "vm-123");
    assert_eq!(entry.names, vec!["/vm-123"]);
    assert_eq!(entry.image, "alpine:latest");
    assert_eq!(entry.state, "running");
    assert_eq!(entry.status, "Up");
}

#[test]
fn vm_info_to_list_entry_parses_created_at_to_unix_seconds() {
    let vm = VmInfo::new(
        "vm-created".to_owned(),
        "alpine:latest".to_owned(),
        VmState::Running,
        "2024-01-15T10:30:00Z".to_owned(),
        512,
        1,
    );

    let entry = vm_info_to_list_entry(&vm);
    assert_eq!(entry.created, 1_705_314_600);
}

#[test]
fn vm_info_to_list_entry_stopped_with_exit_code() {
    let mut vm = VmInfo::new(
        "vm-456".to_owned(),
        "busybox".to_owned(),
        VmState::Stopped,
        "2024-01-15T10:30:00Z".to_owned(),
        256,
        1,
    );
    vm.exit_code = Some(137);

    let entry = vm_info_to_list_entry(&vm);
    assert_eq!(entry.state, "exited");
    assert_eq!(entry.status, "Exited (137)");
}

#[test]
fn vm_info_to_list_entry_with_name() {
    let mut vm = VmInfo::new(
        "vm-789".to_owned(),
        "alpine".to_owned(),
        VmState::Running,
        String::new(),
        512,
        1,
    );
    vm.name = Some("mycontainer".to_owned());

    let entry = vm_info_to_list_entry(&vm);
    assert_eq!(entry.names, vec!["/mycontainer"]);
}

#[test]
fn vm_info_to_list_entry_ports_mapped() {
    let mut vm = VmInfo::new(
        "vm-port".to_owned(),
        "nginx".to_owned(),
        VmState::Running,
        String::new(),
        512,
        1,
    );
    vm.ports = vec![PortMapping::new(8080, 80)];

    let entry = vm_info_to_list_entry(&vm);
    assert_eq!(entry.ports.len(), 1);
    assert_eq!(entry.ports[0].private_port, 80);
    assert_eq!(entry.ports[0].public_port, Some(8080));
    assert_eq!(entry.ports[0].port_type, "tcp");
}

#[test]
fn vm_info_to_inspect_running() {
    let mut vm = VmInfo::new(
        "vm-inspect".to_owned(),
        "alpine:3.19".to_owned(),
        VmState::Running,
        "2024-06-01T12:00:00Z".to_owned(),
        512,
        2,
    );
    vm.name = Some("test-vm".to_owned());
    vm.ports = vec![PortMapping::new(8080, 80)];

    let resp = vm_info_to_inspect(&vm);
    assert_eq!(resp.id, "vm-inspect");
    assert_eq!(resp.name, "/test-vm");
    assert!(resp.state.running);
    assert_eq!(resp.state.status, "running");
    assert_eq!(resp.state.pid, 1);
    assert_eq!(resp.config.image, "alpine:3.19");
    // Port bindings should be present
    assert!(resp.host_config.port_bindings.contains_key("80/tcp"));
    // Network settings should have bridge entry
    assert!(resp.network_settings.networks.contains_key("bridge"));
}

#[test]
fn vm_info_to_inspect_stopped() {
    let mut vm = VmInfo::new(
        "vm-stopped".to_owned(),
        "ubuntu".to_owned(),
        VmState::Stopped,
        "2024-06-01T12:00:00Z".to_owned(),
        1024,
        4,
    );
    vm.exit_code = Some(0);

    let resp = vm_info_to_inspect(&vm);
    assert!(!resp.state.running);
    assert_eq!(resp.state.status, "exited");
    assert_eq!(resp.state.pid, 0);
    assert_eq!(resp.state.exit_code, 0);
}

#[test]
fn vm_info_to_inspect_stopped_preserves_named_network_membership() {
    let vm = VmInfo::new(
        "vm-stopped-network".to_owned(),
        "alpine".to_owned(),
        VmState::Stopped,
        String::new(),
        128,
        1,
    );
    let mut config = VmConfig::new("alpine");
    config.networks = vec!["sandbox-network".to_owned()];

    let resp = vm_info_to_inspect_with_config(&vm, &config);
    let network = resp
        .network_settings
        .networks
        .get("sandbox-network")
        .expect("stopped container should retain named-network membership");

    assert_eq!(network.network_i_d, "sandbox-network");
    assert!(network.ip_address.is_empty());
    assert!(network.gateway.is_empty());
}

#[test]
fn vm_info_to_inspect_failed() {
    let vm = VmInfo::new(
        "vm-failed".to_owned(),
        "app".to_owned(),
        VmState::Failed,
        String::new(),
        512,
        1,
    );

    let resp = vm_info_to_inspect(&vm);
    assert!(resp.state.dead);
    assert_eq!(resp.state.exit_code, 0); // no exit_code set, defaults to 0
}

#[test]
fn docker_exec_to_exec_request_maps_fields() {
    let req = ExecCreateRequest {
        cmd: vec!["cat".to_owned(), "/etc/os-release".to_owned()],
        env: Some(vec!["TERM=xterm".to_owned()]),
        working_dir: Some("/tmp".to_owned()),
        tty: Some(true),
        ..Default::default()
    };

    let exec = docker_exec_to_exec_request(&req);
    assert_eq!(exec.cmd, vec!["cat", "/etc/os-release"]);
    assert_eq!(exec.env, vec!["TERM=xterm"]);
    assert_eq!(exec.working_dir.as_deref(), Some("/tmp"));
    assert!(exec.tty);
}

#[test]
fn docker_exec_empty_working_dir_stays_none() {
    let req = ExecCreateRequest {
        cmd: vec!["echo".to_owned(), "hello".to_owned()],
        working_dir: Some(String::new()),
        ..Default::default()
    };

    let exec = docker_exec_to_exec_request(&req);

    assert!(exec.working_dir.is_none());
}

#[test]
fn exec_result_to_inspect_maps_exit_code() {
    let result = ExecResult::new(42, "output".to_owned(), String::new());
    let resp = exec_result_to_inspect("exec-1", "container-1", &result);
    assert_eq!(resp.id, "exec-1");
    assert!(!resp.running);
    assert_eq!(resp.exit_code, Some(42));
    assert_eq!(resp.container_i_d, "container-1");
}

#[test]
fn vm_info_to_wait_returns_exit_code() {
    let mut vm = VmInfo::new(
        "vm-wait".to_owned(),
        "app".to_owned(),
        VmState::Stopped,
        String::new(),
        512,
        1,
    );
    vm.exit_code = Some(2);

    let resp = vm_info_to_wait(&vm);
    assert_eq!(resp.status_code, 2);
}

#[test]
fn vm_info_to_wait_defaults_to_zero() {
    let vm = VmInfo::new(
        "vm-wait".to_owned(),
        "app".to_owned(),
        VmState::Stopped,
        String::new(),
        512,
        1,
    );

    let resp = vm_info_to_wait(&vm);
    assert_eq!(resp.status_code, 0);
}

#[test]
fn parse_bind_mount_rw() {
    let mount = parse_bind_mount("/host/data:/container/data").unwrap();
    assert_eq!(mount.host_path, "/host/data");
    assert_eq!(mount.guest_path, "/container/data");
    assert!(!mount.read_only);
}

#[test]
fn parse_bind_mount_ro() {
    let mount = parse_bind_mount("/host/data:/container/data:ro").unwrap();
    assert_eq!(mount.host_path, "/host/data");
    assert_eq!(mount.guest_path, "/container/data");
    assert!(mount.read_only);
}

#[test]
fn parse_bind_mount_rw_explicit() {
    let mount = parse_bind_mount("/a:/b:rw").unwrap();
    assert!(!mount.read_only);
}

#[test]
fn parse_bind_mount_invalid_single_segment() {
    assert!(parse_bind_mount("justpath").is_none());
}

#[test]
fn vm_info_to_inspect_running_has_health_healthy() {
    let vm = VmInfo::new(
        "vm-health".to_owned(),
        "alpine".to_owned(),
        VmState::Running,
        "2024-06-01T12:00:00Z".to_owned(),
        128,
        1,
    );

    let resp = vm_info_to_inspect(&vm);
    let health = resp.state.health.expect("running VM should have health");
    assert_eq!(health.status, "healthy");
    assert_eq!(health.failing_streak, 0);
}

#[test]
fn vm_info_to_inspect_stopped_no_health() {
    let mut vm = VmInfo::new(
        "vm-stopped".to_owned(),
        "alpine".to_owned(),
        VmState::Stopped,
        String::new(),
        128,
        1,
    );
    vm.exit_code = Some(0);

    let resp = vm_info_to_inspect(&vm);
    assert!(
        resp.state.health.is_none(),
        "stopped VM should have no health"
    );
}

#[test]
fn vm_info_to_inspect_creating_has_health_starting() {
    let vm = VmInfo::new(
        "vm-creating".to_owned(),
        "alpine".to_owned(),
        VmState::Creating,
        String::new(),
        128,
        1,
    );

    let resp = vm_info_to_inspect(&vm);
    let health = resp.state.health.expect("creating VM should have health");
    assert_eq!(health.status, "starting");
}

#[test]
fn docker_create_empty_working_dir_stays_none() {
    // Docker CLI sends WorkingDir as empty string when not specified.
    // This should NOT override VmConfig.working_dir (None = use image default).
    let req = ContainerCreateRequest {
        image: "alpine".to_owned(),
        working_dir: Some(String::new()),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert!(
        config.working_dir.is_none(),
        "empty working_dir should not override default (got {:?})",
        config.working_dir
    );
}

#[test]
fn docker_create_absent_working_dir_stays_none() {
    let req = ContainerCreateRequest {
        image: "alpine".to_owned(),
        working_dir: None,
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert!(config.working_dir.is_none());
}

#[test]
fn docker_create_memory_zero_uses_default() {
    // Docker sends Memory: 0 for "no limit" — should use visor default (512 MiB).
    let req = ContainerCreateRequest {
        image: "alpine".to_owned(),
        host_config: Some(HostConfig {
            memory: Some(0),
            ..Default::default()
        }),
        ..Default::default()
    };

    let config = docker_create_to_vm_config(&req, None, true).unwrap();
    assert_eq!(
        config.memory_mib, 512,
        "Memory: 0 should use default 512 MiB, not {}",
        config.memory_mib
    );
}
