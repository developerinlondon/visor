use super::*;

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Descriptor parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_descriptor_from_json() {
    let json = r#"{
        "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
        "digest": "sha256:abc123def456789",
        "size": 12345
    }"#;
    let desc: Descriptor = serde_json::from_str(json).unwrap();
    assert_eq!(
        desc.media_type,
        "application/vnd.oci.image.layer.v1.tar+gzip"
    );
    assert_eq!(desc.digest, "sha256:abc123def456789");
    assert_eq!(desc.size, 12345);
}

#[test]
fn descriptor_display_includes_digest_and_size() {
    let desc = Descriptor {
        media_type: "application/vnd.oci.image.layer.v1.tar+gzip".into(),
        digest: "sha256:abc123def456789012345".into(),
        size: 98_765,
    };
    let display = format!("{desc}");
    assert!(display.contains("sha256:"), "should contain digest prefix");
    assert!(display.contains("98765"), "should contain byte size");
}

// ---------------------------------------------------------------------------
// Manifest parsing
// ---------------------------------------------------------------------------

const MANIFEST_JSON: &str = r#"{
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.manifest.v1+json",
    "config": {
        "mediaType": "application/vnd.oci.image.config.v1+json",
        "digest": "sha256:configabc123",
        "size": 1024
    },
    "layers": [
        {
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": "sha256:layer1abc",
            "size": 2048
        },
        {
            "mediaType": "application/vnd.oci.image.layer.v1.tar+gzip",
            "digest": "sha256:layer2def",
            "size": 4096
        }
    ]
}"#;

#[test]
fn parse_manifest_full() {
    let manifest: Manifest = serde_json::from_str(MANIFEST_JSON).unwrap();
    assert_eq!(manifest.schema_version, 2);
    assert_eq!(
        manifest.media_type.as_deref(),
        Some("application/vnd.oci.image.manifest.v1+json")
    );
    assert_eq!(manifest.config.digest, "sha256:configabc123");
    assert_eq!(manifest.config.size, 1024);
    assert_eq!(manifest.layers.len(), 2);
    assert_eq!(manifest.layers[0].digest, "sha256:layer1abc");
    assert_eq!(manifest.layers[0].size, 2048);
    assert_eq!(manifest.layers[1].digest, "sha256:layer2def");
}

#[test]
fn parse_manifest_without_media_type() {
    let json = r#"{
        "schemaVersion": 2,
        "config": {
            "mediaType": "application/vnd.oci.image.config.v1+json",
            "digest": "sha256:cfg",
            "size": 64
        },
        "layers": []
    }"#;
    let manifest: Manifest = serde_json::from_str(json).unwrap();
    assert!(manifest.media_type.is_none());
    assert!(manifest.layers.is_empty());
}

#[test]
fn parse_manifest_missing_layers_errors() {
    let json = r#"{
        "schemaVersion": 2,
        "config": {
            "mediaType": "cfg",
            "digest": "sha256:abc",
            "size": 1
        }
    }"#;
    let result: Result<Manifest, _> = serde_json::from_str(json);
    assert!(result.is_err(), "missing 'layers' should fail");
}

#[test]
fn parse_manifest_missing_config_errors() {
    let json = r#"{
        "schemaVersion": 2,
        "layers": []
    }"#;
    let result: Result<Manifest, _> = serde_json::from_str(json);
    assert!(result.is_err(), "missing 'config' should fail");
}

#[test]
fn parse_manifest_invalid_json_errors() {
    let result: Result<Manifest, _> = serde_json::from_str("{{not json}}");
    assert!(result.is_err());
}

#[test]
fn manifest_display_shows_version_and_layer_count() {
    let manifest = Manifest {
        schema_version: 2,
        media_type: Some("application/vnd.oci.image.manifest.v1+json".into()),
        config: Descriptor {
            media_type: "config".into(),
            digest: "sha256:abc".into(),
            size: 0,
        },
        layers: vec![
            Descriptor {
                media_type: "layer".into(),
                digest: "sha256:l1".into(),
                size: 100,
            },
            Descriptor {
                media_type: "layer".into(),
                digest: "sha256:l2".into(),
                size: 200,
            },
        ],
    };
    let display = format!("{manifest}");
    assert!(display.contains('2'), "should contain schema version");
    assert!(display.contains("2 layer"), "should contain layer count");
}

#[test]
fn manifest_display_singular_layer() {
    let manifest = Manifest {
        schema_version: 2,
        media_type: None,
        config: Descriptor {
            media_type: "config".into(),
            digest: "sha256:abc".into(),
            size: 0,
        },
        layers: vec![Descriptor {
            media_type: "layer".into(),
            digest: "sha256:l1".into(),
            size: 100,
        }],
    };
    let display = format!("{manifest}");
    assert!(
        display.contains("1 layer"),
        "should say 'layer' not 'layers' for singular"
    );
}

#[test]
fn manifest_debug_contains_struct_name() {
    let manifest = Manifest {
        schema_version: 2,
        media_type: None,
        config: Descriptor {
            media_type: "config".into(),
            digest: "sha256:abc".into(),
            size: 0,
        },
        layers: vec![],
    };
    let debug = format!("{manifest:?}");
    assert!(debug.contains("Manifest"));
    assert!(debug.contains("schema_version"));
}

// ---------------------------------------------------------------------------
// Www-Authenticate header parsing
// ---------------------------------------------------------------------------

#[test]
fn parse_www_authenticate_docker_hub() {
    let header = r#"Bearer realm="https://auth.docker.io/token",service="registry.docker.io",scope="repository:library/alpine:pull""#;
    let params = parse_www_authenticate(header).unwrap();
    assert_eq!(params.get("realm").unwrap(), "https://auth.docker.io/token");
    assert_eq!(params.get("service").unwrap(), "registry.docker.io");
    assert_eq!(
        params.get("scope").unwrap(),
        "repository:library/alpine:pull"
    );
}

#[test]
fn parse_www_authenticate_with_spaces() {
    let header = r#"Bearer realm="https://auth.example.io/token", service="registry.example.io""#;
    let params = parse_www_authenticate(header).unwrap();
    assert_eq!(
        params.get("realm").unwrap(),
        "https://auth.example.io/token"
    );
    assert_eq!(params.get("service").unwrap(), "registry.example.io");
}

#[test]
fn parse_www_authenticate_basic_returns_none() {
    let result = parse_www_authenticate("Basic realm=\"example\"");
    assert!(result.is_none(), "non-Bearer schemes should return None");
}

#[test]
fn parse_www_authenticate_empty_returns_none() {
    let result = parse_www_authenticate("");
    assert!(result.is_none());
}

// ---------------------------------------------------------------------------
// Auth URL building
// ---------------------------------------------------------------------------

#[test]
fn build_auth_url_docker_hub() {
    let mut params = HashMap::new();
    params.insert("realm".into(), "https://auth.docker.io/token".into());
    params.insert("service".into(), "registry.docker.io".into());

    let url = build_auth_url(&params, "library/alpine").unwrap();
    assert_eq!(
        url,
        "https://auth.docker.io/token?service=registry.docker.io&scope=repository:library/alpine:pull"
    );
}

#[test]
fn build_auth_url_missing_realm_errors() {
    let mut params = HashMap::new();
    params.insert("service".into(), "registry.docker.io".into());

    let result = build_auth_url(&params, "library/alpine");
    assert!(result.is_err(), "missing realm should error");
}

#[test]
fn build_auth_url_missing_service_errors() {
    let mut params = HashMap::new();
    params.insert("realm".into(), "https://auth.docker.io/token".into());

    let result = build_auth_url(&params, "library/alpine");
    assert!(result.is_err(), "missing service should error");
}

// ---------------------------------------------------------------------------
// RegistryClient::new
// ---------------------------------------------------------------------------

#[test]
fn client_new_docker_io_maps_to_registry_1() {
    let client = RegistryClient::new("docker.io").unwrap();
    assert_eq!(client.base_url, "https://registry-1.docker.io");
}

#[test]
fn client_new_index_docker_io_maps_to_registry_1() {
    let client = RegistryClient::new("index.docker.io").unwrap();
    assert_eq!(client.base_url, "https://registry-1.docker.io");
}

#[test]
fn client_new_custom_registry() {
    let client = RegistryClient::new("ghcr.io").unwrap();
    assert_eq!(client.base_url, "https://ghcr.io");
}

#[test]
fn client_new_with_port() {
    let client = RegistryClient::new("localhost:5000").unwrap();
    assert_eq!(client.base_url, "https://localhost:5000");
}

#[test]
fn client_new_strips_trailing_slash() {
    let client = RegistryClient::new("ghcr.io/").unwrap();
    assert_eq!(client.base_url, "https://ghcr.io");
}

#[test]
fn client_new_empty_string_errors() {
    let result = RegistryClient::new("");
    assert!(result.is_err());
}

#[test]
fn client_new_has_no_initial_token() {
    let client = RegistryClient::new("ghcr.io").unwrap();
    assert!(client.token.is_none());
}

// ---------------------------------------------------------------------------
// Integration: pull alpine manifest from Docker Hub (network required)
// ---------------------------------------------------------------------------

#[tokio::test]
#[ignore = "requires network access to Docker Hub"]
async fn pull_manifest_alpine_from_docker_hub() {
    let mut client = RegistryClient::new("docker.io").unwrap();
    client.authenticate("library/alpine").await.unwrap();

    let manifest = client
        .pull_manifest("library/alpine", "latest")
        .await
        .unwrap();

    assert_eq!(manifest.schema_version, 2);
    assert!(
        !manifest.layers.is_empty(),
        "alpine should have at least one layer"
    );
    for layer in &manifest.layers {
        assert!(
            layer.digest.starts_with("sha256:"),
            "layer digest should start with sha256:"
        );
        assert!(layer.size > 0, "layer size should be positive");
    }
}

// ---------------------------------------------------------------------------
// Platform deserialization
// ---------------------------------------------------------------------------

#[test]
fn parse_platform_linux_amd64() {
    let json = r#"{
        "architecture": "amd64",
        "os": "linux"
    }"#;
    let platform: Platform = serde_json::from_str(json).unwrap();
    assert_eq!(platform.architecture, "amd64");
    assert_eq!(platform.os, "linux");
    assert!(platform.variant.is_none());
}

#[test]
fn parse_platform_with_variant() {
    let json = r#"{
        "architecture": "arm",
        "os": "linux",
        "variant": "v7"
    }"#;
    let platform: Platform = serde_json::from_str(json).unwrap();
    assert_eq!(platform.architecture, "arm");
    assert_eq!(platform.os, "linux");
    assert_eq!(platform.variant.as_deref(), Some("v7"));
}

// ---------------------------------------------------------------------------
// PlatformDescriptor deserialization
// ---------------------------------------------------------------------------

#[test]
fn parse_platform_descriptor_with_platform() {
    let json = r#"{
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "digest": "sha256:abc123",
        "size": 528,
        "platform": {
            "architecture": "amd64",
            "os": "linux"
        }
    }"#;
    let desc: PlatformDescriptor = serde_json::from_str(json).unwrap();
    assert_eq!(
        desc.media_type,
        "application/vnd.oci.image.manifest.v1+json"
    );
    assert_eq!(desc.digest, "sha256:abc123");
    assert_eq!(desc.size, 528);
    let platform = desc.platform.unwrap();
    assert_eq!(platform.architecture, "amd64");
    assert_eq!(platform.os, "linux");
}

#[test]
fn parse_platform_descriptor_without_platform() {
    let json = r#"{
        "mediaType": "application/vnd.oci.image.manifest.v1+json",
        "digest": "sha256:attestation",
        "size": 100
    }"#;
    let desc: PlatformDescriptor = serde_json::from_str(json).unwrap();
    assert!(desc.platform.is_none());
}

// ---------------------------------------------------------------------------
// ManifestIndex deserialization
// ---------------------------------------------------------------------------

const INDEX_JSON: &str = r#"{
    "schemaVersion": 2,
    "mediaType": "application/vnd.oci.image.index.v1+json",
    "manifests": [
        {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": "sha256:amd64digest",
            "size": 528,
            "platform": {
                "architecture": "amd64",
                "os": "linux"
            }
        },
        {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": "sha256:arm64digest",
            "size": 528,
            "platform": {
                "architecture": "arm64",
                "os": "linux"
            }
        },
        {
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": "sha256:armv7digest",
            "size": 528,
            "platform": {
                "architecture": "arm",
                "os": "linux",
                "variant": "v7"
            }
        }
    ]
}"#;

#[test]
fn parse_manifest_index() {
    let index: ManifestIndex = serde_json::from_str(INDEX_JSON).unwrap();
    assert_eq!(index.schema_version, 2);
    assert_eq!(
        index.media_type.as_deref(),
        Some("application/vnd.oci.image.index.v1+json")
    );
    assert_eq!(index.manifests.len(), 3);
    assert_eq!(index.manifests[0].digest, "sha256:amd64digest");
    assert_eq!(index.manifests[1].digest, "sha256:arm64digest");
    assert_eq!(index.manifests[2].digest, "sha256:armv7digest");
}

#[test]
fn parse_manifest_index_without_media_type() {
    let json = r#"{
        "schemaVersion": 2,
        "manifests": []
    }"#;
    let index: ManifestIndex = serde_json::from_str(json).unwrap();
    assert!(index.media_type.is_none());
    assert!(index.manifests.is_empty());
}

// ---------------------------------------------------------------------------
// find_native_platform platform selection
// ---------------------------------------------------------------------------

#[test]
fn find_native_platform_selects_correct_arch() {
    let index: ManifestIndex = serde_json::from_str(INDEX_JSON).unwrap();
    let descriptor = find_native_platform(&index).unwrap();
    let platform = descriptor.platform.as_ref().unwrap();
    assert_eq!(platform.architecture, NATIVE_OCI_ARCH);
    assert_eq!(platform.os, "linux");
}

#[test]
fn find_native_platform_errors_when_no_match() {
    // Build an index with only an architecture that is NOT the native one.
    let other_arch = if NATIVE_OCI_ARCH == "arm64" {
        "s390x"
    } else {
        "ppc64le"
    };
    let json = format!(
        r#"{{
        "schemaVersion": 2,
        "manifests": [{{
            "mediaType": "application/vnd.oci.image.manifest.v1+json",
            "digest": "sha256:other",
            "size": 528,
            "platform": {{ "architecture": "{other_arch}", "os": "linux" }}
        }}]
    }}"#
    );
    let index: ManifestIndex = serde_json::from_str(&json).unwrap();
    let result = find_native_platform(&index);
    assert!(result.is_err(), "should error when no native arch entry");
}

#[test]
fn find_native_platform_errors_on_empty_manifests() {
    let json = r#"{
        "schemaVersion": 2,
        "manifests": []
    }"#;
    let index: ManifestIndex = serde_json::from_str(json).unwrap();
    let result = find_native_platform(&index);
    assert!(result.is_err(), "should error on empty manifests list");
}

#[test]
fn find_native_platform_skips_entries_without_platform() {
    let json = format!(
        r#"{{
        "schemaVersion": 2,
        "manifests": [
            {{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "sha256:noplatform",
                "size": 100
            }},
            {{
                "mediaType": "application/vnd.oci.image.manifest.v1+json",
                "digest": "sha256:nativefound",
                "size": 200,
                "platform": {{ "architecture": "{}", "os": "linux" }}
            }}
        ]
    }}"#,
        NATIVE_OCI_ARCH
    );
    let index: ManifestIndex = serde_json::from_str(&json).unwrap();
    let descriptor = find_native_platform(&index).unwrap();
    assert_eq!(descriptor.digest, "sha256:nativefound");
}
