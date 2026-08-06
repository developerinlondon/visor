use super::*;
use crate::comms::backend::{CommsBackend, CommsError};

#[tokio::test]
async fn windows_backend_connect_returns_unsupported() {
    let backend = WindowsCommsBackend;
    let result = backend.connect(3, 52).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(err, CommsError::Unsupported),
        "expected Unsupported, got: {err:?}"
    );
}
