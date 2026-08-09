//! Warm VM pool manager: pre-warmed VMs per image with fast acquisition.
//!
//! The [`PoolManager`] maintains a set of pre-created VMs organized by OCI
//! image reference. When a client requests a VM for a specific image, the pool
//! returns an already-booted instance instead of going through the full
//! OCI pull → rootfs → boot pipeline.
//!
//! # Architecture
//!
//! ```text
//! PoolManager
//!   ├── pools: HashMap<image, Vec<PooledVm>>
//!   ├── config: PoolConfig (sizes per image)
//!   ├── snapshot_cache: SnapshotCache
//!   └── backend: Arc<dyn ExecutionBackend>
//!         ├── create(VmConfig{detach: true}) → VmInfo
//!         └── create_from_snapshot(VmConfig, &Path) → VmInfo
//! ```

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use utoipa::ToSchema;

use super::snapshot_cache::SnapshotCache;
use crate::backend::{ExecutionBackend, VmConfig, VmInfo};

// ── PoolConfig ──────────────────────────────────────────────────

/// Configuration for the warm VM pool.
///
/// Specifies the default pool size and per-image overrides.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PoolConfig {
    /// Default number of warm VMs to maintain per image when no
    /// image-specific override is configured.
    pub default_size: usize,
    /// Per-image pool configuration overrides.
    pub image_configs: HashMap<String, ImagePoolConfig>,
}

impl Default for PoolConfig {
    fn default() -> Self {
        Self {
            default_size: 3,
            image_configs: HashMap::new(),
        }
    }
}

/// Per-image pool configuration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ImagePoolConfig {
    /// Number of warm VMs to keep in the pool for this image.
    pub size: usize,
    /// Memory allocation in MiB for each pooled VM.
    pub memory_mib: u32,
}

// ── PooledVm ────────────────────────────────────────────────────

/// A pre-warmed VM sitting in the pool waiting to be acquired.
#[derive(Debug)]
struct PooledVm {
    /// Runtime information about this VM.
    info: VmInfo,
}

// ── PoolStatus ──────────────────────────────────────────────────

/// Status of the warm VM pool, returned by the API.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct PoolStatus {
    /// Number of available (pre-warmed) VMs per image.
    pub images: HashMap<String, ImagePoolStatus>,
    /// Total number of pooled VMs across all images.
    pub total: usize,
}

/// Per-image pool status.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct ImagePoolStatus {
    /// Number of available pre-warmed VMs for this image.
    pub available: usize,
    /// Configured target pool size for this image.
    pub target: usize,
}

// ── PoolManager ─────────────────────────────────────────────────

/// Manages warm VM pools organized by OCI image reference.
///
/// Pre-creates VMs in detached mode via the execution backend so they
/// are ready for instant acquisition. Each image can have its own pool
/// size configuration.
pub struct PoolManager {
    config: PoolConfig,
    pools: RwLock<HashMap<String, Vec<PooledVm>>>,
    backend: Arc<dyn ExecutionBackend>,
    snapshot_cache: SnapshotCache,
}

impl PoolManager {
    /// Creates a new pool manager with the given config, backend, and snapshot cache.
    #[must_use]
    pub fn new(
        config: PoolConfig,
        backend: Arc<dyn ExecutionBackend>,
        snapshot_cache: SnapshotCache,
    ) -> Self {
        Self {
            config,
            pools: RwLock::new(HashMap::new()),
            backend,
            snapshot_cache,
        }
    }

    /// Returns a reference to the snapshot cache.
    #[must_use]
    pub fn snapshot_cache(&self) -> &SnapshotCache {
        &self.snapshot_cache
    }

    /// Acquires a pre-warmed VM for the given image.
    ///
    /// Checks in order:
    /// 1. Pool — returns a pre-warmed VM if one exists for this image.
    /// 2. Snapshot cache — creates a VM from a cached snapshot (fast path).
    /// 3. On-demand — creates a VM from scratch via the full OCI pipeline.
    ///
    /// # Errors
    ///
    /// Returns an error if all creation paths fail.
    pub async fn acquire(&self, image: &str) -> anyhow::Result<VmInfo> {
        // 1. Try to pop a pre-warmed VM from the pool.
        {
            let mut pools = self.pools.write().await;
            if let Some(pool) = pools.get_mut(image) {
                if let Some(vm) = pool.pop() {
                    tracing::info!(image, vm_id = %vm.info.id, "acquired VM from warm pool");
                    return Ok(vm.info);
                }
            }
        }

        // 2. Check snapshot cache for fast restore.
        let config = self.vm_config_for_image(image);
        let snapshot_key = super::snapshot_cache::snapshot_key_for_config(&config)
            .context("build snapshot cache key for pooled image")?;
        if self.snapshot_cache.has_snapshot(&snapshot_key) {
            let snapshot_path = self.snapshot_cache.snapshot_path(&snapshot_key);
            if let Some(snapshot_dir) = snapshot_path.parent() {
                tracing::info!(image, snapshot_dir = %snapshot_dir.display(), "restoring VM from snapshot");
                return self
                    .backend
                    .create_from_snapshot(config, snapshot_dir)
                    .await
                    .context("VM creation from snapshot");
            }
        }

        // 3. Fallback: create on-demand.
        tracing::info!(
            image,
            "no warm VM or snapshot available, creating on-demand"
        );
        self.backend
            .create(config)
            .await
            .context("on-demand VM creation after empty pool")
    }

    /// Acquires a pre-warmed VM for the given image, using the caller's
    /// config for fallback creation instead of the pool's default config.
    ///
    /// Checks the pool first, then the snapshot cache, then falls back to
    /// on-demand creation with the user's original config preserved.
    ///
    /// # Errors
    ///
    /// Returns an error if all creation paths fail.
    pub async fn acquire_with_config(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        // 1. Reuse a pre-warmed VM only when its complete creation config
        // matches. A running VM cannot retroactively adopt resource, command,
        // network, or filesystem settings from a different request.
        if config == self.vm_config_for_image(&config.image) {
            let mut pools = self.pools.write().await;
            if let Some(pool) = pools.get_mut(&config.image) {
                if let Some(vm) = pool.pop() {
                    tracing::info!(
                        image = %config.image,
                        vm_id = %vm.info.id,
                        "acquired VM from warm pool (user config)",
                    );
                    return Ok(vm.info);
                }
            }
        }

        // 2. Check snapshot cache for fast restore.
        if super::snapshot_cache::supports_snapshot_fast_path(&config) {
            let snapshot_key = super::snapshot_cache::snapshot_key_for_config(&config)
                .context("build snapshot cache key for user config")?;
            if self.snapshot_cache.has_snapshot(&snapshot_key) {
                let snapshot_path = self.snapshot_cache.snapshot_path(&snapshot_key);
                if let Some(snapshot_dir) = snapshot_path.parent() {
                    tracing::info!(
                        image = %config.image,
                        snapshot_dir = %snapshot_dir.display(),
                        "restoring VM from snapshot (user config)",
                    );
                    return self
                        .backend
                        .create_from_snapshot(config, snapshot_dir)
                        .await
                        .context("VM creation from snapshot with user config");
                }
            }
        }

        // 3. Fallback: create on-demand with the user's original config.
        tracing::info!(image = %config.image, "no warm VM or snapshot available, creating on-demand with user config");
        self.backend
            .create(config)
            .await
            .context("on-demand VM creation with user config after empty pool")
    }

    /// Pre-warms the pool by creating `count` VMs for the given image.
    ///
    /// VMs are created in detached mode so they boot and remain running.
    /// Successfully created VMs are added to the pool even if some
    /// creations fail.
    ///
    /// # Errors
    ///
    /// Returns an error if all VM creations fail. Partial success is not
    /// considered an error — a warning is logged for each failure.
    pub async fn warm(&self, image: &str, count: usize) -> anyhow::Result<()> {
        tracing::info!(image, count, "warming pool");
        let mut created = 0u32;
        let mut last_err: Option<anyhow::Error> = None;

        for i in 0..count {
            let config = self.vm_config_for_image(image);
            match self.backend.create(config).await {
                Ok(info) => {
                    let mut pools = self.pools.write().await;
                    let pool = pools.entry(image.to_owned()).or_default();
                    pool.push(PooledVm { info });
                    created += 1;
                }
                Err(e) => {
                    tracing::warn!(image, index = i, error = %e, "failed to warm VM");
                    last_err = Some(e);
                }
            }
        }

        if created == 0 {
            if let Some(e) = last_err {
                return Err(e).context(format!("all {count} warm attempts failed for {image}"));
            }
        }

        tracing::info!(image, created, "pool warming complete");
        Ok(())
    }

    /// Returns the current pool status: available VMs per image and totals.
    pub async fn status(&self) -> PoolStatus {
        let pools = self.pools.read().await;
        let mut images = HashMap::new();
        let mut total = 0;

        for (image, vms) in pools.iter() {
            let available = vms.len();
            total += available;
            let target = self.target_size(image);
            images.insert(image.clone(), ImagePoolStatus { available, target });
        }

        // Include configured images that have no VMs yet.
        for (image, img_cfg) in &self.config.image_configs {
            images.entry(image.clone()).or_insert(ImagePoolStatus {
                available: 0,
                target: img_cfg.size,
            });
        }

        PoolStatus { images, total }
    }

    /// Drains all pools, stopping every pooled VM.
    ///
    /// After draining, the pools are empty. Any errors stopping individual
    /// VMs are logged as warnings but do not prevent other VMs from being
    /// stopped.
    ///
    /// # Errors
    ///
    /// Returns an error if no VMs could be stopped at all (all stop
    /// attempts fail). Partial success is not an error.
    pub async fn drain(&self) -> anyhow::Result<()> {
        let all_vms: Vec<(String, VmInfo)> = {
            let mut pools = self.pools.write().await;
            let mut collected = Vec::new();
            for (image, vms) in pools.drain() {
                for vm in vms {
                    collected.push((image.clone(), vm.info));
                }
            }
            collected
        };

        if all_vms.is_empty() {
            return Ok(());
        }

        tracing::info!(count = all_vms.len(), "draining warm pool");
        let mut stopped = 0u32;

        for (image, vm) in &all_vms {
            if let Err(e) = self.backend.stop(&vm.id, 10).await {
                tracing::warn!(
                    image,
                    vm_id = %vm.id,
                    error = %e,
                    "failed to stop pooled VM during drain"
                );
            } else {
                stopped += 1;
            }
        }

        tracing::info!(stopped, total = all_vms.len(), "pool drain complete");
        Ok(())
    }

    /// Refill all known pools up to their configured target sizes.
    ///
    /// Considers both explicitly configured images and any images that already
    /// have warm VMs tracked in memory.
    ///
    /// # Errors
    ///
    /// Returns an error if one or more refill attempts fail.
    pub async fn refill_once(&self) -> anyhow::Result<()> {
        let refill_targets: Vec<(String, usize)> = {
            let pools = self.pools.read().await;
            let mut desired = Vec::new();
            let mut seen = std::collections::BTreeSet::new();

            for image in self.config.image_configs.keys().chain(pools.keys()) {
                if !seen.insert(image.clone()) {
                    continue;
                }
                let available = pools.get(image).map_or(0, Vec::len);
                let target = self.target_size(image);
                if available < target {
                    desired.push((image.clone(), target - available));
                }
            }

            desired
        };

        if refill_targets.is_empty() {
            return Ok(());
        }

        let mut errors = Vec::new();
        for (image, missing) in refill_targets {
            if let Err(error) = self.warm(&image, missing).await {
                tracing::warn!(image, missing, error = %error, "pool refill failed");
                errors.push(format!("{image}: {error}"));
            }
        }

        if errors.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("pool refill errors: {}", errors.join("; "));
        }
    }

    /// Returns the target pool size for an image.
    fn target_size(&self, image: &str) -> usize {
        self.config
            .image_configs
            .get(image)
            .map_or(self.config.default_size, |cfg| cfg.size)
    }

    /// Builds a `VmConfig` for creating a pooled VM from the given image.
    fn vm_config_for_image(&self, image: &str) -> VmConfig {
        let memory_mib = self
            .config
            .image_configs
            .get(image)
            .map_or(512, |cfg| cfg.memory_mib);

        let mut config = VmConfig::new(image);
        config.memory_mib = memory_mib;
        config.detach = true;
        config
    }
}

#[cfg(test)]
#[path = "manager_test.rs"]
mod tests;
