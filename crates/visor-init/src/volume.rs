//! Volume mount handling for host→guest file sharing.
//!
//! Mounts host directories into the guest at specified paths using
//! bind mounts via the block device or virtio-fs.

use std::path::Path;

use anyhow::Context as _;

use crate::config::VolumeConfig;

/// Mounts all configured volumes inside the guest.
///
/// For each volume, creates the target directory if it doesn't exist,
/// then performs a bind mount from the host path to the guest path.
/// Read-only volumes are remounted with `MS_RDONLY` after the initial bind.
///
/// # Errors
///
/// Returns an error if directory creation or any mount operation fails.
pub fn mount_volumes(volumes: &[VolumeConfig]) -> anyhow::Result<()> {
    for volume in volumes {
        mount_single_volume(volume).with_context(|| {
            format!(
                "failed to mount volume {} -> {}",
                volume.host_path, volume.guest_path
            )
        })?;
    }
    Ok(())
}

/// Mounts a single volume inside the guest.
///
/// # Errors
///
/// Returns an error if the target directory cannot be created or the mount fails.
pub fn mount_single_volume(volume: &VolumeConfig) -> anyhow::Result<()> {
    validate_volume(volume)?;

    let guest_path = Path::new(&volume.guest_path);
    if !guest_path.exists() {
        std::fs::create_dir_all(guest_path)
            .with_context(|| format!("failed to create mount point: {}", volume.guest_path))?;
    }

    if !volume.mount_tag.is_empty() {
        mount_virtiofs_volume(volume)?;
    } else if !volume.device_path.is_empty() {
        mount_block_volume(volume)?;
    } else {
        mount_legacy_bind_volume(volume)?;
    }

    Ok(())
}

fn mount_virtiofs_volume(volume: &VolumeConfig) -> anyhow::Result<()> {
    let mut flags = nix::mount::MsFlags::empty();
    if volume.read_only {
        flags |= nix::mount::MsFlags::MS_RDONLY;
    }

    nix::mount::mount(
        Some(volume.mount_tag.as_str()),
        volume.guest_path.as_str(),
        Some("virtiofs"),
        flags,
        None::<&str>,
    )
    .with_context(|| {
        format!(
            "virtio-fs mount failed: {} -> {}",
            volume.mount_tag, volume.guest_path
        )
    })
}

fn mount_block_volume(volume: &VolumeConfig) -> anyhow::Result<()> {
    let mut flags = nix::mount::MsFlags::empty();
    if volume.read_only {
        flags |= nix::mount::MsFlags::MS_RDONLY;
    }

    let fs_type = if volume.fs_type.is_empty() {
        "ext4"
    } else {
        volume.fs_type.as_str()
    };
    nix::mount::mount(
        Some(volume.device_path.as_str()),
        volume.guest_path.as_str(),
        Some(fs_type),
        flags,
        None::<&str>,
    )
    .with_context(|| {
        format!(
            "block volume mount failed: {} -> {}",
            volume.device_path, volume.guest_path
        )
    })
}

fn mount_legacy_bind_volume(volume: &VolumeConfig) -> anyhow::Result<()> {
    nix::mount::mount(
        Some(volume.host_path.as_str()),
        volume.guest_path.as_str(),
        None::<&str>,
        nix::mount::MsFlags::MS_BIND,
        None::<&str>,
    )
    .with_context(|| {
        format!(
            "bind mount failed: {} -> {}",
            volume.host_path, volume.guest_path
        )
    })?;

    if volume.read_only {
        nix::mount::mount(
            None::<&str>,
            volume.guest_path.as_str(),
            None::<&str>,
            nix::mount::MsFlags::MS_BIND
                | nix::mount::MsFlags::MS_REMOUNT
                | nix::mount::MsFlags::MS_RDONLY,
            None::<&str>,
        )
        .with_context(|| format!("read-only remount failed: {}", volume.guest_path))?;
    }

    Ok(())
}

/// Validates a volume configuration.
///
/// # Errors
///
/// Returns an error if the guest path is not absolute or no mount source is provided.
pub fn validate_volume(volume: &VolumeConfig) -> anyhow::Result<()> {
    anyhow::ensure!(
        !volume.guest_path.is_empty(),
        "volume guest_path must not be empty"
    );
    anyhow::ensure!(
        volume.guest_path.starts_with('/'),
        "volume guest_path must be absolute: {}",
        volume.guest_path
    );
    anyhow::ensure!(
        !volume.host_path.is_empty()
            || !volume.mount_tag.is_empty()
            || !volume.device_path.is_empty(),
        "volume mount source must not be empty"
    );
    Ok(())
}

#[cfg(test)]
#[path = "volume_test.rs"]
mod tests;
