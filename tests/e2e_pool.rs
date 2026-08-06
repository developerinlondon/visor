//! E2E tests for pool warm/acquire endpoints.
//!
//! Tests `GET /v1/pool` and `POST /v1/pool/warm` using an in-process Axum app
//! via `tower::ServiceExt::oneshot`. Uses a mock [`ExecutionBackend`] to avoid
//! real OCI pulls or KVM boot.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use visor_runtime::api::router::{AppState, build_router};
use visor_runtime::api::sse::EventBroadcaster;
use visor_runtime::backend::{ExecRequest, ExecResult, ExecutionBackend, VmConfig, VmInfo};
use visor_runtime::pool::manager::{PoolConfig, PoolManager, PoolStatus};

// ── Mock backend ────────────────────────────────────────────────────

/// A mock execution backend that creates fake VMs for pool testing.
struct MockPoolBackend {
    next_id: std::sync::atomic::AtomicU32,
}

impl MockPoolBackend {
    fn new() -> Self {
        Self {
            next_id: std::sync::atomic::AtomicU32::new(1),
        }
    }
}

#[async_trait::async_trait]
impl ExecutionBackend for MockPoolBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Ok(serde_json::from_value(serde_json::json!({
            "id": format!("pool-vm-{id}"),
            "image": config.image,
            "state": "running",
            "created_at": "2025-01-01T00:00:00Z",
            "memory_mib": config.memory_mib,
            "vcpus": config.vcpus,
            "ports": []
        }))
        .expect("mock VmInfo"))
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(vec![])
    }

    async fn get(&self, _id: &str) -> anyhow::Result<VmInfo> {
        anyhow::bail!("not found")
    }

    async fn exec(&self, _id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        anyhow::bail!("not implemented")
    }

    async fn stop(&self, _id: &str, _timeout_secs: u64) -> anyhow::Result<()> {
        Ok(())
    }

    async fn kill(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn destroy(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn console_output(&self, _id: &str) -> anyhow::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Builds an [`AppState`] with a mock backend and a configured pool manager.
fn test_state_with_pool() -> AppState {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockPoolBackend::new());
    let pool = Arc::new(PoolManager::new(
        PoolConfig::default(),
        backend.clone(),
        visor_runtime::pool::snapshot_cache::SnapshotCache::new(std::path::PathBuf::from(
            "/tmp/visor-e2e-pool-test",
        )),
    ));

    AppState {
        backend,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: std::time::Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: Some(pool),
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            visor_runtime::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(
            visor_runtime::net::dns::DnsRegistry::new(),
        )),
    }
}

/// Builds a JSON request for the given method and URI.
fn json_request(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");

    if let Some(b) = body {
        builder
            .body(Body::from(serde_json::to_vec(&b).expect("serialize body")))
            .expect("build request with body")
    } else {
        builder
            .body(Body::empty())
            .expect("build request without body")
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_pool_status_empty() {
    let app = build_router(test_state_with_pool());
    let response = app
        .oneshot(json_request("GET", "/v1/pool", None))
        .await
        .expect("pool status request should succeed");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET /v1/pool should return 200"
    );

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read pool status response body");
    let status: PoolStatus =
        serde_json::from_slice(&bytes).expect("pool status should deserialize");
    assert_eq!(status.total, 0, "fresh pool should have no warmed VMs");
}

#[tokio::test]
async fn test_pool_warm_request() {
    let app = build_router(test_state_with_pool());

    let body = serde_json::json!({
        "image": "alpine:latest",
        "count": 1
    });
    let response = app
        .oneshot(json_request("POST", "/v1/pool/warm", Some(body)))
        .await
        .expect("pool warm request should succeed");

    // Pool warming should return 200 (sync warm) or 202 (async warm).
    let status = response.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::ACCEPTED,
        "POST /v1/pool/warm should return 200 or 202, got: {status}"
    );
}
