//! macOS codesign automation for Hypervisor.framework entitlements.
//!
//! On macOS, any binary that calls `hv_vm_create()` (HVF) must be ad-hoc
//! codesigned with the `com.apple.security.hypervisor` entitlement.
//! Without it, HVF returns `HV_DENIED` (0xfae94007).
//!
//! This module provides:
//! - [`has_hvf_entitlement`] — check if a binary has the HVF entitlement
//! - [`codesign_binary`] — codesign a binary with entitlements
//! - [`verify_current_binary`] — check the running binary at daemon startup

use std::path::Path;

use anyhow::Context;

/// The macOS entitlement key required for Hypervisor.framework access.
const HVF_ENTITLEMENT: &str = "com.apple.security.hypervisor";

/// Checks whether a binary at the given path has the HVF entitlement.
///
/// Shells out to `codesign -d --entitlements -` and checks for the
/// `com.apple.security.hypervisor` key in the output.
///
/// Returns `false` if the binary is unsigned, the entitlement is missing,
/// or `codesign` is not available.
#[must_use]
pub fn has_hvf_entitlement(binary: &Path) -> bool {
    let output = std::process::Command::new("codesign")
        .args(["-d", "--entitlements", "-"])
        .arg(binary)
        .output();

    match output {
        Ok(out) => {
            let combined = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr),
            );
            output_contains_hvf_entitlement(&combined)
        }
        Err(_) => false,
    }
}

/// Codesigns a binary with the given entitlements plist.
///
/// Performs an ad-hoc signature (`--sign -`) with `--force` to replace
/// any existing signature.
///
/// # Errors
///
/// Returns an error if `codesign` fails (binary not found, invalid
/// entitlements, permission denied).
#[cfg(target_os = "macos")]
#[allow(dead_code)] // Called from tests now; production use in WS3 (WorkerProcessLifecycle)
pub(crate) fn codesign_binary(binary: &Path, entitlements: &Path) -> anyhow::Result<()> {
    let output = std::process::Command::new("codesign")
        .args(["--sign", "-", "--entitlements"])
        .arg(entitlements)
        .arg("--force")
        .arg(binary)
        .output()
        .context("failed to run codesign")?;

    if output.status.success() {
        Ok(())
    } else {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!(
            "codesign failed for {}: {}",
            binary.display(),
            stderr.trim()
        );
    }
}

/// Verifies the currently running binary has the HVF entitlement.
///
/// Intended to be called early in daemon startup on macOS. Returns
/// `Ok(())` if the entitlement is present, or an error with a clear
/// remediation message if not.
///
/// On non-macOS platforms this always returns `Ok(())`.
///
/// # Errors
///
/// Returns an error if the current binary path cannot be resolved
/// or the HVF entitlement is missing.
pub fn verify_current_binary() -> anyhow::Result<()> {
    #[cfg(not(target_os = "macos"))]
    {
        Ok(())
    }
    #[cfg(target_os = "macos")]
    {
        let exe = std::env::current_exe().context("failed to resolve current executable path")?;
        if has_hvf_entitlement(&exe) {
            Ok(())
        } else {
            let msg = hvf_entitlement_missing_message(&exe.to_string_lossy());
            anyhow::bail!("{msg}");
        }
    }
}

/// Checks if codesign output text contains the HVF entitlement key.
///
/// Works with both the human-readable format (`[Key] com.apple.security.hypervisor`)
/// and XML plist format (`<key>com.apple.security.hypervisor</key>`).
fn output_contains_hvf_entitlement(output: &str) -> bool {
    output.contains(HVF_ENTITLEMENT)
}

/// Builds a user-friendly error message for a missing HVF entitlement.
fn hvf_entitlement_missing_message(binary_path: &str) -> String {
    format!(
        "Binary is not codesigned with the {HVF_ENTITLEMENT} entitlement.\n\
         \n\
         Without this entitlement, Hypervisor.framework calls will fail with HV_DENIED (0xfae94007).\n\
         \n\
         To fix, run:\n\
         \n\
         \x20 codesign --sign - --entitlements entitlements.plist --force {binary_path}\n\
         \n\
         Or use: make release-mac"
    )
}

#[cfg(test)]
#[path = "codesign_test.rs"]
mod tests;
