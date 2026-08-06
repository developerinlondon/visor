//! Tests for snapshot fast-path restore (`boot_from_snapshot`).

use std::path::{Path, PathBuf};

use super::*;
use crate::guest_virtualization::GuestVirtualizationMode;

// ── SnapshotRestoreConfig Tests ─────────────────────────────────────

#[test]
fn snapshot_restore_config_construction() {
    let config = SnapshotRestoreConfig {
        snapshot_dir: PathBuf::from("/snapshots/abc123"),
        memory_mib: 256,
        guest_cid: 5,
        guest_virtualization: GuestVirtualizationMode::Standard,
        shared_dirs: vec![PathBuf::from("/host/data")],
        data_disks: Vec::new(),
        network: None,
        networks: Vec::new(),
    };
    assert_eq!(config.snapshot_dir, Path::new("/snapshots/abc123"));
    assert_eq!(config.memory_mib, 256);
    assert_eq!(config.guest_cid, 5);
    assert_eq!(config.shared_dirs.len(), 1);
}

#[test]
fn snapshot_restore_config_empty_shared_dirs() {
    let config = SnapshotRestoreConfig {
        snapshot_dir: PathBuf::from("/snap"),
        memory_mib: 128,
        guest_cid: 3,
        guest_virtualization: GuestVirtualizationMode::Standard,
        shared_dirs: Vec::new(),
        data_disks: Vec::new(),
        network: None,
        networks: Vec::new(),
    };
    assert!(config.shared_dirs.is_empty());
}

#[test]
fn snapshot_restore_config_debug_format() {
    let config = SnapshotRestoreConfig {
        snapshot_dir: PathBuf::from("/snap"),
        memory_mib: 512,
        guest_cid: 7,
        guest_virtualization: GuestVirtualizationMode::Standard,
        shared_dirs: Vec::new(),
        data_disks: Vec::new(),
        network: None,
        networks: Vec::new(),
    };
    let debug = format!("{config:?}");
    assert!(
        debug.contains("SnapshotRestoreConfig"),
        "Debug should contain type name: {debug}"
    );
    assert!(
        debug.contains("512"),
        "Debug should contain memory_mib: {debug}"
    );
}

// ── CpuInitMode Tests ───────────────────────────────────────────────

#[cfg(target_os = "macos")]
#[test]
fn cpu_init_mode_restore_variant_exists() {
    let mode = CpuInitMode::Restore;
    let debug = format!("{mode:?}");
    assert!(
        debug.contains("Restore"),
        "Debug should contain variant name: {debug}"
    );
}

// ── VmBootError::Snapshot Tests ─────────────────────────────────────

#[test]
fn vm_boot_error_snapshot_display() {
    let snap_err = crate::snapshot::SnapshotError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "memory.bin not found",
    ));
    let err = VmBootError::Snapshot(snap_err);
    let msg = format!("{err}");
    assert!(msg.contains("snapshot"), "display: {msg}");
}

#[test]
fn vm_boot_error_from_snapshot_error() {
    let snap_err = crate::snapshot::SnapshotError::Io(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "missing",
    ));
    let vm_err: VmBootError = snap_err.into();
    assert!(
        matches!(vm_err, VmBootError::Snapshot(_)),
        "From<SnapshotError> should produce VmBootError::Snapshot"
    );
}

// ── boot_from_snapshot error path tests ─────────────────────────────

#[test]
fn boot_from_snapshot_fails_with_missing_snapshot_dir() {
    let config = SnapshotRestoreConfig {
        snapshot_dir: PathBuf::from("/nonexistent/snapshot/dir"),
        memory_mib: 128,
        guest_cid: 3,
        guest_virtualization: GuestVirtualizationMode::Standard,
        shared_dirs: Vec::new(),
        data_disks: Vec::new(),
        network: None,
        networks: Vec::new(),
    };
    let result = boot_from_snapshot(&config);
    assert!(
        result.is_err(),
        "boot_from_snapshot should fail with missing snapshot dir"
    );
}

#[test]
fn boot_from_snapshot_fails_with_empty_snapshot_dir() {
    let dir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
    // Empty dir — no memory.bin or cpu_state.json
    let config = SnapshotRestoreConfig {
        snapshot_dir: dir.path().to_path_buf(),
        memory_mib: 128,
        guest_cid: 3,
        guest_virtualization: GuestVirtualizationMode::Standard,
        shared_dirs: Vec::new(),
        data_disks: Vec::new(),
        network: None,
        networks: Vec::new(),
    };
    let result = boot_from_snapshot(&config);
    assert!(
        result.is_err(),
        "boot_from_snapshot should fail with empty snapshot dir (no memory.bin)"
    );
}
