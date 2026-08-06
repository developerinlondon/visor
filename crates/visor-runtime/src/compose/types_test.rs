use std::collections::HashMap;

use super::*;

#[test]
fn compose_service_defaults() {
    let svc: ComposeService = serde_yaml::from_str(
        r#"
image: "nginx:latest"
"#,
    )
    .unwrap();

    assert_eq!(svc.image, "nginx:latest");
    assert!(svc.command.is_none());
    assert!(svc.ports.is_empty());
    assert!(svc.volumes.is_empty());
    assert!(svc.networks.is_empty());
    assert!(svc.mem_limit.is_none());
    assert!(svc.cpus.is_none());
    assert!(svc.hostname.is_none());
    assert!(svc.working_dir.is_none());
    assert!(svc.labels.is_empty());
    assert!(matches!(svc.environment, ComposeEnvironment::Empty));
    assert!(matches!(svc.depends_on, ComposeDependsOn::Empty));
}

#[test]
fn compose_project_validate_missing_image() {
    let project = ComposeProject {
        name: None,
        services: HashMap::from([(
            "web".to_owned(),
            ComposeService {
                image: String::new(),
                command: None,
                environment: ComposeEnvironment::Empty,
                ports: Vec::new(),
                volumes: Vec::new(),
                depends_on: ComposeDependsOn::Empty,
                networks: Vec::new(),
                mem_limit: None,
                cpus: None,
                hostname: None,
                working_dir: None,
                labels: HashMap::new(),
            },
        )]),
        networks: HashMap::new(),
        volumes: HashMap::new(),
    };

    let result = project.validate();
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("image"), "expected 'image' in error: {msg}");
    assert!(msg.contains("web"), "expected 'web' in error: {msg}");
}

#[test]
fn compose_project_validate_depends_on_target_exists() {
    let project = ComposeProject {
        name: None,
        services: HashMap::from([(
            "web".to_owned(),
            ComposeService {
                image: "nginx:latest".to_owned(),
                command: None,
                environment: ComposeEnvironment::Empty,
                ports: Vec::new(),
                volumes: Vec::new(),
                depends_on: ComposeDependsOn::Simple(vec!["db".to_owned()]),
                networks: Vec::new(),
                mem_limit: None,
                cpus: None,
                hostname: None,
                working_dir: None,
                labels: HashMap::new(),
            },
        )]),
        networks: HashMap::new(),
        volumes: HashMap::new(),
    };

    let result = project.validate();
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("db"), "expected 'db' in error: {msg}");
}

#[test]
fn compose_network_defaults() {
    let net: ComposeNetwork = serde_yaml::from_str("{}").unwrap();

    assert!(net.driver.is_none());
    assert!(net.ipam.is_none());
    assert!(!net.external);
}

#[test]
fn compose_volume_named() {
    let vol: ComposeVolumeConfig = serde_yaml::from_str("{}").unwrap();

    assert!(vol.driver.is_none());
    assert!(!vol.external);
}

#[test]
fn compose_environment_list_round_trip() {
    let env: ComposeEnvironment = serde_yaml::from_str("[\"KEY=VALUE\", \"FOO=BAR\"]").unwrap();

    match env {
        ComposeEnvironment::List(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0], "KEY=VALUE");
            assert_eq!(items[1], "FOO=BAR");
        }
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn compose_environment_map_round_trip() {
    let env: ComposeEnvironment = serde_yaml::from_str(
        r"
KEY: VALUE
FOO: BAR
",
    )
    .unwrap();

    match env {
        ComposeEnvironment::Map(map) => {
            assert_eq!(map.get("KEY").unwrap(), "VALUE");
            assert_eq!(map.get("FOO").unwrap(), "BAR");
        }
        other => panic!("expected Map, got {other:?}"),
    }
}

#[test]
fn compose_depends_on_simple_round_trip() {
    let dep: ComposeDependsOn = serde_yaml::from_str("[\"db\", \"redis\"]").unwrap();

    match dep {
        ComposeDependsOn::Simple(items) => {
            assert_eq!(items, vec!["db", "redis"]);
        }
        other => panic!("expected Simple, got {other:?}"),
    }
}

#[test]
fn compose_depends_on_extended_round_trip() {
    let dep: ComposeDependsOn = serde_yaml::from_str(
        r"
db:
  condition: service_healthy
redis:
  condition: service_started
",
    )
    .unwrap();

    match dep {
        ComposeDependsOn::Extended(map) => {
            assert_eq!(map.len(), 2);
            assert_eq!(
                map.get("db").unwrap().condition.as_deref(),
                Some("service_healthy")
            );
            assert_eq!(
                map.get("redis").unwrap().condition.as_deref(),
                Some("service_started")
            );
        }
        other => panic!("expected Extended, got {other:?}"),
    }
}

#[test]
fn compose_port_short_round_trip() {
    let port: ComposePort = serde_yaml::from_str("\"8080:80\"").unwrap();

    match port {
        ComposePort::Short(s) => assert_eq!(s, "8080:80"),
        other => panic!("expected Short, got {other:?}"),
    }
}

#[test]
fn compose_port_long_round_trip() {
    let port: ComposePort = serde_yaml::from_str(
        r"
target: 80
published: 8080
protocol: tcp
",
    )
    .unwrap();

    match port {
        ComposePort::Long {
            target,
            published,
            protocol,
        } => {
            assert_eq!(target, 80);
            assert_eq!(published, Some(8080));
            assert_eq!(protocol.as_deref(), Some("tcp"));
        }
        other => panic!("expected Long, got {other:?}"),
    }
}

#[test]
fn compose_project_validate_valid_project() {
    let project = ComposeProject {
        name: Some("myapp".to_owned()),
        services: HashMap::from([
            (
                "web".to_owned(),
                ComposeService {
                    image: "nginx:latest".to_owned(),
                    command: None,
                    environment: ComposeEnvironment::Empty,
                    ports: Vec::new(),
                    volumes: Vec::new(),
                    depends_on: ComposeDependsOn::Simple(vec!["db".to_owned()]),
                    networks: Vec::new(),
                    mem_limit: None,
                    cpus: None,
                    hostname: None,
                    working_dir: None,
                    labels: HashMap::new(),
                },
            ),
            (
                "db".to_owned(),
                ComposeService {
                    image: "postgres:16".to_owned(),
                    command: None,
                    environment: ComposeEnvironment::Empty,
                    ports: Vec::new(),
                    volumes: Vec::new(),
                    depends_on: ComposeDependsOn::Empty,
                    networks: Vec::new(),
                    mem_limit: None,
                    cpus: None,
                    hostname: None,
                    working_dir: None,
                    labels: HashMap::new(),
                },
            ),
        ]),
        networks: HashMap::new(),
        volumes: HashMap::new(),
    };

    assert!(project.validate().is_ok());
}
