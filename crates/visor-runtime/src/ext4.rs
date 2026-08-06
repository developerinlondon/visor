//! ext4 tool discovery for volume and rootfs operations.
//!
//! Locates `mke2fs` and `resize2fs` binaries from the `e2fsprogs` package,
//! checking both `PATH` and well-known platform-specific install locations
//! (Homebrew on macOS, `/usr/sbin` on Linux).
//!
//! When a required tool is missing, error messages include platform-specific
//! installation instructions so the user can resolve the issue without
//! searching documentation.

use std::path::{Path, PathBuf};

/// Well-known directories where e2fsprogs tools may be installed.
///
/// Includes Homebrew paths (macOS) and standard sbin paths (Linux).
/// Non-existent directories are silently skipped at runtime.
const FALLBACK_DIRS: &[&str] = &[
    "/opt/homebrew/opt/e2fsprogs/sbin", // Homebrew on Apple Silicon
    "/usr/local/opt/e2fsprogs/sbin",    // Homebrew on Intel Mac
    "/usr/sbin",
    "/sbin",
];

/// Locate the `mke2fs` binary.
///
/// Searches `PATH` via `which`, then falls back to well-known locations
/// including Homebrew prefixes on macOS.
///
/// # Errors
///
/// Returns an error with platform-specific installation instructions if
/// `mke2fs` cannot be found.
pub fn find_mke2fs() -> anyhow::Result<PathBuf> {
    find_tool("mke2fs")
}

/// Locate the `resize2fs` binary.
///
/// Searches `PATH` via `which`, then falls back to well-known locations
/// including Homebrew prefixes on macOS.
///
/// # Errors
///
/// Returns an error with platform-specific installation instructions if
/// `resize2fs` cannot be found.
pub fn find_resize2fs() -> anyhow::Result<PathBuf> {
    find_tool("resize2fs")
}

/// Shared logic for locating an e2fsprogs tool by name.
///
/// Search order:
/// 1. `which <name>` — finds it if already on `PATH`
/// 2. Platform-specific well-known locations (see [`FALLBACK_DIRS`])
fn find_tool(name: &str) -> anyhow::Result<PathBuf> {
    // Try `which` first — works when the tool is on PATH.
    if let Some(path) = which_tool(name) {
        return Ok(path);
    }

    // Fallback: check well-known platform-specific locations.
    for dir in FALLBACK_DIRS {
        let candidate = Path::new(dir).join(name);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    anyhow::bail!(not_found_message(name))
}

/// Try to find a tool on `PATH` using `which`.
fn which_tool(name: &str) -> Option<PathBuf> {
    let output = std::process::Command::new("which")
        .arg(name)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() {
        return None;
    }

    let p = Path::new(&path);
    if p.exists() {
        Some(PathBuf::from(path))
    } else {
        None
    }
}

/// Build a user-friendly error message with platform-specific install instructions.
fn not_found_message(tool: &str) -> String {
    format!(
        "{tool} not found — required for creating ext4 filesystem images.\n\n\
         visor uses e2fsprogs to create ext4 volumes and VM root filesystems.\n\n\
         Install e2fsprogs:\n  \
           macOS:    brew install e2fsprogs\n  \
           Ubuntu:   sudo apt install e2fsprogs\n  \
           Fedora:   sudo dnf install e2fsprogs\n  \
           Arch:     sudo pacman -S e2fsprogs"
    )
}

#[cfg(test)]
#[path = "ext4_test.rs"]
mod tests;
