//! Rootfs builder: directory tree to ext4 image.
//!
//! Converts a merged layer directory into a sparse ext4 filesystem
//! image that can be attached to a VM as a virtio-blk device.
//!
//! Uses the `mke2fs` command-line tool (from e2fsprogs) with the `-d` flag
//! to populate an ext4 image directly from a directory tree, avoiding
//! any need for a kernel-level ext4 implementation.

use std::path::{Path, PathBuf};

use anyhow::Context;

/// Minimum image size in megabytes. ext4 requires a baseline amount of space
/// for superblocks, group descriptors, journal, and inode tables.
const MIN_IMAGE_SIZE_MB: u64 = 64;

/// Options for building a rootfs ext4 image.
///
/// All fields have sensible defaults via [`Default`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RootfsOptions {
    /// Filesystem label written into the ext4 superblock (default: `"visor-rootfs"`).
    pub label: String,
    /// Extra megabytes to add beyond the measured content size (default: 64).
    pub extra_size_mb: u64,
    /// Whether to create a sparse image file (default: `true`).
    /// mke2fs creates sparse files by default, so this primarily
    /// documents intent.
    pub sparse: bool,
}

impl Default for RootfsOptions {
    fn default() -> Self {
        Self {
            label: "visor-rootfs".into(),
            extra_size_mb: 256,
            sparse: true,
        }
    }
}

/// Builds a sparse ext4 filesystem image from a directory tree.
///
/// Uses `mke2fs` (from e2fsprogs) to create the image. The source
/// directory is copied into the image verbatim via the `-d` flag.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RootfsBuilder {
    source_dir: PathBuf,
    output: PathBuf,
    options: RootfsOptions,
}

impl RootfsBuilder {
    /// Create a new builder with default options.
    ///
    /// # Errors
    ///
    /// Does not error at construction time. Errors are deferred to [`build`](Self::build).
    pub fn new(source_dir: impl Into<PathBuf>, output: impl Into<PathBuf>) -> Self {
        Self {
            source_dir: source_dir.into(),
            output: output.into(),
            options: RootfsOptions::default(),
        }
    }

    /// Create a new builder with explicit options.
    ///
    /// # Errors
    ///
    /// Does not error at construction time. Errors are deferred to [`build`](Self::build).
    pub fn with_options(
        source_dir: impl Into<PathBuf>,
        output: impl Into<PathBuf>,
        options: RootfsOptions,
    ) -> Self {
        Self {
            source_dir: source_dir.into(),
            output: output.into(),
            options,
        }
    }

    /// Build the ext4 image from the source directory.
    ///
    /// Walks the source directory to calculate the required size, then
    /// invokes `mke2fs` to create a populated ext4 filesystem image at
    /// the output path.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The source directory does not exist
    /// - `mke2fs` is not found on `PATH`
    /// - `mke2fs` exits with a non-zero status
    /// - Filesystem I/O fails
    pub fn build(&self) -> anyhow::Result<PathBuf> {
        // Validate source directory exists.
        if !self.source_dir.is_dir() {
            anyhow::bail!(
                "source directory does not exist: {}",
                self.source_dir.display()
            );
        }

        // Check that mke2fs is available.
        let mke2fs = crate::ext4::find_mke2fs()?;

        // Calculate required image size.
        let content_bytes =
            calculate_dir_size(&self.source_dir).context("calculate source directory size")?;
        let extra_bytes = self.options.extra_size_mb * 1024 * 1024;
        let min_bytes = MIN_IMAGE_SIZE_MB * 1024 * 1024;
        let total_bytes = (content_bytes + extra_bytes).max(min_bytes);

        // mke2fs with -b 4096 expects size in 4K-block units.
        let block_size: u64 = 4096;
        let blocks = total_bytes.div_ceil(block_size);

        // Build the mke2fs command.
        let output = std::process::Command::new(&mke2fs)
            .arg("-t")
            .arg("ext4")
            .arg("-d")
            .arg(&self.source_dir)
            .arg("-L")
            .arg(&self.options.label)
            .arg("-F")
            .arg("-E")
            .arg("lazy_itable_init=0,lazy_journal_init=0")
            .arg("-m")
            .arg("0")
            .arg("-b")
            .arg(block_size.to_string())
            .arg(&self.output)
            .arg(blocks.to_string())
            .output()
            .context("spawn mke2fs process")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            anyhow::bail!("mke2fs failed (exit {}): {stderr}", output.status);
        }

        Ok(self.output.clone())
    }
}

/// Recursively calculate the total size of regular files in a directory.
///
/// Symlinks are not followed (their target size is not counted). Only
/// regular file sizes contribute to the total.
///
/// # Errors
///
/// Returns an error if any directory entry cannot be read.
pub(crate) fn calculate_dir_size(dir: &Path) -> anyhow::Result<u64> {
    let mut total: u64 = 0;
    walk_dir_size(dir, &mut total)?;
    Ok(total)
}

/// Recursive helper for [`calculate_dir_size`].
fn walk_dir_size(dir: &Path, total: &mut u64) -> anyhow::Result<()> {
    let entries =
        std::fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))?;

    for entry in entries {
        let entry = entry.with_context(|| format!("read entry in {}", dir.display()))?;
        let ft = entry
            .file_type()
            .with_context(|| format!("file type of {}", entry.path().display()))?;

        if ft.is_file() {
            let meta = entry
                .metadata()
                .with_context(|| format!("metadata of {}", entry.path().display()))?;
            *total += meta.len();
        } else if ft.is_dir() {
            walk_dir_size(&entry.path(), total)?;
        }
        // Symlinks: skip (don't follow, don't count).
    }

    Ok(())
}

#[cfg(test)]
#[path = "rootfs_test.rs"]
mod tests;
