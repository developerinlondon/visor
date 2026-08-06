use std::path::Path;

use super::*;

#[test]
fn not_found_message_contains_tool_name() {
    let msg = not_found_message("mke2fs");
    assert!(
        msg.contains("mke2fs"),
        "error message should name the missing tool"
    );
}

#[test]
fn not_found_message_contains_brew_instructions() {
    let msg = not_found_message("mke2fs");
    assert!(
        msg.contains("brew install e2fsprogs"),
        "error message should include macOS install instructions"
    );
}

#[test]
fn not_found_message_contains_apt_instructions() {
    let msg = not_found_message("resize2fs");
    assert!(
        msg.contains("apt install e2fsprogs"),
        "error message should include Ubuntu install instructions"
    );
}

#[test]
fn not_found_message_contains_dnf_instructions() {
    let msg = not_found_message("mke2fs");
    assert!(
        msg.contains("dnf install e2fsprogs"),
        "error message should include Fedora install instructions"
    );
}

#[test]
fn not_found_message_explains_purpose() {
    let msg = not_found_message("mke2fs");
    assert!(
        msg.contains("ext4"),
        "error message should explain what ext4 tools are for"
    );
}

#[test]
fn which_tool_returns_none_for_nonexistent_binary() {
    let result = which_tool("this_binary_definitely_does_not_exist_xyz");
    assert!(result.is_none(), "should return None for missing binary");
}

#[test]
fn find_tool_fails_for_nonexistent_binary() {
    let result = find_tool("this_binary_definitely_does_not_exist_xyz");
    assert!(result.is_err(), "should fail for missing binary");

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("not found"),
        "error should say tool was not found"
    );
    assert!(
        err.contains("brew install"),
        "error should include install instructions"
    );
}

#[test]
fn fallback_dirs_includes_homebrew_paths() {
    let has_homebrew = FALLBACK_DIRS
        .iter()
        .any(|d| d.contains("homebrew") || d.contains("/usr/local/opt"));
    assert!(has_homebrew, "fallback dirs should include Homebrew paths");
}

#[test]
fn fallback_dirs_includes_sbin() {
    let has_sbin = FALLBACK_DIRS.iter().any(|d| d.contains("sbin"));
    assert!(has_sbin, "fallback dirs should include sbin paths");
}

// Integration tests — only run when e2fsprogs is actually installed.

#[test]
fn find_mke2fs_succeeds_when_installed() {
    // This test verifies that find_mke2fs works when e2fsprogs is present.
    // It will be skipped (pass vacuously) if e2fsprogs is not installed,
    // since we can't require it in CI everywhere.
    if which_tool("mke2fs").is_none() && !homebrew_e2fsprogs_exists() {
        // e2fsprogs not installed — skip gracefully.
        return;
    }

    let result = find_mke2fs();
    assert!(result.is_ok(), "find_mke2fs should succeed: {result:?}");

    let path = result.unwrap();
    assert!(
        path.exists(),
        "returned path should exist: {}",
        path.display()
    );
    assert!(
        path.to_string_lossy().contains("mke2fs"),
        "path should contain mke2fs: {}",
        path.display()
    );
}

#[test]
fn find_resize2fs_succeeds_when_installed() {
    if which_tool("resize2fs").is_none() && !homebrew_e2fsprogs_exists() {
        return;
    }

    let result = find_resize2fs();
    assert!(result.is_ok(), "find_resize2fs should succeed: {result:?}");

    let path = result.unwrap();
    assert!(
        path.exists(),
        "returned path should exist: {}",
        path.display()
    );
    assert!(
        path.to_string_lossy().contains("resize2fs"),
        "path should contain resize2fs: {}",
        path.display()
    );
}

/// Check if Homebrew e2fsprogs is installed in a well-known location.
fn homebrew_e2fsprogs_exists() -> bool {
    Path::new("/opt/homebrew/opt/e2fsprogs/sbin/mke2fs").exists()
        || Path::new("/usr/local/opt/e2fsprogs/sbin/mke2fs").exists()
}
