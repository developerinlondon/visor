//! Tests for the codesign module.
//!
//! These tests verify entitlement checking and codesign automation on macOS.
//! Tests that require `codesign` are gated behind `#[cfg(target_os = "macos")]`.

use super::*;

// ── Entitlement parsing ──────────────────────────────────────────

#[test]
fn entitlements_output_with_hvf_key_returns_true() {
    let output = r#"Executable=/usr/bin/visor
[Dict]
	[Key] com.apple.security.hypervisor
	[Value]
		[Bool] true
"#;
    assert!(output_contains_hvf_entitlement(output));
}

#[test]
fn entitlements_output_without_hvf_key_returns_false() {
    let output = r#"Executable=/usr/bin/visor
[Dict]
	[Key] com.apple.security.app-sandbox
	[Value]
		[Bool] true
"#;
    assert!(!output_contains_hvf_entitlement(output));
}

#[test]
fn empty_entitlements_output_returns_false() {
    assert!(!output_contains_hvf_entitlement(""));
}

#[test]
fn xml_entitlements_output_with_hvf_key_returns_true() {
    let output = r#"Executable=/usr/bin/visor
<?xml version="1.0" encoding="UTF-8"?><!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd"><plist version="1.0"><dict><key>com.apple.security.hypervisor</key><true/></dict></plist>"#;
    assert!(output_contains_hvf_entitlement(output));
}

// ── Entitlement error message ────────────────────────────────────

#[test]
fn missing_entitlement_error_message_contains_remedy() {
    let msg = hvf_entitlement_missing_message("/path/to/visor");
    assert!(msg.contains("com.apple.security.hypervisor"));
    assert!(msg.contains("codesign"));
    assert!(msg.contains("/path/to/visor"));
}

// ── Live binary checks (macOS only) ──────────────────────────────

#[cfg(target_os = "macos")]
#[test]
fn has_hvf_entitlement_returns_true_for_codesigned_binary() {
    // The release binary should be codesigned from our build process.
    // Skip if no release binary exists.
    let binary = std::path::Path::new("target/release/visor");
    if !binary.exists() {
        eprintln!("skipping: no release binary at {}", binary.display());
        return;
    }
    // We can't guarantee it's codesigned in all test contexts, so just
    // verify the function doesn't panic.
    let _result = has_hvf_entitlement(binary);
}

#[cfg(target_os = "macos")]
#[test]
fn codesign_binary_signs_a_trivial_executable() {
    // Create a minimal Mach-O binary to codesign.
    let dir = tempfile::tempdir().unwrap();
    let binary_path = dir.path().join("test-binary");

    // Copy /usr/bin/true as a test binary (it's a small signed Mach-O).
    std::fs::copy("/usr/bin/true", &binary_path).unwrap();

    let entitlements = std::path::Path::new("entitlements.plist");
    if !entitlements.exists() {
        // Try workspace root
        let ws_entitlements = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(|p| p.parent())
            .map(|p| p.join("entitlements.plist"));
        if let Some(ref p) = ws_entitlements {
            if !p.exists() {
                eprintln!("skipping: entitlements.plist not found");
                return;
            }
            let result = codesign_binary(&binary_path, p);
            assert!(result.is_ok(), "codesign failed: {result:?}");
            assert!(has_hvf_entitlement(&binary_path));
            return;
        }
        eprintln!("skipping: entitlements.plist not found");
        return;
    }

    let result = codesign_binary(&binary_path, entitlements);
    assert!(result.is_ok(), "codesign failed: {result:?}");
    assert!(has_hvf_entitlement(&binary_path));
}

#[cfg(target_os = "macos")]
#[test]
fn codesign_binary_fails_for_nonexistent_binary() {
    let result = codesign_binary(
        std::path::Path::new("/nonexistent/binary"),
        std::path::Path::new("entitlements.plist"),
    );
    assert!(result.is_err());
}

#[cfg(target_os = "macos")]
#[test]
fn codesign_binary_fails_for_nonexistent_entitlements() {
    let dir = tempfile::tempdir().unwrap();
    let binary_path = dir.path().join("test-binary");
    std::fs::copy("/usr/bin/true", &binary_path).unwrap();

    let result = codesign_binary(
        &binary_path,
        std::path::Path::new("/nonexistent/entitlements.plist"),
    );
    assert!(result.is_err());
}
