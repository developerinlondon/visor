use std::fs;
use std::io::Write;

use super::*;
use crate::testutil::tempdir;

// ── ImageReference Parsing Tests ────────────────────────────────────────

#[test]
fn parse_simple_image() {
    let r = ImageReference::parse("myapp").unwrap();
    assert_eq!(r.registry, "docker.io");
    assert_eq!(r.repository, "library/myapp");
    assert_eq!(r.tag, "latest");
}

#[test]
fn parse_tagged_image() {
    let r = ImageReference::parse("myapp:v1").unwrap();
    assert_eq!(r.registry, "docker.io");
    assert_eq!(r.repository, "library/myapp");
    assert_eq!(r.tag, "v1");
}

#[test]
fn parse_user_image() {
    let r = ImageReference::parse("user/myapp:v1").unwrap();
    assert_eq!(r.registry, "docker.io");
    assert_eq!(r.repository, "user/myapp");
    assert_eq!(r.tag, "v1");
}

#[test]
fn parse_custom_registry() {
    let r = ImageReference::parse("ghcr.io/user/myapp:v1").unwrap();
    assert_eq!(r.registry, "ghcr.io");
    assert_eq!(r.repository, "user/myapp");
    assert_eq!(r.tag, "v1");
}

#[test]
fn parse_with_port() {
    let r = ImageReference::parse("localhost:5000/myapp:v1").unwrap();
    assert_eq!(r.registry, "localhost:5000");
    assert_eq!(r.repository, "myapp");
    assert_eq!(r.tag, "v1");
}

// ── Docker Config Auth Tests ────────────────────────────────────────────

#[test]
fn docker_config_basic_auth() {
    let tmp = tempdir().unwrap();
    let config_path = tmp.path().join("config.json");

    // base64("testuser:testpass") = "dGVzdHVzZXI6dGVzdHBhc3M="
    let config_json = r#"{
        "auths": {
            "https://index.docker.io/v1/": {
                "auth": "dGVzdHVzZXI6dGVzdHBhc3M="
            }
        }
    }"#;
    let mut f = fs::File::create(&config_path).unwrap();
    f.write_all(config_json.as_bytes()).unwrap();

    let auth = parse_docker_config_auth(&config_path, "docker.io").unwrap();
    match auth {
        RegistryAuth::Basic { username, password } => {
            assert_eq!(username, "testuser");
            assert_eq!(password, "testpass");
        }
        RegistryAuth::Anonymous => panic!("expected Basic auth, got Anonymous"),
    }
}

// ── RegistryAuth Construction Tests ─────────────────────────────────────

#[test]
fn anonymous_auth_created() {
    let pusher = RegistryPusher::new(RegistryAuth::Anonymous);
    // Verify the pusher was constructed — auth should be accessible.
    assert!(matches!(pusher.auth(), RegistryAuth::Anonymous));
}

// ── PushResult Field Tests ──────────────────────────────────────────────

#[test]
fn push_result_fields() {
    let result = PushResult {
        digest: "sha256:abc123".to_owned(),
        layers_pushed: 3,
        layers_skipped: 1,
        bytes_uploaded: 1024,
    };

    assert_eq!(result.digest, "sha256:abc123");
    assert_eq!(result.layers_pushed, 3);
    assert_eq!(result.layers_skipped, 1);
    assert_eq!(result.bytes_uploaded, 1024);
}

// ── ImageReference Edge Cases ───────────────────────────────────────────

#[test]
fn parse_empty_reference_fails() {
    assert!(ImageReference::parse("").is_err());
}

#[test]
fn parse_ecr_registry() {
    let r = ImageReference::parse("123456789.dkr.ecr.us-east-1.amazonaws.com/myapp:v2").unwrap();
    assert_eq!(r.registry, "123456789.dkr.ecr.us-east-1.amazonaws.com");
    assert_eq!(r.repository, "myapp");
    assert_eq!(r.tag, "v2");
}

#[test]
fn docker_config_missing_registry_returns_anonymous() {
    let tmp = tempdir().unwrap();
    let config_path = tmp.path().join("config.json");

    let config_json = r#"{
        "auths": {
            "ghcr.io": {
                "auth": "dGVzdHVzZXI6dGVzdHBhc3M="
            }
        }
    }"#;
    let mut f = fs::File::create(&config_path).unwrap();
    f.write_all(config_json.as_bytes()).unwrap();

    // Looking for docker.io but only ghcr.io is configured → Anonymous.
    let auth = parse_docker_config_auth(&config_path, "docker.io").unwrap();
    assert!(matches!(auth, RegistryAuth::Anonymous));
}
