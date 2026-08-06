use super::*;
use crate::backend::{PortMapping, VmConfig};

// ── Test helpers ─────────────────────────────────────────────────

fn sample_config() -> VmConfig {
    serde_json::from_str(r#"{"image": "alpine:latest", "memory_mib": 256, "vcpus": 2}"#).unwrap()
}

fn sample_meta(id: &str) -> VmMeta {
    VmMeta {
        id: id.to_owned(),
        name: Some("test-vm".to_owned()),
        image: "alpine:latest".to_owned(),
        config: sample_config(),
        created_at: "2026-01-15T10:30:00Z".to_owned(),
        cid: 3,
        memory_mib: 256,
        vcpus: 2,
        ports: vec![PortMapping::new(8080, 80)],
    }
}

// ── Round-trip serialization ────────────────────────────────────

#[test]
fn test_vm_meta_serializes_round_trip() {
    let meta = sample_meta("vm-rt-001");
    let json = serde_json::to_string_pretty(&meta).unwrap();
    let restored: VmMeta = serde_json::from_str(&json).unwrap();

    assert_eq!(restored.id, "vm-rt-001");
    assert_eq!(restored.name.as_deref(), Some("test-vm"));
    assert_eq!(restored.image, "alpine:latest");
    assert_eq!(restored.created_at, "2026-01-15T10:30:00Z");
    assert_eq!(restored.cid, 3);
    assert_eq!(restored.memory_mib, 256);
    assert_eq!(restored.vcpus, 2);
    assert_eq!(restored.ports.len(), 1);
    assert_eq!(restored.ports[0].host_port, 8080);
    assert_eq!(restored.ports[0].guest_port, 80);
    assert_eq!(restored.config.image, "alpine:latest");
    assert_eq!(restored.config.memory_mib, 256);
    assert_eq!(restored.config.vcpus, 2);
}

// ── Snapshot writes state files ─────────────────────────────────

#[test]
fn test_snapshot_vm_writes_state_files() {
    let dir = crate::testutil::tempdir("visor-runtime-state-").unwrap();
    let vm_dir = dir.path().join("vm-snap-001");

    let meta = sample_meta("vm-snap-001");
    save_vm_meta(&vm_dir, &meta).unwrap();

    assert!(vm_dir.join("vm_meta.json").exists());

    let contents = std::fs::read_to_string(vm_dir.join("vm_meta.json")).unwrap();
    assert!(contents.contains("vm-snap-001"));
    assert!(contents.contains("alpine:latest"));
}

// ── Restore reads state files ───────────────────────────────────

#[test]
fn test_restore_vm_reads_state_files() {
    let dir = crate::testutil::tempdir("visor-runtime-state-").unwrap();
    let vm_dir = dir.path().join("vm-restore-001");

    let original = sample_meta("vm-restore-001");
    save_vm_meta(&vm_dir, &original).unwrap();

    let restored = load_vm_meta(&vm_dir).unwrap();

    assert_eq!(restored.id, "vm-restore-001");
    assert_eq!(restored.name.as_deref(), Some("test-vm"));
    assert_eq!(restored.image, "alpine:latest");
    assert_eq!(restored.cid, 3);
    assert_eq!(restored.memory_mib, 256);
    assert_eq!(restored.vcpus, 2);
    assert_eq!(restored.ports.len(), 1);
}

// ── Scan state dir finds VMs ────────────────────────────────────

#[test]
fn test_scan_state_dir_finds_vms() {
    let dir = crate::testutil::tempdir("visor-runtime-state-").unwrap();

    // Create two VM directories with valid metadata.
    for id in &["vm-scan-a", "vm-scan-b"] {
        let vm_dir = dir.path().join(id);
        save_vm_meta(&vm_dir, &sample_meta(id)).unwrap();
    }

    let mut ids = scan_state_dir(dir.path()).unwrap();
    ids.sort();

    assert_eq!(ids.len(), 2);
    assert_eq!(ids[0], "vm-scan-a");
    assert_eq!(ids[1], "vm-scan-b");
}

// ── Scan empty state dir ────────────────────────────────────────

#[test]
fn test_scan_state_dir_empty() {
    let dir = crate::testutil::tempdir("visor-runtime-state-").unwrap();
    let ids = scan_state_dir(dir.path()).unwrap();
    assert!(ids.is_empty());
}

// ── Crash recovery removes incomplete ───────────────────────────

#[test]
fn test_crash_recovery_removes_incomplete() {
    let dir = crate::testutil::tempdir("visor-runtime-state-").unwrap();

    // Valid VM with metadata.
    save_vm_meta(&dir.path().join("vm-valid"), &sample_meta("vm-valid")).unwrap();

    // Incomplete VM — directory exists but no vm_meta.json.
    std::fs::create_dir_all(dir.path().join("vm-incomplete")).unwrap();

    let removed = cleanup_incomplete(dir.path()).unwrap();
    assert_eq!(removed, 1);

    // Valid should still exist, incomplete should be gone.
    assert!(dir.path().join("vm-valid").exists());
    assert!(!dir.path().join("vm-incomplete").exists());
}

// ── Snapshot creates state directory ────────────────────────────

#[test]
fn test_snapshot_creates_state_directory() {
    let dir = crate::testutil::tempdir("visor-runtime-state-").unwrap();
    let vm_dir = dir.path().join("deep").join("nested").join("vm-mkdir-001");

    assert!(!vm_dir.exists());

    let meta = sample_meta("vm-mkdir-001");
    save_vm_meta(&vm_dir, &meta).unwrap();

    assert!(vm_dir.exists());
    assert!(vm_dir.join("vm_meta.json").exists());
}
