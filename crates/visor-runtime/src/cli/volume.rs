//! `visor volume` — manage persistent volumes.
//!
//! Provides create, list, remove, and resize operations for persistent ext4
//! volumes that can be attached to VMs. All operations work directly on the
//! local filesystem without requiring a running daemon.

use anyhow::Context;
use clap::Subcommand;

use crate::volume::VolumeManager;

/// Volume management subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum VolumeCommand {
    /// Create a new persistent volume.
    Create(CreateArgs),
    /// List all persistent volumes.
    Ls,
    /// Remove a persistent volume.
    Rm(RemoveArgs),
    /// Resize a persistent volume (grow only).
    Resize(ResizeArgs),
}

/// Arguments for `visor volume create`.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct CreateArgs {
    /// Volume name (alphanumeric, hyphens, underscores).
    pub name: String,
    /// Volume size in MiB.
    #[arg(long)]
    pub size: u64,
}

/// Arguments for `visor volume rm`.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct RemoveArgs {
    /// Volume name to remove.
    pub name: String,
}

/// Arguments for `visor volume resize`.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct ResizeArgs {
    /// Volume name to resize.
    pub name: String,
    /// New volume size in MiB (must be larger than current).
    #[arg(long)]
    pub size: u64,
}

/// Executes a volume subcommand.
///
/// All operations work directly on the local filesystem using
/// [`VolumeManager`] without requiring a running daemon.
///
/// # Errors
///
/// Returns an error if the volume directory cannot be determined or
/// the requested volume operation fails.
pub fn execute(command: VolumeCommand) -> anyhow::Result<()> {
    let base_dir = VolumeManager::default_dir().context("failed to determine volume directory")?;
    let mgr = VolumeManager::new(&base_dir).context("failed to initialize volume manager")?;

    match command {
        VolumeCommand::Create(args) => {
            let info = mgr
                .create(&args.name, args.size)
                .with_context(|| format!("failed to create volume '{}'", args.name))?;
            println!(
                "Created volume '{}' ({} MiB) at {}",
                info.name, info.size_mib, info.path
            );
        }
        VolumeCommand::Ls => {
            let volumes = mgr.list().context("failed to list volumes")?;

            println!("{:<20}  {:>8}  {:<24}  PATH", "NAME", "SIZE", "CREATED");
            for vol in &volumes {
                println!(
                    "{:<20}  {:>5} MiB  {:<24}  {}",
                    vol.name, vol.size_mib, vol.created_at, vol.path,
                );
            }
        }
        VolumeCommand::Rm(args) => {
            mgr.remove(&args.name)
                .with_context(|| format!("failed to remove volume '{}'", args.name))?;
            println!("Removed volume '{}'", args.name);
        }
        VolumeCommand::Resize(args) => {
            let info = mgr
                .resize(&args.name, args.size)
                .with_context(|| format!("failed to resize volume '{}'", args.name))?;
            println!("Resized volume '{}' to {} MiB", info.name, info.size_mib);
        }
    }

    Ok(())
}
