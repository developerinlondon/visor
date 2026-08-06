use super::*;

#[test]
fn parse_bare_name_defaults_to_docker_hub_library() {
    let r = ImageReference::parse("alpine").unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "library/alpine");
    assert_eq!(r.tag().unwrap().as_ref(), "latest");
    assert!(r.digest().is_none());
}

#[test]
fn parse_name_with_tag() {
    let r = ImageReference::parse("alpine:3.20").unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "library/alpine");
    assert_eq!(r.tag().unwrap().as_ref(), "3.20");
    assert!(r.digest().is_none());
}

#[test]
fn parse_numeric_tag() {
    let r = ImageReference::parse("ubuntu:22.04").unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "library/ubuntu");
    assert_eq!(r.tag().unwrap().as_ref(), "22.04");
}

#[test]
fn parse_user_repo_with_tag() {
    let r = ImageReference::parse("myuser/myapp:v1").unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "myuser/myapp");
    assert_eq!(r.tag().unwrap().as_ref(), "v1");
    assert!(r.digest().is_none());
}

#[test]
fn parse_user_repo_no_tag_defaults_latest() {
    let r = ImageReference::parse("myuser/myapp").unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "myuser/myapp");
    assert_eq!(r.tag().unwrap().as_ref(), "latest");
}

#[test]
fn parse_full_registry_reference() {
    let r = ImageReference::parse("ghcr.io/owner/repo:tag").unwrap();
    assert_eq!(r.registry().as_ref(), "ghcr.io");
    assert_eq!(r.repository().as_ref(), "owner/repo");
    assert_eq!(r.tag().unwrap().as_ref(), "tag");
    assert!(r.digest().is_none());
}

#[test]
fn parse_registry_with_port() {
    let r = ImageReference::parse("registry.example.com:5000/repo:tag").unwrap();
    assert_eq!(r.registry().as_ref(), "registry.example.com:5000");
    assert_eq!(r.repository().as_ref(), "repo");
    assert_eq!(r.tag().unwrap().as_ref(), "tag");
}

#[test]
fn parse_registry_with_port_nested_repo() {
    let r = ImageReference::parse("registry.example.com:5000/org/repo:v2").unwrap();
    assert_eq!(r.registry().as_ref(), "registry.example.com:5000");
    assert_eq!(r.repository().as_ref(), "org/repo");
    assert_eq!(r.tag().unwrap().as_ref(), "v2");
}

#[test]
fn parse_digest_reference() {
    let digest = "sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
    let input = format!("alpine@{digest}");
    let r = ImageReference::parse(&input).unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "library/alpine");
    assert!(r.tag().is_none());
    assert_eq!(r.digest().unwrap().as_ref(), digest);
}

#[test]
fn parse_tag_and_digest() {
    let digest = "sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
    let input = format!("alpine:3.20@{digest}");
    let r = ImageReference::parse(&input).unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "library/alpine");
    assert_eq!(r.tag().unwrap().as_ref(), "3.20");
    assert_eq!(r.digest().unwrap().as_ref(), digest);
}

#[test]
fn parse_full_registry_with_digest() {
    let digest = "sha256:1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef";
    let input = format!("ghcr.io/owner/repo@{digest}");
    let r = ImageReference::parse(&input).unwrap();
    assert_eq!(r.registry().as_ref(), "ghcr.io");
    assert_eq!(r.repository().as_ref(), "owner/repo");
    assert!(r.tag().is_none());
    assert_eq!(r.digest().unwrap().as_ref(), digest);
}

#[test]
fn display_with_tag() {
    let r = ImageReference::parse("alpine:3.20").unwrap();
    assert_eq!(r.to_string(), "docker.io/library/alpine:3.20");
}

#[test]
fn display_with_digest_only() {
    let digest = "sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
    let input = format!("alpine@{digest}");
    let r = ImageReference::parse(&input).unwrap();
    assert_eq!(r.to_string(), format!("docker.io/library/alpine@{digest}"));
}

#[test]
fn display_with_tag_and_digest() {
    let digest = "sha256:abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890";
    let input = format!("alpine:3.20@{digest}");
    let r = ImageReference::parse(&input).unwrap();
    assert_eq!(
        r.to_string(),
        format!("docker.io/library/alpine:3.20@{digest}")
    );
}

#[test]
fn display_round_trip_bare_name() {
    let r = ImageReference::parse("alpine").unwrap();
    let displayed = r.to_string();
    let r2 = ImageReference::parse(&displayed).unwrap();
    assert_eq!(r, r2);
}

#[test]
fn display_round_trip_full_reference() {
    let r = ImageReference::parse("ghcr.io/owner/repo:v1.2.3").unwrap();
    let displayed = r.to_string();
    let r2 = ImageReference::parse(&displayed).unwrap();
    assert_eq!(r, r2);
}

#[test]
fn from_str_trait_works() {
    let r: ImageReference = "alpine:3.20".parse().unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.tag().unwrap().as_ref(), "3.20");
}

#[test]
fn error_empty_string() {
    let err = ImageReference::parse("").unwrap_err();
    assert!(
        err.to_string().contains("empty"),
        "expected 'empty' in error: {err}"
    );
}

#[test]
fn error_invalid_characters() {
    let err = ImageReference::parse("INVALID!!image@@ref").unwrap_err();
    assert!(err.to_string().contains("invalid"));
}

#[test]
fn error_trailing_colon() {
    let err = ImageReference::parse("alpine:").unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("tag"), "error should mention 'tag': {msg}");
}

#[test]
fn error_trailing_at() {
    let err = ImageReference::parse("alpine@").unwrap_err();
    let msg = format!("{err:#}");
    assert!(
        msg.contains("digest"),
        "error should mention 'digest': {msg}"
    );
}

#[test]
fn parse_localhost_registry() {
    let r = ImageReference::parse("localhost/myapp:dev").unwrap();
    assert_eq!(r.registry().as_ref(), "localhost");
    assert_eq!(r.repository().as_ref(), "myapp");
    assert_eq!(r.tag().unwrap().as_ref(), "dev");
}

#[test]
fn parse_localhost_with_port() {
    let r = ImageReference::parse("localhost:5000/myapp:dev").unwrap();
    assert_eq!(r.registry().as_ref(), "localhost:5000");
    assert_eq!(r.repository().as_ref(), "myapp");
    assert_eq!(r.tag().unwrap().as_ref(), "dev");
}

#[test]
fn parse_deeply_nested_repository() {
    let r = ImageReference::parse("ghcr.io/org/team/project/image:latest").unwrap();
    assert_eq!(r.registry().as_ref(), "ghcr.io");
    assert_eq!(r.repository().as_ref(), "org/team/project/image");
    assert_eq!(r.tag().unwrap().as_ref(), "latest");
}

#[test]
fn clone_and_eq() {
    let r1 = ImageReference::parse("alpine:3.20").unwrap();
    let r2 = r1.clone();
    assert_eq!(r1, r2);
}

#[test]
fn debug_format() {
    let r = ImageReference::parse("alpine").unwrap();
    let debug = format!("{r:?}");
    assert!(debug.contains("ImageReference"));
    assert!(debug.contains("docker.io"));
}
