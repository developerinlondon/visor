//! VM lifecycle routes: create, list, get, start, destroy, exec, stop.
//!
//! All routes use the shared [`AppState`](crate::api::router::AppState) to
//! access the execution backend and are annotated with `utoipa` for `OpenAPI`
//! documentation.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use crate::api::router::AppState;
use crate::api::sse::VmEvent;
use crate::backend::{ExecRequest, ExecResult, VmConfig, VmInfo};

/// API error wrapper that converts `anyhow::Error` into an HTTP response.
///
/// Maps error messages to appropriate HTTP status codes:
/// - "not found" / "image not found" → 404
/// - "authentication failed" / "unauthorized" → 401
/// - "rate limited" → 429
/// - Everything else → 500
pub struct ApiError(anyhow::Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> axum::response::Response {
        let msg = format!("{:#}", self.0);
        let status = if msg.contains("not found") {
            StatusCode::NOT_FOUND
        } else if msg.contains("authentication failed") || msg.contains("unauthorized") {
            StatusCode::UNAUTHORIZED
        } else if msg.contains("rate limited") {
            StatusCode::TOO_MANY_REQUESTS
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        let body = serde_json::json!({ "error": msg });
        (status, Json(body)).into_response()
    }
}

impl<E: Into<anyhow::Error>> From<E> for ApiError {
    fn from(err: E) -> Self {
        Self(err.into())
    }
}

/// Creates a new VM from the given configuration.
///
/// Returns the created VM info with a `201 Created` status.
///
/// # Errors
///
/// Returns an error if the backend fails to create the VM.
#[utoipa::path(
    post,
    path = "",
    tag = "vms",
    request_body = VmConfig,
    responses(
        (status = 201, description = "VM created successfully", body = VmInfo),
        (status = 500, description = "Failed to create VM")
    )
)]
pub async fn create_vm(
    State(state): State<AppState>,
    Json(config): Json<VmConfig>,
) -> Result<(StatusCode, Json<VmInfo>), ApiError> {
    let info = if let Some(ref pool) = state.pool {
        pool.acquire_with_config(config).await?
    } else {
        state.backend.create(config).await?
    };

    // Register VM name in DNS if a name was assigned.
    if let Some(ref name) = info.name {
        // DNS registration is best-effort — don't fail VM creation if DNS fails.
        state
            .dns
            .write()
            .await
            .register(name, std::net::Ipv4Addr::UNSPECIFIED);
    }

    state.events.send(VmEvent::new("vm.created", &info.id));
    Ok((StatusCode::CREATED, Json(info)))
}

/// Lists all known VMs.
///
/// # Errors
///
/// Returns an error if the backend state cannot be read.
#[utoipa::path(
    get,
    path = "",
    tag = "vms",
    responses(
        (status = 200, description = "List of all VMs", body = Vec<VmInfo>)
    )
)]
pub async fn list_vms(State(state): State<AppState>) -> Result<Json<Vec<VmInfo>>, ApiError> {
    let vms = state.backend.list().await?;
    Ok(Json(vms))
}

/// Gets information about a specific VM by ID.
///
/// # Errors
///
/// Returns an error if the VM is not found.
#[utoipa::path(
    get,
    path = "/{id}",
    tag = "vms",
    params(
        ("id" = String, Path, description = "VM identifier")
    ),
    responses(
        (status = 200, description = "VM information", body = VmInfo),
        (status = 500, description = "VM not found")
    )
)]
pub async fn get_vm(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<VmInfo>, ApiError> {
    let info = state.backend.get(&id).await?;
    Ok(Json(info))
}

/// Destroys a VM, removing all associated resources.
///
/// # Errors
///
/// Returns an error if the VM is not found or cannot be destroyed.
#[utoipa::path(
    delete,
    path = "/{id}",
    tag = "vms",
    params(
        ("id" = String, Path, description = "VM identifier")
    ),
    responses(
        (status = 204, description = "VM destroyed"),
        (status = 500, description = "VM not found or cannot be destroyed")
    )
)]
pub async fn destroy_vm(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    // Look up VM name before destroying (for DNS unregistration).
    let vm_name = state.backend.get(&id).await.ok().and_then(|info| info.name);

    state.backend.destroy(&id).await?;

    // Unregister VM name from DNS (best-effort).
    if let Some(ref name) = vm_name {
        state.dns.write().await.unregister(name);
    }

    state.events.send(VmEvent::new("vm.destroyed", &id));

    Ok(StatusCode::NO_CONTENT)
}

/// Executes a command inside a running VM.
///
/// # Errors
///
/// Returns an error if the VM is not found or not running.
#[utoipa::path(
    post,
    path = "/{id}/exec",
    tag = "vms",
    params(
        ("id" = String, Path, description = "VM identifier")
    ),
    request_body = ExecRequest,
    responses(
        (status = 200, description = "Command executed", body = ExecResult),
        (status = 500, description = "VM not found or not running")
    )
)]
pub async fn exec_vm(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ExecRequest>,
) -> Result<Json<ExecResult>, ApiError> {
    let result = state.backend.exec(&id, req).await?;
    Ok(Json(result))
}

/// Query parameters for the stop endpoint.
#[derive(Debug, serde::Deserialize)]
pub struct StopQuery {
    /// Grace period in seconds before force-killing (default: 10).
    #[serde(default = "default_stop_timeout")]
    pub t: u64,
}

fn default_stop_timeout() -> u64 {
    10
}

/// Stops a running VM with an optional grace period.
///
/// Accepts `?t=N` query parameter for the grace period in seconds (default: 10).
/// This matches the Docker API convention.
///
/// # Errors
///
/// Returns an error if the VM is not found or cannot be stopped.
#[utoipa::path(
    post,
    path = "/{id}/stop",
    tag = "vms",
    params(
        ("id" = String, Path, description = "VM identifier"),
        ("t" = u64, Query, description = "Grace period in seconds (default: 10)")
    ),
    responses(
        (status = 204, description = "VM stopped"),
        (status = 500, description = "VM not found or cannot be stopped")
    )
)]
pub async fn stop_vm(
    State(state): State<AppState>,
    Path(id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<StopQuery>,
) -> Result<StatusCode, ApiError> {
    // Look up VM name for DNS unregistration.
    let vm_name = state.backend.get(&id).await.ok().and_then(|info| info.name);

    state.backend.stop(&id, query.t).await?;

    // Unregister from DNS on stop.
    if let Some(ref name) = vm_name {
        state.dns.write().await.unregister(name);
    }

    state.events.send(VmEvent::new("vm.stopped", &id));

    Ok(StatusCode::NO_CONTENT)
}

/// Starts a previously stopped or failed VM again.
///
/// # Errors
///
/// Returns an error if the VM is not found, cannot be restarted from its
/// current state, or the boot path fails.
#[utoipa::path(
    post,
    path = "/{id}/start",
    tag = "vms",
    params(
        ("id" = String, Path, description = "VM identifier")
    ),
    responses(
        (status = 200, description = "VM started", body = VmInfo),
        (status = 404, description = "VM not found"),
        (status = 500, description = "VM could not be started")
    )
)]
pub async fn start_vm(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<VmInfo>, ApiError> {
    let info = state.backend.start(&id).await?;

    if let Some(ref name) = info.name {
        state
            .dns
            .write()
            .await
            .register(name, std::net::Ipv4Addr::UNSPECIFIED);
    }

    state.events.send(VmEvent::new("vm.started", &id));

    Ok(Json(info))
}

/// Force-kills a running VM immediately (no graceful shutdown).
///
/// # Errors
///
/// Returns an error if the VM is not found.
#[utoipa::path(
    post,
    path = "/{id}/kill",
    tag = "vms",
    params(
        ("id" = String, Path, description = "VM identifier")
    ),
    responses(
        (status = 204, description = "VM killed"),
        (status = 404, description = "VM not found")
    )
)]
pub async fn kill_vm(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<StatusCode, ApiError> {
    state.backend.kill(&id).await?;

    state.events.send(VmEvent::new("vm.killed", &id));

    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
#[path = "vms_test.rs"]
mod tests;
