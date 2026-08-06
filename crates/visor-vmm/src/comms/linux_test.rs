use super::*;
use crate::comms::backend::CommsBackend;

// ── LinuxCommsBackend construction ──────────────────────────────────

#[test]
fn linux_comms_backend_default() {
    let _backend = LinuxCommsBackend::default();
}

#[test]
fn linux_comms_backend_new() {
    let _backend = LinuxCommsBackend::new();
}

// ── Connect to invalid CID ──────────────────────────────────────────

#[tokio::test]
async fn connect_to_invalid_cid_returns_error() {
    let backend = LinuxCommsBackend::new();
    // CID 0 is reserved (hypervisor) — connecting should fail immediately.
    let result = backend.connect(0, 9999).await;
    assert!(result.is_err(), "connecting to CID 0 should fail");
}
