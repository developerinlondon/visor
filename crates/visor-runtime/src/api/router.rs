//! Axum router construction with shared application state.
//!
//! Builds the complete API router with all routes, `OpenAPI` spec via `utoipa`,
//! and Swagger UI at `/docs`.

use std::sync::Arc;

use utoipa::OpenApi;
use utoipa_axum::router::OpenApiRouter;
use utoipa_axum::routes;
use utoipa_swagger_ui::SwaggerUi;

use crate::api::sse::EventBroadcaster;
use crate::backend::ExecutionBackend;
use crate::net::dns::DnsRegistry;
use crate::pool::health::HealthCheckLoop;
use crate::pool::manager::PoolManager;
/// Shared application state passed to all route handlers.
#[derive(Clone)]
pub struct AppState {
    /// The execution backend for VM lifecycle operations.
    pub backend: Arc<dyn ExecutionBackend>,
    /// SSE event broadcaster.
    pub events: Arc<EventBroadcaster>,
    /// Daemon start time for uptime calculation.
    pub start_time: std::time::Instant,
    /// Shutdown signal — notify to trigger graceful daemon shutdown.
    pub shutdown: Arc<tokio::sync::Notify>,
    /// Optional health check loop for VM health monitoring.
    pub health: Option<Arc<HealthCheckLoop>>,
    /// Optional warm pool manager for pre-warmed VMs.
    pub pool: Option<Arc<PoolManager>>,
    /// Network manager for user-defined virtual networks.
    pub networks: Arc<tokio::sync::RwLock<crate::api::routes::networks::NetworkManager>>,
    /// Shared DNS registry for VM name resolution.
    pub dns: Arc<tokio::sync::RwLock<DnsRegistry>>,
}

/// `OpenAPI` document definition with tags for grouping endpoints.
#[derive(OpenApi)]
#[openapi(
    tags(
        (name = "vms", description = "VM lifecycle management"),
        (name = "system", description = "System information and health"),
        (name = "pool", description = "Warm VM pool management")
    )
)]
struct ApiDoc;

/// Builds the complete Axum router with all routes and `OpenAPI` documentation.
///
/// # Routes
///
/// | Method | Path | Description |
/// |--------|------|-------------|
/// | GET | `/v1/info` | System information |
/// | GET | `/v1/health` | Health check |
/// | POST | `/v1/shutdown` | Graceful daemon shutdown |
/// | GET | `/v1/events` | SSE event stream |
/// | POST | `/v1/vms` | Create a VM |
/// | GET | `/v1/vms` | List all VMs |
/// | GET | `/v1/vms/{id}` | Get VM info |
/// | DELETE | `/v1/vms/{id}` | Destroy a VM |
/// | POST | `/v1/vms/{id}/exec` | Execute command in VM |
/// | POST | `/v1/vms/{id}/stop` | Stop a VM |
/// | GET | `/v1/vms/{id}/health` | Per-VM health check |
/// | GET | `/v1/vms/{id}/attach` | WebSocket: interactive shell |
/// | GET | `/v1/vms/{id}/logs` | WebSocket: log streaming |
/// | GET | `/v1/metrics` | Prometheus metrics |
/// | GET | `/v1/pool` | Pool status |
/// | POST | `/v1/pool/warm` | Warm pool for image |
/// | POST | `/v1/pool/drain` | Drain all pools |
/// | GET | `/v1/images` | List cached images |
/// | POST | `/v1/images/pull` | Pull an image |
/// | GET | `/v1/images/{ref}` | Inspect cached image |
/// | DELETE | `/v1/images/{ref}` | Remove cached image |
///
/// Swagger UI is served at `/docs`.
pub fn build_router(state: AppState) -> axum::Router {
    let (router, api) = OpenApiRouter::with_openapi(ApiDoc::openapi())
        // System routes
        .routes(routes!(super::routes::info::get_info))
        .routes(routes!(super::routes::info::health_check))
        .routes(routes!(super::routes::info::shutdown_daemon))
        .routes(routes!(super::sse::event_stream))
        // VM routes (nested under /v1/vms)
        .nest("/v1/vms", vm_routes())
        // Pool routes
        .routes(routes!(super::routes::pool::get_pool_status))
        .routes(routes!(super::routes::pool::warm_pool))
        .routes(routes!(super::routes::pool::drain_pool))
        .split_for_parts();

    router
        .merge(SwaggerUi::new("/docs").url("/api-docs/openapi.json", api))
        .merge(super::routes::networks::network_routes())
        // Image routes
        .route(
            "/v1/images",
            axum::routing::get(super::routes::images::list_images),
        )
        .route(
            "/v1/images/pull",
            axum::routing::post(super::routes::images::pull_image),
        )
        .route(
            "/v1/images/{reference}",
            axum::routing::get(super::routes::images::inspect_image)
                .delete(super::routes::images::delete_image),
        )
        // WebSocket routes (not part of OpenAPI spec — raw axum routes)
        .route(
            "/v1/vms/{id}/attach",
            axum::routing::get(super::ws::ws_attach),
        )
        .route("/v1/vms/{id}/logs", axum::routing::get(super::ws::ws_logs))
        // Prometheus metrics (not part of OpenAPI spec — raw text format)
        .route(
            "/v1/metrics",
            axum::routing::get(super::routes::metrics::get_metrics),
        )
        .with_state(state)
}

/// Builds the VM sub-router with CRUD + exec + start/stop routes.
fn vm_routes() -> OpenApiRouter<AppState> {
    OpenApiRouter::new()
        .routes(routes!(
            super::routes::vms::create_vm,
            super::routes::vms::list_vms
        ))
        .routes(routes!(
            super::routes::vms::get_vm,
            super::routes::vms::destroy_vm
        ))
        .routes(routes!(super::routes::vms::exec_vm))
        .routes(routes!(super::routes::vms::start_vm))
        .routes(routes!(super::routes::vms::stop_vm))
        .routes(routes!(super::routes::vms::kill_vm))
        .routes(routes!(super::routes::health::get_vm_health))
}

#[cfg(test)]
#[path = "router_test.rs"]
mod tests;
