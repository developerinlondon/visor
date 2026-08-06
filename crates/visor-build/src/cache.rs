//! Content-addressable build cache for Dockerfile instructions.
//!
//! Caches layer outputs keyed by instruction content + parent state,
//! allowing incremental rebuilds where unchanged instructions complete
//! instantly.
//!
//! # Cache directory layout
//!
//! ```text
//! ~/.visor/build-cache/
//!   instructions.json        ← cache key → entry mapping
//!   layers/
//!     sha256/{hash}          ← layer tar.gz blobs
//! ```

use std::collections::HashMap;
use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

// ── Constants ───────────────────────────────────────────────────────────

/// Name of the cache index file within the cache directory.
const INDEX_FILE: &str = "instructions.json";

/// Subdirectory for layer blobs.
const LAYERS_DIR: &str = "layers";

/// Algorithm prefix for content-addressable storage.
const SHA256_DIR: &str = "sha256";

/// Null byte separator used between key components during hashing.
const KEY_SEPARATOR: u8 = 0x00;

// ── CacheEntry ──────────────────────────────────────────────────────────

/// A single cached instruction result.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CacheEntry {
    /// The cache key (sha256 hash).
    pub key: String,
    /// Digest of the cached layer (sha256:...).
    pub layer_digest: String,
    /// `DiffID` of the cached layer.
    pub diff_id: String,
    /// Compressed size in bytes.
    pub compressed_size: u64,
    /// Whether this is a metadata-only instruction (no layer data).
    pub empty_layer: bool,
    /// When the entry was created (unix timestamp).
    pub created_at: u64,
}

// ── CacheKey ────────────────────────────────────────────────────────────

/// Cache key computation for different instruction types.
///
/// All keys use SHA-256 with null-byte separators between components,
/// producing deterministic `sha256:{hex}` strings.
pub struct CacheKey;

impl CacheKey {
    /// Compute cache key for a RUN instruction.
    ///
    /// Key = `sha256(instruction_text + parent_layer_digest)`.
    #[must_use]
    pub fn for_run(instruction: &str, parent_digest: &str) -> String {
        compute_key(&[instruction, parent_digest])
    }

    /// Compute cache key for a COPY/ADD instruction.
    ///
    /// Key = `sha256(instruction_text + parent_layer_digest + content_hash)`.
    #[must_use]
    pub fn for_copy(instruction: &str, parent_digest: &str, content_hash: &str) -> String {
        compute_key(&[instruction, parent_digest, content_hash])
    }

    /// Compute cache key for metadata instructions (ENV, WORKDIR, CMD, etc).
    ///
    /// Key = `sha256(instruction_text + parent_digest)`.
    #[must_use]
    pub fn for_metadata(instruction: &str, parent_digest: &str) -> String {
        compute_key(&[instruction, parent_digest])
    }

    /// Compute content hash of source files for COPY/ADD cache keys.
    ///
    /// Walks source paths, hashing file content + mtime + mode.
    /// Entries are sorted by path for deterministic output.
    ///
    /// # Errors
    ///
    /// Returns an error if any path cannot be read or metadata is unavailable.
    pub fn content_hash(paths: &[PathBuf]) -> anyhow::Result<String> {
        let mut file_hashes: Vec<(String, Vec<u8>)> = Vec::new();

        for path in paths {
            collect_file_hashes(path, path, &mut file_hashes)
                .with_context(|| format!("hashing content at {}", path.display()))?;
        }

        // Sort by relative path for determinism.
        file_hashes.sort_by(|a, b| a.0.cmp(&b.0));

        // Combine all file hashes into a single digest.
        let mut hasher = Sha256::new();
        for (_rel_path, hash) in &file_hashes {
            hasher.update(hash);
        }

        Ok(format!("sha256:{:x}", hasher.finalize()))
    }
}

/// Compute a SHA-256 cache key from multiple string components.
///
/// Components are separated by null bytes to prevent ambiguity.
fn compute_key(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([KEY_SEPARATOR]);
    }
    format!("sha256:{:x}", hasher.finalize())
}

/// Recursively collect file hashes from a path.
///
/// For each file, hashes: `{relative_path}\0{mtime}\0{mode}\0{content}`.
/// Directories are recursed into with children sorted by name.
fn collect_file_hashes(
    root: &Path,
    path: &Path,
    out: &mut Vec<(String, Vec<u8>)>,
) -> anyhow::Result<()> {
    let metadata =
        fs::metadata(path).with_context(|| format!("reading metadata for {}", path.display()))?;

    if metadata.is_file() {
        let rel_path = path
            .strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .to_string();

        let mtime = metadata
            .modified()
            .unwrap_or(SystemTime::UNIX_EPOCH)
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mode = file_mode(&metadata);

        let mut content = Vec::new();
        fs::File::open(path)
            .with_context(|| format!("opening {}", path.display()))?
            .read_to_end(&mut content)
            .with_context(|| format!("reading {}", path.display()))?;

        let mut hasher = Sha256::new();
        hasher.update(rel_path.as_bytes());
        hasher.update([KEY_SEPARATOR]);
        hasher.update(mtime.to_string().as_bytes());
        hasher.update([KEY_SEPARATOR]);
        hasher.update(mode.to_string().as_bytes());
        hasher.update([KEY_SEPARATOR]);
        hasher.update(&content);

        let hash = hasher.finalize().to_vec();
        out.push((rel_path, hash));
    } else if metadata.is_dir() {
        let mut entries: Vec<PathBuf> = fs::read_dir(path)
            .with_context(|| format!("reading directory {}", path.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .collect();

        // Sort for determinism.
        entries.sort();

        for entry in entries {
            collect_file_hashes(root, &entry, out)?;
        }
    }

    Ok(())
}

/// Extract file mode (Unix permissions) from metadata.
///
/// On non-Unix platforms, returns a default mode of 0o644.
#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode()
}

#[cfg(not(unix))]
fn file_mode(_metadata: &fs::Metadata) -> u32 {
    0o644
}

// ── BuildCache ──────────────────────────────────────────────────────────

/// Content-addressable build cache for Dockerfile instructions.
///
/// Caches layer outputs keyed by instruction content + parent state,
/// allowing incremental rebuilds where unchanged instructions complete
/// instantly.
pub struct BuildCache {
    cache_dir: PathBuf,
    entries: HashMap<String, CacheEntry>,
}

impl BuildCache {
    /// Open or create a build cache at the given directory.
    ///
    /// Creates the cache directory structure if it does not exist.
    /// Loads the existing cache index from `instructions.json` if present.
    ///
    /// # Errors
    ///
    /// Returns an error if the directory cannot be created or the
    /// index file cannot be parsed.
    pub fn open(cache_dir: PathBuf) -> anyhow::Result<Self> {
        // Create directory structure.
        let layers_dir = cache_dir.join(LAYERS_DIR).join(SHA256_DIR);
        fs::create_dir_all(&layers_dir)
            .with_context(|| format!("creating cache directory {}", layers_dir.display()))?;

        // Load existing index if present.
        let index_path = cache_dir.join(INDEX_FILE);
        let entries = if index_path.exists() {
            let data = fs::read_to_string(&index_path)
                .with_context(|| format!("reading cache index {}", index_path.display()))?;
            serde_json::from_str(&data)
                .with_context(|| format!("parsing cache index {}", index_path.display()))?
        } else {
            HashMap::new()
        };

        Ok(Self { cache_dir, entries })
    }

    /// Look up a cached result by key.
    #[must_use]
    pub fn get(&self, key: &str) -> Option<&CacheEntry> {
        self.entries.get(key)
    }

    /// Store a cache entry.
    ///
    /// # Errors
    ///
    /// Returns an error if the entry cannot be stored.
    pub fn put(&mut self, entry: CacheEntry) -> anyhow::Result<()> {
        self.entries.insert(entry.key.clone(), entry);
        Ok(())
    }

    /// Save cache index to disk as `instructions.json`.
    ///
    /// # Errors
    ///
    /// Returns an error if the index file cannot be written.
    pub fn save(&self) -> anyhow::Result<()> {
        let index_path = self.cache_dir.join(INDEX_FILE);
        let json =
            serde_json::to_string_pretty(&self.entries).context("serializing cache index")?;
        fs::write(&index_path, json)
            .with_context(|| format!("writing cache index {}", index_path.display()))
    }

    /// Prune entries older than `max_age` seconds.
    ///
    /// Returns the number of entries removed.
    ///
    /// # Errors
    ///
    /// Returns an error if the current time cannot be determined.
    pub fn prune(&mut self, max_age: u64) -> anyhow::Result<usize> {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .context("determining current time")?
            .as_secs();

        let cutoff = now.saturating_sub(max_age);
        let before = self.entries.len();

        self.entries
            .retain(|_key, entry| entry.created_at >= cutoff);

        Ok(before - self.entries.len())
    }

    /// Store layer blob data in the cache (content-addressable by digest).
    ///
    /// # Errors
    ///
    /// Returns an error if the blob cannot be written to disk.
    pub fn store_layer(&self, digest: &str, data: &[u8]) -> anyhow::Result<()> {
        let blob_path = self.blob_path(digest)?;
        fs::write(&blob_path, data)
            .with_context(|| format!("writing layer blob {}", blob_path.display()))
    }

    /// Load layer blob data from cache by digest.
    ///
    /// Returns `None` if the digest is not present in the cache.
    ///
    /// # Errors
    ///
    /// Returns an error if the blob file exists but cannot be read.
    pub fn load_layer(&self, digest: &str) -> anyhow::Result<Option<Vec<u8>>> {
        let blob_path = self.blob_path(digest)?;

        if !blob_path.exists() {
            return Ok(None);
        }

        let data = fs::read(&blob_path)
            .with_context(|| format!("reading layer blob {}", blob_path.display()))?;
        Ok(Some(data))
    }

    /// Resolve a digest to the corresponding blob file path.
    fn blob_path(&self, digest: &str) -> anyhow::Result<PathBuf> {
        let hash = digest
            .strip_prefix("sha256:")
            .context("digest missing sha256: prefix")?;
        Ok(self.cache_dir.join(LAYERS_DIR).join(SHA256_DIR).join(hash))
    }
}

#[cfg(test)]
#[path = "cache_test.rs"]
mod tests;
