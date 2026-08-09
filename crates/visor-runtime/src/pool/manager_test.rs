use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use async_trait::async_trait;

use crate::backend::{ExecRequest, ExecResult, ExecutionBackend, VmConfig, VmInfo, VmState};
use crate::pool::snapshot_cache::{SnapshotCache, snapshot_key_for_config};

use super::*;

// ── Mock backend ────────────────────────────────────────────────

/// Mock backend that creates fake VMs without real KVM.
struct MockBackend {
    next_id: AtomicU32,
    /// Simulate creation failure when true.
    fail_create: bool,
}

impl MockBackend {
    fn new() -> Self {
        Self {
            next_id: AtomicU32::new(1),
            fail_create: false,
        }
    }

    fn failing() -> Self {
        Self {
            next_id: AtomicU32::new(1),
            fail_create: true,
        }
    }
}

#[async_trait]
impl ExecutionBackend for MockBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        if self.fail_create {
            anyhow::bail!("mock create failure");
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(VmInfo::new(
            format!("mock-vm-{id}"),
            config.image,
            VmState::Running,
            "2025-01-01T00:00:00Z".to_owned(),
            config.memory_mib,
            config.vcpus,
        ))
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(vec![])
    }

    async fn get(&self, _id: &str) -> anyhow::Result<VmInfo> {
        anyhow::bail!("not implemented in mock")
    }

    async fn exec(&self, _id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        anyhow::bail!("not implemented in mock")
    }

    async fn stop(&self, _id: &str, _timeout_secs: u64) -> anyhow::Result<()> {
        Ok(())
    }

    async fn kill(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn destroy(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn console_output(&self, _id: &str) -> anyhow::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

// ── Snapshot-aware mock ─────────────────────────────────────────

/// Mock backend that tracks whether `create_from_snapshot` was called.
struct SnapshotAwareMockBackend {
    next_id: AtomicU32,
    from_snapshot_called: AtomicBool,
    create_called: AtomicBool,
}

impl SnapshotAwareMockBackend {
    fn new() -> Self {
        Self {
            next_id: AtomicU32::new(100),
            from_snapshot_called: AtomicBool::new(false),
            create_called: AtomicBool::new(false),
        }
    }

    fn was_create_from_snapshot_called(&self) -> bool {
        self.from_snapshot_called.load(Ordering::SeqCst)
    }

    fn was_create_called(&self) -> bool {
        self.create_called.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl ExecutionBackend for SnapshotAwareMockBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        self.create_called.store(true, Ordering::SeqCst);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(VmInfo::new(
            format!("mock-vm-{id}"),
            config.image,
            VmState::Running,
            "2025-01-01T00:00:00Z".to_owned(),
            config.memory_mib,
            config.vcpus,
        ))
    }

    async fn create_from_snapshot(
        &self,
        config: VmConfig,
        _snapshot_dir: &std::path::Path,
    ) -> anyhow::Result<VmInfo> {
        self.from_snapshot_called.store(true, Ordering::SeqCst);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(VmInfo::new(
            format!("snap-vm-{id}"),
            config.image,
            VmState::Running,
            "2025-01-01T00:00:00Z".to_owned(),
            config.memory_mib,
            config.vcpus,
        ))
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(vec![])
    }

    async fn get(&self, _id: &str) -> anyhow::Result<VmInfo> {
        anyhow::bail!("not implemented in mock")
    }

    async fn exec(&self, _id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        anyhow::bail!("not implemented in mock")
    }

    async fn stop(&self, _id: &str, _timeout_secs: u64) -> anyhow::Result<()> {
        Ok(())
    }

    async fn kill(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn destroy(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn console_output(&self, _id: &str) -> anyhow::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

// ── Helpers ─────────────────────────────────────────────────────

fn default_pool_config() -> PoolConfig {
    PoolConfig::default()
}

fn pool_config_with_image(image: &str, size: usize, memory_mib: u32) -> PoolConfig {
    let mut image_configs = HashMap::new();
    image_configs.insert(image.to_owned(), ImagePoolConfig { size, memory_mib });
    PoolConfig {
        default_size: 3,
        image_configs,
    }
}

/// Returns a `SnapshotCache` pointing at a non-existent temp directory.
///
/// Since no snapshot directories exist, `has_snapshot()` always returns `false`.
fn default_snapshot_cache() -> SnapshotCache {
    SnapshotCache::new(
        pool_test_root().join(format!("visor-pool-test-default-{}", std::process::id())),
    )
}

/// Creates a temp directory for snapshot cache tests.
fn temp_cache_dir(test_name: &str) -> PathBuf {
    let root = pool_test_root();
    std::fs::create_dir_all(&root).expect("create pool test root");
    let dir = root.join(format!(
        "visor-pool-test-{test_name}-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create temp cache dir");
    dir
}

fn pool_test_root() -> PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("visor-runtime-pool-tests")
}

fn write_snapshot_bundle(cache_dir: &std::path::Path, key: &str) {
    let snapshot_dir = cache_dir.join(key.replace(':', "_"));
    std::fs::create_dir_all(&snapshot_dir).unwrap();
    std::fs::write(snapshot_dir.join("memory.bin"), b"memory").unwrap();
    std::fs::write(snapshot_dir.join("cpu_state.json"), b"{}").unwrap();
    std::fs::write(snapshot_dir.join("rootfs.ext4"), b"rootfs").unwrap();
}

// ── PoolConfig defaults ─────────────────────────────────────────

#[test]
fn pool_config_default_size_is_3() {
    let config = PoolConfig::default();
    assert_eq!(config.default_size, 3);
}

#[test]
fn pool_config_default_has_no_image_configs() {
    let config = PoolConfig::default();
    assert!(config.image_configs.is_empty());
}

// ── PoolManager::new ────────────────────────────────────────────

#[tokio::test]
async fn new_creates_empty_pool() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    let status = manager.status().await;
    assert_eq!(status.total, 0);
}

// ── PoolManager::warm ───────────────────────────────────────────

#[tokio::test]
async fn warm_creates_vms_in_pool() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    manager.warm("alpine:latest", 2).await.unwrap();

    let status = manager.status().await;
    assert_eq!(status.total, 2);
    let img_status = status.images.get("alpine:latest").unwrap();
    assert_eq!(img_status.available, 2);
}

#[tokio::test]
async fn warm_with_zero_count_succeeds() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    manager.warm("alpine:latest", 0).await.unwrap();
    assert_eq!(manager.status().await.total, 0);
}

#[tokio::test]
async fn warm_returns_error_when_all_creations_fail() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::failing());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    let result = manager.warm("alpine:latest", 3).await;
    assert!(result.is_err(), "expected error when all creations fail");
}

// ── PoolManager::acquire ────────────────────────────────────────

#[tokio::test]
async fn acquire_returns_pooled_vm() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    manager.warm("alpine:latest", 1).await.unwrap();

    let vm = manager.acquire("alpine:latest").await.unwrap();
    assert_eq!(vm.image, "alpine:latest");
    assert_eq!(vm.state, VmState::Running);

    // Pool should now be empty
    assert_eq!(manager.status().await.total, 0);
}

#[tokio::test]
async fn acquire_falls_back_to_on_demand_when_pool_empty() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    // Don't warm — pool is empty
    let vm = manager.acquire("ubuntu:latest").await.unwrap();
    assert_eq!(vm.image, "ubuntu:latest");
}

#[tokio::test]
async fn acquire_returns_error_when_both_pool_and_fallback_fail() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::failing());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    let result = manager.acquire("alpine:latest").await;
    assert!(result.is_err());
}

// ── PoolManager::status ─────────────────────────────────────────

#[tokio::test]
async fn status_includes_configured_images_with_zero_vms() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let config = pool_config_with_image("alpine:latest", 5, 256);
    let manager = PoolManager::new(config, backend, default_snapshot_cache());

    let status = manager.status().await;
    let img = status.images.get("alpine:latest").unwrap();
    assert_eq!(img.available, 0);
    assert_eq!(img.target, 5);
}

#[tokio::test]
async fn status_uses_default_target_for_unconfigured_images() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    manager.warm("debian:latest", 1).await.unwrap();

    let status = manager.status().await;
    let img = status.images.get("debian:latest").unwrap();
    assert_eq!(img.target, 3); // default_size
}

// ── PoolManager::drain ──────────────────────────────────────────

#[tokio::test]
async fn drain_empties_all_pools() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    manager.warm("alpine:latest", 2).await.unwrap();
    manager.warm("ubuntu:latest", 1).await.unwrap();
    assert_eq!(manager.status().await.total, 3);

    manager.drain().await.unwrap();
    assert_eq!(manager.status().await.total, 0);
}

#[tokio::test]
async fn drain_on_empty_pool_succeeds() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    manager.drain().await.unwrap();
}

#[tokio::test]
async fn refill_once_creates_missing_warm_capacity() {
    let config = pool_config_with_image("alpine:latest", 2, 256);
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(config, backend, default_snapshot_cache());

    manager.refill_once().await.unwrap();

    let status = manager.status().await;
    let alpine = status.images.get("alpine:latest").unwrap();
    assert_eq!(alpine.available, 2);
    assert_eq!(alpine.target, 2);
}

#[tokio::test]
async fn refill_once_returns_error_when_warm_fails() {
    let config = pool_config_with_image("alpine:latest", 1, 256);
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::failing());
    let manager = PoolManager::new(config, backend, default_snapshot_cache());

    let err = manager.refill_once().await.unwrap_err();

    assert!(
        err.to_string().contains("pool refill errors"),
        "unexpected error: {err:?}"
    );
}

// ── Image-specific config ───────────────────────────────────────

#[tokio::test]
async fn warm_uses_image_specific_memory_config() {
    let backend = Arc::new(MockBackend::new());
    let backend_clone: Arc<dyn ExecutionBackend> = backend.clone();
    let config = pool_config_with_image("alpine:latest", 2, 256);
    let manager = PoolManager::new(config, backend_clone, default_snapshot_cache());

    manager.warm("alpine:latest", 1).await.unwrap();

    let vm = manager.acquire("alpine:latest").await.unwrap();
    assert_eq!(vm.memory_mib, 256);
}

// ── PoolStatus serialization ────────────────────────────────────

#[test]
fn pool_status_serializes_correctly() {
    let mut images = HashMap::new();
    images.insert(
        "alpine:latest".to_owned(),
        ImagePoolStatus {
            available: 2,
            target: 3,
        },
    );
    let status = PoolStatus { images, total: 2 };

    let json = serde_json::to_value(&status).unwrap();
    assert_eq!(json["total"], 2);
    assert_eq!(json["images"]["alpine:latest"]["available"], 2);
    assert_eq!(json["images"]["alpine:latest"]["target"], 3);
}

#[test]
fn pool_status_deserializes_round_trip() {
    let mut images = HashMap::new();
    images.insert(
        "test:latest".to_owned(),
        ImagePoolStatus {
            available: 1,
            target: 5,
        },
    );
    let status = PoolStatus { images, total: 1 };

    let json = serde_json::to_string(&status).unwrap();
    let deserialized: PoolStatus = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized.total, 1);
    assert_eq!(deserialized.images.get("test:latest").unwrap().available, 1);
}

// ── PoolManager::acquire_with_config ─────────────────────────

#[tokio::test]
async fn acquire_with_config_returns_pooled_vm_when_creation_config_matches() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    manager.warm("alpine:latest", 1).await.unwrap();

    let mut config = VmConfig::new("alpine:latest");
    config.detach = true;

    let vm = manager.acquire_with_config(config).await.unwrap();
    assert_eq!(vm.image, "alpine:latest");

    assert_eq!(manager.status().await.total, 0);
}

#[tokio::test]
async fn acquire_with_config_preserves_incompatible_warm_vm_when_limits_differ() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    manager.warm("alpine:latest", 1).await.unwrap();

    let mut config = VmConfig::new("alpine:latest");
    config.detach = true;
    config.process_limit = Some(256);
    config.rootfs_extra_size_mib = Some(1024);

    let vm = manager.acquire_with_config(config).await.unwrap();

    assert_eq!(vm.id, "mock-vm-2");
    assert_eq!(manager.status().await.total, 1);
}

#[tokio::test]
async fn acquire_with_config_falls_back_to_create_with_user_config() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    // Pool is empty — no warm VMs
    let mut config = VmConfig::new("alpine:latest");
    config.memory_mib = 1024;
    config.vcpus = 2;

    let vm = manager.acquire_with_config(config).await.unwrap();
    assert_eq!(vm.image, "alpine:latest");
    // Backend mock uses the config we passed, so it should use user's memory/vcpus
    assert_eq!(vm.memory_mib, 1024);
    assert_eq!(vm.vcpus, 2);
}

#[tokio::test]
async fn acquire_with_config_returns_error_when_both_fail() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::failing());
    let manager = PoolManager::new(default_pool_config(), backend, default_snapshot_cache());

    let config = VmConfig::new("alpine:latest");
    let result = manager.acquire_with_config(config).await;
    assert!(result.is_err());
}

// ── Snapshot cache integration ──────────────────────────────────

#[test]
fn pool_manager_has_snapshot_cache() {
    let cache = SnapshotCache::new(PathBuf::from("/tmp/visor-test-cache"));
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, cache);

    assert_eq!(
        manager.snapshot_cache().cache_dir(),
        std::path::Path::new("/tmp/visor-test-cache")
    );
}

#[tokio::test]
async fn acquire_uses_snapshot_when_available() {
    let cache_dir = temp_cache_dir("acquire-snapshot");
    let cache = SnapshotCache::new(cache_dir.clone());

    let image = "sha256:test123";
    let key = snapshot_key_for_config(&VmConfig::new(image)).unwrap();
    write_snapshot_bundle(&cache_dir, &key);
    assert!(cache.has_snapshot(&key), "sanity: snapshot dir must exist");

    let backend = Arc::new(SnapshotAwareMockBackend::new());
    let backend_dyn: Arc<dyn ExecutionBackend> = backend.clone();
    let manager = PoolManager::new(default_pool_config(), backend_dyn, cache);

    let vm = manager.acquire(image).await.unwrap();
    assert_eq!(vm.image, image);
    assert!(
        backend.was_create_from_snapshot_called(),
        "expected create_from_snapshot to be called"
    );
    assert!(
        !backend.was_create_called(),
        "expected create NOT to be called when snapshot exists"
    );

    let _ = std::fs::remove_dir_all(&cache_dir);
}

#[tokio::test]
async fn acquire_falls_back_to_create_when_no_snapshot() {
    let cache_dir = temp_cache_dir("acquire-no-snapshot");
    let cache = SnapshotCache::new(cache_dir.clone());

    // No snapshot directory exists for this image.
    let image = "no-snapshot:latest";

    let backend = Arc::new(SnapshotAwareMockBackend::new());
    let backend_dyn: Arc<dyn ExecutionBackend> = backend.clone();
    let manager = PoolManager::new(default_pool_config(), backend_dyn, cache);

    let vm = manager.acquire(image).await.unwrap();
    assert_eq!(vm.image, image);
    assert!(
        backend.was_create_called(),
        "expected create to be called when no snapshot"
    );
    assert!(
        !backend.was_create_from_snapshot_called(),
        "expected create_from_snapshot NOT to be called"
    );

    let _ = std::fs::remove_dir_all(&cache_dir);
}

#[tokio::test]
async fn acquire_with_config_skips_snapshot_when_volumes_are_present() {
    let cache_dir = temp_cache_dir("acquire-config-volumes");
    let cache = SnapshotCache::new(cache_dir.clone());

    let mut config = VmConfig::new("alpine:latest");
    config
        .volumes
        .push(crate::backend::VolumeMount::read_only("/host", "/guest"));

    let backend = Arc::new(SnapshotAwareMockBackend::new());
    let backend_dyn: Arc<dyn ExecutionBackend> = backend.clone();
    let manager = PoolManager::new(default_pool_config(), backend_dyn, cache);

    let vm = manager.acquire_with_config(config).await.unwrap();
    assert_eq!(vm.image, "alpine:latest");
    assert!(
        backend.was_create_called(),
        "expected create to be called when volumes disable snapshots"
    );
    assert!(
        !backend.was_create_from_snapshot_called(),
        "expected create_from_snapshot NOT to be called when volumes disable snapshots"
    );

    let _ = std::fs::remove_dir_all(&cache_dir);
}

#[tokio::test]
async fn acquire_with_config_skips_plain_snapshot_when_networking_changes() {
    let cache_dir = temp_cache_dir("acquire-config-network");
    let cache = SnapshotCache::new(cache_dir.clone());

    let mut plain_config = VmConfig::new("alpine:latest");
    plain_config.network_enabled = false;
    let plain_key = snapshot_key_for_config(&plain_config).unwrap();
    write_snapshot_bundle(&cache_dir, &plain_key);
    assert!(
        cache.has_snapshot(&plain_key),
        "sanity: snapshot dir must exist"
    );

    let mut config = VmConfig::new("alpine:latest");
    config.network_enabled = true;

    let backend = Arc::new(SnapshotAwareMockBackend::new());
    let backend_dyn: Arc<dyn ExecutionBackend> = backend.clone();
    let manager = PoolManager::new(default_pool_config(), backend_dyn, cache);

    let vm = manager.acquire_with_config(config).await.unwrap();
    assert_eq!(vm.image, "alpine:latest");
    assert!(
        backend.was_create_called(),
        "expected create to be called when networking changes the snapshot key"
    );
    assert!(
        !backend.was_create_from_snapshot_called(),
        "expected create_from_snapshot NOT to be called when plain snapshot key does not match"
    );

    let _ = std::fs::remove_dir_all(&cache_dir);
}

#[tokio::test]
async fn warm_does_not_save_snapshot() {
    let cache_dir = temp_cache_dir("warm-no-snapshot");
    let cache = SnapshotCache::new(cache_dir.clone());

    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::new());
    let manager = PoolManager::new(default_pool_config(), backend, cache);

    manager.warm("alpine:latest", 2).await.unwrap();

    // Snapshot cache should remain empty — warm does not save snapshots.
    assert!(
        manager.snapshot_cache().list_cached().is_empty(),
        "warm should not create snapshots"
    );

    let _ = std::fs::remove_dir_all(&cache_dir);
}
