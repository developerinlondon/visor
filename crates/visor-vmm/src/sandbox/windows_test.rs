use super::*;
use crate::sandbox::backend::{SandboxBackend, SandboxError};

#[test]
fn windows_sandbox_apply_returns_unsupported() {
    let backend = WindowsSandbox;
    let result = backend.apply();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, SandboxError::Unsupported),
        "expected Unsupported, got: {err:?}"
    );
}
