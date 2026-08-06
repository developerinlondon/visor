//! Disk-based snapshot cache for pre-warmed VM images.
//!
//! Manages filesystem paths for cached VM snapshots. The actual snapshot
//! save/restore uses the snapshot module from `visor-vmm` — this
//! module only handles cache directory layout and eviction.
//!
//! # Cache Layout
//!
//! ```text
//! ~/.visor/cache/
//! └── snapshots/
//!     ├── sha256:abc123.../snapshot.bin
//!     └── sha256:def456.../snapshot.bin
//! ```

use std::io;
use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use sha2::{Digest, Sha256};
use visor_types::VmConfig;

const MEMORY_FILE: &str = "memory.bin";
const CPU_STATE_FILE: &str = "cpu_state.json";
const ROOTFS_FILE: &str = "rootfs.ext4";

#[derive(Serialize)]
struct SnapshotCacheKey<'a> {
    image: &'a str,
    entrypoint: &'a [String],
    cmd: &'a [String],
    env: &'a [String],
    working_dir: Option<&'a str>,
    memory_mib: u32,
    vcpus: u32,
    ports: &'a [visor_types::PortMapping],
    volumes: &'a [visor_types::VolumeMount],
    extra_hosts: &'a [visor_types::HostEntry],
    networks: &'a [String],
    network_enabled: bool,
    guest_virtualization: visor_types::GuestVirtualizationMode,
    mode: Option<&'a str>,
}

/// Returns `true` when a VM config can safely use the snapshot fast path.
///
/// Snapshot bundles currently exclude external mutable volume state, so
/// configs with attached volumes must always cold-boot.
#[must_use]
pub fn supports_snapshot_fast_path(config: &VmConfig) -> bool {
    config.volumes.is_empty() && config.service_names.is_empty() && config.service_ports.is_empty()
}

/// Builds a stable cache key for a VM snapshot-able config.
///
/// The key includes the guest-shaping fields that affect the booted VM state
/// and is hashed to a `sha256:<hex>` digest for filesystem-safe cache paths.
///
/// # Errors
///
/// Returns an error if the config cannot be serialized for hashing.
pub fn snapshot_key_for_config(config: &VmConfig) -> anyhow::Result<String> {
    let key = SnapshotCacheKey {
        image: &config.image,
        entrypoint: &config.entrypoint,
        cmd: &config.cmd,
        env: &config.env,
        working_dir: config.working_dir.as_deref(),
        memory_mib: config.memory_mib,
        vcpus: config.vcpus,
        ports: &config.ports,
        volumes: &config.volumes,
        extra_hosts: &config.extra_hosts,
        networks: &config.networks,
        network_enabled: config.network_enabled,
        guest_virtualization: config.guest_virtualization,
        mode: config.mode.as_deref(),
    };
    let payload = serde_json::to_vec(&key).context("serialize snapshot cache key")?;
    let digest = Sha256::digest(payload);
    Ok(format!("sha256:{digest:x}"))
}

/// Manages snapshot cache on disk for pre-warmed VM images.
///
/// Each snapshot is stored in a subdirectory named by its image digest.
/// The cache directory is created lazily on first write (by the snapshot
/// module); this struct only manages paths and eviction.
#[non_exhaustive]
pub struct SnapshotCache {
    /// Root cache directory (e.g. `~/.visor/cache/snapshots/`).
    cache_dir: PathBuf,
}

impl SnapshotCache {
    /// Returns the default snapshot cache directory.
    ///
    /// # Errors
    ///
    /// Returns an error if neither `VISOR_HOME` nor `HOME` is available.
    pub fn default_dir() -> anyhow::Result<PathBuf> {
        Ok(crate::paths::persistent_subdir("cache")?.join("snapshots"))
    }

    /// Creates a new snapshot cache rooted at the given directory.
    ///
    /// Does not create the directory — that happens when a snapshot is
    /// actually saved by the snapshot module.
    #[must_use]
    pub fn new(cache_dir: PathBuf) -> Self {
        Self { cache_dir }
    }

    /// Returns the root cache directory.
    #[must_use]
    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    /// Returns `true` if a snapshot exists for the given image digest.
    ///
    /// Checks for the existence of the digest subdirectory within the cache.
    #[must_use]
    pub fn has_snapshot(&self, image_digest: &str) -> bool {
        let dir = self.digest_dir(image_digest);
        dir.join(MEMORY_FILE).exists()
            && dir.join(CPU_STATE_FILE).exists()
            && dir.join(ROOTFS_FILE).exists()
    }

    /// Returns the filesystem path where a snapshot for the given digest
    /// would be stored.
    ///
    /// The path may not exist yet — use [`has_snapshot`](Self::has_snapshot)
    /// to check.
    #[must_use]
    pub fn snapshot_path(&self, image_digest: &str) -> PathBuf {
        self.snapshot_dir(image_digest).join(MEMORY_FILE)
    }

    /// Returns the directory for a cached snapshot bundle.
    #[must_use]
    pub fn snapshot_dir(&self, image_digest: &str) -> PathBuf {
        self.digest_dir(image_digest)
    }

    /// Lists all cached image digests.
    ///
    /// Returns digest strings for all subdirectories in the cache directory.
    /// Returns an empty list if the cache directory does not exist.
    #[must_use]
    pub fn list_cached(&self) -> Vec<String> {
        let Ok(entries) = std::fs::read_dir(&self.cache_dir) else {
            return Vec::new();
        };

        entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                if entry.path().is_dir() {
                    entry.file_name().to_str().map(String::from)
                } else {
                    None
                }
            })
            .collect()
    }

    /// Evicts the cached snapshot for the given image digest.
    ///
    /// Removes the entire digest subdirectory and its contents.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the directory exists but cannot be removed.
    /// Returns `Ok(())` if the directory does not exist (idempotent).
    pub fn evict(&self, image_digest: &str) -> io::Result<()> {
        let dir = self.digest_dir(image_digest);
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        Ok(())
    }

    /// Returns the subdirectory for a specific digest.
    fn digest_dir(&self, image_digest: &str) -> PathBuf {
        // Sanitize the digest to be a valid directory name by replacing
        // colons with underscores (e.g. "sha256:abc" → "sha256_abc").
        let safe_name = image_digest.replace(':', "_");
        self.cache_dir.join(safe_name)
    }
}

#[cfg(test)]
#[path = "snapshot_cache_test.rs"]
mod tests;
