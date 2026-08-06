//! Shell access for interactive debugging.
//!
//! Provides shell sessions via `visor shell` by spawning `/bin/sh` (from the
//! OCI image) or falling back to `/bin/toybox sh` for minimal images.

use std::path::Path;

use anyhow::Context as _;

/// Search paths for finding a usable shell binary, in priority order.
pub const SHELL_SEARCH_PATHS: &[&str] = &["/bin/sh", "/bin/bash", "/bin/ash", "/bin/toybox"];

/// Finds the first available shell binary from the search paths.
///
/// Returns the path to the shell binary, or `None` if no shell is found.
#[must_use]
pub fn find_shell() -> Option<&'static str> {
    SHELL_SEARCH_PATHS
        .iter()
        .copied()
        .find(|p| Path::new(p).exists())
}

/// Resolves a shell command and arguments for spawning.
///
/// If a shell is found via [`find_shell`], returns the command and args.
/// For `/bin/toybox`, the shell subcommand `"sh"` is appended automatically.
///
/// # Errors
///
/// Returns an error if no shell binary is found on the system.
pub fn resolve_shell_command() -> anyhow::Result<Vec<String>> {
    let shell = find_shell().context("no shell binary found in guest rootfs")?;

    if shell.ends_with("toybox") {
        Ok(vec![shell.to_owned(), "sh".to_owned()])
    } else {
        Ok(vec![shell.to_owned()])
    }
}

/// Spawns an interactive shell process.
///
/// Uses [`resolve_shell_command`] to find the shell, then spawns it
/// with the given environment variables.
///
/// # Errors
///
/// Returns an error if no shell is found or the process fails to spawn.
pub fn spawn_shell(env: &[(String, String)]) -> anyhow::Result<std::process::Child> {
    let cmd = resolve_shell_command()?;
    let mut command = std::process::Command::new(&cmd[0]);
    command.args(&cmd[1..]);
    for (key, value) in env {
        command.env(key, value);
    }
    command.spawn().context("failed to spawn shell process")
}

#[cfg(test)]
#[path = "shell_test.rs"]
mod tests;
