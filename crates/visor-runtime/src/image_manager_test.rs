use visor_types::ImageManager;

use super::RuntimeImageManager;

#[tokio::test]
async fn list_images_reads_tags_from_store() {
    let dir = crate::testutil::tempdir("visor-runtime-image-manager-").unwrap();
    let store = visor_build::ImageStore::new(dir.path().to_path_buf());
    store.tag("alpine:latest", "sha256:aaa").unwrap();
    store.tag("busybox:latest", "sha256:bbb").unwrap();

    let manager = RuntimeImageManager::new(dir.path().to_path_buf());
    let images = manager.list_images().await.unwrap();

    assert_eq!(images.len(), 2);
}

#[tokio::test]
async fn inspect_image_returns_tagged_digest() {
    let dir = crate::testutil::tempdir("visor-runtime-image-manager-").unwrap();
    let store = visor_build::ImageStore::new(dir.path().to_path_buf());
    store.tag("alpine:latest", "sha256:abc123").unwrap();

    let manager = RuntimeImageManager::new(dir.path().to_path_buf());
    let image = manager.inspect_image("alpine:latest").await.unwrap();

    assert_eq!(image.id, "sha256:abc123");
    assert_eq!(image.repo_tags, vec!["alpine:latest"]);
}

#[tokio::test]
async fn inspect_image_falls_back_to_docker_hub_short_name() {
    let dir = crate::testutil::tempdir("visor-runtime-image-manager-").unwrap();
    let store = visor_build::ImageStore::new(dir.path().to_path_buf());
    store
        .tag("docker.io/library/alpine:latest", "sha256:abc123")
        .unwrap();

    let manager = RuntimeImageManager::new(dir.path().to_path_buf());
    let image = manager.inspect_image("alpine:latest").await.unwrap();

    assert_eq!(image.id, "sha256:abc123");
}

#[tokio::test]
async fn remove_image_deletes_tag() {
    let dir = crate::testutil::tempdir("visor-runtime-image-manager-").unwrap();
    let store = visor_build::ImageStore::new(dir.path().to_path_buf());
    store.tag("alpine:latest", "sha256:abc123").unwrap();

    let manager = RuntimeImageManager::new(dir.path().to_path_buf());
    manager.remove_image("alpine:latest").await.unwrap();

    let images = manager.list_images().await.unwrap();
    assert!(images.is_empty());
}
