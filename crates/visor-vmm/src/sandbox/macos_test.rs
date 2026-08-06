use super::*;
use crate::sandbox::backend::SandboxBackend;

#[test]
fn macos_sandbox_new_succeeds() {
    let sandbox = MacosSandbox::new();
    assert!(sandbox.is_ok(), "MacosSandbox::new() should succeed");
}

#[test]
fn macos_sandbox_with_capabilities_constructs() {
    let caps = nono::CapabilitySet::new();
    let sandbox = MacosSandbox::with_capabilities(caps);
    // Verify we can reference the sandbox as a SandboxBackend.
    let _: &dyn SandboxBackend = &sandbox;
}

#[test]
fn macos_sandbox_with_empty_capabilities_constructs() {
    // An empty CapabilitySet is valid — it just allows nothing.
    let caps = nono::CapabilitySet::new();
    let sandbox = MacosSandbox::with_capabilities(caps);
    // apply() would work but we don't call it (irreversible).
    let _: &dyn SandboxBackend = &sandbox;
}

#[test]
fn macos_sandbox_with_custom_paths_constructs() {
    let caps = nono::CapabilitySet::new()
        .allow_path("/usr/bin", nono::AccessMode::Read)
        .expect("valid path");
    let sandbox = MacosSandbox::with_capabilities(caps);
    let _: &dyn SandboxBackend = &sandbox;
}

#[test]
fn macos_sandbox_default_has_capabilities() {
    // MacosSandbox::new() should construct with default VMM capabilities
    // without error. We can't inspect the internal capabilities directly,
    // but we know it configures paths that exist on macOS.
    let sandbox = MacosSandbox::new().expect("should succeed");
    let _: &dyn SandboxBackend = &sandbox;
}

// NOTE: We do NOT test apply() here because Seatbelt is irreversible —
// calling it would sandbox the test runner process permanently, breaking
// all subsequent tests. Integration tests should cover apply() in an
// isolated subprocess.
