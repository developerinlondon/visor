//! E2E tests for the exec endpoint.
//!
//! Tests `POST /v1/vms/{id}/exec` using an in-process Axum app via
//! `tower::ServiceExt::oneshot`. Uses a mock [`ExecutionBackend`] to avoid
//! requiring a running KVM daemon.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use visor_runtime::api::router::{AppState, build_router};
use visor_runtime::api::sse::EventBroadcaster;
use visor_runtime::backend::{
    ExecRequest, ExecResult, ExecutionBackend, VmConfig, VmInfo, VmmBackend,
};

// ── Helpers ─────────────────────────────────────────────────────────

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

/// Creates a mock `VmInfo` via JSON deserialization (bypasses `#[non_exhaustive]`).
fn mock_vm_info(id: &str, image: &str) -> VmInfo {
    serde_json::from_value(serde_json::json!({
        "id": id,
        "name": format!("{id}-name"),
        "image": image,
        "state": "running",
        "created_at": "2025-01-01T00:00:00Z",
        "memory_mib": 512,
        "vcpus": 1,
        "ports": []
    }))
    .expect("mock VmInfo should deserialize")
}

/// Creates a mock `ExecResult` via JSON deserialization.
fn mock_exec_result(exit_code: i32, stdout: &str) -> ExecResult {
    serde_json::from_value(serde_json::json!({
        "exit_code": exit_code,
        "stdout": stdout,
        "stderr": ""
    }))
    .expect("mock ExecResult should deserialize")
}

// ── Mock backend ────────────────────────────────────────────────────

/// A mock execution backend that returns canned responses for exec.
struct MockExecBackend;

#[async_trait::async_trait]
impl ExecutionBackend for MockExecBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        Ok(mock_vm_info("mock-vm", &config.image))
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(vec![mock_vm_info("mock-vm", "alpine:latest")])
    }

    async fn get(&self, id: &str) -> anyhow::Result<VmInfo> {
        if id == "mock-vm" {
            Ok(mock_vm_info("mock-vm", "alpine:latest"))
        } else {
            Err(anyhow::anyhow!("vm not found: {id}"))
        }
    }

    async fn exec(&self, id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        if id == "mock-vm" {
            Ok(mock_exec_result(0, "mock exec output\n"))
        } else {
            Err(anyhow::anyhow!("vm not found: {id}"))
        }
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

/// Builds an [`AppState`] with the mock exec backend.
fn mock_exec_state() -> AppState {
    AppState {
        backend: Arc::new(MockExecBackend) as Arc<dyn ExecutionBackend>,
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

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_exec_on_nonexistent_vm_returns_404() {
    let app = build_router(test_app_state());
    let body = serde_json::json!({ "cmd": ["echo", "hello"] });
    let response = app
        .oneshot(json_request("POST", "/v1/vms/fake/exec", Some(body)))
        .await
        .expect("exec request should not panic");

    assert_eq!(
        response.status(),
        StatusCode::NOT_FOUND,
        "exec on nonexistent VM should return 404"
    );

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read error response body");
    let error: serde_json::Value =
        serde_json::from_slice(&bytes).expect("error body should be valid JSON");
    assert!(
        error["error"].as_str().is_some(),
        "error response should have an 'error' field"
    );
}

#[tokio::test]
async fn test_exec_with_mock_backend() {
    let app = build_router(mock_exec_state());
    let body = serde_json::json!({ "cmd": ["echo", "hello"] });
    let response = app
        .oneshot(json_request("POST", "/v1/vms/mock-vm/exec", Some(body)))
        .await
        .expect("exec request should succeed");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "exec on mock VM should return 200"
    );

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read exec response body");
    let result: ExecResult =
        serde_json::from_slice(&bytes).expect("exec response should deserialize");
    assert_eq!(result.exit_code, 0, "mock exec should return exit code 0");
    assert!(
        result.stdout.contains("mock exec output"),
        "mock exec stdout should contain expected output, got: {:?}",
        result.stdout
    );
}
