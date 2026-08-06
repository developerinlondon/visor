//! Build-layer tracking for image building inside the guest.
//!
//! The current build VM mutates its real guest rootfs while executing build
//! instructions. To produce OCI layers without depending on guest overlayfs
//! kernel support, this module snapshots filesystem metadata and computes a
//! tar-based diff after each instruction.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use base64::Engine;
use sha2::Digest;

use crate::agent::SnapshotLayerResult;

/// Well-known base path for the overlay filesystem.
pub const OVERLAY_BASE: &str = "/tmp/build-overlay";

/// Path to the overlay upper directory (captures filesystem changes).
pub const OVERLAY_UPPER: &str = "/tmp/build-overlay/upper";

/// Path to the overlay work directory (required by overlayfs).
pub const OVERLAY_WORK: &str = "/tmp/build-overlay/work";

/// Path to the merged view of lower + upper.
pub const OVERLAY_MERGED: &str = "/tmp/build-overlay/merged";

/// State for a build overlay.
#[derive(Debug)]
pub struct BuildOverlay {
    lower_dir: PathBuf,
    baseline: BTreeMap<PathBuf, SnapshotEntry>,
}

impl BuildOverlay {
    /// Initialize filesystem diff tracking for the build root.
    ///
    /// Captures a baseline snapshot of the current guest filesystem rooted at
    /// `lower_dir`. Non-absolute inputs fall back to `"/"` because the build
    /// engine currently passes stage aliases rather than guest paths.
    ///
    /// # Errors
    ///
    /// Returns an error if the baseline filesystem snapshot cannot be read.
    pub fn init(lower_dir: &str) -> anyhow::Result<Self> {
        let lower_dir = normalize_lower_dir(lower_dir);
        let baseline =
            capture_manifest(&lower_dir).context("failed to capture baseline filesystem")?;

        Ok(Self {
            lower_dir,
            baseline,
        })
    }

    /// Snapshot the upper directory as a tar.gz, returning the data
    /// and both compressed/uncompressed digests.
    ///
    /// # Errors
    ///
    /// Returns an error if tar creation, compression, or hashing fails.
    pub fn snapshot_layer(&mut self) -> anyhow::Result<SnapshotLayerResult> {
        let current =
            capture_manifest(&self.lower_dir).context("failed to capture current filesystem")?;
        let tar_bytes = build_diff_archive(&self.lower_dir, &self.baseline, &current)
            .context("failed to build layer diff archive")?;

        // 2. Compute sha256 of uncompressed tar
        let uncompressed_hash = sha2::Sha256::digest(&tar_bytes);
        let uncompressed_digest = format!("sha256:{}", hex_encode(&uncompressed_hash));

        // 3. Gzip compress
        let mut gz_bytes = Vec::new();
        {
            let mut encoder =
                flate2::write::GzEncoder::new(&mut gz_bytes, flate2::Compression::default());
            encoder
                .write_all(&tar_bytes)
                .context("failed to gzip compress tar")?;
            encoder
                .finish()
                .context("failed to finish gzip compression")?;
        }

        // 4. Compute sha256 of compressed data
        let compressed_hash = sha2::Sha256::digest(&gz_bytes);
        let compressed_digest = format!("sha256:{}", hex_encode(&compressed_hash));
        let compressed_size =
            u64::try_from(gz_bytes.len()).context("compressed size exceeds u64")?;

        // 5. Base64 encode
        let data = base64::engine::general_purpose::STANDARD.encode(&gz_bytes);

        Ok(SnapshotLayerResult {
            data,
            compressed_digest,
            uncompressed_digest,
            compressed_size,
        })
    }

    /// Refresh the baseline snapshot for the next build instruction.
    ///
    /// # Errors
    ///
    /// Returns an error if the refreshed baseline cannot be captured.
    pub fn flatten(&mut self) -> anyhow::Result<()> {
        self.baseline =
            capture_manifest(&self.lower_dir).context("failed to refresh baseline filesystem")?;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SnapshotKind {
    Directory,
    File,
    Symlink,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SnapshotEntry {
    kind: SnapshotKind,
    mode: u32,
    uid: u32,
    gid: u32,
    digest: Option<String>,
    link_target: Option<PathBuf>,
}

fn normalize_lower_dir(lower_dir: &str) -> PathBuf {
    if lower_dir.is_empty() || !lower_dir.starts_with('/') {
        PathBuf::from("/")
    } else {
        PathBuf::from(lower_dir)
    }
}

fn capture_manifest(root: &Path) -> anyhow::Result<BTreeMap<PathBuf, SnapshotEntry>> {
    let mut manifest = BTreeMap::new();
    visit_path(root, root, &mut manifest)
        .with_context(|| format!("walk filesystem tree rooted at {}", root.display()))?;
    Ok(manifest)
}

fn visit_path(
    root: &Path,
    dir: &Path,
    manifest: &mut BTreeMap<PathBuf, SnapshotEntry>,
) -> anyhow::Result<()> {
    for entry_result in
        std::fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))?
    {
        let entry = entry_result.with_context(|| format!("read entry in {}", dir.display()))?;
        let path = entry.path();
        if should_ignore(root, &path) {
            continue;
        }

        let metadata = std::fs::symlink_metadata(&path)
            .with_context(|| format!("read metadata for {}", path.display()))?;
        let rel = path
            .strip_prefix(root)
            .with_context(|| format!("strip root {} from {}", root.display(), path.display()))?
            .to_path_buf();

        let entry_snapshot = if metadata.file_type().is_dir() {
            SnapshotEntry {
                kind: SnapshotKind::Directory,
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                digest: None,
                link_target: None,
            }
        } else if metadata.file_type().is_symlink() {
            let link_target = std::fs::read_link(&path)
                .with_context(|| format!("read link {}", path.display()))?;
            SnapshotEntry {
                kind: SnapshotKind::Symlink,
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                digest: None,
                link_target: Some(link_target),
            }
        } else if metadata.file_type().is_file() {
            SnapshotEntry {
                kind: SnapshotKind::File,
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                digest: Some(digest_file(&path)?),
                link_target: None,
            }
        } else {
            continue;
        };

        manifest.insert(rel, entry_snapshot);

        if metadata.file_type().is_dir() {
            visit_path(root, &path, manifest)?;
        }
    }
    Ok(())
}

fn should_ignore(root: &Path, path: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };

    if rel == Path::new("proc") || rel.starts_with("proc/") {
        return true;
    }
    if rel == Path::new("sys") || rel.starts_with("sys/") {
        return true;
    }
    rel == Path::new("tmp/build-overlay") || rel.starts_with("tmp/build-overlay/")
}

fn digest_file(path: &Path) -> anyhow::Result<String> {
    let mut file = std::fs::File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut hasher = sha2::Sha256::new();
    let mut buf = [0u8; 8192];
    loop {
        let read = file
            .read(&mut buf)
            .with_context(|| format!("read {}", path.display()))?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hex_encode(&hasher.finalize()))
}

fn build_diff_archive(
    root: &Path,
    baseline: &BTreeMap<PathBuf, SnapshotEntry>,
    current: &BTreeMap<PathBuf, SnapshotEntry>,
) -> anyhow::Result<Vec<u8>> {
    let mut builder = tar::Builder::new(Vec::new());
    let mut deleted_roots: Vec<PathBuf> = Vec::new();

    for rel in baseline.keys() {
        if current.contains_key(rel) || has_deleted_ancestor(rel, &deleted_roots) {
            continue;
        }
        append_whiteout(&mut builder, rel)?;
        deleted_roots.push(rel.clone());
    }

    for (rel, current_entry) in current {
        if baseline.get(rel) == Some(current_entry) {
            continue;
        }
        append_entry(&mut builder, root, rel, current_entry)?;
    }

    builder.finish().context("finish layer tar archive")?;
    builder.into_inner().context("extract layer tar buffer")
}

fn has_deleted_ancestor(path: &Path, deleted_roots: &[PathBuf]) -> bool {
    path.ancestors().skip(1).any(|ancestor| {
        !ancestor.as_os_str().is_empty() && deleted_roots.iter().any(|deleted| deleted == ancestor)
    })
}

fn append_whiteout(builder: &mut tar::Builder<Vec<u8>>, rel: &Path) -> anyhow::Result<()> {
    let file_name = rel
        .file_name()
        .with_context(|| format!("build whiteout for {}", rel.display()))?
        .to_string_lossy();
    let whiteout_name = format!(".wh.{file_name}");
    let whiteout_rel = match rel.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.join(whiteout_name),
        _ => PathBuf::from(whiteout_name),
    };

    let mut header = tar::Header::new_gnu();
    header.set_entry_type(tar::EntryType::Regular);
    header.set_size(0);
    header.set_mode(0o644);
    header.set_uid(0);
    header.set_gid(0);
    header.set_cksum();
    builder
        .append_data(&mut header, &whiteout_rel, std::io::empty())
        .with_context(|| format!("append whiteout {}", whiteout_rel.display()))
}

fn append_entry(
    builder: &mut tar::Builder<Vec<u8>>,
    root: &Path,
    rel: &Path,
    entry: &SnapshotEntry,
) -> anyhow::Result<()> {
    let full_path = root.join(rel);
    let metadata = std::fs::symlink_metadata(&full_path)
        .with_context(|| format!("read metadata for {}", full_path.display()))?;
    let mut header = tar::Header::new_gnu();
    populate_header(&mut header, &metadata);

    match entry.kind {
        SnapshotKind::Directory => {
            header.set_entry_type(tar::EntryType::Directory);
            header.set_size(0);
            header.set_cksum();
            builder
                .append_data(&mut header, rel, std::io::empty())
                .with_context(|| format!("append directory {}", rel.display()))?;
        }
        SnapshotKind::File => {
            let mut file = std::fs::File::open(&full_path)
                .with_context(|| format!("open {}", full_path.display()))?;
            header.set_entry_type(tar::EntryType::Regular);
            header.set_size(metadata.size());
            header.set_cksum();
            builder
                .append_data(&mut header, rel, &mut file)
                .with_context(|| format!("append file {}", rel.display()))?;
        }
        SnapshotKind::Symlink => {
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            let link_target = entry
                .link_target
                .as_ref()
                .with_context(|| format!("missing link target for {}", rel.display()))?;
            header
                .set_link_name(link_target)
                .with_context(|| format!("set link target for {}", rel.display()))?;
            header.set_cksum();
            builder
                .append_data(&mut header, rel, std::io::empty())
                .with_context(|| format!("append symlink {}", rel.display()))?;
        }
    }

    Ok(())
}

fn populate_header(header: &mut tar::Header, metadata: &std::fs::Metadata) {
    header.set_mode(metadata.mode() & 0o7777);
    header.set_uid(u64::from(metadata.uid()));
    header.set_gid(u64::from(metadata.gid()));
    let mtime = u64::try_from(metadata.mtime().max(0)).unwrap_or_default();
    header.set_mtime(mtime);
}

/// Encode bytes as lowercase hex string.
fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().fold(String::new(), |mut acc, b| {
        use std::fmt::Write;
        let _ = write!(acc, "{b:02x}");
        acc
    })
}

#[cfg(test)]
#[path = "overlay_test.rs"]
mod tests;
