use std::collections::HashMap;
use std::fs;
use std::path::Path;

use super::*;
use crate::engine::ImageMetadata;
use crate::layer::{LayerCreator, ProcessedLayer};
use crate::testutil::tempdir;
use serde_json::Value;

// ── Test Helpers ────────────────────────────────────────────────────────

/// Create a minimal `ProcessedLayer` from a simple file entry.
fn make_test_layer(name: &str, content: &[u8]) -> ProcessedLayer {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, name, content).unwrap();
    let tar_data = builder.into_inner().unwrap();

    LayerCreator::from_tar(&tar_data, &[]).unwrap()
}

/// Create metadata with common fields populated.
fn make_test_metadata() -> ImageMetadata {
    ImageMetadata {
        cmd: Some(vec!["/bin/sh".to_owned()]),
        entrypoint: Some(vec!["/app/run".to_owned()]),
        env: vec![
            ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
            ("APP_ENV".to_owned(), "production".to_owned()),
        ],
        working_dir: Some("/app".to_owned()),
        user: Some("appuser".to_owned()),
        exposed_ports: vec![(8080, "tcp".to_owned())],
        labels: vec![("version".to_owned(), "1.0".to_owned())],
        shell: None,
        stop_signal: Some("SIGTERM".to_owned()),
        volumes: vec!["/data".to_owned()],
    }
}

/// Read a blob from the OCI layout by digest string (`sha256:abcd...`).
fn read_blob(output_dir: &Path, digest: &str) -> Vec<u8> {
    let hash = digest.strip_prefix("sha256:").unwrap();
    let blob_path = output_dir.join("blobs").join("sha256").join(hash);
    fs::read(blob_path).unwrap()
}

/// Parse a blob as JSON.
fn read_blob_json(output_dir: &Path, digest: &str) -> Value {
    let data = read_blob(output_dir, digest);
    serde_json::from_slice(&data).unwrap()
}

/// Create a minimal Docker image archive with one layer and one tag.
fn make_docker_image_archive() -> Vec<u8> {
    let mut layer_builder = tar::Builder::new(Vec::new());
    let mut layer_header = tar::Header::new_gnu();
    let layer_contents = b"hello from loaded image\n";
    layer_header.set_size(layer_contents.len() as u64);
    layer_header.set_mode(0o644);
    layer_header.set_entry_type(tar::EntryType::Regular);
    layer_header.set_cksum();
    layer_builder
        .append_data(&mut layer_header, "hello.txt", &layer_contents[..])
        .unwrap();
    let layer_tar = layer_builder.into_inner().unwrap();

    let config = serde_json::json!({
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "Cmd": ["cat", "/hello.txt"],
            "Entrypoint": ["/bin/sh", "-lc"],
            "Env": ["HELLO=world"],
            "WorkingDir": "/workspace",
            "User": "visor",
            "ExposedPorts": {"8080/tcp": {}},
            "Labels": {"org.opencontainers.image.title": "loaded"},
            "StopSignal": "SIGTERM",
            "Volumes": {"/data": {}}
        }
    });
    let manifest = serde_json::json!([
        {
            "Config": "config.json",
            "RepoTags": ["loaded:test"],
            "Layers": ["layer.tar"]
        }
    ]);

    let mut archive_builder = tar::Builder::new(Vec::new());
    for (path, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("config.json", serde_json::to_vec(&config).unwrap()),
        ("layer.tar", layer_tar),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        archive_builder
            .append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }

    archive_builder.into_inner().unwrap()
}

fn make_gzipped_docker_image_archive() -> Vec<u8> {
    let mut layer_builder = tar::Builder::new(Vec::new());
    let mut layer_header = tar::Header::new_gnu();
    let layer_contents = b"hello from precompressed image\n";
    layer_header.set_size(layer_contents.len() as u64);
    layer_header.set_mode(0o644);
    layer_header.set_entry_type(tar::EntryType::Regular);
    layer_header.set_cksum();
    layer_builder
        .append_data(&mut layer_header, "hello.txt", &layer_contents[..])
        .unwrap();
    let layer_tar = layer_builder.into_inner().unwrap();

    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    std::io::Write::write_all(&mut encoder, &layer_tar).unwrap();
    let gzipped_layer_tar = encoder.finish().unwrap();

    let config = serde_json::json!({
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "Cmd": ["cat", "/hello.txt"]
        }
    });
    let manifest = serde_json::json!([
        {
            "Config": "config.json",
            "RepoTags": ["loaded:gzip"],
            "Layers": ["layer.tar"]
        }
    ]);

    let mut archive_builder = tar::Builder::new(Vec::new());
    for (path, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("config.json", serde_json::to_vec(&config).unwrap()),
        ("layer.tar", gzipped_layer_tar),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        archive_builder
            .append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }

    archive_builder.into_inner().unwrap()
}

fn make_invalid_docker_image_archive() -> Vec<u8> {
    let mut archive_builder = tar::Builder::new(Vec::new());
    let bytes = b"missing manifest";
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    archive_builder
        .append_data(&mut header, "readme.txt", &bytes[..])
        .unwrap();
    archive_builder.into_inner().unwrap()
}

// ── ImageAssembler Tests ────────────────────────────────────────────────

#[test]
fn assemble_single_layer() {
    let dir = tempdir().unwrap();
    let layer = make_test_layer("hello.txt", b"hello world");
    let metadata = make_test_metadata();

    let result = ImageAssembler::assemble(&[layer], &metadata, dir.path());
    assert!(result.is_ok(), "assemble failed: {result:?}");

    let stored = result.unwrap();
    assert!(stored.manifest_digest.starts_with("sha256:"));
    assert!(stored.config_digest.starts_with("sha256:"));
    assert!(stored.total_size > 0);
    assert_eq!(stored.os, "linux");
}

#[test]
fn assemble_creates_oci_layout_file() {
    let dir = tempdir().unwrap();
    let layer = make_test_layer("file.txt", b"data");
    let metadata = ImageMetadata::default();

    ImageAssembler::assemble(&[layer], &metadata, dir.path()).unwrap();

    let layout_path = dir.path().join("oci-layout");
    assert!(layout_path.exists(), "oci-layout file missing");

    let content = fs::read_to_string(&layout_path).unwrap();
    let parsed: Value = serde_json::from_str(&content).unwrap();
    assert_eq!(parsed["imageLayoutVersion"], "1.0.0");
}

#[test]
fn assemble_creates_blobs_dir() {
    let dir = tempdir().unwrap();
    let layer = make_test_layer("app.bin", b"binary");
    let metadata = ImageMetadata::default();

    let stored = ImageAssembler::assemble(&[layer.clone()], &metadata, dir.path()).unwrap();

    let blobs_dir = dir.path().join("blobs").join("sha256");
    assert!(blobs_dir.is_dir(), "blobs/sha256/ directory missing");

    // Config blob must exist.
    let config_hash = stored.config_digest.strip_prefix("sha256:").unwrap();
    assert!(blobs_dir.join(config_hash).exists(), "config blob missing");

    // Manifest blob must exist.
    let manifest_hash = stored.manifest_digest.strip_prefix("sha256:").unwrap();
    assert!(
        blobs_dir.join(manifest_hash).exists(),
        "manifest blob missing"
    );

    // Layer blob must exist.
    let layer_hash = layer.digest.strip_prefix("sha256:").unwrap();
    assert!(blobs_dir.join(layer_hash).exists(), "layer blob missing");
}

#[test]
fn config_has_correct_architecture() {
    let dir = tempdir().unwrap();
    let layer = make_test_layer("f.txt", b"x");
    let metadata = ImageMetadata::default();

    let stored = ImageAssembler::assemble(&[layer], &metadata, dir.path()).unwrap();

    let config = read_blob_json(dir.path(), &stored.config_digest);

    // Must be a valid OCI architecture.
    let arch = config["architecture"].as_str().unwrap();
    assert!(
        ["amd64", "arm64"].contains(&arch),
        "unexpected architecture: {arch}"
    );
    assert_eq!(config["os"].as_str().unwrap(), "linux");
}

#[test]
fn config_has_metadata() {
    let dir = tempdir().unwrap();
    let layer = make_test_layer("f.txt", b"x");
    let metadata = make_test_metadata();

    let stored = ImageAssembler::assemble(&[layer], &metadata, dir.path()).unwrap();

    let config = read_blob_json(dir.path(), &stored.config_digest);
    let container_config = &config["config"];

    // CMD
    let cmd: Vec<String> = serde_json::from_value(container_config["Cmd"].clone()).unwrap();
    assert_eq!(cmd, vec!["/bin/sh"]);

    // ENTRYPOINT
    let ep: Vec<String> = serde_json::from_value(container_config["Entrypoint"].clone()).unwrap();
    assert_eq!(ep, vec!["/app/run"]);

    // ENV
    let env: Vec<String> = serde_json::from_value(container_config["Env"].clone()).unwrap();
    assert!(env.contains(&"PATH=/usr/bin:/bin".to_owned()));
    assert!(env.contains(&"APP_ENV=production".to_owned()));

    // WORKDIR
    assert_eq!(container_config["WorkingDir"].as_str().unwrap(), "/app");

    // USER
    assert_eq!(container_config["User"].as_str().unwrap(), "appuser");

    // STOPSIGNAL
    assert_eq!(container_config["StopSignal"].as_str().unwrap(), "SIGTERM");

    // EXPOSEDPORTS
    let ports = container_config["ExposedPorts"].as_object().unwrap();
    assert!(ports.contains_key("8080/tcp"));

    // VOLUMES
    let volumes = container_config["Volumes"].as_object().unwrap();
    assert!(volumes.contains_key("/data"));

    // LABELS
    let labels = container_config["Labels"].as_object().unwrap();
    assert_eq!(labels["version"].as_str().unwrap(), "1.0");
}

#[test]
fn manifest_references_config() {
    let dir = tempdir().unwrap();
    let layer = make_test_layer("f.txt", b"x");
    let metadata = ImageMetadata::default();

    let stored = ImageAssembler::assemble(&[layer], &metadata, dir.path()).unwrap();

    let manifest = read_blob_json(dir.path(), &stored.manifest_digest);

    // Config descriptor must point to the actual config blob.
    let config_digest = manifest["config"]["digest"].as_str().unwrap();
    assert_eq!(config_digest, stored.config_digest);

    // Config blob must be readable.
    let config_blob = read_blob(dir.path(), config_digest);
    assert!(!config_blob.is_empty());
}

#[test]
fn manifest_references_layers() {
    let dir = tempdir().unwrap();
    let layer1 = make_test_layer("a.txt", b"aaa");
    let layer2 = make_test_layer("b.txt", b"bbb");
    let metadata = ImageMetadata::default();

    let stored =
        ImageAssembler::assemble(&[layer1.clone(), layer2.clone()], &metadata, dir.path()).unwrap();

    let manifest = read_blob_json(dir.path(), &stored.manifest_digest);
    let layers = manifest["layers"].as_array().unwrap();

    assert_eq!(layers.len(), 2);

    // Each layer descriptor must reference an existing blob.
    for (i, expected) in [&layer1, &layer2].iter().enumerate() {
        let digest = layers[i]["digest"].as_str().unwrap();
        assert_eq!(digest, expected.digest);

        let blob = read_blob(dir.path(), digest);
        assert_eq!(blob, expected.compressed_data);
    }
}

#[test]
fn index_references_manifest() {
    let dir = tempdir().unwrap();
    let layer = make_test_layer("f.txt", b"x");
    let metadata = ImageMetadata::default();

    let stored = ImageAssembler::assemble(&[layer], &metadata, dir.path()).unwrap();

    let index_path = dir.path().join("index.json");
    assert!(index_path.exists(), "index.json missing");

    let index: Value = serde_json::from_str(&fs::read_to_string(index_path).unwrap()).unwrap();
    let manifests = index["manifests"].as_array().unwrap();
    assert_eq!(manifests.len(), 1);

    let manifest_digest = manifests[0]["digest"].as_str().unwrap();
    assert_eq!(manifest_digest, stored.manifest_digest);
}

// ── ImageStore Tests ────────────────────────────────────────────────────

#[test]
fn image_store_tag_and_get() {
    let dir = tempdir().unwrap();
    let store = ImageStore::new(dir.path().to_path_buf());

    store.tag("myapp:latest", "sha256:abcd1234").unwrap();

    let result = store.get_by_tag("myapp:latest").unwrap();
    assert_eq!(result, Some("sha256:abcd1234".to_owned()));

    // Non-existent tag returns None.
    let missing = store.get_by_tag("nonexistent:v1").unwrap();
    assert_eq!(missing, None);
}

#[test]
fn image_store_list_tags() {
    let dir = tempdir().unwrap();
    let store = ImageStore::new(dir.path().to_path_buf());

    store.tag("app:v1", "sha256:aaa").unwrap();
    store.tag("app:v2", "sha256:bbb").unwrap();
    store.tag("other:latest", "sha256:ccc").unwrap();

    let tags = store.list_tags().unwrap();
    assert_eq!(tags.len(), 3);

    let tag_map: HashMap<String, String> = tags.into_iter().collect();
    assert_eq!(tag_map["app:v1"], "sha256:aaa");
    assert_eq!(tag_map["app:v2"], "sha256:bbb");
    assert_eq!(tag_map["other:latest"], "sha256:ccc");
}

#[test]
fn image_store_remove_tag() {
    let dir = tempdir().unwrap();
    let store = ImageStore::new(dir.path().to_path_buf());

    store.tag("app:v1", "sha256:aaa").unwrap();
    store.tag("app:v2", "sha256:bbb").unwrap();

    let removed = store.remove_tag("app:v1").unwrap();
    assert!(removed);

    let result = store.get_by_tag("app:v1").unwrap();
    assert_eq!(result, None);

    // Other tags unaffected.
    let v2 = store.get_by_tag("app:v2").unwrap();
    assert_eq!(v2, Some("sha256:bbb".to_owned()));

    // Removing non-existent tag returns false.
    let removed_again = store.remove_tag("app:v1").unwrap();
    assert!(!removed_again);
}

#[test]
fn image_store_load_docker_archive_imports_metadata_and_tags() {
    let dir = tempdir().unwrap();
    let store = ImageStore::new(dir.path().to_path_buf());

    let loaded_tags = store
        .load_docker_archive(&make_docker_image_archive())
        .unwrap();
    assert_eq!(loaded_tags, vec!["loaded:test".to_owned()]);

    let manifest_digest = store
        .get_by_tag("loaded:test")
        .unwrap()
        .expect("loaded tag should exist");
    let digest_hex = manifest_digest.strip_prefix("sha256:").unwrap();
    let image_dir = dir.path().join(digest_hex);
    let manifest = read_blob_json(&image_dir, &manifest_digest);
    let config_digest = manifest["config"]["digest"]
        .as_str()
        .expect("manifest config digest");
    let config = read_blob_json(&image_dir, config_digest);
    let container = &config["config"];

    let cmd: Vec<String> = serde_json::from_value(container["Cmd"].clone()).unwrap();
    let entrypoint: Vec<String> = serde_json::from_value(container["Entrypoint"].clone()).unwrap();
    let env: Vec<String> = serde_json::from_value(container["Env"].clone()).unwrap();

    assert_eq!(cmd, vec!["cat", "/hello.txt"]);
    assert_eq!(entrypoint, vec!["/bin/sh", "-lc"]);
    assert!(env.contains(&"HELLO=world".to_owned()));
    assert_eq!(container["WorkingDir"], "/workspace");
    assert_eq!(container["User"], "visor");
    assert_eq!(container["StopSignal"], "SIGTERM");
    assert!(
        container["ExposedPorts"]
            .as_object()
            .unwrap()
            .contains_key("8080/tcp")
    );
    assert!(
        container["Labels"]
            .as_object()
            .unwrap()
            .contains_key("org.opencontainers.image.title")
    );
    assert!(
        container["Volumes"]
            .as_object()
            .unwrap()
            .contains_key("/data")
    );
}

#[test]
fn image_store_load_docker_archive_requires_manifest_json() {
    let dir = tempdir().unwrap();
    let store = ImageStore::new(dir.path().to_path_buf());

    let error = store
        .load_docker_archive(&make_invalid_docker_image_archive())
        .expect_err("archive without manifest.json should fail");

    let message = format!("{error:#}");
    assert!(message.contains("docker archive missing manifest.json"));
}

#[test]
fn image_store_load_docker_archive_accepts_precompressed_layer_entries() {
    let dir = tempdir().unwrap();
    let store = ImageStore::new(dir.path().to_path_buf());

    let loaded_tags = store
        .load_docker_archive(&make_gzipped_docker_image_archive())
        .unwrap();

    assert_eq!(loaded_tags, vec!["loaded:gzip".to_owned()]);

    let manifest_digest = store
        .get_by_tag("loaded:gzip")
        .unwrap()
        .expect("gzip-loaded tag should be present");
    let manifest = read_blob_json(
        dir.path()
            .join(manifest_digest.strip_prefix("sha256:").unwrap())
            .as_path(),
        &manifest_digest,
    );
    let layer_digest = manifest["layers"][0]["digest"].as_str().unwrap();
    let layer_bytes = read_blob(
        dir.path()
            .join(manifest_digest.strip_prefix("sha256:").unwrap())
            .as_path(),
        layer_digest,
    );

    let decoder = flate2::read::GzDecoder::new(&layer_bytes[..]);
    let mut archive = tar::Archive::new(decoder);
    let mut entries = archive.entries().unwrap();
    let mut file = entries.next().unwrap().unwrap();
    let path = file.path().unwrap().into_owned();
    let mut contents = String::new();
    std::io::Read::read_to_string(&mut file, &mut contents).unwrap();

    assert_eq!(path, std::path::PathBuf::from("hello.txt"));
    assert_eq!(contents, "hello from precompressed image\n");
}
