//! Local OCI content cache.
//!
//! Stores layer tarballs by content digest and resolved manifests by
//! image reference in `~/.visor/cache/`.
//!
//! # Directory Layout
//!
//! ```text
//! {root}/
//!   blobs/
//!     sha256/
//!       44136fa355b311bba0343a...   ← layer tarball or config blob
//!       e3b0c44298fc1c149afbf4...   ← another blob
//!   manifests/
//!     registry-1.docker.io_library_alpine_latest.json  ← resolved manifest
//! ```
//!
//! Digests follow the OCI content-addressable format: `sha256:<hex>`.
//! Writes are atomic (temp file + rename) to prevent partial blobs.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use sha2::{Digest, Sha256};

/// Content-addressable blob cache for OCI layers, config blobs, and manifests.
///
/// Blobs are stored under `{root}/blobs/sha256/{hex_digest}`.
/// Manifests are stored under `{root}/manifests/{sanitized_key}.json`.
/// Writes use atomic temp-file-then-rename to prevent corruption.
#[non_exhaustive]
pub struct LayerCache {
    /// Root directory of the cache (e.g. `~/.visor/cache`).
    pub root: PathBuf,
    /// Path to the `blobs/sha256/` subdirectory.
    pub blobs_dir: PathBuf,
    /// Path to the `manifests/` subdirectory.
    pub manifests_dir: PathBuf,
}

impl LayerCache {
    /// Create a new layer cache rooted at `root`.
    ///
    /// Creates `{root}/blobs/sha256/` and `{root}/manifests/` if they do not
    /// exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the directories cannot be created.
    pub fn new(root: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let root = root.into();
        let blobs_dir = root.join("blobs").join("sha256");
        fs::create_dir_all(&blobs_dir).context("failed to create cache blobs directory")?;
        let manifests_dir = root.join("manifests");
        fs::create_dir_all(&manifests_dir).context("failed to create cache manifests directory")?;
        Ok(Self {
            root,
            blobs_dir,
            manifests_dir,
        })
    }

    /// Returns the default cache path: `$VISOR_HOME/cache` or
    /// `$HOME/.visor/cache`.
    ///
    /// # Errors
    ///
    /// Returns an error if neither `VISOR_HOME` nor `HOME` is available.
    pub fn default_path() -> anyhow::Result<PathBuf> {
        crate::paths::persistent_subdir("cache").context("determine layer cache path")
    }

    /// Check whether a blob for `digest` exists in the cache.
    #[must_use]
    pub fn has(&self, digest: &str) -> bool {
        self.blob_path(digest).is_file()
    }

    /// Return the filesystem path where `digest` would be stored.
    ///
    /// The path is `{root}/blobs/sha256/{hex}` where `hex` is the digest
    /// with the `sha256:` prefix stripped.
    #[must_use]
    pub fn blob_path(&self, digest: &str) -> PathBuf {
        let hex = digest.strip_prefix("sha256:").unwrap_or(digest);
        self.blobs_dir.join(hex)
    }

    /// Return the path to the cached blob if it exists, or `None`.
    ///
    /// # Errors
    ///
    /// Returns an error if the filesystem cannot be queried.
    pub fn get(&self, digest: &str) -> anyhow::Result<Option<PathBuf>> {
        let path = self.blob_path(digest);
        if path.is_file() {
            Ok(Some(path))
        } else {
            Ok(None)
        }
    }

    /// Write `data` into the cache under `digest`, verifying the SHA-256 hash.
    ///
    /// The write is atomic: data is written to a temporary file first, then
    /// renamed into place. If the blob already exists, it is overwritten
    /// atomically.
    ///
    /// # Errors
    ///
    /// Returns an error if the computed SHA-256 does not match `digest`,
    /// or if I/O fails.
    pub fn put(&self, digest: &str, data: &[u8]) -> anyhow::Result<PathBuf> {
        verify_digest(digest, data)?;

        let dest = self.blob_path(digest);
        atomic_write(&self.blobs_dir, &dest, data)?;
        Ok(dest)
    }

    /// Copy a file into the cache under `digest`, verifying the SHA-256 hash.
    ///
    /// The source file is read, verified, and written atomically into the
    /// cache. The source is not removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the computed SHA-256 does not match `digest`,
    /// the source file cannot be read, or I/O fails.
    pub fn put_from_file(&self, digest: &str, source: &Path) -> anyhow::Result<PathBuf> {
        let data = fs::read(source)
            .with_context(|| format!("failed to read source file {}", source.display()))?;
        self.put(digest, &data)
    }

    /// Remove a cached blob. Does nothing if the blob does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be removed.
    pub fn remove(&self, digest: &str) -> anyhow::Result<()> {
        let path = self.blob_path(digest);
        if path.is_file() {
            fs::remove_file(&path)
                .with_context(|| format!("failed to remove blob {}", path.display()))?;
        }
        Ok(())
    }

    /// Return the total size of all cached blobs in bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read.
    pub fn size(&self) -> anyhow::Result<u64> {
        let mut total: u64 = 0;
        let entries = fs::read_dir(&self.blobs_dir).context("failed to read blobs directory")?;
        for entry in entries {
            let entry = entry.context("failed to read directory entry")?;
            let meta = entry.metadata().context("failed to read blob metadata")?;
            if meta.is_file() {
                total += meta.len();
            }
        }
        Ok(total)
    }

    /// Remove all cached blobs.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be read or files cannot be
    /// removed.
    pub fn clear(&self) -> anyhow::Result<()> {
        let entries = fs::read_dir(&self.blobs_dir).context("failed to read blobs directory")?;
        for entry in entries {
            let entry = entry.context("failed to read directory entry")?;
            let path = entry.path();
            if path.is_file() {
                fs::remove_file(&path)
                    .with_context(|| format!("failed to remove blob {}", path.display()))?;
            }
        }
        Ok(())
    }

    // ── Manifest cache ──────────────────────────────────────────────

    /// Build the filesystem key for a cached manifest.
    ///
    /// Produces `{manifests_dir}/{registry}_{repo}_{tag}.json` with all
    /// `/` replaced by `_` to stay in a flat directory.
    #[must_use]
    pub fn manifest_key(&self, registry: &str, repository: &str, tag: &str) -> PathBuf {
        let safe = format!(
            "{}_{}_{}",
            registry.replace('/', "_"),
            repository.replace('/', "_"),
            tag.replace('/', "_"),
        );
        self.manifests_dir.join(format!("{safe}.json"))
    }

    /// Return the cached manifest bytes if present.
    ///
    /// # Errors
    ///
    /// Returns an error if the file exists but cannot be read.
    pub fn get_manifest(
        &self,
        registry: &str,
        repository: &str,
        tag: &str,
    ) -> anyhow::Result<Option<Vec<u8>>> {
        let path = self.manifest_key(registry, repository, tag);
        if path.is_file() {
            let data = fs::read(&path)
                .with_context(|| format!("failed to read cached manifest {}", path.display()))?;
            Ok(Some(data))
        } else {
            Ok(None)
        }
    }

    /// Store resolved manifest bytes in the cache.
    ///
    /// The write is atomic (temp file + rename).
    ///
    /// # Errors
    ///
    /// Returns an error if I/O fails.
    pub fn put_manifest(
        &self,
        registry: &str,
        repository: &str,
        tag: &str,
        data: &[u8],
    ) -> anyhow::Result<PathBuf> {
        let dest = self.manifest_key(registry, repository, tag);
        atomic_write(&self.manifests_dir, &dest, data)?;
        Ok(dest)
    }
}

/// Verify that `data` hashes to the given `digest`.
fn verify_digest(digest: &str, data: &[u8]) -> anyhow::Result<()> {
    let expected_hex = digest.strip_prefix("sha256:").unwrap_or(digest);
    let actual_hex = hex::encode(Sha256::digest(data));
    if actual_hex != expected_hex {
        bail!("digest mismatch: expected {expected_hex}, got {actual_hex}");
    }
    Ok(())
}

/// Write `data` to a temp file in `dir`, then atomically rename to `dest`.
fn atomic_write(dir: &Path, dest: &Path, data: &[u8]) -> anyhow::Result<()> {
    let tmp_path = dir.join(format!(".tmp-{}", std::process::id()));
    let mut file =
        fs::File::create(&tmp_path).context("failed to create temp file for atomic write")?;
    file.write_all(data).context("failed to write blob data")?;
    file.sync_all().context("failed to sync blob data")?;
    fs::rename(&tmp_path, dest)
        .with_context(|| format!("failed to rename temp file to {}", dest.display()))?;
    Ok(())
}

#[cfg(test)]
#[path = "cache_test.rs"]
mod tests;
