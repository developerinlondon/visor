use std::fs;

use super::*;
use crate::testutil::tempdir;

// ── CacheKey Tests ──────────────────────────────────────────────────────

#[test]
fn cache_key_run_deterministic() {
    let key1 = CacheKey::for_run("RUN apt-get update", "sha256:parent1");
    let key2 = CacheKey::for_run("RUN apt-get update", "sha256:parent1");
    assert_eq!(key1, key2, "same inputs must produce same key");
    assert!(key1.starts_with("sha256:"), "key must have sha256: prefix");
    // Must be a valid hex sha256 (64 hex chars after prefix).
    assert_eq!(key1.len(), 7 + 64, "sha256:<64 hex chars>");
}

#[test]
fn cache_key_run_changes_with_instruction() {
    let key1 = CacheKey::for_run("RUN apt-get update", "sha256:parent1");
    let key2 = CacheKey::for_run("RUN apt-get install curl", "sha256:parent1");
    assert_ne!(
        key1, key2,
        "different instructions must produce different keys"
    );
}

#[test]
fn cache_key_run_changes_with_parent() {
    let key1 = CacheKey::for_run("RUN apt-get update", "sha256:aaa");
    let key2 = CacheKey::for_run("RUN apt-get update", "sha256:bbb");
    assert_ne!(
        key1, key2,
        "different parent digests must produce different keys"
    );
}

#[test]
fn cache_key_copy_includes_content() {
    let key1 = CacheKey::for_copy("COPY . /app", "sha256:parent1", "sha256:content_v1");
    let key2 = CacheKey::for_copy("COPY . /app", "sha256:parent1", "sha256:content_v2");
    assert_ne!(
        key1, key2,
        "different content hashes must produce different keys"
    );

    // Same content → same key.
    let key3 = CacheKey::for_copy("COPY . /app", "sha256:parent1", "sha256:content_v1");
    assert_eq!(key1, key3);
}

#[test]
fn cache_key_metadata_deterministic() {
    let key1 = CacheKey::for_metadata("ENV FOO=bar", "sha256:parent1");
    let key2 = CacheKey::for_metadata("ENV FOO=bar", "sha256:parent1");
    assert_eq!(key1, key2);

    // Different instruction → different key.
    let key3 = CacheKey::for_metadata("WORKDIR /app", "sha256:parent1");
    assert_ne!(key1, key3);
}

// ── BuildCache Lifecycle Tests ──────────────────────────────────────────

#[test]
fn cache_open_creates_dir() {
    let tmp = tempdir().unwrap();
    let cache_dir = tmp.path().join("build-cache");

    // Directory does not exist yet.
    assert!(!cache_dir.exists());

    let cache = BuildCache::open(cache_dir.clone()).unwrap();
    assert!(cache_dir.exists(), "open must create the cache directory");

    // Layers subdirectory must also exist.
    assert!(cache_dir.join("layers").join("sha256").exists());

    // Empty cache returns None for any key.
    assert!(cache.get("sha256:nonexistent").is_none());
}

#[test]
fn cache_put_and_get() {
    let tmp = tempdir().unwrap();
    let mut cache = BuildCache::open(tmp.path().join("cache")).unwrap();

    let entry = CacheEntry {
        key: "sha256:abc123".to_owned(),
        layer_digest: "sha256:layerdigest".to_owned(),
        diff_id: "sha256:diffid".to_owned(),
        compressed_size: 4096,
        empty_layer: false,
        created_at: 1_700_000_000,
    };

    cache.put(entry.clone()).unwrap();

    let retrieved = cache.get("sha256:abc123").expect("entry must be present");
    assert_eq!(retrieved.key, "sha256:abc123");
    assert_eq!(retrieved.layer_digest, "sha256:layerdigest");
    assert_eq!(retrieved.diff_id, "sha256:diffid");
    assert_eq!(retrieved.compressed_size, 4096);
    assert!(!retrieved.empty_layer);
    assert_eq!(retrieved.created_at, 1_700_000_000);
}

#[test]
fn cache_save_and_reload() {
    let tmp = tempdir().unwrap();
    let cache_dir = tmp.path().join("cache");

    // Create and populate.
    {
        let mut cache = BuildCache::open(cache_dir.clone()).unwrap();
        cache
            .put(CacheEntry {
                key: "sha256:entry1".to_owned(),
                layer_digest: "sha256:layer1".to_owned(),
                diff_id: "sha256:diff1".to_owned(),
                compressed_size: 100,
                empty_layer: false,
                created_at: 1_700_000_000,
            })
            .unwrap();
        cache
            .put(CacheEntry {
                key: "sha256:entry2".to_owned(),
                layer_digest: "sha256:layer2".to_owned(),
                diff_id: "sha256:diff2".to_owned(),
                compressed_size: 200,
                empty_layer: true,
                created_at: 1_700_000_001,
            })
            .unwrap();
        cache.save().unwrap();
    }

    // Reopen and verify.
    let cache = BuildCache::open(cache_dir).unwrap();
    assert!(cache.get("sha256:entry1").is_some());
    assert!(cache.get("sha256:entry2").is_some());

    let e2 = cache.get("sha256:entry2").unwrap();
    assert!(e2.empty_layer);
    assert_eq!(e2.compressed_size, 200);
}

#[test]
fn cache_prune_removes_old() {
    let tmp = tempdir().unwrap();
    let mut cache = BuildCache::open(tmp.path().join("cache")).unwrap();

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    // Old entry (1 day + 1 second ago).
    cache
        .put(CacheEntry {
            key: "sha256:old".to_owned(),
            layer_digest: "sha256:layer_old".to_owned(),
            diff_id: "sha256:diff_old".to_owned(),
            compressed_size: 50,
            empty_layer: false,
            created_at: now - 86_401,
        })
        .unwrap();

    // Recent entry (10 seconds ago).
    cache
        .put(CacheEntry {
            key: "sha256:recent".to_owned(),
            layer_digest: "sha256:layer_recent".to_owned(),
            diff_id: "sha256:diff_recent".to_owned(),
            compressed_size: 75,
            empty_layer: false,
            created_at: now - 10,
        })
        .unwrap();

    // Prune entries older than 1 day (86400 seconds).
    let pruned = cache.prune(86_400).unwrap();
    assert_eq!(pruned, 1, "should prune exactly one old entry");

    assert!(cache.get("sha256:old").is_none(), "old entry must be gone");
    assert!(
        cache.get("sha256:recent").is_some(),
        "recent entry must remain"
    );
}

// ── Layer Blob Storage Tests ────────────────────────────────────────────

#[test]
fn cache_store_and_load_layer() {
    let tmp = tempdir().unwrap();
    let cache = BuildCache::open(tmp.path().join("cache")).unwrap();

    let layer_data = b"fake-compressed-layer-data-12345";
    let digest = "sha256:deadbeefcafe";

    cache.store_layer(digest, layer_data).unwrap();

    let loaded = cache
        .load_layer(digest)
        .unwrap()
        .expect("layer must be loadable after store");
    assert_eq!(loaded, layer_data);

    // Non-existent digest returns None.
    let missing = cache.load_layer("sha256:nonexistent").unwrap();
    assert!(missing.is_none());
}

// ── Content Hash Tests ──────────────────────────────────────────────────

#[test]
fn content_hash_deterministic() {
    let tmp = tempdir().unwrap();
    let dir = tmp.path();

    // Create a few files.
    fs::write(dir.join("file1.txt"), "hello world").unwrap();
    fs::create_dir_all(dir.join("subdir")).unwrap();
    fs::write(dir.join("subdir").join("file2.txt"), "nested content").unwrap();

    let hash1 = CacheKey::content_hash(&[dir.to_path_buf()]).unwrap();
    let hash2 = CacheKey::content_hash(&[dir.to_path_buf()]).unwrap();

    assert_eq!(hash1, hash2, "same files must produce same hash");
    assert!(hash1.starts_with("sha256:"));

    // Modifying content changes the hash.
    fs::write(dir.join("file1.txt"), "modified content").unwrap();
    let hash3 = CacheKey::content_hash(&[dir.to_path_buf()]).unwrap();
    assert_ne!(hash1, hash3, "modified files must produce different hash");
}

#[test]
fn content_hash_individual_files() {
    let tmp = tempdir().unwrap();
    let file_path = tmp.path().join("single.txt");
    fs::write(&file_path, "single file content").unwrap();

    let hash1 = CacheKey::content_hash(&[file_path.clone()]).unwrap();
    let hash2 = CacheKey::content_hash(&[file_path]).unwrap();
    assert_eq!(hash1, hash2);
    assert!(hash1.starts_with("sha256:"));
}
