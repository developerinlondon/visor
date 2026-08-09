use std::collections::HashSet;

use nix::mount::MsFlags;

use super::*;

#[test]
fn init_mounts_has_expected_entry_count() {
    assert_eq!(INIT_MOUNTS.len(), 7);
}

#[test]
fn all_mount_entries_have_non_empty_fields() {
    for entry in INIT_MOUNTS {
        assert!(
            !entry.source.is_empty(),
            "source is empty for {}",
            entry.target
        );
        assert!(
            !entry.target.is_empty(),
            "target is empty for {}",
            entry.source
        );
        assert!(
            !entry.fstype.is_empty(),
            "fstype is empty for {}",
            entry.target
        );
    }
}

#[test]
fn all_mount_targets_are_absolute_paths() {
    for entry in INIT_MOUNTS {
        assert!(
            entry.target.starts_with('/'),
            "target {} does not start with /",
            entry.target
        );
    }
}

#[test]
fn proc_mount_has_nosuid_nodev_noexec_flags() {
    let proc_entry = INIT_MOUNTS
        .iter()
        .find(|e| e.fstype == "proc")
        .expect("proc mount must exist");

    let expected = MsFlags::MS_NOSUID
        .union(MsFlags::MS_NODEV)
        .union(MsFlags::MS_NOEXEC);
    assert_eq!(proc_entry.flags, expected);
}

#[test]
fn sysfs_mount_has_nosuid_nodev_noexec_flags() {
    let sysfs_entry = INIT_MOUNTS
        .iter()
        .find(|e| e.fstype == "sysfs")
        .expect("sysfs mount must exist");

    let expected = MsFlags::MS_NOSUID
        .union(MsFlags::MS_NODEV)
        .union(MsFlags::MS_NOEXEC);
    assert_eq!(sysfs_entry.flags, expected);
}

#[test]
fn dev_mount_has_nosuid_flag() {
    let dev_entry = INIT_MOUNTS
        .iter()
        .find(|e| e.target == "/dev")
        .expect("/dev mount must exist");

    assert!(dev_entry.flags.contains(MsFlags::MS_NOSUID));
}

#[test]
fn devpts_mount_data_contains_newinstance() {
    let devpts_entry = INIT_MOUNTS
        .iter()
        .find(|e| e.fstype == "devpts")
        .expect("devpts mount must exist");

    let data = devpts_entry.data.expect("devpts must have data");
    assert!(
        data.contains("newinstance"),
        "devpts data should contain 'newinstance', got: {data}"
    );
}

#[test]
fn mount_entry_display_formatting() {
    let entry = MountEntry {
        source: "proc",
        target: "/proc",
        fstype: "proc",
        flags: MsFlags::empty(),
        data: None,
    };
    let display = format!("{entry}");
    assert_eq!(display, "proc on /proc type proc");
}

#[test]
fn mount_entry_debug_formatting() {
    let entry = MountEntry {
        source: "tmpfs",
        target: "/run",
        fstype: "tmpfs",
        flags: MsFlags::MS_NOSUID,
        data: Some("mode=0755"),
    };
    let debug = format!("{entry:?}");
    assert!(
        debug.contains("tmpfs"),
        "debug should contain source: {debug}"
    );
    assert!(
        debug.contains("/run"),
        "debug should contain target: {debug}"
    );
    assert!(
        debug.contains("mode=0755"),
        "debug should contain data: {debug}"
    );
}

#[test]
fn no_duplicate_mount_targets() {
    let mut targets = HashSet::new();
    for entry in INIT_MOUNTS {
        assert!(
            targets.insert(entry.target),
            "duplicate mount target: {}",
            entry.target
        );
    }
}

#[test]
fn pivot_root_rejects_relative_path() {
    let err = validate_pivot_root_path("relative/path").unwrap_err();
    assert!(
        err.to_string().contains("absolute"),
        "error should mention absolute path: {err}"
    );
}

#[test]
fn pivot_root_rejects_root_path() {
    let err = validate_pivot_root_path("/").unwrap_err();
    assert!(
        err.to_string().contains("must not be /"),
        "error should mention must not be /: {err}"
    );
}

#[test]
fn pivot_root_accepts_valid_path() {
    validate_pivot_root_path("/mnt/rootfs").unwrap();
}

#[test]
fn proc_and_sysfs_have_noexec_for_security() {
    let security_targets = ["/proc", "/sys"];
    for target in &security_targets {
        let entry = INIT_MOUNTS
            .iter()
            .find(|e| e.target == *target)
            .unwrap_or_else(|| panic!("{target} mount must exist"));

        assert!(
            entry.flags.contains(MsFlags::MS_NOEXEC),
            "{target} should have MS_NOEXEC for security"
        );
    }
}

#[test]
fn cgroup2_mount_exists_for_nested_runtimes() {
    let entry = INIT_MOUNTS
        .iter()
        .find(|e| e.target == "/sys/fs/cgroup")
        .expect("/sys/fs/cgroup mount must exist");

    assert_eq!(entry.source, "cgroup2");
    assert_eq!(entry.fstype, "cgroup2");
    assert!(entry.flags.contains(MsFlags::MS_NOSUID));
    assert!(entry.flags.contains(MsFlags::MS_NODEV));
    assert!(entry.flags.contains(MsFlags::MS_NOEXEC));
}

#[test]
fn process_limit_moves_init_into_a_limited_cgroup() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("cgroup.subtree_control"), "").unwrap();

    configure_process_limit_at(root.path(), 256).unwrap();

    assert_eq!(
        std::fs::read_to_string(root.path().join("cgroup.subtree_control")).unwrap(),
        "+pids"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("visor/cgroup.procs")).unwrap(),
        "0"
    );
    assert_eq!(
        std::fs::read_to_string(root.path().join("visor/pids.max")).unwrap(),
        "256"
    );
}

#[test]
fn process_limit_rejects_zero_before_creating_a_cgroup() {
    let root = tempfile::tempdir().unwrap();

    let error = configure_process_limit_at(root.path(), 0).unwrap_err();

    assert!(error.to_string().contains("greater than zero"));
    assert!(!root.path().join("visor").exists());
}
