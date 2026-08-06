use std::collections::HashMap;
use std::io::Write;

use super::*;
use crate::compose::types::*;

/// Creates a mock variable lookup from a `HashMap`.
fn mock_vars(vars: HashMap<&str, &str>) -> impl Fn(&str) -> Result<String, std::env::VarError> {
    let owned: HashMap<String, String> = vars
        .into_iter()
        .map(|(k, v)| (k.to_owned(), v.to_owned()))
        .collect();

    move |name: &str| {
        owned
            .get(name)
            .cloned()
            .ok_or(std::env::VarError::NotPresent)
    }
}

/// Creates a mock lookup that always returns "not present".
fn empty_vars() -> impl Fn(&str) -> Result<String, std::env::VarError> {
    |_: &str| Err(std::env::VarError::NotPresent)
}

#[test]
fn parse_minimal_compose() {
    let yaml = r"
services:
  web:
    image: nginx:latest
";

    let project = parse_compose(yaml).unwrap();
    assert_eq!(project.services.len(), 1);
    assert_eq!(project.services["web"].image, "nginx:latest");
}

#[test]
fn parse_full_compose() {
    let yaml = r#"
name: myapp
services:
  web:
    image: nginx:latest
    ports:
      - "8080:80"
    environment:
      NGINX_HOST: example.com
    volumes:
      - "data:/usr/share/nginx/html"
    depends_on:
      - api
    networks:
      - frontend
  api:
    image: myapi:v1
    ports:
      - target: 3000
        published: 3000
    environment:
      - DATABASE_URL=postgres://db:5432/app
    depends_on:
      db:
        condition: service_healthy
    networks:
      - frontend
      - backend
  db:
    image: postgres:16
    environment:
      POSTGRES_PASSWORD: secret
    volumes:
      - "pgdata:/var/lib/postgresql/data"
    networks:
      - backend
networks:
  frontend: {}
  backend:
    driver: bridge
    ipam:
      config:
        - subnet: 172.28.0.0/16
volumes:
  data: {}
  pgdata:
    driver: local
"#;

    let project = parse_compose(yaml).unwrap();

    assert_eq!(project.name.as_deref(), Some("myapp"));
    assert_eq!(project.services.len(), 3);
    assert_eq!(project.networks.len(), 2);
    assert_eq!(project.volumes.len(), 2);

    // Check web service.
    let web = &project.services["web"];
    assert_eq!(web.image, "nginx:latest");
    assert_eq!(web.ports.len(), 1);
    assert_eq!(web.networks, vec!["frontend"]);

    // Check api service.
    let api = &project.services["api"];
    assert_eq!(api.image, "myapi:v1");

    // Check db service.
    let db = &project.services["db"];
    assert_eq!(db.image, "postgres:16");

    // Check backend network has IPAM config.
    let backend = &project.networks["backend"];
    assert_eq!(backend.driver.as_deref(), Some("bridge"));
    let ipam = backend.ipam.as_ref().unwrap();
    assert_eq!(ipam.config[0].subnet.as_deref(), Some("172.28.0.0/16"));

    // Check pgdata volume has driver.
    let pgdata = &project.volumes["pgdata"];
    assert_eq!(pgdata.driver.as_deref(), Some("local"));
}

#[test]
fn parse_variable_interpolation() {
    let vars = mock_vars(HashMap::from([("VISOR_TEST_IMAGE", "myimage:v2")]));

    let yaml = r#"
services:
  web:
    image: "${VISOR_TEST_IMAGE}"
"#;

    let project = parse_compose_with_vars(yaml, vars).unwrap();
    assert_eq!(project.services["web"].image, "myimage:v2");
}

#[test]
fn parse_variable_interpolation_unset() {
    let yaml = r#"
services:
  web:
    image: "${VISOR_TEST_UNSET_VAR:-nginx:latest}"
"#;

    let project = parse_compose_with_vars(yaml, empty_vars()).unwrap();
    assert_eq!(project.services["web"].image, "nginx:latest");
}

#[test]
fn parse_invalid_yaml() {
    let yaml = "services: [this is not valid: yaml: {{{}}}";

    let result = parse_compose(yaml);
    assert!(result.is_err());
}

#[test]
fn parse_unknown_version() {
    let yaml = r#"
version: "99.99"
services:
  web:
    image: nginx:latest
"#;

    // version field is informational — should parse anyway.
    let project = parse_compose(yaml).unwrap();
    assert_eq!(project.services["web"].image, "nginx:latest");
}

#[test]
fn parse_depends_on_simple() {
    let yaml = r"
services:
  web:
    image: nginx:latest
    depends_on:
      - db
  db:
    image: postgres:16
";

    let project = parse_compose(yaml).unwrap();
    match &project.services["web"].depends_on {
        ComposeDependsOn::Simple(deps) => {
            assert_eq!(deps, &["db"]);
        }
        other => panic!("expected Simple, got {other:?}"),
    }
}

#[test]
fn parse_depends_on_extended() {
    let yaml = r"
services:
  web:
    image: nginx:latest
    depends_on:
      db:
        condition: service_healthy
  db:
    image: postgres:16
";

    let project = parse_compose(yaml).unwrap();
    match &project.services["web"].depends_on {
        ComposeDependsOn::Extended(map) => {
            assert_eq!(
                map.get("db").unwrap().condition.as_deref(),
                Some("service_healthy")
            );
        }
        other => panic!("expected Extended, got {other:?}"),
    }
}

#[test]
fn parse_ports_short_syntax() {
    let yaml = r#"
services:
  web:
    image: nginx:latest
    ports:
      - "8080:80"
      - "443:443"
"#;

    let project = parse_compose(yaml).unwrap();
    let ports = &project.services["web"].ports;
    assert_eq!(ports.len(), 2);

    match &ports[0] {
        ComposePort::Short(s) => assert_eq!(s, "8080:80"),
        other => panic!("expected Short, got {other:?}"),
    }
    match &ports[1] {
        ComposePort::Short(s) => assert_eq!(s, "443:443"),
        other => panic!("expected Short, got {other:?}"),
    }
}

#[test]
fn parse_ports_long_syntax() {
    let yaml = r"
services:
  web:
    image: nginx:latest
    ports:
      - target: 80
        published: 8080
        protocol: tcp
";

    let project = parse_compose(yaml).unwrap();
    let ports = &project.services["web"].ports;
    assert_eq!(ports.len(), 1);

    match &ports[0] {
        ComposePort::Long {
            target,
            published,
            protocol,
        } => {
            assert_eq!(*target, 80);
            assert_eq!(*published, Some(8080));
            assert_eq!(protocol.as_deref(), Some("tcp"));
        }
        other => panic!("expected Long, got {other:?}"),
    }
}

#[test]
fn parse_environment_list() {
    let yaml = r"
services:
  web:
    image: nginx:latest
    environment:
      - KEY=VALUE
      - FOO=BAR
";

    let project = parse_compose(yaml).unwrap();
    match &project.services["web"].environment {
        ComposeEnvironment::List(items) => {
            assert_eq!(items.len(), 2);
            assert_eq!(items[0], "KEY=VALUE");
            assert_eq!(items[1], "FOO=BAR");
        }
        other => panic!("expected List, got {other:?}"),
    }
}

#[test]
fn parse_environment_map() {
    let yaml = r"
services:
  web:
    image: nginx:latest
    environment:
      KEY: VALUE
      FOO: BAR
";

    let project = parse_compose(yaml).unwrap();
    match &project.services["web"].environment {
        ComposeEnvironment::Map(map) => {
            assert_eq!(map.get("KEY").unwrap(), "VALUE");
            assert_eq!(map.get("FOO").unwrap(), "BAR");
        }
        other => panic!("expected Map, got {other:?}"),
    }
}

#[test]
fn parse_networks_custom() {
    let yaml = r"
services:
  web:
    image: nginx:latest
    networks:
      - mynet
networks:
  mynet:
    driver: bridge
    ipam:
      driver: default
      config:
        - subnet: 10.5.0.0/16
          gateway: 10.5.0.1
";

    let project = parse_compose(yaml).unwrap();
    let net = &project.networks["mynet"];
    assert_eq!(net.driver.as_deref(), Some("bridge"));

    let ipam = net.ipam.as_ref().unwrap();
    assert_eq!(ipam.driver.as_deref(), Some("default"));
    assert_eq!(ipam.config[0].subnet.as_deref(), Some("10.5.0.0/16"));
    assert_eq!(ipam.config[0].gateway.as_deref(), Some("10.5.0.1"));
}

#[test]
fn parse_compose_from_file() {
    let yaml = r#"
services:
  web:
    image: nginx:latest
    ports:
      - "80:80"
"#;

    let mut tmpfile = crate::testutil::named_temp_file("visor-runtime-compose-").unwrap();
    tmpfile.write_all(yaml.as_bytes()).unwrap();
    tmpfile.flush().unwrap();

    let project = parse_compose_file(tmpfile.path()).unwrap();
    assert_eq!(project.services.len(), 1);
    assert_eq!(project.services["web"].image, "nginx:latest");
}

#[test]
fn parse_multiple_services() {
    let yaml = r#"
services:
  frontend:
    image: node:20
    depends_on:
      - api
    ports:
      - "3000:3000"
  api:
    image: rust:1.85
    depends_on:
      - db
      - cache
    ports:
      - "8080:8080"
  db:
    image: postgres:16
    environment:
      POSTGRES_PASSWORD: secret
    ports:
      - "5432:5432"
  cache:
    image: redis:7
    ports:
      - "6379:6379"
"#;

    let project = parse_compose(yaml).unwrap();
    assert_eq!(project.services.len(), 4);
    assert!(project.services.contains_key("frontend"));
    assert!(project.services.contains_key("api"));
    assert!(project.services.contains_key("db"));
    assert!(project.services.contains_key("cache"));

    // Verify dependency chain.
    match &project.services["frontend"].depends_on {
        ComposeDependsOn::Simple(deps) => assert_eq!(deps, &["api"]),
        other => panic!("expected Simple, got {other:?}"),
    }
    match &project.services["api"].depends_on {
        ComposeDependsOn::Simple(deps) => {
            assert!(deps.contains(&"db".to_owned()));
            assert!(deps.contains(&"cache".to_owned()));
        }
        other => panic!("expected Simple, got {other:?}"),
    }
}

#[test]
fn parse_variable_interpolation_with_dash_default() {
    // ${VAR-default} syntax (without colon — only uses default if var is unset).
    let yaml = r#"
services:
  web:
    image: "${VISOR_TEST_DASH_VAR-fallback:v1}"
"#;

    let project = parse_compose_with_vars(yaml, empty_vars()).unwrap();
    assert_eq!(project.services["web"].image, "fallback:v1");
}

#[test]
fn parse_compose_file_missing_file() {
    let result = parse_compose_file(std::path::Path::new("/nonexistent/compose.yml"));
    assert!(result.is_err());
}

#[test]
fn parse_service_with_all_optional_fields() {
    let yaml = r#"
services:
  app:
    image: myapp:latest
    command: ["python", "app.py"]
    hostname: myhost
    working_dir: /app
    mem_limit: 512m
    cpus: 1.5
    labels:
      com.example.env: production
      com.example.tier: frontend
"#;

    let project = parse_compose(yaml).unwrap();
    let app = &project.services["app"];
    assert_eq!(app.image, "myapp:latest");
    assert_eq!(
        app.command.as_deref().unwrap(),
        &["python".to_owned(), "app.py".to_owned()]
    );
    assert_eq!(app.hostname.as_deref(), Some("myhost"));
    assert_eq!(app.working_dir.as_deref(), Some("/app"));
    assert_eq!(app.mem_limit.as_deref(), Some("512m"));
    assert!((app.cpus.unwrap() - 1.5).abs() < f64::EPSILON);
    assert_eq!(app.labels.get("com.example.env").unwrap(), "production");
    assert_eq!(app.labels.get("com.example.tier").unwrap(), "frontend");
}
