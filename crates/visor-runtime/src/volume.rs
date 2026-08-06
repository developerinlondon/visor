//! Persistent volume management for visor.
//!
//! Manages ext4 volume files at `~/.visor/volumes/`, providing create, list,
//! inspect, remove, and resize operations. Volumes are sparse ext4 files
//! created with `truncate` + `mke2fs`, and resized with `truncate` + `resize2fs`.
//!
//! # Volume Layout
//!
//! ```text
//! ~/.visor/volumes/
//! ├── myvolume.ext4     # Sparse ext4 filesystem image
//! ├── myvolume.json     # Volume metadata (name, size, created_at)
//! ├── data.ext4
//! └── data.json
//! ```

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// Metadata for a persistent volume.
///
/// Stored as JSON alongside the volume's ext4 file and returned by
/// management operations (create, list, inspect, resize).
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct VolumeInfo {
    /// Volume name (alphanumeric, hyphens, underscores).
    pub name: String,
    /// Volume size in MiB.
    pub size_mib: u64,
    /// ISO 8601 UTC timestamp of when the volume was created.
    pub created_at: String,
    /// Absolute path to the ext4 volume file.
    pub path: String,
}

/// Manages persistent volumes stored as sparse ext4 files.
///
/// Volumes live under a configurable base directory (default `~/.visor/volumes/`).
/// Each volume consists of two files:
/// - `{name}.ext4` — sparse ext4 filesystem image
/// - `{name}.json` — JSON metadata (name, size, `created_at`, path)
#[non_exhaustive]
pub struct VolumeManager {
    /// Base directory for volume storage.
    base_dir: PathBuf,
}

impl VolumeManager {
    /// Creates a new `VolumeManager` with the given base directory.
    ///
    /// Creates the base directory if it doesn't exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the base directory cannot be created.
    pub fn new(base_dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(base_dir).with_context(|| {
            format!("failed to create volume directory: {}", base_dir.display())
        })?;
        Ok(Self {
            base_dir: base_dir.to_owned(),
        })
    }

    /// Returns the default volume directory (`$VISOR_HOME/volumes/` or
    /// `$HOME/.visor/volumes/`).
    ///
    /// # Errors
    ///
    /// Returns an error if neither `VISOR_HOME` nor `HOME` is available.
    pub fn default_dir() -> anyhow::Result<PathBuf> {
        crate::paths::persistent_subdir("volumes").context("determine volume directory")
    }

    /// Creates a new volume with the given name and size.
    ///
    /// Creates a sparse file with `truncate` and formats it as ext4 with `mke2fs`.
    ///
    /// # Errors
    ///
    /// Returns an error if the name is invalid, a volume with the same name
    /// already exists, the size is zero, or the filesystem tools fail.
    pub fn create(&self, name: &str, size_mib: u64) -> anyhow::Result<VolumeInfo> {
        validate_name(name)?;

        let ext4_path = self.ext4_path(name);
        let meta_path = self.meta_path(name);

        anyhow::ensure!(!ext4_path.exists(), "volume '{name}' already exists");
        anyhow::ensure!(size_mib > 0, "volume size must be greater than 0 MiB");

        // Create sparse file with truncate.
        let size_bytes = size_mib * 1024 * 1024;
        let output = std::process::Command::new("truncate")
            .arg("-s")
            .arg(size_bytes.to_string())
            .arg(&ext4_path)
            .output()
            .context("failed to run truncate")?;

        anyhow::ensure!(
            output.status.success(),
            "truncate failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Format as ext4 with mke2fs.
        let mke2fs = crate::ext4::find_mke2fs()?;
        let output = std::process::Command::new(&mke2fs)
            .args(["-t", "ext4", "-F"])
            .arg(&ext4_path)
            .output()
            .context("failed to run mke2fs")?;

        if !output.status.success() {
            // Clean up the sparse file on mke2fs failure.
            let _ = std::fs::remove_file(&ext4_path);
            anyhow::bail!("mke2fs failed: {}", String::from_utf8_lossy(&output.stderr));
        }

        let info = VolumeInfo {
            name: name.to_owned(),
            size_mib,
            created_at: crate::timeutil::utc_now_iso8601(),
            path: ext4_path.display().to_string(),
        };

        let json =
            serde_json::to_string_pretty(&info).context("failed to serialize volume metadata")?;
        std::fs::write(&meta_path, json).context("failed to write volume metadata")?;

        Ok(info)
    }

    /// Lists all volumes sorted by name.
    ///
    /// Reads all `.json` metadata files in the volume directory.
    ///
    /// # Errors
    ///
    /// Returns an error if the volume directory cannot be read or a metadata
    /// file is malformed.
    pub fn list(&self) -> anyhow::Result<Vec<VolumeInfo>> {
        let mut volumes = Vec::new();

        let entries =
            std::fs::read_dir(&self.base_dir).context("failed to read volume directory")?;

        for entry in entries {
            let entry = entry.context("failed to read directory entry")?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                let data = std::fs::read_to_string(&path)
                    .with_context(|| format!("failed to read {}", path.display()))?;
                let info: VolumeInfo = serde_json::from_str(&data)
                    .with_context(|| format!("failed to parse {}", path.display()))?;
                volumes.push(info);
            }
        }

        volumes.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(volumes)
    }

    /// Inspects a volume by name, returning its metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if the volume does not exist or its metadata cannot
    /// be read.
    pub fn inspect(&self, name: &str) -> anyhow::Result<VolumeInfo> {
        let meta_path = self.meta_path(name);
        anyhow::ensure!(meta_path.exists(), "volume '{name}' not found");

        let data = std::fs::read_to_string(&meta_path)
            .with_context(|| format!("failed to read metadata for volume '{name}'"))?;
        let info: VolumeInfo = serde_json::from_str(&data)
            .with_context(|| format!("failed to parse metadata for volume '{name}'"))?;

        Ok(info)
    }

    /// Removes a volume and its metadata files.
    ///
    /// # Errors
    ///
    /// Returns an error if the volume does not exist or its files cannot
    /// be deleted.
    pub fn remove(&self, name: &str) -> anyhow::Result<()> {
        let ext4_path = self.ext4_path(name);
        let meta_path = self.meta_path(name);

        anyhow::ensure!(meta_path.exists(), "volume '{name}' not found");

        if ext4_path.exists() {
            std::fs::remove_file(&ext4_path)
                .with_context(|| format!("failed to remove volume file for '{name}'"))?;
        }

        std::fs::remove_file(&meta_path)
            .with_context(|| format!("failed to remove metadata for '{name}'"))?;

        Ok(())
    }

    /// Resizes a volume to a new size (grow only).
    ///
    /// Extends the sparse file with `truncate` and expands the filesystem
    /// with `resize2fs`. Shrinking is not supported.
    ///
    /// # Errors
    ///
    /// Returns an error if the volume does not exist, the new size is not
    /// larger than the current size, or the filesystem tools fail.
    pub fn resize(&self, name: &str, new_size_mib: u64) -> anyhow::Result<VolumeInfo> {
        let mut info = self.inspect(name)?;

        anyhow::ensure!(
            new_size_mib > info.size_mib,
            "new size ({new_size_mib} MiB) must be larger than current size ({} MiB)",
            info.size_mib
        );

        let ext4_path = self.ext4_path(name);

        // Extend the sparse file.
        let size_bytes = new_size_mib * 1024 * 1024;
        let output = std::process::Command::new("truncate")
            .arg("-s")
            .arg(size_bytes.to_string())
            .arg(&ext4_path)
            .output()
            .context("failed to run truncate for resize")?;

        anyhow::ensure!(
            output.status.success(),
            "truncate failed during resize: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Expand the ext4 filesystem to fill the new size.
        let resize2fs = crate::ext4::find_resize2fs()?;
        let output = std::process::Command::new(&resize2fs)
            .arg(&ext4_path)
            .output()
            .context("failed to run resize2fs")?;

        anyhow::ensure!(
            output.status.success(),
            "resize2fs failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        // Update metadata.
        info.size_mib = new_size_mib;
        let meta_path = self.meta_path(name);
        let json = serde_json::to_string_pretty(&info)
            .context("failed to serialize updated volume metadata")?;
        std::fs::write(&meta_path, json).context("failed to write updated volume metadata")?;

        Ok(info)
    }

    /// Returns the path to a volume's ext4 file.
    fn ext4_path(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{name}.ext4"))
    }

    /// Returns the path to a volume's metadata file.
    fn meta_path(&self, name: &str) -> PathBuf {
        self.base_dir.join(format!("{name}.json"))
    }
}

/// Validates that a volume name is well-formed.
///
/// Names must be 1–64 characters, start with an alphanumeric character,
/// and contain only alphanumeric characters, hyphens, or underscores.
fn validate_name(name: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!name.is_empty(), "volume name must not be empty");
    anyhow::ensure!(
        name.len() <= 64,
        "volume name must be 64 characters or fewer"
    );
    anyhow::ensure!(
        name.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'),
        "volume name must contain only alphanumeric characters, hyphens, and underscores"
    );
    anyhow::ensure!(
        name.starts_with(|c: char| c.is_ascii_alphanumeric()),
        "volume name must start with an alphanumeric character"
    );
    Ok(())
}

#[cfg(test)]
#[path = "volume_test.rs"]
mod tests;
