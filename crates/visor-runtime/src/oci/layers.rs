//! OCI layer merging with whiteout handling.
//!
//! Unpacks and merges multiple OCI layers in order, handling
//! `.wh.` whiteout files and `.wh..wh..opq` opaque whiteouts
//! per the OCI image specification.
//!
//! # Whiteout semantics
//!
//! - A file named `.wh.<name>` in a layer means "delete `<name>` from lower layers."
//!   The `.wh.*` marker itself is never written to disk.
//! - A file named `.wh..wh..opq` inside a directory means "delete all prior contents
//!   of this directory." Only entries from the current (and subsequent) layers survive.
//!
//! Layers are applied bottom-to-top: the first element of the slice is the base
//! layer, and the last element wins on conflicts.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use flate2::read::GzDecoder;

/// Prefix for OCI whiteout files.
const WHITEOUT_PREFIX: &str = ".wh.";

/// Name of the opaque whiteout marker.
const OPAQUE_WHITEOUT: &str = ".wh..wh..opq";

/// Merges OCI image layers into a single directory tree.
///
/// Each layer is a gzipped tar archive. Layers are unpacked in order
/// (first = bottom, last = top), with OCI whiteout files processed to
/// handle deletions and opaque directory replacements.
#[non_exhaustive]
pub struct LayerMerger {
    target: PathBuf,
}

impl LayerMerger {
    /// Create a new merger targeting `target`.
    ///
    /// The target directory is created (including parents) if it does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the target directory cannot be created.
    pub fn new(target: impl Into<PathBuf>) -> anyhow::Result<Self> {
        let target = target.into();
        fs::create_dir_all(&target)
            .with_context(|| format!("create target directory {}", target.display()))?;
        Ok(Self { target })
    }

    /// Returns the target directory path.
    #[must_use]
    pub fn target(&self) -> &Path {
        &self.target
    }

    /// Unpack a single gzipped tar layer into the target directory.
    ///
    /// Handles OCI whiteout files: `.wh.<name>` deletes the named entry,
    /// `.wh..wh..opq` removes all prior contents of the parent directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the file cannot be opened, decompressed, or
    /// extracted, or if a tar entry contains an unsafe path.
    pub fn unpack_layer(&self, layer_tar_gz: &Path) -> anyhow::Result<()> {
        let file = fs::File::open(layer_tar_gz)
            .with_context(|| format!("open layer {}", layer_tar_gz.display()))?;
        self.unpack_layer_from_reader(file)
    }

    /// Unpack a gzipped tar layer from an arbitrary reader.
    ///
    /// Behaves identically to [`Self::unpack_layer`] but reads from a
    /// generic `Read` source (e.g. a cache stream).
    ///
    /// # Errors
    ///
    /// Returns an error if the stream is not valid gzip/tar or contains
    /// unsafe paths.
    pub fn unpack_layer_from_reader(&self, reader: impl Read) -> anyhow::Result<()> {
        let decoder = GzDecoder::new(reader);
        let mut archive = tar::Archive::new(decoder);
        archive.set_preserve_permissions(true);

        let entries = archive.entries().context("read tar entries from layer")?;

        for entry_result in entries {
            let mut entry = entry_result.context("read tar entry")?;
            let path = entry.path().context("read entry path")?.into_owned();

            // Safety: reject path traversal attempts.
            if path_is_unsafe(&path) {
                bail!("refusing tar entry with unsafe path: {}", path.display());
            }

            let file_name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_owned(),
                None => continue,
            };

            // Opaque whiteout: clear existing directory contents immediately
            // so subsequent entries in this layer populate a clean directory.
            if file_name == OPAQUE_WHITEOUT {
                if let Some(parent) = path.parent() {
                    let abs = self.target.join(parent);
                    if abs.is_dir() {
                        clear_directory(&abs).with_context(|| {
                            format!("apply opaque whiteout to {}", abs.display())
                        })?;
                    }
                }
                continue;
            }

            // Regular whiteout: delete the named entry immediately.
            if let Some(stripped) = file_name.strip_prefix(WHITEOUT_PREFIX) {
                let target = if let Some(parent) = path.parent() {
                    self.target.join(parent).join(stripped)
                } else {
                    self.target.join(stripped)
                };
                remove_entry(&target)?;
                continue;
            }

            // Skip device nodes — we are building a rootfs, not replicating
            // block/char devices.
            let entry_type = entry.header().entry_type();
            if entry_type == tar::EntryType::Block || entry_type == tar::EntryType::Char {
                continue;
            }

            let dest = self.target.join(&path);

            match entry_type {
                tar::EntryType::Directory => {
                    fs::create_dir_all(&dest)
                        .with_context(|| format!("create directory {}", dest.display()))?;

                    // Preserve directory permissions from tar header.
                    let mode = entry.header().mode().unwrap_or(0o755);
                    set_permissions(&dest, mode)?;
                }
                tar::EntryType::Symlink => {
                    let link_target = entry
                        .link_name()
                        .context("read symlink target")?
                        .ok_or_else(|| {
                            anyhow::anyhow!("symlink {} has no target", path.display())
                        })?;
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).with_context(|| {
                            format!("create parent for symlink {}", dest.display())
                        })?;
                    }
                    // Remove existing file/symlink if present before creating new one.
                    let _ = fs::remove_file(&dest);
                    std::os::unix::fs::symlink(link_target.as_ref(), &dest)
                        .with_context(|| format!("create symlink {}", dest.display()))?;
                }
                tar::EntryType::Link => {
                    let link_target = entry
                        .link_name()
                        .context("read hardlink target")?
                        .ok_or_else(|| {
                            anyhow::anyhow!("hardlink {} has no target", path.display())
                        })?;
                    let link_dest = self.target.join(link_target.as_ref());
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).with_context(|| {
                            format!("create parent for hardlink {}", dest.display())
                        })?;
                    }
                    let _ = fs::remove_file(&dest);
                    fs::hard_link(&link_dest, &dest)
                        .with_context(|| format!("create hardlink {}", dest.display()))?;
                }
                _ => {
                    // Regular file (or anything else we treat as a file).
                    if let Some(parent) = dest.parent() {
                        fs::create_dir_all(parent).with_context(|| {
                            format!("create parent directory for {}", dest.display())
                        })?;
                    }
                    // Remove existing file before writing (handles overwrite).
                    let _ = fs::remove_file(&dest);
                    entry
                        .unpack(&dest)
                        .with_context(|| format!("unpack entry {}", dest.display()))?;
                }
            }
        }

        Ok(())
    }

    /// Merge multiple layers in order into the target directory.
    ///
    /// The first path in `layers` is the base (bottom) layer; the last
    /// is the topmost layer and wins on conflicts.
    ///
    /// # Errors
    ///
    /// Returns an error if any layer cannot be unpacked.
    pub fn merge_layers(&self, layers: &[PathBuf]) -> anyhow::Result<()> {
        for (i, layer) in layers.iter().enumerate() {
            self.unpack_layer(layer)
                .with_context(|| format!("unpack layer {i} ({})", layer.display()))?;
        }
        Ok(())
    }
}

/// Returns `true` if a tar entry path is unsafe (absolute or contains `..`).
fn path_is_unsafe(path: &Path) -> bool {
    path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
}

/// Remove all entries inside `dir` but keep the directory itself.
fn clear_directory(dir: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("read directory {}", dir.display()))? {
        let entry = entry.context("read directory entry")?;
        remove_entry(&entry.path())?;
    }
    Ok(())
}

/// Remove a file, symlink, or directory tree at `path`. No-op if it does not exist.
fn remove_entry(path: &Path) -> anyhow::Result<()> {
    match path.symlink_metadata() {
        Ok(meta) if meta.is_dir() => {
            fs::remove_dir_all(path)
                .with_context(|| format!("whiteout remove directory {}", path.display()))?;
        }
        Ok(_) => {
            fs::remove_file(path)
                .with_context(|| format!("whiteout remove file {}", path.display()))?;
        }
        Err(_) => { /* path does not exist — nothing to remove */ }
    }
    Ok(())
}

/// Set Unix permissions on a path.
fn set_permissions(path: &Path, mode: u32) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let perms = fs::Permissions::from_mode(mode);
    fs::set_permissions(path, perms)
        .with_context(|| format!("set permissions on {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
#[path = "layers_test.rs"]
mod tests;
