use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use crate::api::router::{AppState, build_router};
use crate::api::sse::EventBroadcaster;
use crate::backend::{ExecutionBackend, VmmBackend};
use crate::pool::health::{HealthCheckConfig, HealthCheckLoop, HealthChecker};

use super::*;

// ── Mock pinger for route tests ──────────────────────────────────

struct AlwaysHealthyPinger;

#[async_trait::async_trait]
impl crate::pool::health::VsockHealthPinger for AlwaysHealthyPinger {
    async fn ping(&self, _cid: u32, _timeout: Duration) -> anyhow::Result<()> {
        Ok(())
    }
}

fn test_state_with_health() -> AppState {
    let events = Arc::new(EventBroadcaster::new(16));
    let config = HealthCheckConfig::default();
    let checker = HealthChecker::new(Arc::new(AlwaysHealthyPinger), config.clone());
    let health_loop = Arc::new(HealthCheckLoop::new(checker, events.clone(), config));

    AppState {
        backend: Arc::new(VmmBackend::new()) as Arc<dyn ExecutionBackend>,
        events,
        start_time: Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: Some(health_loop),
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    }
}

fn test_state_without_health() -> AppState {
    AppState {
        backend: Arc::new(VmmBackend::new()) as Arc<dyn ExecutionBackend>,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    }
}

// ── GET /v1/health (daemon health) ───────────────────────────────

#[tokio::test]
async fn daemon_health_returns_200_ok() {
    let app = build_router(test_state_with_health());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// ── GET /v1/vms/{id}/health ──────────────────────────────────────

#[tokio::test]
async fn vm_health_returns_404_for_missing_vm() {
    let app = build_router(test_state_with_health());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/vms/nonexistent/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn vm_health_returns_404_for_nonexistent_vm() {
    let app = build_router(test_state_with_health());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/vms/fake-id/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn daemon_health_returns_200_without_health_loop() {
    let app = build_router(test_state_without_health());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// ── DaemonHealth serialization ───────────────────────────────────

#[test]
fn daemon_health_response_serializes_correctly() {
    let resp = DaemonHealthResponse {
        status: "ok".to_owned(),
        uptime_secs: 42,
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["status"], "ok");
    assert_eq!(json["uptime_secs"], 42);
}
