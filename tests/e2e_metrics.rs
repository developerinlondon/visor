//! E2E tests for the Prometheus metrics endpoint.
//!
//! Tests `GET /v1/metrics` using an in-process Axum app via `tower::ServiceExt::oneshot`.

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

/// Helper: fetches metrics and returns `(status, body_text)`.
async fn fetch_metrics(state: AppState) -> (StatusCode, String) {
    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/metrics")
                .body(Body::empty())
                .expect("build metrics request"),
        )
        .await
        .expect("metrics request should succeed");

    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read metrics response body");
    let text = String::from_utf8(bytes.to_vec()).expect("metrics body should be valid UTF-8");
    (status, text)
}

#[tokio::test]
async fn test_metrics_endpoint_returns_prometheus_format() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/metrics")
                .body(Body::empty())
                .expect("build metrics request"),
        )
        .await
        .expect("metrics request should succeed");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET /v1/metrics should return 200"
    );

    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type header should exist")
        .to_str()
        .expect("content-type should be valid string");
    assert!(
        content_type.starts_with("text/plain; version=0.0.4"),
        "expected Prometheus text format content type, got: {content_type}"
    );

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read metrics response body");
    let text = String::from_utf8(bytes.to_vec()).expect("metrics body should be valid UTF-8");
    assert!(
        text.contains("visor_"),
        "metrics response should contain visor_ prefixed metrics"
    );
}

#[tokio::test]
async fn test_metrics_contains_vm_counts() {
    let (status, text) = fetch_metrics(test_app_state()).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("visor_vms_total"),
        "metrics should contain visor_vms_total gauge"
    );
    assert!(
        text.contains("visor_vms_running"),
        "metrics should contain visor_vms_running gauge"
    );
}
