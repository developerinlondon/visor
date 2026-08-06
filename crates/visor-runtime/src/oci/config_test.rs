use super::*;

#[test]
fn parse_full_config() {
    let json = br#"{
        "config": {
            "Cmd": ["/bin/sh", "-c", "echo hello"],
            "Entrypoint": ["/docker-entrypoint.sh"],
            "Env": ["PATH=/usr/local/bin:/usr/bin", "HOME=/root"],
            "WorkingDir": "/app",
            "User": "nobody",
            "ExposedPorts": {"8080/tcp": {}, "443/tcp": {}},
            "Labels": {"maintainer": "test@example.com", "version": "1.0"},
            "StopSignal": "SIGTERM"
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert_eq!(
        config.cmd.as_deref().unwrap(),
        &["/bin/sh", "-c", "echo hello"]
    );
    assert_eq!(
        config.entrypoint.as_deref().unwrap(),
        &["/docker-entrypoint.sh"]
    );
    assert_eq!(
        config.env,
        vec!["PATH=/usr/local/bin:/usr/bin", "HOME=/root"]
    );
    assert_eq!(config.working_dir.as_deref().unwrap(), "/app");
    assert_eq!(config.user.as_deref().unwrap(), "nobody");
    assert_eq!(config.exposed_ports, vec![443, 8080]);
    assert_eq!(config.labels.get("maintainer").unwrap(), "test@example.com");
    assert_eq!(config.labels.get("version").unwrap(), "1.0");
    assert_eq!(config.stop_signal.as_deref().unwrap(), "SIGTERM");
}

#[test]
fn parse_minimal_config_only_cmd() {
    let json = br#"{
        "config": {
            "Cmd": ["/bin/sh"]
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert_eq!(config.cmd.as_deref().unwrap(), &["/bin/sh"]);
    assert!(config.entrypoint.is_none());
    assert!(config.env.is_empty());
    assert!(config.working_dir.is_none());
    assert!(config.user.is_none());
    assert!(config.exposed_ports.is_empty());
    assert!(config.labels.is_empty());
    assert!(config.stop_signal.is_none());
}

#[test]
fn parse_config_entrypoint_only() {
    let json = br#"{
        "config": {
            "Entrypoint": ["/entrypoint.sh", "--verbose"]
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert!(config.cmd.is_none());
    assert_eq!(
        config.entrypoint.as_deref().unwrap(),
        &["/entrypoint.sh", "--verbose"]
    );
}

#[test]
fn parse_config_entrypoint_and_cmd() {
    let json = br#"{
        "config": {
            "Entrypoint": ["/entrypoint.sh"],
            "Cmd": ["--help"]
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert_eq!(config.entrypoint.as_deref().unwrap(), &["/entrypoint.sh"]);
    assert_eq!(config.cmd.as_deref().unwrap(), &["--help"]);
}

#[test]
fn parse_exposed_ports() {
    let json = br#"{
        "config": {
            "ExposedPorts": {"8080/tcp": {}, "443/tcp": {}, "53/udp": {}}
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    // Ports should be sorted
    assert_eq!(config.exposed_ports, vec![53, 443, 8080]);
}

#[test]
fn parse_environment_variables() {
    let json = br#"{
        "config": {
            "Env": [
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin",
                "LANG=C.UTF-8",
                "JAVA_HOME=/usr/lib/jvm/java-17"
            ]
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert_eq!(config.env.len(), 3);
    assert_eq!(
        config.env[0],
        "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
    );
    assert_eq!(config.env[1], "LANG=C.UTF-8");
    assert_eq!(config.env[2], "JAVA_HOME=/usr/lib/jvm/java-17");
}

#[test]
fn parse_labels() {
    let json = br#"{
        "config": {
            "Labels": {
                "org.opencontainers.image.title": "myapp",
                "org.opencontainers.image.version": "2.1.0",
                "maintainer": "dev@example.com"
            }
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert_eq!(config.labels.len(), 3);
    assert_eq!(
        config.labels.get("org.opencontainers.image.title").unwrap(),
        "myapp"
    );
    assert_eq!(
        config
            .labels
            .get("org.opencontainers.image.version")
            .unwrap(),
        "2.1.0"
    );
}

#[test]
fn effective_command_both_entrypoint_and_cmd() {
    let json = br#"{
        "config": {
            "Entrypoint": ["/entrypoint.sh"],
            "Cmd": ["--config", "/etc/app.conf"]
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();
    let cmd = config.effective_command();

    assert_eq!(cmd, vec!["/entrypoint.sh", "--config", "/etc/app.conf"]);
}

#[test]
fn effective_command_only_cmd() {
    let json = br#"{
        "config": {
            "Cmd": ["/bin/sh", "-c", "echo hello"]
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();
    let cmd = config.effective_command();

    assert_eq!(cmd, vec!["/bin/sh", "-c", "echo hello"]);
}

#[test]
fn effective_command_only_entrypoint() {
    let json = br#"{
        "config": {
            "Entrypoint": ["/usr/bin/myapp"]
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();
    let cmd = config.effective_command();

    assert_eq!(cmd, vec!["/usr/bin/myapp"]);
}

#[test]
fn effective_command_neither() {
    let json = br#"{
        "config": {}
    }"#;

    let config = ImageConfig::from_json(json).unwrap();
    let cmd = config.effective_command();

    assert!(cmd.is_empty());
}

#[test]
fn parse_real_world_alpine_config() {
    let json = br#"{
        "architecture": "amd64",
        "config": {
            "Hostname": "",
            "Domainname": "",
            "User": "",
            "AttachStdin": false,
            "AttachStdout": false,
            "AttachStderr": false,
            "Tty": false,
            "OpenStdin": false,
            "StdinOnce": false,
            "Env": [
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
            ],
            "Cmd": ["/bin/sh"],
            "Image": "sha256:abcdef1234567890",
            "Volumes": null,
            "WorkingDir": "",
            "Entrypoint": null,
            "OnBuild": null,
            "Labels": null
        },
        "container": "abc123",
        "created": "2024-01-01T00:00:00.000000000Z",
        "docker_version": "20.10.23",
        "history": [],
        "os": "linux",
        "rootfs": {
            "type": "layers",
            "diff_ids": ["sha256:def456"]
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert_eq!(config.cmd.as_deref().unwrap(), &["/bin/sh"]);
    assert!(config.entrypoint.is_none());
    assert_eq!(
        config.env,
        vec!["PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"]
    );
    // Empty string WorkingDir should become None
    assert!(config.working_dir.is_none());
    // Empty string User should become None
    assert!(config.user.is_none());
    assert!(config.exposed_ports.is_empty());
    assert!(config.labels.is_empty());
    assert!(config.stop_signal.is_none());
}

#[test]
fn parse_real_world_ubuntu_config() {
    let json = br#"{
        "architecture": "amd64",
        "config": {
            "Hostname": "",
            "Domainname": "",
            "User": "",
            "AttachStdin": false,
            "AttachStdout": false,
            "AttachStderr": false,
            "Tty": false,
            "OpenStdin": false,
            "StdinOnce": false,
            "Env": [
                "PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
            ],
            "Cmd": ["/bin/bash"],
            "Image": "sha256:1234567890abcdef",
            "Volumes": null,
            "WorkingDir": "",
            "Entrypoint": null,
            "OnBuild": null,
            "Labels": {
                "org.opencontainers.image.ref.name": "ubuntu",
                "org.opencontainers.image.version": "24.04"
            }
        },
        "container": "def456",
        "created": "2024-06-01T00:00:00.000000000Z",
        "docker_version": "20.10.23",
        "history": [],
        "os": "linux",
        "rootfs": {
            "type": "layers",
            "diff_ids": ["sha256:abc789"]
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert_eq!(config.cmd.as_deref().unwrap(), &["/bin/bash"]);
    assert!(config.entrypoint.is_none());
    assert_eq!(config.labels.len(), 2);
    assert_eq!(
        config
            .labels
            .get("org.opencontainers.image.ref.name")
            .unwrap(),
        "ubuntu"
    );
    assert_eq!(
        config
            .labels
            .get("org.opencontainers.image.version")
            .unwrap(),
        "24.04"
    );
}

#[test]
fn error_invalid_json() {
    let json = b"not valid json {{{";
    let result = ImageConfig::from_json(json);

    assert!(result.is_err());
    let err = format!("{:#}", result.unwrap_err());
    assert!(
        err.contains("parse OCI image config"),
        "error should mention parsing: {err}"
    );
}

#[test]
fn error_missing_config_key() {
    let json = br#"{"architecture": "amd64"}"#;
    let result = ImageConfig::from_json(json);

    assert!(result.is_err());
    let err = format!("{:#}", result.unwrap_err());
    assert!(
        err.contains("config"),
        "error should mention config key: {err}"
    );
}

#[test]
fn handle_empty_config_object() {
    let json = br#"{"config": {}}"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert!(config.cmd.is_none());
    assert!(config.entrypoint.is_none());
    assert!(config.env.is_empty());
    assert!(config.working_dir.is_none());
    assert!(config.user.is_none());
    assert!(config.exposed_ports.is_empty());
    assert!(config.labels.is_empty());
    assert!(config.stop_signal.is_none());
}

#[test]
fn exposed_ports_ignores_invalid_port_format() {
    let json = br#"{
        "config": {
            "ExposedPorts": {"8080/tcp": {}, "invalid": {}, "notanumber/tcp": {}}
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    // Only valid port should be parsed
    assert_eq!(config.exposed_ports, vec![8080]);
}

#[test]
fn parse_null_fields_gracefully() {
    let json = br#"{
        "config": {
            "Cmd": null,
            "Entrypoint": null,
            "Env": null,
            "WorkingDir": null,
            "User": null,
            "ExposedPorts": null,
            "Labels": null,
            "StopSignal": null
        }
    }"#;

    let config = ImageConfig::from_json(json).unwrap();

    assert!(config.cmd.is_none());
    assert!(config.entrypoint.is_none());
    assert!(config.env.is_empty());
    assert!(config.working_dir.is_none());
    assert!(config.user.is_none());
    assert!(config.exposed_ports.is_empty());
    assert!(config.labels.is_empty());
    assert!(config.stop_signal.is_none());
}

#[test]
fn config_is_clone_and_debug() {
    let json = br#"{"config": {"Cmd": ["/bin/sh"]}}"#;
    let config = ImageConfig::from_json(json).unwrap();

    let cloned = config.clone();
    assert_eq!(cloned.cmd, config.cmd);

    let debug = format!("{config:?}");
    assert!(debug.contains("ImageConfig"));
}
