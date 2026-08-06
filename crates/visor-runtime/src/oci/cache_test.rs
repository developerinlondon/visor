use super::*;

use std::fs;

use tempfile::TempDir;

/// Helper: create a `LayerCache` rooted in a fresh tempdir.
fn temp_cache() -> (TempDir, LayerCache) {
    let dir = crate::testutil::tempdir("visor-runtime-oci-cache-").unwrap();
    let cache = LayerCache::new(dir.path()).unwrap();
    (dir, cache)
}

/// A valid SHA-256 digest for the string `b"hello world"`.
fn hello_digest() -> String {
    use sha2::{Digest, Sha256};
    let hash = Sha256::digest(b"hello world");
    format!("sha256:{}", hex::encode(hash))
}

// ── Construction ──────────────────────────────────────────────────────

#[test]
fn new_creates_blobs_directory() {
    let (dir, _cache) = temp_cache();
    let blobs_dir = dir.path().join("blobs").join("sha256");
    assert!(blobs_dir.is_dir());
}

#[test]
fn default_path_returns_home_visor_cache() {
    // HOME is set in the CI/test environment; verify the suffix.
    let path = LayerCache::default_path().unwrap();
    assert!(
        path.ends_with(".visor/cache"),
        "expected path ending in .visor/cache, got {}",
        path.display()
    );
}

// ── blob_path ─────────────────────────────────────────────────────────

#[test]
fn blob_path_returns_correct_format() {
    let (_dir, cache) = temp_cache();
    let hex_part = "a".repeat(64);
    let digest = format!("sha256:{hex_part}");
    let expected = cache.root.join("blobs").join("sha256").join(&hex_part);
    assert_eq!(cache.blob_path(&digest), expected);
}

// ── has ───────────────────────────────────────────────────────────────

#[test]
fn has_returns_false_for_missing_blob() {
    let (_dir, cache) = temp_cache();
    let digest = format!("sha256:{}", "b".repeat(64));
    assert!(!cache.has(&digest));
}

#[test]
fn has_returns_true_after_put() {
    let (_dir, cache) = temp_cache();
    let digest = hello_digest();
    cache.put(&digest, b"hello world").unwrap();
    assert!(cache.has(&digest));
}

// ── put / get round-trip ──────────────────────────────────────────────

#[test]
fn put_and_get_round_trip() {
    let (_dir, cache) = temp_cache();
    let digest = hello_digest();
    let path = cache.put(&digest, b"hello world").unwrap();

    assert!(path.is_file());
    assert_eq!(fs::read(&path).unwrap(), b"hello world");

    let got = cache.get(&digest).unwrap();
    assert_eq!(got, Some(path));
}

#[test]
fn get_returns_none_for_nonexistent_digest() {
    let (_dir, cache) = temp_cache();
    let digest = format!("sha256:{}", "c".repeat(64));
    assert_eq!(cache.get(&digest).unwrap(), None);
}

// ── put with wrong digest ─────────────────────────────────────────────

#[test]
fn put_rejects_mismatched_digest() {
    let (_dir, cache) = temp_cache();
    let wrong = format!("sha256:{}", "0".repeat(64));
    let err = cache.put(&wrong, b"hello world").unwrap_err();
    assert!(
        err.to_string().contains("digest mismatch"),
        "expected 'digest mismatch' in error: {err}"
    );
}

// ── put_from_file ─────────────────────────────────────────────────────

#[test]
fn put_from_file_works() {
    let (_dir, cache) = temp_cache();
    let digest = hello_digest();

    let src = crate::testutil::tempdir("visor-runtime-oci-cache-src-").unwrap();
    let src_path = src.path().join("layer.tar.gz");
    fs::write(&src_path, b"hello world").unwrap();

    let dest = cache.put_from_file(&digest, &src_path).unwrap();
    assert!(dest.is_file());
    assert_eq!(fs::read(&dest).unwrap(), b"hello world");
    assert!(cache.has(&digest));
}

#[test]
fn put_from_file_rejects_mismatched_digest() {
    let (_dir, cache) = temp_cache();
    let wrong = format!("sha256:{}", "0".repeat(64));

    let src = crate::testutil::tempdir("visor-runtime-oci-cache-src-").unwrap();
    let src_path = src.path().join("layer.tar.gz");
    fs::write(&src_path, b"hello world").unwrap();

    let err = cache.put_from_file(&wrong, &src_path).unwrap_err();
    assert!(
        err.to_string().contains("digest mismatch"),
        "expected 'digest mismatch' in error: {err}"
    );
}

// ── remove ────────────────────────────────────────────────────────────

#[test]
fn remove_deletes_blob() {
    let (_dir, cache) = temp_cache();
    let digest = hello_digest();
    cache.put(&digest, b"hello world").unwrap();
    assert!(cache.has(&digest));

    cache.remove(&digest).unwrap();
    assert!(!cache.has(&digest));
}

#[test]
fn remove_nonexistent_is_ok() {
    let (_dir, cache) = temp_cache();
    let digest = format!("sha256:{}", "d".repeat(64));
    // Removing something that doesn't exist should not error.
    cache.remove(&digest).unwrap();
}

// ── size ──────────────────────────────────────────────────────────────

#[test]
fn size_returns_correct_total() {
    let (_dir, cache) = temp_cache();
    assert_eq!(cache.size().unwrap(), 0);

    let digest = hello_digest();
    cache.put(&digest, b"hello world").unwrap();
    assert_eq!(cache.size().unwrap(), 11); // "hello world" = 11 bytes
}

#[test]
fn size_sums_multiple_blobs() {
    use sha2::{Digest, Sha256};
    let (_dir, cache) = temp_cache();

    let data_a = b"aaaa";
    let digest_a = format!("sha256:{}", hex::encode(Sha256::digest(data_a)));
    cache.put(&digest_a, data_a).unwrap();

    let data_b = b"bbbbbb";
    let digest_b = format!("sha256:{}", hex::encode(Sha256::digest(data_b)));
    cache.put(&digest_b, data_b).unwrap();

    assert_eq!(cache.size().unwrap(), 10); // 4 + 6
}

// ── clear ─────────────────────────────────────────────────────────────

#[test]
fn clear_removes_all_blobs() {
    use sha2::{Digest, Sha256};
    let (_dir, cache) = temp_cache();
    let digest = hello_digest();
    cache.put(&digest, b"hello world").unwrap();

    let other = format!("sha256:{}", hex::encode(Sha256::digest(b"other")));
    cache.put(&other, b"other").unwrap();

    cache.clear().unwrap();
    assert!(!cache.has(&digest));
    assert!(!cache.has(&other));
    assert_eq!(cache.size().unwrap(), 0);
}

// ── multiple blobs independent ────────────────────────────────────────

#[test]
fn multiple_blobs_stored_independently() {
    use sha2::{Digest, Sha256};
    let (_dir, cache) = temp_cache();

    let data_a = b"alpha";
    let digest_a = format!("sha256:{}", hex::encode(Sha256::digest(data_a)));
    cache.put(&digest_a, data_a).unwrap();

    let data_b = b"beta";
    let digest_b = format!("sha256:{}", hex::encode(Sha256::digest(data_b)));
    cache.put(&digest_b, data_b).unwrap();

    let path_a = cache.get(&digest_a).unwrap().unwrap();
    let path_b = cache.get(&digest_b).unwrap().unwrap();

    assert_eq!(fs::read(path_a).unwrap(), data_a);
    assert_eq!(fs::read(path_b).unwrap(), data_b);
}

// ── idempotent put ────────────────────────────────────────────────────

#[test]
fn put_same_digest_twice_is_idempotent() {
    let (_dir, cache) = temp_cache();
    let digest = hello_digest();
    let path1 = cache.put(&digest, b"hello world").unwrap();
    let path2 = cache.put(&digest, b"hello world").unwrap();
    assert_eq!(path1, path2);
    assert_eq!(fs::read(&path1).unwrap(), b"hello world");
}

// ── manifest cache ──────────────────────────────────────────────────

#[test]
fn manifest_key_sanitizes_slashes() {
    let (_dir, cache) = temp_cache();
    let path = cache.manifest_key("registry-1.docker.io", "library/alpine", "latest");
    let filename = path.file_name().unwrap().to_str().unwrap();
    assert_eq!(filename, "registry-1.docker.io_library_alpine_latest.json");
    assert!(!filename.contains('/'));
}

#[test]
fn get_manifest_returns_none_when_missing() {
    let (_dir, cache) = temp_cache();
    let result = cache
        .get_manifest("docker.io", "library/alpine", "latest")
        .unwrap();
    assert!(result.is_none());
}

#[test]
fn put_and_get_manifest_round_trip() {
    let (_dir, cache) = temp_cache();
    let data = br#"{"schemaVersion":2,"layers":[]}"#;
    cache
        .put_manifest("docker.io", "library/alpine", "3.20", data)
        .unwrap();
    let cached = cache
        .get_manifest("docker.io", "library/alpine", "3.20")
        .unwrap();
    assert_eq!(cached.as_deref(), Some(data.as_slice()));
}

#[test]
fn put_manifest_overwrites_existing() {
    let (_dir, cache) = temp_cache();
    let v1 = b"{\"v\":1}";
    let v2 = b"{\"v\":2}";
    cache.put_manifest("r", "repo", "latest", v1).unwrap();
    cache.put_manifest("r", "repo", "latest", v2).unwrap();
    let cached = cache.get_manifest("r", "repo", "latest").unwrap().unwrap();
    assert_eq!(cached, v2);
}

#[test]
fn manifests_dir_created_on_cache_init() {
    let (_dir, cache) = temp_cache();
    assert!(cache.manifests_dir.is_dir());
}
