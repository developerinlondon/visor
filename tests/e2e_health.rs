//! E2E tests for the health check endpoint.
//!
//! Tests `GET /v1/health` using an in-process Axum app via `tower::ServiceExt::oneshot`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use visor_runtime::api::router::{AppState, build_router};
use visor_runtime::api::sse::EventBroadcaster;
use visor_runtime::backend::{ExecutionBackend, VmmBackend};

/// Builds an [`AppState`] with a fresh [`VmmBackend`] for API testing.
fn test_app_state() -> AppState {
    AppState {
        backend: Arc::new(VmmBackend::new()) as Arc<dyn ExecutionBackend>,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: std::time::Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            visor_runtime::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(
            visor_runtime::net::dns::DnsRegistry::new(),
        )),
    }
}

#[tokio::test]
async fn test_health_endpoint_returns_ok() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/health")
                .body(Body::empty())
                .expect("build health request"),
        )
        .await
        .expect("health request should succeed");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET /v1/health should return 200 OK"
    );
}
