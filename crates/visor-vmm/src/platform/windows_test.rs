use super::*;
use crate::platform::{Platform, PlatformError};

#[test]
fn whp_platform_new_returns_unsupported() {
    let result = WhpPlatform::new();
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, PlatformError::Unsupported),
        "expected Unsupported, got: {err:?}"
    );
}
