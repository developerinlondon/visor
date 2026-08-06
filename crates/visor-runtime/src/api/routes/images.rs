//! Image management routes: list, pull, inspect, remove.
//!
//! HTTP API for managing cached OCI images. All routes operate on the
//! [`LayerCache`](crate::oci::cache::LayerCache) and registry client.

use axum::Json;
use axum::extract::Path;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use super::vms::ApiError;
use crate::oci::cache::LayerCache;
use crate::oci::reference::ImageReference;
use crate::oci::registry::RegistryClient;

/// Cached image metadata returned by the list/inspect endpoints.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct ImageInfo {
    /// Image reference (e.g. `docker.io/library/alpine:latest`).
    pub reference: String,
    /// Registry host.
    pub registry: String,
    /// Repository name.
    pub repository: String,
    /// Tag or digest.
    pub tag: String,
    /// Total size of cached layers in bytes.
    pub size_bytes: u64,
    /// Number of layers.
    pub layers: usize,
}

/// Request body for pulling an image.
#[derive(Debug, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct PullImageRequest {
    /// Image reference to pull (e.g. `alpine:latest`).
    pub image: String,
}

/// Lists all cached OCI images.
///
/// Scans the manifest cache directory and returns metadata for each
/// cached image.
///
/// # Errors
///
/// Returns an error if the cache cannot be read.
#[utoipa::path(
    get,
    path = "/v1/images",
    tag = "images",
    responses(
        (status = 200, description = "List of cached images", body = Vec<ImageInfo>)
    )
)]
pub async fn list_images() -> Result<Json<Vec<ImageInfo>>, ApiError> {
    let cache = open_cache()?;
    let images = scan_cached_images(&cache)?;
    Ok(Json(images))
}

/// Pulls an OCI image from a registry and caches it locally.
///
/// Downloads the manifest and all layers. If already cached, this is a
/// fast no-op for layers that haven't changed.
///
/// # Errors
///
/// Returns an error if the image cannot be found or the pull fails.
#[utoipa::path(
    post,
    path = "/v1/images/pull",
    tag = "images",
    request_body = PullImageRequest,
    responses(
        (status = 200, description = "Image pulled successfully", body = ImageInfo),
        (status = 404, description = "Image not found")
    )
)]
pub async fn pull_image(Json(req): Json<PullImageRequest>) -> Result<Json<ImageInfo>, ApiError> {
    use anyhow::Context as _;

    let image_ref = ImageReference::parse(&req.image).context("parse image reference")?;

    let registry = image_ref.registry().as_ref().to_owned();
    let repository = image_ref.repository().as_ref().to_owned();
    let tag = image_ref
        .tag()
        .map_or_else(|| "latest".to_owned(), |t| t.as_ref().to_owned());

    let cache = open_cache()?;

    // Authenticate and pull manifest
    let mut client = RegistryClient::new(&registry).context("create registry client")?;
    client
        .authenticate(&repository)
        .await
        .context("authenticate with registry")?;

    let manifest = client
        .pull_manifest(&repository, &tag)
        .await
        .context(format!("pull manifest for '{}'", req.image))?;

    // Cache manifest
    let manifest_bytes = serde_json::to_vec(&manifest).context("serialize manifest")?;
    cache
        .put_manifest(&registry, &repository, &tag, &manifest_bytes)
        .context("cache manifest")?;

    // Cache config blob
    if cache
        .get(&manifest.config.digest)
        .context("check config cache")?
        .is_none()
    {
        let blob = client
            .pull_blob(&repository, &manifest.config.digest)
            .await
            .context("pull config blob")?;
        cache
            .put(&manifest.config.digest, &blob)
            .context("cache config blob")?;
    }

    // Cache layers
    let mut total_size: u64 = 0;
    for layer in &manifest.layers {
        total_size += layer.size;
        if cache.has(&layer.digest) {
            continue;
        }
        let blob = client
            .pull_blob(&repository, &layer.digest)
            .await
            .with_context(|| format!("pull layer {}", layer.digest))?;
        cache
            .put(&layer.digest, &blob)
            .with_context(|| format!("cache layer {}", layer.digest))?;
    }

    let info = ImageInfo {
        reference: req.image,
        registry,
        repository,
        tag,
        size_bytes: total_size,
        layers: manifest.layers.len(),
    };

    Ok(Json(info))
}

/// Returns detailed information about a cached image.
///
/// # Errors
///
/// Returns an error if the image is not cached.
#[utoipa::path(
    get,
    path = "/v1/images/{reference}",
    tag = "images",
    params(
        ("reference" = String, Path, description = "Image reference (url-encoded)")
    ),
    responses(
        (status = 200, description = "Image details", body = ImageInfo),
        (status = 404, description = "Image not cached")
    )
)]
pub async fn inspect_image(Path(reference): Path<String>) -> Result<Json<ImageInfo>, ApiError> {
    use anyhow::Context as _;

    let image_ref = ImageReference::parse(&reference).context("parse image reference")?;

    let registry = image_ref.registry().as_ref().to_owned();
    let repository = image_ref.repository().as_ref().to_owned();
    let tag = image_ref
        .tag()
        .map_or_else(|| "latest".to_owned(), |t| t.as_ref().to_owned());

    let cache = open_cache()?;

    let manifest_bytes = cache
        .get_manifest(&registry, &repository, &tag)
        .context("check manifest cache")?
        .ok_or_else(|| anyhow::anyhow!("image not found in cache: {reference}"))?;

    let manifest: crate::oci::registry::Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse cached manifest")?;

    let total_size: u64 = manifest.layers.iter().map(|l| l.size).sum();

    Ok(Json(ImageInfo {
        reference,
        registry,
        repository,
        tag,
        size_bytes: total_size,
        layers: manifest.layers.len(),
    }))
}

/// Removes a cached image and its layers.
///
/// # Errors
///
/// Returns an error if the image is not cached or cannot be removed.
#[utoipa::path(
    delete,
    path = "/v1/images/{reference}",
    tag = "images",
    params(
        ("reference" = String, Path, description = "Image reference (url-encoded)")
    ),
    responses(
        (status = 204, description = "Image removed"),
        (status = 404, description = "Image not cached")
    )
)]
pub async fn delete_image(Path(reference): Path<String>) -> Result<StatusCode, ApiError> {
    use anyhow::Context as _;

    let image_ref = ImageReference::parse(&reference).context("parse image reference")?;

    let registry = image_ref.registry().as_ref().to_owned();
    let repository = image_ref.repository().as_ref().to_owned();
    let tag = image_ref
        .tag()
        .map_or_else(|| "latest".to_owned(), |t| t.as_ref().to_owned());

    let cache = open_cache()?;

    // Read manifest to find layers to remove
    let manifest_bytes = cache
        .get_manifest(&registry, &repository, &tag)
        .context("check manifest cache")?
        .ok_or_else(|| anyhow::anyhow!("image not found in cache: {reference}"))?;

    let manifest: crate::oci::registry::Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse cached manifest")?;

    // Remove layers and config blob
    for layer in &manifest.layers {
        cache.remove(&layer.digest).context("remove layer")?;
    }
    cache
        .remove(&manifest.config.digest)
        .context("remove config blob")?;

    // Remove manifest file
    let manifest_path = cache.manifest_key(&registry, &repository, &tag);
    if manifest_path.is_file() {
        std::fs::remove_file(&manifest_path).context("remove manifest file")?;
    }

    Ok(StatusCode::NO_CONTENT)
}

// ── Helpers ────────────────────────────────────────────────────────

/// Opens the default layer cache.
fn open_cache() -> Result<LayerCache, ApiError> {
    use anyhow::Context as _;
    let path = LayerCache::default_path().context("determine cache path")?;
    let cache = LayerCache::new(path).context("open layer cache")?;
    Ok(cache)
}

/// Scans the manifest cache directory and builds image info from each cached manifest.
fn scan_cached_images(cache: &LayerCache) -> Result<Vec<ImageInfo>, ApiError> {
    use anyhow::Context as _;

    let mut images = Vec::new();
    let entries = std::fs::read_dir(&cache.manifests_dir).context("read manifests directory")?;

    for entry in entries {
        let entry = entry.context("read manifest entry")?;
        let path = entry.path();

        if !path.is_file() {
            continue;
        }

        let filename = match path.file_stem().and_then(|s| s.to_str()) {
            Some(f) => f.to_owned(),
            None => continue,
        };

        // Parse filename: registry_repo_tag (underscores replaced slashes)
        // Format: registry-1.docker.io_library_alpine_latest
        let parts: Vec<&str> = filename.splitn(3, '_').collect();
        if parts.len() < 3 {
            continue;
        }

        let registry = parts[0].to_owned();

        // Last part after final _ is the tag, middle is repository
        // But repository can contain _ (e.g., library_alpine)
        // The filename format is: {registry}_{repo_with_underscores}_{tag}
        // We need to split the remaining part on the last _
        let rest = &filename[registry.len() + 1..];
        let (repository, tag) = match rest.rsplit_once('_') {
            Some((repo, t)) => (repo.replace('_', "/"), t.to_owned()),
            None => continue,
        };

        // Try to read the manifest for layer count and size
        let Ok(data) = std::fs::read(&path) else {
            continue;
        };

        let Ok(manifest): Result<crate::oci::registry::Manifest, _> = serde_json::from_slice(&data)
        else {
            continue;
        };

        let total_size: u64 = manifest.layers.iter().map(|l| l.size).sum();

        images.push(ImageInfo {
            reference: format!("{registry}/{repository}:{tag}"),
            registry,
            repository,
            tag,
            size_bytes: total_size,
            layers: manifest.layers.len(),
        });
    }

    Ok(images)
}

#[cfg(test)]
#[path = "images_test.rs"]
mod tests;
