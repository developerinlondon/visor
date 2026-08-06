use std::collections::HashMap;

use serde_json::{Value, json};

use super::*;

#[test]
fn container_create_request_deserializes_from_docker_json() {
    let input = json!({
        "Image": "alpine:latest",
        "Cmd": ["echo", "hello"],
        "Env": ["FOO=bar"],
        "WorkingDir": "/app",
        "Tty": true,
        "HostConfig": {
            "PortBindings": {
                "80/tcp": [{"HostPort": "8080"}]
            },
            "Binds": ["/host:/guest:ro"],
            "Memory": 536870912_u64
        }
    });

    let req: ContainerCreateRequest = serde_json::from_value(input).unwrap();
    assert_eq!(req.image, "alpine:latest");
    assert_eq!(req.cmd.as_ref().unwrap(), &["echo", "hello"]);
    assert_eq!(req.env.as_ref().unwrap(), &["FOO=bar"]);
    assert_eq!(req.working_dir.as_deref(), Some("/app"));
    assert_eq!(req.tty, Some(true));

    let hc = req.host_config.unwrap();
    let bindings = hc.port_bindings.unwrap();
    assert_eq!(bindings["80/tcp"][0].host_port.as_deref(), Some("8080"));
    assert_eq!(hc.binds.unwrap(), vec!["/host:/guest:ro"]);
    assert_eq!(hc.memory, Some(536_870_912));
}

#[test]
fn container_create_request_defaults_on_empty_json() {
    let req: ContainerCreateRequest = serde_json::from_str("{}").unwrap();
    assert_eq!(req.image, "");
    assert!(req.cmd.is_none());
    assert!(req.env.is_none());
    assert!(req.host_config.is_none());
}

#[test]
fn container_create_response_serializes_with_pascal_case() {
    let resp = ContainerCreateResponse {
        id: "abc123".to_owned(),
        warnings: vec!["some warning".to_owned()],
    };

    let json: Value = serde_json::to_value(&resp).unwrap();
    // Docker uses "Id" (PascalCase)
    assert_eq!(json["Id"], "abc123");
    assert_eq!(json["Warnings"][0], "some warning");
    // Ensure no lowercase "id" key
    assert!(json.get("id").is_none());
}

#[test]
fn port_binding_serde_round_trip() {
    let binding = PortBinding {
        host_ip: Some("0.0.0.0".to_owned()),
        host_port: Some("8080".to_owned()),
    };

    let json_str = serde_json::to_string(&binding).unwrap();
    let parsed: PortBinding = serde_json::from_str(&json_str).unwrap();
    assert_eq!(parsed.host_ip.as_deref(), Some("0.0.0.0"));
    assert_eq!(parsed.host_port.as_deref(), Some("8080"));
}

#[test]
fn exec_create_request_deserializes() {
    let input = json!({
        "Cmd": ["ls", "-la"],
        "Env": ["DEBUG=1"],
        "WorkingDir": "/tmp",
        "AttachStdin": true,
        "AttachStdout": true,
        "AttachStderr": false,
        "Tty": true,
        "Detach": false
    });

    let req: ExecCreateRequest = serde_json::from_value(input).unwrap();
    assert_eq!(req.cmd, vec!["ls", "-la"]);
    assert_eq!(req.env.as_ref().unwrap(), &["DEBUG=1"]);
    assert_eq!(req.working_dir.as_deref(), Some("/tmp"));
    assert_eq!(req.attach_stdin, Some(true));
    assert_eq!(req.tty, Some(true));
    assert_eq!(req.detach, Some(false));
}

#[test]
fn exec_create_request_default_has_attach_stdout() {
    let req = ExecCreateRequest::default();
    assert!(req.cmd.is_empty());
    assert_eq!(req.attach_stdin, Some(false));
    assert_eq!(req.attach_stdout, Some(true));
    assert_eq!(req.attach_stderr, Some(true));
}

#[test]
fn container_inspect_response_serializes_special_fields() {
    let resp = ContainerInspectResponse {
        id: "abc".to_owned(),
        name: "/mycontainer".to_owned(),
        created: "2024-01-01T00:00:00Z".to_owned(),
        state: ContainerState {
            status: "running".to_owned(),
            running: true,
            paused: false,
            restarting: false,
            oom_killed: false,
            dead: false,
            pid: 42,
            exit_code: 0,
            error: String::new(),
            started_at: "2024-01-01T00:00:00Z".to_owned(),
            finished_at: String::new(),
            health: None,
        },
        config: ContainerConfig {
            image: "alpine".to_owned(),
            cmd: None,
            env: None,
            working_dir: String::new(),
            labels: HashMap::new(),
        },
        host_config: HostConfigResponse {
            port_bindings: HashMap::new(),
            binds: Vec::new(),
        },
        network_settings: NetworkSettings {
            networks: HashMap::new(),
        },
        mounts: vec![MountPoint {
            mount_type: "bind".to_owned(),
            source: "/host".to_owned(),
            destination: "/guest".to_owned(),
            rw: true,
        }],
    };

    let json: Value = serde_json::to_value(&resp).unwrap();
    // OOMKilled uses explicit rename
    assert_eq!(json["State"]["OOMKilled"], false);
    // Mounts use Type (explicit rename) and RW (explicit rename)
    assert_eq!(json["Mounts"][0]["Type"], "bind");
    assert_eq!(json["Mounts"][0]["RW"], true);
}

#[test]
fn container_list_entry_serializes_image_id() {
    let entry = ContainerListEntry {
        id: "abc".to_owned(),
        names: vec!["/test".to_owned()],
        image: "alpine".to_owned(),
        image_i_d: "sha256:abc123".to_owned(),
        command: "echo hello".to_owned(),
        created: 1_704_067_200,
        state: "running".to_owned(),
        status: "Up 5 minutes".to_owned(),
        ports: Vec::new(),
        labels: HashMap::new(),
    };

    let json: Value = serde_json::to_value(&entry).unwrap();
    // PascalCase transformation should produce "ImageID"
    assert!(json.get("ImageID").is_some() || json.get("ImageId").is_some());
}

#[test]
fn exec_inspect_response_uses_explicit_id_rename() {
    let resp = ExecInspectResponse {
        id: "exec123".to_owned(),
        running: false,
        exit_code: Some(0),
        container_i_d: "container456".to_owned(),
    };

    let json: Value = serde_json::to_value(&resp).unwrap();
    // ExecInspectResponse.id has #[serde(rename = "ID")]
    assert_eq!(json["ID"], "exec123");
}

#[test]
fn volume_list_response_serializes() {
    let resp = VolumeListResponse {
        volumes: vec![VolumeEntry {
            name: "my-vol".to_owned(),
            driver: "local".to_owned(),
            mountpoint: "/var/lib/docker/volumes/my-vol".to_owned(),
            labels: HashMap::new(),
            scope: "local".to_owned(),
        }],
        warnings: Vec::new(),
    };

    let json: Value = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["Volumes"][0]["Name"], "my-vol");
    assert_eq!(json["Volumes"][0]["Driver"], "local");
}

#[test]
fn docker_error_serializes_lowercase_message() {
    let err = DockerError {
        message: "something went wrong".to_owned(),
    };

    let json: Value = serde_json::to_value(&err).unwrap();
    // DockerError does NOT have rename_all — message stays lowercase
    assert_eq!(json["message"], "something went wrong");
}

#[test]
fn network_create_response_serializes() {
    let resp = NetworkCreateResponse {
        id: "net123".to_owned(),
        warning: String::new(),
    };

    let json: Value = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["Id"], "net123");
    assert_eq!(json["Warning"], "");
}

#[test]
fn container_port_type_uses_explicit_rename() {
    let port = ContainerPort {
        private_port: 80,
        public_port: Some(8080),
        port_type: "tcp".to_owned(),
    };

    let json: Value = serde_json::to_value(&port).unwrap();
    // port_type has #[serde(rename = "Type")]
    assert_eq!(json["Type"], "tcp");
    assert_eq!(json["PrivatePort"], 80);
    assert_eq!(json["PublicPort"], 8080);
}

#[test]
fn health_state_serializes_to_docker_json() {
    let health = HealthState {
        status: "healthy".to_owned(),
        failing_streak: 0,
        log: Vec::new(),
    };

    let json: Value = serde_json::to_value(&health).unwrap();
    assert_eq!(json["Status"], "healthy");
    assert_eq!(json["FailingStreak"], 0);
    assert!(json["Log"].as_array().unwrap().is_empty());
}

#[test]
fn health_state_with_log_entries() {
    let health = HealthState {
        status: "unhealthy".to_owned(),
        failing_streak: 3,
        log: vec![HealthLogEntry {
            start: "2024-06-01T12:00:00Z".to_owned(),
            end: "2024-06-01T12:00:02Z".to_owned(),
            exit_code: 1,
            output: "connection refused".to_owned(),
        }],
    };

    let json: Value = serde_json::to_value(&health).unwrap();
    assert_eq!(json["Status"], "unhealthy");
    assert_eq!(json["FailingStreak"], 3);
    assert_eq!(json["Log"][0]["ExitCode"], 1);
    assert_eq!(json["Log"][0]["Output"], "connection refused");
}

#[test]
fn container_state_health_field_omitted_when_none() {
    let state = ContainerState {
        status: "running".to_owned(),
        running: true,
        paused: false,
        restarting: false,
        oom_killed: false,
        dead: false,
        pid: 1,
        exit_code: 0,
        error: String::new(),
        started_at: String::new(),
        finished_at: String::new(),
        health: None,
    };

    let json: Value = serde_json::to_value(&state).unwrap();
    assert!(json.get("Health").is_none());
}

#[test]
fn container_state_health_field_present_when_some() {
    let state = ContainerState {
        status: "running".to_owned(),
        running: true,
        paused: false,
        restarting: false,
        oom_killed: false,
        dead: false,
        pid: 1,
        exit_code: 0,
        error: String::new(),
        started_at: String::new(),
        finished_at: String::new(),
        health: Some(HealthState {
            status: "healthy".to_owned(),
            failing_streak: 0,
            log: Vec::new(),
        }),
    };

    let json: Value = serde_json::to_value(&state).unwrap();
    assert_eq!(json["Health"]["Status"], "healthy");
}
