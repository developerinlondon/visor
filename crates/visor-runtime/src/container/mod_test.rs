use super::*;
use crate::backend::{ExecutionBackend, VmConfig, VmState};

/// Helper to create a minimal [`VmConfig`] for testing.
fn test_config(name: Option<&str>) -> VmConfig {
    let mut config = VmConfig::new("alpine:latest");
    config.name = name.map(ToOwned::to_owned);
    config
}

#[test]
fn test_container_backend_new() {
    let _backend = ContainerBackend::new();
}

#[tokio::test]
async fn test_list_empty() {
    let backend = ContainerBackend::new();
    let vms = backend.list().await.unwrap();
    assert!(vms.is_empty());
}

#[tokio::test]
async fn test_create_and_list() {
    let backend = ContainerBackend::new();
    let info = backend.create(test_config(None)).await.unwrap();
    let vms = backend.list().await.unwrap();
    assert_eq!(vms.len(), 1);
    assert_eq!(vms[0].id, info.id);
}

#[tokio::test]
async fn test_create_and_get() {
    let backend = ContainerBackend::new();
    let info = backend.create(test_config(None)).await.unwrap();
    let retrieved = backend.get(&info.id).await.unwrap();
    assert_eq!(retrieved.id, info.id);
    assert_eq!(retrieved.image, "alpine:latest");
}

#[tokio::test]
async fn test_get_nonexistent() {
    let backend = ContainerBackend::new();
    let result = backend.get("nonexistent-id").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_stop_nonexistent() {
    let backend = ContainerBackend::new();
    let result = backend.stop("nonexistent-id", 10).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_destroy_nonexistent() {
    let backend = ContainerBackend::new();
    let result = backend.destroy("nonexistent-id").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn test_destroy_removes_from_list() {
    let backend = ContainerBackend::new();
    let info = backend.create(test_config(None)).await.unwrap();
    backend.destroy(&info.id).await.unwrap();
    let vms = backend.list().await.unwrap();
    assert!(vms.is_empty());
}

#[tokio::test]
async fn test_stop_marks_stopped() {
    let backend = ContainerBackend::new();
    let info = backend.create(test_config(None)).await.unwrap();
    backend.stop(&info.id, 10).await.unwrap();
    let updated = backend.get(&info.id).await.unwrap();
    assert_eq!(updated.state, VmState::Stopped);
}

#[tokio::test]
async fn test_create_with_name() {
    let backend = ContainerBackend::new();
    let info = backend
        .create(test_config(Some("my-container")))
        .await
        .unwrap();
    assert_eq!(info.name.as_deref(), Some("my-container"));
}

#[tokio::test]
async fn test_create_assigns_unique_ids() {
    let backend = ContainerBackend::new();
    let info1 = backend.create(test_config(None)).await.unwrap();
    let info2 = backend.create(test_config(None)).await.unwrap();
    assert_ne!(info1.id, info2.id);
}
