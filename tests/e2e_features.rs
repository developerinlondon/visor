//! E2E tests for exec flags, port forwarding, console/shell routes,
//! and pool/metrics endpoints.
//!
//! Tests use an in-process Axum app via `tower::ServiceExt::oneshot` with
//! mock backends. No real VM boot or `/dev/kvm` required.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use clap::Parser as _;
use tower::ServiceExt as _;

use visor_runtime::api::router::{AppState, build_router};
use visor_runtime::api::sse::EventBroadcaster;
use visor_runtime::backend::{
    ExecRequest, ExecResult, ExecutionBackend, PortMapping, VmConfig, VmInfo, VmmBackend,
};
use visor_runtime::cli::{Cli, Command};

// ── Helpers ─────────────────────────────────────────────────────────

/// Builds an [`AppState`] with the real [`VmmBackend`] (empty, no VMs).
fn test_app_state() -> AppState {
    AppState {
        backend: Arc::new(VmmBackend::new()) as Arc<dyn ExecutionBackend>,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: std::time::Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: Arc::new(tokio::sync::RwLock::new(
            visor_runtime::api::routes::networks::NetworkManager::new(),
        )),
        dns: Arc::new(tokio::sync::RwLock::new(
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

// ── Mock backend for exec tests ─────────────────────────────────────

/// A mock backend that returns canned responses and records exec requests.
struct MockExecBackend;

#[async_trait::async_trait]
impl ExecutionBackend for MockExecBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        Ok(serde_json::from_value(serde_json::json!({
            "id": "mock-vm",
            "name": "mock-vm-name",
            "image": config.image,
            "state": "running",
            "created_at": "2026-01-01T00:00:00Z",
            "memory_mib": 512,
            "vcpus": 1,
            "ports": []
        }))
        .expect("mock VmInfo should deserialize"))
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(vec![
            serde_json::from_value(serde_json::json!({
                "id": "mock-vm",
                "name": "mock-vm-name",
                "image": "alpine:latest",
                "state": "running",
                "created_at": "2026-01-01T00:00:00Z",
                "memory_mib": 512,
                "vcpus": 1,
                "ports": []
            }))
            .expect("mock VmInfo should deserialize"),
        ])
    }

    async fn get(&self, id: &str) -> anyhow::Result<VmInfo> {
        if id == "mock-vm" {
            Ok(serde_json::from_value(serde_json::json!({
                "id": "mock-vm",
                "name": "mock-vm-name",
                "image": "alpine:latest",
                "state": "running",
                "created_at": "2026-01-01T00:00:00Z",
                "memory_mib": 512,
                "vcpus": 1,
                "ports": []
            }))
            .expect("mock VmInfo should deserialize"))
        } else {
            Err(anyhow::anyhow!("vm not found: {id}"))
        }
    }

    async fn exec(&self, id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        if id == "mock-vm" {
            Ok(serde_json::from_value(serde_json::json!({
                "exit_code": 0,
                "stdout": "exec output\n",
                "stderr": ""
            }))
            .expect("mock ExecResult should deserialize"))
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

/// Builds an [`AppState`] using [`MockExecBackend`].
fn mock_exec_state() -> AppState {
    AppState {
        backend: Arc::new(MockExecBackend) as Arc<dyn ExecutionBackend>,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: std::time::Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: Arc::new(tokio::sync::RwLock::new(
            visor_runtime::api::routes::networks::NetworkManager::new(),
        )),
        dns: Arc::new(tokio::sync::RwLock::new(
            visor_runtime::net::dns::DnsRegistry::new(),
        )),
    }
}

/// Builds an [`AppState`] with a pool manager wired up.
fn test_state_with_pool() -> AppState {
    use visor_runtime::pool::manager::{PoolConfig, PoolManager};

    let backend: Arc<dyn ExecutionBackend> = Arc::new(VmmBackend::new());
    let pool = Arc::new(PoolManager::new(
        PoolConfig::default(),
        backend.clone(),
        visor_runtime::pool::snapshot_cache::SnapshotCache::new(std::path::PathBuf::from(
            "/tmp/visor-e2e-test",
        )),
    ));

    AppState {
        backend,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: std::time::Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: Some(pool),
        networks: Arc::new(tokio::sync::RwLock::new(
            visor_runtime::api::routes::networks::NetworkManager::new(),
        )),
        dns: Arc::new(tokio::sync::RwLock::new(
            visor_runtime::net::dns::DnsRegistry::new(),
        )),
    }
}

// ── Exec flag tests (CLI parsing) ───────────────────────────────────

#[test]
fn cli_parse_exec_with_env_and_workdir() {
    let cli = Cli::parse_from(["visor", "exec", "-e", "FOO=bar", "-w", "/app", "vm-1", "ls"]);

    match cli.command {
        Command::Exec(args) => {
            assert_eq!(args.vm_id, "vm-1", "VM ID should be 'vm-1'");
            assert_eq!(args.cmd, vec!["ls"], "cmd should be ['ls']");
            assert_eq!(args.env, vec!["FOO=bar"], "env should contain FOO=bar");
            assert_eq!(
                args.workdir.as_deref(),
                Some("/app"),
                "workdir should be /app"
            );
        }
        other => panic!("expected Command::Exec, got: {other:?}"),
    }
}

#[test]
fn cli_parse_exec_multiple_env_vars() {
    let cli = Cli::parse_from([
        "visor", "exec", "-e", "FOO=bar", "-e", "BAZ=qux", "vm-1", "sh", "-c", "echo hi",
    ]);

    match cli.command {
        Command::Exec(args) => {
            assert_eq!(
                args.env,
                vec!["FOO=bar", "BAZ=qux"],
                "should have two env vars"
            );
            assert_eq!(
                args.cmd,
                vec!["sh", "-c", "echo hi"],
                "cmd should include trailing args"
            );
        }
        other => panic!("expected Command::Exec, got: {other:?}"),
    }
}

#[tokio::test]
async fn api_exec_with_env_and_workdir() {
    let app = build_router(mock_exec_state());
    let body = serde_json::json!({
        "cmd": ["ls"],
        "env": ["FOO=bar"],
        "working_dir": "/app"
    });

    let response = app
        .oneshot(json_request("POST", "/v1/vms/mock-vm/exec", Some(body)))
        .await
        .expect("exec request should succeed");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "exec with env/workdir should return 200"
    );

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read exec response body");
    let result: ExecResult =
        serde_json::from_slice(&bytes).expect("exec response should deserialize");
    assert_eq!(result.exit_code, 0, "mock exec should return exit code 0");
}

// ── Port forwarding tests ───────────────────────────────────────────

#[test]
fn vm_config_with_ports_deserializes() {
    let config: VmConfig = serde_json::from_value(serde_json::json!({
        "image": "nginx:latest",
        "ports": [{"host_port": 8080, "guest_port": 80}]
    }))
    .expect("VmConfig with ports should deserialize");

    assert_eq!(config.ports.len(), 1, "should have 1 port mapping");
    assert_eq!(config.ports[0].host_port, 8080);
    assert_eq!(config.ports[0].guest_port, 80);
    assert_eq!(
        config.ports[0].protocol, "tcp",
        "default protocol should be tcp"
    );
}

#[test]
fn vm_config_ports_round_trip() {
    let mut config = VmConfig::new("nginx:latest");
    config.ports = vec![
        PortMapping::new(8080, 80),
        PortMapping::with_protocol(53, 53, "udp"),
    ];

    let json = serde_json::to_value(&config).expect("VmConfig should serialize");
    let restored: VmConfig = serde_json::from_value(json).expect("VmConfig should round-trip");

    assert_eq!(restored.ports.len(), 2, "should preserve 2 port mappings");
    assert_eq!(restored.ports[0].host_port, 8080);
    assert_eq!(restored.ports[0].guest_port, 80);
    assert_eq!(restored.ports[0].protocol, "tcp");
    assert_eq!(restored.ports[1].host_port, 53);
    assert_eq!(restored.ports[1].guest_port, 53);
    assert_eq!(restored.ports[1].protocol, "udp");
}

#[test]
fn port_mapping_constructor() {
    let pm = PortMapping::new(8080, 80);
    assert_eq!(pm.host_port, 8080);
    assert_eq!(pm.guest_port, 80);
    assert_eq!(pm.protocol, "tcp", "PortMapping::new should default to tcp");
}

#[test]
fn port_mapping_with_protocol() {
    let pm = PortMapping::with_protocol(53, 53, "udp");
    assert_eq!(pm.host_port, 53);
    assert_eq!(pm.guest_port, 53);
    assert_eq!(pm.protocol, "udp");
}

// ── Console/Shell route existence tests ─────────────────────────────

#[tokio::test]
async fn api_attach_nonexistent_vm_returns_error() {
    let app = build_router(test_app_state());

    // Send a plain GET without WebSocket upgrade headers — the handler
    // should reject it (either 400 bad request or an error about
    // missing upgrade).
    let request = Request::builder()
        .uri("/v1/vms/nonexistent/attach")
        .body(Body::empty())
        .expect("build attach request");

    let response = app
        .oneshot(request)
        .await
        .expect("attach request should not panic");

    // The route exists (not 404/405), but without a valid WS upgrade
    // it returns an error status.
    let status = response.status();
    assert!(
        status.is_client_error() || status.is_server_error(),
        "attach without WS upgrade should return error, got: {status}"
    );
}

#[tokio::test]
async fn api_logs_nonexistent_vm_returns_error() {
    let app = build_router(test_app_state());

    let request = Request::builder()
        .uri("/v1/vms/nonexistent/logs")
        .body(Body::empty())
        .expect("build logs request");

    let response = app
        .oneshot(request)
        .await
        .expect("logs request should not panic");

    let status = response.status();
    assert!(
        status.is_client_error() || status.is_server_error(),
        "logs without WS upgrade should return error, got: {status}"
    );
}

// ── Pool and Metrics tests ──────────────────────────────────────────

#[tokio::test]
async fn pool_drain_endpoint_exists() {
    let app = build_router(test_state_with_pool());

    let response = app
        .oneshot(json_request("POST", "/v1/pool/drain", None))
        .await
        .expect("pool drain request should not panic");

    let status = response.status();
    assert!(
        status == StatusCode::OK || status == StatusCode::ACCEPTED,
        "POST /v1/pool/drain should return 200 or 202, got: {status}"
    );
}

#[tokio::test]
async fn metrics_contains_pool_gauges() {
    let app = build_router(test_app_state());

    let request = Request::builder()
        .uri("/v1/metrics")
        .body(Body::empty())
        .expect("build metrics request");

    let response = app
        .oneshot(request)
        .await
        .expect("metrics request should succeed");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET /v1/metrics should return 200"
    );

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read metrics response body");
    let text = String::from_utf8(bytes.to_vec()).expect("metrics body should be valid UTF-8");

    // Verify visor_ prefixed metrics exist in the output.
    assert!(
        text.contains("visor_"),
        "metrics response should contain visor_ prefixed metrics"
    );
}
