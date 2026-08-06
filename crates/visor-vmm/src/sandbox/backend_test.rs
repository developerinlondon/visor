use super::*;

// ── Mock implementation for trait testing ─────────────────────────────

/// Mock sandbox that always succeeds.
struct MockSandbox;

impl SandboxBackend for MockSandbox {
    fn apply(&self) -> Result<(), SandboxError> {
        Ok(())
    }
}

/// Mock sandbox that always fails with a specific error.
struct FailingSandbox {
    error_msg: String,
}

impl SandboxBackend for FailingSandbox {
    fn apply(&self) -> Result<(), SandboxError> {
        Err(SandboxError::Install(self.error_msg.clone()))
    }
}

// ── SandboxBackend trait via mock ─────────────────────────────────────

#[test]
fn mock_sandbox_apply_succeeds() {
    let sandbox = MockSandbox;
    assert!(sandbox.apply().is_ok());
}

#[test]
fn failing_sandbox_returns_install_error() {
    let sandbox = FailingSandbox {
        error_msg: "permission denied".to_owned(),
    };
    let result = sandbox.apply();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, SandboxError::Install(_)),
        "expected Install error, got: {err:?}"
    );
}

#[test]
fn sandbox_backend_is_object_safe_via_send_sync() {
    // Verify SandboxBackend can be used as a trait bound with Send + Sync.
    fn assert_send_sync<T: SandboxBackend>(_t: &T) {}
    let sandbox = MockSandbox;
    assert_send_sync(&sandbox);
}

// ── SandboxError tests ───────────────────────────────────────────────

#[test]
fn sandbox_error_compile_display() {
    let err = SandboxError::Compile("bad arch".to_owned());
    let msg = format!("{err}");
    assert!(msg.contains("bad arch"), "should contain detail: {msg}");
    assert!(
        msg.to_lowercase().contains("compil"),
        "should mention compilation: {msg}"
    );
}

#[test]
fn sandbox_error_install_display() {
    let err = SandboxError::Install("prctl failed".to_owned());
    let msg = format!("{err}");
    assert!(msg.contains("prctl failed"), "should contain detail: {msg}");
    assert!(
        msg.to_lowercase().contains("install"),
        "should mention installation: {msg}"
    );
}

#[test]
fn sandbox_error_unsupported_display() {
    let err = SandboxError::Unsupported;
    let msg = format!("{err}");
    assert!(
        msg.to_lowercase().contains("unsupported") || msg.to_lowercase().contains("not supported"),
        "Unsupported error should mention 'unsupported': {msg}"
    );
}

#[test]
fn sandbox_error_is_debug() {
    let err = SandboxError::Unsupported;
    let debug = format!("{err:?}");
    assert!(!debug.is_empty(), "Debug should produce output");
}
