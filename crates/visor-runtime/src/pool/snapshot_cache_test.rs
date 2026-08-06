use std::fs;

use super::*;
use visor_types::{GuestVirtualizationMode, VmConfig, VolumeMount};

// ── SnapshotCache::new ──────────────────────────────────────────

#[test]
fn new_stores_cache_dir() {
    let cache = SnapshotCache::new("/tmp/test-cache".into());
    assert_eq!(cache.cache_dir().to_str().unwrap(), "/tmp/test-cache");
}

// ── SnapshotCache::has_snapshot ─────────────────────────────────

#[test]
fn has_snapshot_returns_false_for_nonexistent_dir() {
    let cache = SnapshotCache::new("/tmp/nonexistent-visor-cache-12345".into());
    assert!(!cache.has_snapshot("sha256:abc123"));
}

#[test]
fn has_snapshot_returns_true_when_dir_exists() {
    let tmp = crate::testutil::tempdir("visor-runtime-snapshot-cache-").unwrap();
    let cache = SnapshotCache::new(tmp.path().to_path_buf());

    // Create the digest directory
    let digest_dir = tmp.path().join("sha256_abc123");
    fs::create_dir_all(&digest_dir).unwrap();
    fs::write(digest_dir.join("memory.bin"), b"memory").unwrap();
    fs::write(digest_dir.join("cpu_state.json"), b"{}").unwrap();
    fs::write(digest_dir.join("rootfs.ext4"), b"rootfs").unwrap();

    assert!(cache.has_snapshot("sha256:abc123"));
}

// ── SnapshotCache::snapshot_path ────────────────────────────────

#[test]
fn snapshot_path_returns_correct_path() {
    let cache = SnapshotCache::new("/cache".into());
    let path = cache.snapshot_path("sha256:abc123");
    assert_eq!(path.to_str().unwrap(), "/cache/sha256_abc123/memory.bin");
}

#[test]
fn snapshot_path_sanitizes_colons() {
    let cache = SnapshotCache::new("/cache".into());
    let path = cache.snapshot_path("sha256:deadbeef");
    assert!(
        !path.to_str().unwrap().contains(':'),
        "path should not contain colons: {path:?}"
    );
}

// ── SnapshotCache::list_cached ──────────────────────────────────

#[test]
fn list_cached_returns_empty_for_nonexistent_dir() {
    let cache = SnapshotCache::new("/tmp/nonexistent-visor-cache-67890".into());
    assert!(cache.list_cached().is_empty());
}

#[test]
fn list_cached_returns_digest_names() {
    let tmp = crate::testutil::tempdir("visor-runtime-snapshot-cache-").unwrap();
    let cache = SnapshotCache::new(tmp.path().to_path_buf());

    // Create two digest directories
    fs::create_dir_all(tmp.path().join("sha256_aaa")).unwrap();
    fs::create_dir_all(tmp.path().join("sha256_bbb")).unwrap();

    // Create a file (should be ignored — only directories count)
    fs::write(tmp.path().join("not-a-dir.txt"), "ignored").unwrap();

    let mut cached = cache.list_cached();
    cached.sort();
    assert_eq!(cached, vec!["sha256_aaa", "sha256_bbb"]);
}

// ── SnapshotCache::evict ────────────────────────────────────────

#[test]
fn evict_removes_digest_directory() {
    let tmp = crate::testutil::tempdir("visor-runtime-snapshot-cache-").unwrap();
    let cache = SnapshotCache::new(tmp.path().to_path_buf());

    // Create digest dir with a file inside
    let digest_dir = tmp.path().join("sha256_evict");
    fs::create_dir_all(&digest_dir).unwrap();
    fs::write(digest_dir.join("memory.bin"), b"data").unwrap();
    fs::write(digest_dir.join("cpu_state.json"), b"{}").unwrap();
    fs::write(digest_dir.join("rootfs.ext4"), b"rootfs").unwrap();

    assert!(cache.has_snapshot("sha256:evict"));
    cache.evict("sha256:evict").unwrap();
    assert!(!cache.has_snapshot("sha256:evict"));
}

#[test]
fn evict_is_idempotent_for_nonexistent_digest() {
    let tmp = crate::testutil::tempdir("visor-runtime-snapshot-cache-").unwrap();
    let cache = SnapshotCache::new(tmp.path().to_path_buf());

    // Evicting a non-existent digest should succeed
    cache.evict("sha256:nonexistent").unwrap();
}

#[test]
fn snapshot_key_for_config_changes_when_command_changes() {
    let mut first = VmConfig::new("alpine:latest");
    first.cmd = vec!["echo".to_owned(), "one".to_owned()];
    let mut second = VmConfig::new("alpine:latest");
    second.cmd = vec!["echo".to_owned(), "two".to_owned()];

    let first_key = snapshot_key_for_config(&first).unwrap();
    let second_key = snapshot_key_for_config(&second).unwrap();

    assert_ne!(first_key, second_key);
}

#[test]
fn snapshot_key_for_config_changes_when_networking_changes() {
    let first = VmConfig::new("alpine:latest");
    let mut second = VmConfig::new("alpine:latest");
    second.network_enabled = false;

    let first_key = snapshot_key_for_config(&first).unwrap();
    let second_key = snapshot_key_for_config(&second).unwrap();

    assert_ne!(first_key, second_key);
}

#[test]
fn snapshot_key_for_config_changes_when_guest_virtualization_changes() {
    let first = VmConfig::new("alpine:latest");
    let mut second = VmConfig::new("alpine:latest");
    second.guest_virtualization = GuestVirtualizationMode::Nested;

    let first_key = snapshot_key_for_config(&first).unwrap();
    let second_key = snapshot_key_for_config(&second).unwrap();

    assert_ne!(first_key, second_key);
}

#[test]
fn supports_snapshot_fast_path_rejects_configs_with_volumes() {
    let mut config = VmConfig::new("alpine:latest");
    config
        .volumes
        .push(VolumeMount::read_only("/host", "/guest"));

    assert!(!supports_snapshot_fast_path(&config));
}

#[test]
fn supports_snapshot_fast_path_rejects_configs_with_service_aliases() {
    let mut config = VmConfig::new("alpine:latest");
    config.service_names = vec!["api".to_owned()];

    assert!(!supports_snapshot_fast_path(&config));
}

#[test]
fn supports_snapshot_fast_path_rejects_configs_with_service_ports() {
    let mut config = VmConfig::new("alpine:latest");
    config.service_ports = vec![visor_types::ServicePort::new(8080, "tcp")];

    assert!(!supports_snapshot_fast_path(&config));
}
