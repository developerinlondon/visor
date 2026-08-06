//! Docker Engine API compatibility layer for visor.
//!
//! Translates Docker Engine API requests into visor `ExecutionBackend`
//! calls, allowing stock Docker tooling (`docker` CLI, `docker-compose`,
//! Testcontainers) to drive visor microVMs without modification.
//!
//! # Usage
//!
//! ```rust,ignore
//! use visor_docker::docker_router;
//!
//! let backend: Arc<dyn ExecutionBackend> = /* ... */;
//! let router = docker_router(backend);
//! ```
//!
//! The returned [`axum::Router`] handles all `/v1.XX/` prefixed Docker
//! API paths alongside visor's native `/v1/` routes on the same socket.

use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::sync::Arc;

use async_trait::async_trait;
use axum::Router;
use axum::http::header::HeaderValue;
use tokio::sync::Mutex;
use visor_types::{BuildService, ExecutionBackend, ImageManager};

pub mod handlers;
pub mod translate;
pub mod types;

#[cfg(test)]
#[path = "lib_test.rs"]
mod tests;

/// Shared state for Docker API handlers.
///
/// Holds a reference to the VM execution backend and exec session
/// storage that all Docker operations are translated into.
#[derive(Clone)]
pub struct DockerState {
    /// Backend for VM lifecycle operations.
    pub backend: Arc<dyn ExecutionBackend>,
    /// In-memory exec session storage for the two-phase create/start flow.
    pub exec_sessions: handlers::ExecSessions,
    /// Optional build service for real Dockerfile builds.
    pub build_service: Option<Arc<dyn BuildService>>,
    /// Optional image store for persisted OCI images.
    pub image_store: Option<Arc<visor_build::ImageStore>>,
    /// Optional image manager for real pull/inspect/remove operations.
    pub(crate) image_manager: Option<Arc<dyn ImageManager>>,
    /// In-memory Docker network metadata used by Compose-style workflows.
    pub(crate) networks: handlers::DockerNetworks,
    /// In-memory Docker volume metadata used by Compose-style workflows.
    pub(crate) volumes: handlers::DockerVolumes,
    /// Logical Docker containers and their bound backend VM IDs.
    pub(crate) containers: handlers::DockerContainers,
    /// Optional service-discovery registry for Docker network aliases.
    pub(crate) service_discovery: Option<Arc<dyn ServiceDiscovery>>,
}

/// Optional service-discovery registry used by the Docker shim.
#[async_trait]
pub trait ServiceDiscovery: Send + Sync {
    /// Register a Docker-visible name to an IP address.
    async fn register_name(&self, name: &str, ip: Ipv4Addr);

    /// Remove a Docker-visible name from the registry.
    async fn unregister_name(&self, name: &str);

    /// Returns the current set of registered names and addresses.
    async fn snapshot_names(&self) -> Vec<(String, Ipv4Addr)>;
}
/// API version advertised to Docker clients.
///
/// We support Docker Engine API v1.45 (Docker 27.x). Clients negotiate
/// down to this version automatically via `GET /_ping`.
pub const API_VERSION: &str = "1.45";

/// Minimum API version we accept from clients.
pub const MIN_API_VERSION: &str = "1.24";

/// Builds the Docker-compatible API router.
///
/// Mount this alongside visor's native router on the same socket.
/// Docker paths (`/_ping`, `/version`, `/v1.XX/containers/*`) don't
/// collide with visor paths (`/v1/vms/*`).
///
/// Pass `None` for `build_service` to use fake progress messages
/// (useful for testing). Pass `Some(service)` to execute real builds.
/// Pass `image_store` to enable real image listing/inspection from disk.
///
/// # Examples
///
/// ```rust,ignore
/// let app = Router::new()
///     .merge(native_visor_router)
///     .merge(docker_router(backend, None, None));
/// ```
pub fn docker_router(
    backend: Arc<dyn ExecutionBackend>,
    build_service: Option<Arc<dyn BuildService>>,
    image_store: Option<Arc<visor_build::ImageStore>>,
) -> Router {
    docker_router_with_service_discovery(backend, build_service, image_store, None, None)
}

/// Builds the Docker-compatible API router with an optional image manager.
///
/// Use this when the caller can provide a real image lifecycle implementation
/// for Docker-compatible `pull`, `inspect`, `images`, and `rmi` flows.
pub fn docker_router_with_image_manager(
    backend: Arc<dyn ExecutionBackend>,
    build_service: Option<Arc<dyn BuildService>>,
    image_store: Option<Arc<visor_build::ImageStore>>,
    image_manager: Option<Arc<dyn ImageManager>>,
) -> Router {
    docker_router_with_service_discovery(backend, build_service, image_store, image_manager, None)
}

/// Builds the Docker-compatible API router with optional image and service-discovery support.
pub fn docker_router_with_service_discovery(
    backend: Arc<dyn ExecutionBackend>,
    build_service: Option<Arc<dyn BuildService>>,
    image_store: Option<Arc<visor_build::ImageStore>>,
    image_manager: Option<Arc<dyn ImageManager>>,
    service_discovery: Option<Arc<dyn ServiceDiscovery>>,
) -> Router {
    let state = DockerState {
        backend,
        exec_sessions: Arc::new(Mutex::new(HashMap::new())),
        build_service,
        image_store,
        image_manager,
        networks: Arc::new(Mutex::new(HashMap::new())),
        volumes: Arc::new(Mutex::new(HashMap::new())),
        containers: Arc::new(Mutex::new(HashMap::new())),
        service_discovery,
    };

    let versioned = versioned_routes();
    let versioned_with_system = versioned
        .clone()
        .route("/version", axum::routing::get(handlers::version));

    Router::new()
        // Version-prefixed paths (e.g. /v1.45/containers/json)
        .nest("/v{version}", versioned_with_system)
        // Unversioned paths (some clients omit the version prefix)
        .merge(versioned)
        // Top-level endpoints (never versioned)
        .route(
            "/_ping",
            axum::routing::get(handlers::ping).head(handlers::ping),
        )
        .route("/version", axum::routing::get(handlers::version))
        // Api-Version header on every response
        .layer(axum::middleware::from_fn(add_docker_headers))
        .with_state(state)
}

/// Registers all versioned Docker API routes.
fn versioned_routes() -> Router<DockerState> {
    Router::new()
        // Container CRUD
        .route(
            "/containers/json",
            axum::routing::get(handlers::container_list),
        )
        .route("/events", axum::routing::get(handlers::events))
        .route(
            "/containers/create",
            axum::routing::post(handlers::container_create),
        )
        .route(
            "/containers/{id}/json",
            axum::routing::get(handlers::container_inspect),
        )
        .route(
            "/containers/{id}/start",
            axum::routing::post(handlers::container_start),
        )
        .route(
            "/containers/{id}/stop",
            axum::routing::post(handlers::container_stop),
        )
        .route(
            "/containers/{id}/kill",
            axum::routing::post(handlers::container_kill),
        )
        .route(
            "/containers/{id}",
            axum::routing::delete(handlers::container_remove),
        )
        .route(
            "/containers/{id}/wait",
            axum::routing::post(handlers::container_wait),
        )
        .route(
            "/containers/{id}/attach",
            axum::routing::post(handlers::container_attach),
        )
        .route(
            "/containers/{id}/logs",
            axum::routing::get(handlers::container_logs),
        )
        .route(
            "/containers/{id}/archive",
            axum::routing::put(handlers::container_archive_put),
        )
        // Exec
        .route(
            "/containers/{id}/exec",
            axum::routing::post(handlers::exec_create),
        )
        .route(
            "/exec/{id}/start",
            axum::routing::post(handlers::exec_start),
        )
        .route(
            "/exec/{id}/json",
            axum::routing::get(handlers::exec_inspect),
        )
        // Images
        .route("/images/json", axum::routing::get(handlers::image_list))
        .route("/images/load", axum::routing::post(handlers::image_load))
        .route(
            "/images/create",
            axum::routing::post(handlers::image_create),
        )
        .route(
            "/images/{name}/json",
            axum::routing::get(handlers::image_inspect),
        )
        .route(
            "/images/{name}",
            axum::routing::delete(handlers::image_remove),
        )
        // Networks — chain GET and DELETE on same path to avoid route collision
        .route("/networks", axum::routing::get(handlers::network_list))
        .route(
            "/networks/create",
            axum::routing::post(handlers::network_create),
        )
        .route(
            "/networks/{id}",
            axum::routing::get(handlers::network_inspect).delete(handlers::network_remove),
        )
        // Volumes
        .route("/volumes", axum::routing::get(handlers::volume_list))
        .route(
            "/volumes/create",
            axum::routing::post(handlers::volume_create),
        )
        .route(
            "/volumes/{name}",
            axum::routing::get(handlers::volume_inspect).delete(handlers::volume_remove),
        )
        // Build
        .route("/build", axum::routing::post(handlers::build_image))
        // System info (must be inside versioned_routes so /v1.45/info works)
        .route("/info", axum::routing::get(handlers::info))
}

/// Middleware that adds Docker version headers to every response.
async fn add_docker_headers(
    request: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    tracing::info!(
        method = %request.method(),
        uri = %request.uri(),
        "docker api request",
    );
    let mut response = next.run(request).await;
    tracing::info!(status = %response.status(), "docker api response");
    let headers = response.headers_mut();
    headers.insert("Api-Version", HeaderValue::from_static(API_VERSION));
    headers.insert("Server", HeaderValue::from_static("visor"));
    headers.insert("OSType", HeaderValue::from_static("linux"));
    response
}
