//! Per-VM health check endpoints.
//!
//! - `GET /v1/vms/{id}/health` — per-VM health status via vsock ping tracking

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::router::AppState;
use crate::pool::health::{HealthStatus, VmHealthReport};

/// Daemon-level health response for `GET /v1/health`.
#[derive(Debug, Serialize, serde::Deserialize, ToSchema)]
#[non_exhaustive]
pub struct DaemonHealthResponse {
    /// Overall daemon status (`"ok"`).
    pub status: String,
    /// Daemon uptime in seconds.
    pub uptime_secs: u64,
}

/// Per-VM health response for `GET /v1/vms/{id}/health`.
///
/// Returns the VM's current health status and consecutive failure count.
///
/// # Errors
///
/// Returns `404 Not Found` if the VM does not exist.
#[utoipa::path(
    get,
    path = "/v1/vms/{id}/health",
    tag = "vms",
    params(
        ("id" = String, Path, description = "VM identifier")
    ),
    responses(
        (status = 200, description = "VM health status", body = VmHealthReport),
        (status = 404, description = "VM not found")
    )
)]
pub async fn get_vm_health(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<VmHealthReport>, StatusCode> {
    // Verify the VM exists in the backend.
    state
        .backend
        .get(&id)
        .await
        .map_err(|_| StatusCode::NOT_FOUND)?;

    // Look up health data from the health check loop.
    let report = if let Some(ref health) = state.health {
        health.report(&id).await.unwrap_or_else(|| VmHealthReport {
            vm_id: id.clone(),
            status: HealthStatus::Unknown,
            consecutive_failures: 0,
        })
    } else {
        VmHealthReport {
            vm_id: id.clone(),
            status: HealthStatus::Unknown,
            consecutive_failures: 0,
        }
    };

    Ok(Json(report))
}

#[cfg(test)]
#[path = "health_test.rs"]
mod tests;
