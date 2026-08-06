//! OCI layer creation from guest overlay snapshots.
//!
//! Takes raw tar data from guest overlay snapshots and produces
//! OCI-compliant layers with proper whiteouts, hardlink tracking,
//! and dual digest computation.
//!
//! # Layer processing pipeline
//!
//! 1. Path normalization (`./` prefix)
//! 2. Linux overlayfs → OCI whiteout conversion
//! 3. Opaque directory → `.wh..wh..opq` marker insertion
//! 4. Hardlink preservation with normalized link targets
//! 5. Excluded path filtering (secret mounts)
//! 6. Dual SHA-256 digest computation (compressed + uncompressed)

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use sha2::{Digest, Sha256};

/// OCI media type for gzipped tar layers.
pub const OCI_LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// OCI whiteout prefix for deleted files.
const WHITEOUT_PREFIX: &str = ".wh.";

/// OCI opaque whiteout marker filename.
const OPAQUE_WHITEOUT: &str = ".wh..wh..opq";

/// Pax extended attribute key for overlayfs opaque directories.
///
/// When the `tar` crate encounters a pax extended header with this key
/// set to `"y"`, it indicates the directory should be treated as an
/// opaque whiteout in OCI.
const OVERLAY_OPAQUE_XATTR: &str = "SCHILY.xattr.trusted.overlay.opaque";

// ── HashWriter ──────────────────────────────────────────────────────────

/// Wraps a writer and computes SHA-256 of all data written through it.
///
/// Used to compute OCI digests (both compressed and uncompressed) in a
/// single streaming pass.
pub(crate) struct HashWriter<W: Write> {
    inner: W,
    hasher: Sha256,
}

impl<W: Write> HashWriter<W> {
    /// Create a new `HashWriter` wrapping the given writer.
    pub(crate) fn new(inner: W) -> Self {
        Self {
            inner,
            hasher: Sha256::new(),
        }
    }

    /// Consume the writer and return the inner writer plus the hex digest.
    ///
    /// The digest is formatted as `sha256:<hex>`.
    pub(crate) fn finish(self) -> (W, String) {
        let hash = self.hasher.finalize();
        let digest = format!("sha256:{hash:x}");
        (self.inner, digest)
    }
}

impl<W: Write> Write for HashWriter<W> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = self.inner.write(buf)?;
        self.hasher.update(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

// ── ProcessedLayer ──────────────────────────────────────────────────────

/// A fully processed OCI layer ready for image assembly.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ProcessedLayer {
    /// Compressed layer data (tar.gz).
    pub compressed_data: Vec<u8>,
    /// SHA-256 of compressed data (`sha256:abcd...`).
    pub digest: String,
    /// SHA-256 of uncompressed tar data (`sha256:efgh...`) — this is the `DiffID`.
    pub diff_id: String,
    /// Compressed size in bytes.
    pub compressed_size: u64,
    /// Uncompressed size in bytes.
    pub uncompressed_size: u64,
    /// OCI media type.
    pub media_type: String,
    /// Whether this layer is empty (no changes).
    pub empty: bool,
}

impl ProcessedLayer {
    /// Create a new processed layer.
    #[must_use]
    pub fn new(
        compressed_data: Vec<u8>,
        digest: String,
        diff_id: String,
        compressed_size: u64,
        uncompressed_size: u64,
        media_type: String,
        empty: bool,
    ) -> Self {
        Self {
            compressed_data,
            digest,
            diff_id,
            compressed_size,
            uncompressed_size,
            media_type,
            empty,
        }
    }
}

// ── LayerCreator ────────────────────────────────────────────────────────

/// Takes raw snapshot data and produces an OCI-compliant layer.
///
/// Handles:
/// - Base64 + gzip decoding of guest snapshots
/// - Path normalization (`./` prefix)
/// - Linux overlayfs → OCI whiteout conversion
/// - Hardlink preservation with normalized targets
/// - Excluded path filtering (secret mounts)
/// - Dual SHA-256 digest computation (compressed + uncompressed)
pub struct LayerCreator;

impl LayerCreator {
    /// Process raw snapshot data (base64-encoded tar.gz from guest) into an OCI layer.
    ///
    /// Steps:
    /// 1. Decode base64 → tar.gz bytes
    /// 2. Decompress gzip → tar bytes
    /// 3. Validate and transform tar entries (whiteouts, paths, hardlinks)
    /// 4. Re-compress to tar.gz
    /// 5. Compute dual digests (compressed + uncompressed)
    ///
    /// # Errors
    ///
    /// Returns an error if base64 decoding, decompression, or tar processing fails.
    pub fn process_snapshot(
        data: &str,
        excluded_paths: &[String],
    ) -> anyhow::Result<ProcessedLayer> {
        use base64::Engine as _;

        let compressed = base64::engine::general_purpose::STANDARD
            .decode(data)
            .context("base64 decoding of snapshot data")?;

        let mut decoder = GzDecoder::new(&compressed[..]);
        let mut tar_bytes = Vec::new();
        decoder
            .read_to_end(&mut tar_bytes)
            .context("gzip decompression of snapshot data")?;

        Self::from_tar(&tar_bytes, excluded_paths)
    }

    /// Create a processed layer from raw tar bytes (no base64, no gzip).
    ///
    /// Used for testing and for COPY/ADD layers created on host side.
    ///
    /// # Errors
    ///
    /// Returns an error if tar processing fails.
    pub fn from_tar(tar_data: &[u8], excluded_paths: &[String]) -> anyhow::Result<ProcessedLayer> {
        let (transformed_tar, entry_count) =
            transform_tar(tar_data, excluded_paths).context("transforming tar entries")?;

        let uncompressed_size = transformed_tar.len() as u64;

        // Digest pipeline (all owned — no borrows):
        //   uncompressed_hasher → gz_encoder → compressed_hasher → Vec<u8>
        let compressed_hasher = HashWriter::new(Vec::new());
        let gz_encoder = GzEncoder::new(compressed_hasher, Compression::default());
        let mut uncompressed_hasher = HashWriter::new(gz_encoder);

        uncompressed_hasher
            .write_all(&transformed_tar)
            .context("writing transformed tar")?;

        // Unwind: uncompressed hash → gzip finish → compressed hash.
        let (gz_encoder, diff_id) = uncompressed_hasher.finish();
        let compressed_hasher = gz_encoder.finish().context("finishing gzip compression")?;
        let (compressed_data, digest) = compressed_hasher.finish();
        let compressed_size = compressed_data.len() as u64;

        Ok(ProcessedLayer {
            compressed_data,
            digest,
            diff_id,
            compressed_size,
            uncompressed_size,
            media_type: OCI_LAYER_MEDIA_TYPE.to_owned(),
            empty: entry_count == 0,
        })
    }
}

// ── Tar Transformation ──────────────────────────────────────────────────

/// Transform a raw tar archive: normalize paths, convert whiteouts,
/// preserve hardlinks, and filter excluded paths.
///
/// Returns `(transformed_tar_bytes, entry_count)`.
fn transform_tar(tar_data: &[u8], excluded_paths: &[String]) -> anyhow::Result<(Vec<u8>, usize)> {
    let mut archive = tar::Archive::new(tar_data);
    let mut output = tar::Builder::new(Vec::new());
    let mut entry_count: usize = 0;

    for entry_result in archive.entries().context("reading tar entries")? {
        let mut entry = entry_result.context("reading tar entry")?;
        let header = entry.header().clone();
        let raw_path = entry.path().context("reading entry path")?.to_path_buf();

        let normalized = normalize_path(&raw_path);
        let norm_str = normalized.to_string_lossy();

        if is_excluded(&norm_str, excluded_paths) {
            io::copy(&mut entry, &mut io::sink()).ok();
            continue;
        }

        // Check for opaque whiteout xattr before consuming entry data.
        let has_opaque_xattr = check_opaque_xattr(&mut entry)?;
        let entry_type = header.entry_type();

        // Convert overlayfs char device (0,0) → OCI whiteout.
        if is_overlay_whiteout(&header) {
            write_oci_whiteout(&mut output, &normalized)?;
            entry_count += 1;
            continue;
        }

        // Preserve existing hardlinks with normalized target path.
        if entry_type == tar::EntryType::Link {
            let link_target = entry
                .link_name()
                .context("reading hardlink target")?
                .map(|p| normalize_path(&p));
            if let Some(target) = link_target {
                write_hardlink(&mut output, &normalized, &target)?;
                entry_count += 1;
                continue;
            }
        }

        // Write the entry with its normalized path.
        write_entry(&mut output, &normalized, &header, &mut entry)?;
        entry_count += 1;

        // Opaque directory → add `.wh..wh..opq` marker.
        if has_opaque_xattr && entry_type == tar::EntryType::Directory {
            let opaque_path = normalized.join(OPAQUE_WHITEOUT);
            write_empty_file(&mut output, &opaque_path)?;
            entry_count += 1;
        }
    }

    let tar_bytes = output.into_inner().context("finalizing transformed tar")?;
    Ok((tar_bytes, entry_count))
}

// ── Path Helpers ────────────────────────────────────────────────────────

/// Normalize a tar entry path to start with `./`.
fn normalize_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    if s.starts_with("./") || s == "." {
        return path.to_path_buf();
    }
    if let Some(stripped) = s.strip_prefix('/') {
        PathBuf::from(format!("./{stripped}"))
    } else {
        PathBuf::from(format!("./{s}"))
    }
}

/// Check whether a path should be excluded.
fn is_excluded(path: &str, excluded_paths: &[String]) -> bool {
    excluded_paths
        .iter()
        .any(|exc| path.starts_with(exc.as_str()))
}

// ── Whiteout Detection ───────────────────────────────────────────────────

/// Check if a tar entry is a Linux overlayfs whiteout (char device 0,0).
fn is_overlay_whiteout(header: &tar::Header) -> bool {
    if header.entry_type() != tar::EntryType::Char {
        return false;
    }
    let major = header.device_major().ok().flatten().unwrap_or(u32::MAX);
    let minor = header.device_minor().ok().flatten().unwrap_or(u32::MAX);
    major == 0 && minor == 0
}

/// Check pax extensions on an entry for the overlayfs opaque xattr.
///
/// Must be called before consuming the entry data.
fn check_opaque_xattr<R: Read>(entry: &mut tar::Entry<'_, R>) -> anyhow::Result<bool> {
    let pax = entry.pax_extensions().context("reading pax extensions")?;
    let Some(mut pax) = pax else {
        return Ok(false);
    };
    Ok(pax.any(|ext| ext.is_ok_and(|e| e.key_bytes() == OVERLAY_OPAQUE_XATTR.as_bytes())))
}

// ── Tar Writers ────────────────────────────────────────────────────────

/// Write an OCI whiteout entry: empty file named `.wh.<filename>`.
fn write_oci_whiteout(builder: &mut tar::Builder<Vec<u8>>, path: &Path) -> anyhow::Result<()> {
    let file_name = path
        .file_name()
        .map(|n| n.to_string_lossy())
        .unwrap_or_default();
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let whiteout_name = format!("{WHITEOUT_PREFIX}{file_name}");
    let whiteout_path = parent.join(whiteout_name);
    write_empty_file(builder, &whiteout_path)
}

/// Write a hardlink entry pointing to `target`.
fn write_hardlink(
    builder: &mut tar::Builder<Vec<u8>>,
    path: &Path,
    target: &Path,
) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_entry_type(tar::EntryType::Link);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_link(&mut header, path, target)
        .context("writing hardlink entry")?;
    Ok(())
}

/// Write a tar entry with a normalized path, copying data from the source.
fn write_entry<R: Read>(
    builder: &mut tar::Builder<Vec<u8>>,
    path: &Path,
    original_header: &tar::Header,
    data: &mut R,
) -> anyhow::Result<()> {
    let mut new_header = original_header.clone();
    new_header.set_cksum();
    builder
        .append_data(&mut new_header, path, data)
        .context("writing tar entry")?;
    Ok(())
}

/// Write an empty regular file entry (whiteout markers, opaque markers).
fn write_empty_file(builder: &mut tar::Builder<Vec<u8>>, path: &Path) -> anyhow::Result<()> {
    let mut header = tar::Header::new_gnu();
    header.set_size(0);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder
        .append_data(&mut header, path, io::empty())
        .context("writing empty file entry")?;
    Ok(())
}

#[cfg(test)]
#[path = "layer_test.rs"]
mod tests;
