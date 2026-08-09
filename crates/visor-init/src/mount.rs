//! Filesystem mount operations for guest boot.
//!
//! Handles mounting `/proc`, `/sys`, `/dev`, and performing `pivot_root`
//! to switch to the OCI rootfs.

use std::fs;
use std::path::Path;

use anyhow::{Context, bail};
use nix::mount::{MntFlags, MsFlags, mount, umount2};
use nix::sys::stat::{self, Mode, SFlag, makedev};

/// A single filesystem mount entry for guest initialization.
///
/// Describes what to mount, where, and with which flags. Used in
/// [`INIT_MOUNTS`] to define the standard mount sequence for guest boot.
#[derive(Debug, Clone, Copy)]
pub struct MountEntry {
    /// Mount source (e.g., `"proc"`, `"devtmpfs"`).
    pub source: &'static str,
    /// Mount target path (e.g., `"/proc"`, `"/dev"`).
    pub target: &'static str,
    /// Filesystem type (e.g., `"proc"`, `"sysfs"`).
    pub fstype: &'static str,
    /// Mount flags controlling behavior.
    pub flags: MsFlags,
    /// Optional filesystem-specific data string.
    pub data: Option<&'static str>,
}

impl std::fmt::Display for MountEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} on {} type {}", self.source, self.target, self.fstype)
    }
}

/// Standard mount table for guest initialization.
///
/// These filesystems are mounted in order during early guest boot,
/// before the OCI rootfs pivot.
pub const INIT_MOUNTS: &[MountEntry] = &[
    MountEntry {
        source: "proc",
        target: "/proc",
        fstype: "proc",
        flags: MsFlags::MS_NOSUID
            .union(MsFlags::MS_NODEV)
            .union(MsFlags::MS_NOEXEC),
        data: None,
    },
    MountEntry {
        source: "sysfs",
        target: "/sys",
        fstype: "sysfs",
        flags: MsFlags::MS_NOSUID
            .union(MsFlags::MS_NODEV)
            .union(MsFlags::MS_NOEXEC),
        data: None,
    },
    MountEntry {
        source: "cgroup2",
        target: "/sys/fs/cgroup",
        fstype: "cgroup2",
        flags: MsFlags::MS_NOSUID
            .union(MsFlags::MS_NODEV)
            .union(MsFlags::MS_NOEXEC),
        data: None,
    },
    MountEntry {
        source: "devtmpfs",
        target: "/dev",
        fstype: "devtmpfs",
        flags: MsFlags::MS_NOSUID,
        data: Some("mode=0755"),
    },
    MountEntry {
        source: "devpts",
        target: "/dev/pts",
        fstype: "devpts",
        flags: MsFlags::MS_NOSUID.union(MsFlags::MS_NOEXEC),
        data: Some("newinstance,ptmxmode=0666"),
    },
    MountEntry {
        source: "tmpfs",
        target: "/dev/shm",
        fstype: "tmpfs",
        flags: MsFlags::MS_NOSUID.union(MsFlags::MS_NODEV),
        data: Some("mode=1777"),
    },
    MountEntry {
        source: "tmpfs",
        target: "/run",
        fstype: "tmpfs",
        flags: MsFlags::MS_NOSUID.union(MsFlags::MS_NODEV),
        data: Some("mode=0755"),
    },
];

/// Essential device nodes created during guest boot.
///
/// Each entry is `(path, major, minor, mode)`.
const ESSENTIAL_DEVICES: &[(&str, u64, u64, u32)] = &[
    ("/dev/null", 1, 3, 0o666),
    ("/dev/zero", 1, 5, 0o666),
    ("/dev/urandom", 1, 9, 0o444),
];

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const WORKLOAD_CGROUP: &str = "visor";

/// Mount all initial filesystems required for guest boot.
///
/// Iterates [`INIT_MOUNTS`], creates each target directory if it does not
/// exist, and performs the mount syscall. If a mount fails with `EBUSY`
/// (device or resource busy), the error is silently ignored because the
/// kernel may have already mounted the filesystem (e.g. `devtmpfs` at
/// `/dev` is auto-mounted by the kernel when `CONFIG_DEVTMPFS_MOUNT=y`).
///
/// # Errors
///
/// Returns an error if any target directory cannot be created or any
/// mount syscall fails with an error other than `EBUSY`.
#[must_use = "mount errors must be handled"]
pub fn mount_initial_filesystems() -> anyhow::Result<()> {
    for entry in INIT_MOUNTS {
        let target = Path::new(entry.target);
        if !target.exists() {
            fs::create_dir_all(target)
                .with_context(|| format!("failed to create mount target {}", entry.target))?;
        }

        let result = mount(
            Some(entry.source),
            entry.target,
            Some(entry.fstype),
            entry.flags,
            entry.data,
        );

        match result {
            // EBUSY: already mounted (e.g. kernel auto-mounted devtmpfs) — skip
            Ok(()) | Err(nix::errno::Errno::EBUSY) => {}
            Err(e) => {
                return Err(e).with_context(|| {
                    format!("failed to mount {} on {}", entry.source, entry.target)
                });
            }
        }
    }
    Ok(())
}

/// Constrain all guest workload processes through the cgroup v2 PIDs controller.
///
/// The guest init process moves itself into a child cgroup before enabling the
/// controller on the root. Every command it subsequently launches inherits the
/// limit.
///
/// # Errors
///
/// Returns an error when the limit is zero or the guest cgroup hierarchy cannot
/// be configured.
pub fn configure_process_limit(limit: u64) -> anyhow::Result<()> {
    configure_process_limit_at(Path::new(CGROUP_ROOT), limit)
}

fn configure_process_limit_at(root: &Path, limit: u64) -> anyhow::Result<()> {
    anyhow::ensure!(limit > 0, "process limit must be greater than zero");

    let workload = root.join(WORKLOAD_CGROUP);
    fs::create_dir_all(&workload).context("create guest workload cgroup")?;
    fs::write(workload.join("cgroup.procs"), "0")
        .context("move guest init into workload cgroup")?;
    fs::write(root.join("cgroup.subtree_control"), "+pids")
        .context("enable guest pids controller")?;
    fs::write(workload.join("pids.max"), limit.to_string()).context("set guest process limit")?;
    Ok(())
}

/// Validate a new root path for `pivot_root`.
///
/// The path must be absolute (start with `/`) and must not be the
/// filesystem root itself.
///
/// # Errors
///
/// Returns an error if the path is not absolute or is `/`.
fn validate_pivot_root_path(new_root: &str) -> anyhow::Result<()> {
    if !new_root.starts_with('/') {
        bail!("pivot_root: new_root must be an absolute path, got: {new_root}");
    }
    if new_root == "/" {
        bail!("pivot_root: new_root must not be /, cannot pivot to current root");
    }
    Ok(())
}

/// Perform `pivot_root` to switch the guest root filesystem.
///
/// Creates a temporary `.old_root` directory under `new_root`, calls
/// `pivot_root(new_root, old_root)`, changes to the new root, then
/// unmounts and removes the old root mount point.
///
/// # Errors
///
/// Returns an error if `new_root` is invalid, the old root directory
/// cannot be created, `pivot_root` fails, or cleanup fails.
pub fn pivot_root(new_root: &str) -> anyhow::Result<()> {
    validate_pivot_root_path(new_root)?;

    let old_root = format!("{new_root}/.old_root");

    if !Path::new(&old_root).exists() {
        fs::create_dir_all(&old_root)
            .with_context(|| format!("failed to create old_root directory at {old_root}"))?;
    }

    nix::unistd::pivot_root(new_root, old_root.as_str())
        .with_context(|| format!("pivot_root({new_root}, {old_root}) failed"))?;

    std::env::set_current_dir("/").context("failed to chdir to / after pivot_root")?;

    umount2("/.old_root", MntFlags::MNT_DETACH)
        .context("failed to unmount old root at /.old_root")?;

    fs::remove_dir("/.old_root").context("failed to remove /.old_root directory")?;

    Ok(())
}

/// Create essential device nodes if they do not already exist.
///
/// Creates `/dev/null`, `/dev/zero`, and `/dev/urandom` as character
/// devices with the standard Linux major/minor numbers.
///
/// # Errors
///
/// Returns an error if any `mknod` call fails (e.g., insufficient
/// permissions or `/dev` is not mounted).
pub fn create_essential_devices() -> anyhow::Result<()> {
    for &(path, major, minor, mode) in ESSENTIAL_DEVICES {
        if Path::new(path).exists() {
            continue;
        }

        let dev = makedev(major, minor);
        let perm = Mode::from_bits(mode).context("invalid permission bits for essential device")?;

        stat::mknod(path, SFlag::S_IFCHR, perm, dev)
            .with_context(|| format!("failed to create device node {path}"))?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "mount_test.rs"]
mod tests;
