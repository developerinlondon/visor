use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use crate::api::router::{AppState, build_router};
use crate::api::sse::EventBroadcaster;
use crate::backend::{ExecRequest, ExecResult, ExecutionBackend, VmConfig, VmInfo, VmState};
use crate::pool::manager::{PoolConfig, PoolManager};

use super::*;

// ── Mock backend for route tests ────────────────────────────────

struct MockPoolBackend {
    next_id: AtomicU32,
}

impl MockPoolBackend {
    fn new() -> Self {
        Self {
            next_id: AtomicU32::new(1),
        }
    }
}

#[async_trait]
impl ExecutionBackend for MockPoolBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(VmInfo::new(
            format!("pool-vm-{id}"),
            config.image,
            VmState::Running,
            "2025-01-01T00:00:00Z".to_owned(),
            config.memory_mib,
            config.vcpus,
        ))
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

fn test_state_with_pool() -> AppState {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockPoolBackend::new());
    let pool = Arc::new(PoolManager::new(
        PoolConfig::default(),
        backend.clone(),
        crate::pool::snapshot_cache::SnapshotCache::new(std::path::PathBuf::from(
            "/tmp/visor-pool-test",
        )),
    ));

    AppState {
        backend,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: Some(pool),
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    }
}

fn test_state_without_pool() -> AppState {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockPoolBackend::new());
    AppState {
        backend,
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

// ── GET /v1/pool ────────────────────────────────────────────────

#[tokio::test]
async fn get_pool_status_returns_200() {
    let app = build_router(test_state_with_pool());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/pool")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap();
    let status: PoolStatus = serde_json::from_slice(&body).unwrap();
    assert_eq!(status.total, 0);
}

#[tokio::test]
async fn get_pool_status_returns_503_without_pool() {
    let app = build_router(test_state_without_pool());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/pool")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
}

// ── POST /v1/pool/warm ──────────────────────────────────────────

#[tokio::test]
async fn warm_pool_returns_200() {
    let state = test_state_with_pool();
    let app = build_router(state);

    let body = serde_json::json!({
        "image": "alpine:latest",
        "count": 2
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/pool/warm")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// ── POST /v1/pool/drain ─────────────────────────────────────────

#[tokio::test]
async fn drain_pool_returns_200() {
    let app = build_router(test_state_with_pool());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/pool/drain")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

// ── WarmRequest serialization ───────────────────────────────────

#[test]
fn warm_request_deserializes_correctly() {
    let json = r#"{"image": "alpine:latest", "count": 5}"#;
    let req: WarmRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.image, "alpine:latest");
    assert_eq!(req.count, 5);
}

#[test]
fn pool_action_response_serializes_correctly() {
    let resp = PoolActionResponse {
        status: "ok".to_owned(),
    };
    let json = serde_json::to_value(&resp).unwrap();
    assert_eq!(json["status"], "ok");
}
