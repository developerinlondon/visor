//! VM metadata persistence: save/load/scan/cleanup for daemon restart recovery.
//!
//! Each VM's metadata is stored in `~/.visor/state/<vm_id>/vm_meta.json`.
//! On shutdown the daemon snapshots all running VMs; on startup it restores
//! them into the backend as stopped VMs.

use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::{Deserialize, Serialize};

use crate::backend::{PortMapping, VmConfig};

/// VM metadata persisted to disk.
///
/// Contains everything needed to reconstruct a [`VmInfo`](crate::backend::VmInfo)
/// after a daemon restart. Does NOT include live CPU/memory state — that is a
/// follow-up integration with `visor-vmm/snapshot.rs`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VmMeta {
    /// Unique VM identifier (UUID v4).
    pub id: String,
    /// Optional human-readable name.
    pub name: Option<String>,
    /// OCI image the VM was created from.
    pub image: String,
    /// Full VM configuration at creation time.
    pub config: VmConfig,
    /// ISO 8601 timestamp of when the VM was created.
    pub created_at: String,
    /// Vsock context ID (CID 3+).
    pub cid: u32,
    /// Memory allocation in MiB.
    pub memory_mib: u32,
    /// Number of virtual CPUs.
    pub vcpus: u32,
    /// Active port mappings.
    pub ports: Vec<PortMapping>,
}

const META_FILENAME: &str = "vm_meta.json";

/// Returns the default state directory (`$VISOR_HOME/state` or
/// `$HOME/.visor/state`).
///
/// # Errors
///
/// Returns an error if neither `VISOR_HOME` nor `HOME` is available.
pub fn state_dir() -> anyhow::Result<PathBuf> {
    crate::paths::persistent_subdir("state").context("determine VM state directory")
}

/// Returns the state directory for a specific VM (`~/.visor/state/<vm_id>/`).
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined.
pub fn vm_state_dir(vm_id: &str) -> anyhow::Result<PathBuf> {
    Ok(state_dir()?.join(vm_id))
}

/// Writes `vm_meta.json` into the given directory.
///
/// Creates the directory if it does not exist.
///
/// # Errors
///
/// Returns an error if the directory cannot be created or the file cannot be written.
pub fn save_vm_meta(dir: &Path, meta: &VmMeta) -> anyhow::Result<()> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create state directory {}", dir.display()))?;
    let path = dir.join(META_FILENAME);
    let json = serde_json::to_string_pretty(meta).context("serialize VmMeta to JSON")?;
    std::fs::write(&path, json).with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// Reads `vm_meta.json` from the given directory.
///
/// # Errors
///
/// Returns an error if the file does not exist or contains invalid JSON.
pub fn load_vm_meta(dir: &Path) -> anyhow::Result<VmMeta> {
    let path = dir.join(META_FILENAME);
    let data =
        std::fs::read_to_string(&path).with_context(|| format!("read {}", path.display()))?;
    let meta: VmMeta =
        serde_json::from_str(&data).with_context(|| format!("parse {}", path.display()))?;
    Ok(meta)
}

/// Lists VM IDs by scanning subdirectories of `base`.
///
/// Each subdirectory name is treated as a VM ID.
///
/// # Errors
///
/// Returns an error if `base` cannot be read.
pub fn scan_state_dir(base: &Path) -> anyhow::Result<Vec<String>> {
    if !base.exists() {
        return Ok(Vec::new());
    }
    let mut ids = Vec::new();
    let entries = std::fs::read_dir(base)
        .with_context(|| format!("read state directory {}", base.display()))?;
    for entry in entries {
        let entry = entry.context("read directory entry")?;
        if entry.path().is_dir() {
            if let Some(name) = entry.file_name().to_str() {
                ids.push(name.to_owned());
            }
        }
    }
    Ok(ids)
}

/// Removes state directories that lack a `vm_meta.json` file (crash recovery).
///
/// Returns the number of incomplete directories removed.
///
/// # Errors
///
/// Returns an error if `base` cannot be read or a directory cannot be removed.
pub fn cleanup_incomplete(base: &Path) -> anyhow::Result<usize> {
    if !base.exists() {
        return Ok(0);
    }
    let mut removed = 0;
    let entries = std::fs::read_dir(base)
        .with_context(|| format!("read state directory {}", base.display()))?;
    for entry in entries {
        let entry = entry.context("read directory entry")?;
        let path = entry.path();
        if path.is_dir() && !path.join(META_FILENAME).exists() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("remove incomplete state dir {}", path.display()))?;
            removed += 1;
        }
    }
    Ok(removed)
}

/// Removes a VM's entire state directory.
///
/// # Errors
///
/// Returns an error if the home directory cannot be determined or the
/// directory cannot be removed.
pub fn remove_vm_state(vm_id: &str) -> anyhow::Result<()> {
    let dir = vm_state_dir(vm_id)?;
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("remove state dir {}", dir.display()))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "persistence_test.rs"]
mod tests;
