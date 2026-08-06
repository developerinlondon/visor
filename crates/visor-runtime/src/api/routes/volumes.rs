//! Volume management routes: create, list, inspect, remove, resize.
//!
//! HTTP API for managing persistent ext4 volumes. Routes operate on the
//! [`VolumeManager`](crate::volume::VolumeManager) provided through
//! [`VolumeState`].

use std::sync::Arc;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use utoipa::ToSchema;

use super::vms::ApiError;
use crate::volume::{VolumeInfo, VolumeManager};

/// Shared state for volume routes.
///
/// Contains the [`VolumeManager`] instance shared across all volume
/// route handlers.
#[derive(Clone)]
#[non_exhaustive]
pub struct VolumeState {
    /// Volume manager instance.
    pub manager: Arc<VolumeManager>,
}

impl VolumeState {
    /// Creates a new `VolumeState` wrapping the given manager.
    #[must_use]
    pub fn new(manager: Arc<VolumeManager>) -> Self {
        Self { manager }
    }
}

/// Request body for creating a new volume.
#[derive(Debug, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct CreateVolumeRequest {
    /// Volume name (alphanumeric, hyphens, underscores).
    pub name: String,
    /// Volume size in MiB.
    pub size_mib: u64,
}

/// Request body for resizing a volume.
#[derive(Debug, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct ResizeVolumeRequest {
    /// New volume size in MiB (must be larger than current).
    pub size_mib: u64,
}

/// Runs a blocking closure on the Tokio blocking thread pool.
///
/// Wraps [`tokio::task::spawn_blocking`] and flattens the double-`Result`
/// so callers get a single `anyhow::Result<T>`.
async fn run_blocking<F, T>(f: F) -> anyhow::Result<T>
where
    F: FnOnce() -> anyhow::Result<T> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(result) => result,
        Err(e) => Err(anyhow::anyhow!("blocking volume task panicked: {e}")),
    }
}

/// Lists all persistent volumes.
///
/// # Errors
///
/// Returns an error if the volume directory cannot be read.
#[utoipa::path(
    get,
    path = "",
    tag = "volumes",
    responses(
        (status = 200, description = "List of all volumes", body = Vec<VolumeInfo>)
    )
)]
pub async fn list_volumes(
    State(state): State<VolumeState>,
) -> Result<Json<Vec<VolumeInfo>>, ApiError> {
    let manager = Arc::clone(&state.manager);
    let volumes = run_blocking(move || manager.list()).await?;
    Ok(Json(volumes))
}

/// Creates a new persistent volume.
///
/// # Errors
///
/// Returns an error if the name is invalid, a volume with the same name
/// already exists, or the filesystem tools fail.
#[utoipa::path(
    post,
    path = "",
    tag = "volumes",
    request_body = CreateVolumeRequest,
    responses(
        (status = 201, description = "Volume created", body = VolumeInfo),
        (status = 500, description = "Failed to create volume")
    )
)]
pub async fn create_volume(
    State(state): State<VolumeState>,
    Json(req): Json<CreateVolumeRequest>,
) -> Result<(StatusCode, Json<VolumeInfo>), ApiError> {
    let manager = Arc::clone(&state.manager);
    let info = run_blocking(move || manager.create(&req.name, req.size_mib)).await?;
    Ok((StatusCode::CREATED, Json(info)))
}

/// Gets information about a specific volume by name.
///
/// # Errors
///
/// Returns an error if the volume is not found.
#[utoipa::path(
    get,
    path = "/{name}",
    tag = "volumes",
    params(
        ("name" = String, Path, description = "Volume name")
    ),
    responses(
        (status = 200, description = "Volume information", body = VolumeInfo),
        (status = 500, description = "Volume not found")
    )
)]
pub async fn get_volume(
    State(state): State<VolumeState>,
    Path(name): Path<String>,
) -> Result<Json<VolumeInfo>, ApiError> {
    let manager = Arc::clone(&state.manager);
    let info = run_blocking(move || manager.inspect(&name)).await?;
    Ok(Json(info))
}

/// Removes a persistent volume and its metadata.
///
/// # Errors
///
/// Returns an error if the volume is not found or cannot be removed.
#[utoipa::path(
    delete,
    path = "/{name}",
    tag = "volumes",
    params(
        ("name" = String, Path, description = "Volume name")
    ),
    responses(
        (status = 204, description = "Volume removed"),
        (status = 500, description = "Volume not found")
    )
)]
pub async fn delete_volume(
    State(state): State<VolumeState>,
    Path(name): Path<String>,
) -> Result<StatusCode, ApiError> {
    let manager = Arc::clone(&state.manager);
    run_blocking(move || manager.remove(&name)).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// Resizes a persistent volume (grow only).
///
/// Extends the volume's sparse file and expands the ext4 filesystem
/// to fill the new size. Shrinking is not supported.
///
/// # Errors
///
/// Returns an error if the volume is not found, the new size is not
/// larger than the current size, or the filesystem tools fail.
#[utoipa::path(
    post,
    path = "/{name}/resize",
    tag = "volumes",
    params(
        ("name" = String, Path, description = "Volume name")
    ),
    request_body = ResizeVolumeRequest,
    responses(
        (status = 200, description = "Volume resized", body = VolumeInfo),
        (status = 500, description = "Resize failed")
    )
)]
pub async fn resize_volume(
    State(state): State<VolumeState>,
    Path(name): Path<String>,
    Json(req): Json<ResizeVolumeRequest>,
) -> Result<Json<VolumeInfo>, ApiError> {
    let manager = Arc::clone(&state.manager);
    let info = run_blocking(move || manager.resize(&name, req.size_mib)).await?;
    Ok(Json(info))
}

#[cfg(test)]
#[path = "volumes_test.rs"]
mod tests;
