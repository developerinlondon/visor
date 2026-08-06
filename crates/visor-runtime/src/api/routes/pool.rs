//! Warm pool management API endpoints.
//!
//! - `GET /v1/pool` — current pool status (sizes per image)
//! - `POST /v1/pool/warm` — pre-warm VMs for a specific image
//! - `POST /v1/pool/drain` — drain all warm pools

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::router::AppState;
use crate::pool::manager::PoolStatus;

use super::vms::ApiError;

/// Request body for `POST /v1/pool/warm`.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct WarmRequest {
    /// OCI image reference to pre-warm (e.g. `"alpine:latest"`).
    pub image: String,
    /// Number of VMs to pre-warm.
    pub count: usize,
}

/// Response for successful warm/drain operations.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct PoolActionResponse {
    /// Human-readable status message.
    pub status: String,
}

/// Returns the current warm pool status.
///
/// Shows the number of available pre-warmed VMs per image and
/// the configured target sizes.
///
/// # Errors
///
/// Returns 503 if the pool manager is not configured.
#[utoipa::path(
    get,
    path = "/v1/pool",
    tag = "pool",
    responses(
        (status = 200, description = "Pool status", body = PoolStatus),
        (status = 503, description = "Pool manager not configured")
    )
)]
pub async fn get_pool_status(
    State(state): State<AppState>,
) -> Result<Json<PoolStatus>, StatusCode> {
    let pool = state.pool.as_ref().ok_or(StatusCode::SERVICE_UNAVAILABLE)?;
    let status = pool.status().await;
    Ok(Json(status))
}

/// Pre-warms VMs for a specific image.
///
/// Creates the requested number of VMs in detached mode and adds
/// them to the warm pool for the given image.
///
/// # Errors
///
/// Returns 503 if the pool manager is not configured, or 500 if
/// warming fails.
#[utoipa::path(
    post,
    path = "/v1/pool/warm",
    tag = "pool",
    request_body = WarmRequest,
    responses(
        (status = 200, description = "VMs warmed successfully", body = PoolActionResponse),
        (status = 503, description = "Pool manager not configured"),
        (status = 500, description = "Warming failed")
    )
)]
pub async fn warm_pool(
    State(state): State<AppState>,
    Json(req): Json<WarmRequest>,
) -> Result<Json<PoolActionResponse>, ApiError> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("pool manager not configured"))?;

    pool.warm(&req.image, req.count).await?;

    Ok(Json(PoolActionResponse {
        status: format!("warmed {} VMs for {}", req.count, req.image),
    }))
}

/// Drains all warm pools, stopping every pooled VM.
///
/// After draining, the pools are empty. New VMs will be created
/// on-demand until the pool is warmed again.
///
/// # Errors
///
/// Returns 503 if the pool manager is not configured, or 500 if
/// draining fails.
#[utoipa::path(
    post,
    path = "/v1/pool/drain",
    tag = "pool",
    responses(
        (status = 200, description = "Pool drained", body = PoolActionResponse),
        (status = 503, description = "Pool manager not configured"),
        (status = 500, description = "Drain failed")
    )
)]
pub async fn drain_pool(
    State(state): State<AppState>,
) -> Result<Json<PoolActionResponse>, ApiError> {
    let pool = state
        .pool
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("pool manager not configured"))?;

    pool.drain().await?;

    Ok(Json(PoolActionResponse {
        status: "pool drained".to_owned(),
    }))
}

#[cfg(test)]
#[path = "pool_test.rs"]
mod tests;
