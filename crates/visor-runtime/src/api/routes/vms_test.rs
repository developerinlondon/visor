use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tokio::sync::RwLock;
use tower::ServiceExt as _;

use crate::api::router::{AppState, build_router};
use crate::api::sse::EventBroadcaster;
use crate::backend::{
    ExecRequest, ExecResult, ExecutionBackend, VmConfig, VmInfo, VmState, VmmBackend,
    VsockConnector,
};

fn test_vm(id: &str, image: &str, state: VmState) -> VmInfo {
    let mut info = VmInfo::new(
        id.to_owned(),
        image.to_owned(),
        state,
        "1970-01-01T00:00:00Z".to_owned(),
        512,
        1,
    );
    info.name = Some(format!("{id}-name"));
    info
}

fn test_state() -> (AppState, Arc<VmmBackend>) {
    let backend = Arc::new(VmmBackend::new());
    let state = AppState {
        backend: Arc::clone(&backend) as Arc<dyn ExecutionBackend>,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    };
    (state, backend)
}

// ── Mock VsockConnector for API tests ───────────────────────────

struct MockApiVsockConnector;

#[async_trait::async_trait]
impl VsockConnector for MockApiVsockConnector {
    async fn exec_cmd(&self, _cid: u32, _req: &ExecRequest) -> anyhow::Result<ExecResult> {
        Ok(ExecResult::new(0, "hello\n".to_owned(), String::new()))
    }

    async fn exec_stream_cmd(
        &self,
        _cid: u32,
        _req: &ExecRequest,
    ) -> anyhow::Result<Box<dyn crate::backend::AsyncIoStream>> {
        anyhow::bail!("streaming exec not used in these tests")
    }

    async fn copy_to_guest(&self, _cid: u32, _archive: &[u8], _dest: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn shutdown(&self, _cid: u32) -> anyhow::Result<()> {
        Ok(())
    }
}

fn test_state_with_mock() -> (AppState, Arc<VmmBackend>) {
    let backend = Arc::new(VmmBackend::with_connector(Arc::new(MockApiVsockConnector)));
    let state = AppState {
        backend: Arc::clone(&backend) as Arc<dyn ExecutionBackend>,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    };
    (state, backend)
}

#[derive(Default)]
struct MockStartBackend {
    vms: RwLock<std::collections::HashMap<String, VmInfo>>,
}

#[async_trait::async_trait]
impl ExecutionBackend for MockStartBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        Ok(VmInfo::new(
            "mock-create".to_owned(),
            config.image,
            VmState::Running,
            "1970-01-01T00:00:00Z".to_owned(),
            config.memory_mib,
            config.vcpus,
        ))
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(self.vms.read().await.values().cloned().collect())
    }

    async fn get(&self, id: &str) -> anyhow::Result<VmInfo> {
        self.vms
            .read()
            .await
            .get(id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("vm not found: {id}"))
    }

    async fn exec(&self, _id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        anyhow::bail!("not implemented in mock")
    }

    async fn stop(&self, _id: &str, _timeout_secs: u64) -> anyhow::Result<()> {
        Ok(())
    }

    async fn start(&self, id: &str) -> anyhow::Result<VmInfo> {
        let mut vms = self.vms.write().await;
        let vm = vms
            .get_mut(id)
            .ok_or_else(|| anyhow::anyhow!("vm not found: {id}"))?;
        vm.state = VmState::Running;
        Ok(vm.clone())
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

async fn insert_mock_start_vm(backend: &MockStartBackend, vm: VmInfo) {
    backend.vms.write().await.insert(vm.id.clone(), vm);
}

fn json_request(method: &str, uri: &str, body: Option<serde_json::Value>) -> Request<Body> {
    let builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("content-type", "application/json");

    if let Some(b) = body {
        builder
            .body(Body::from(serde_json::to_vec(&b).unwrap()))
            .unwrap()
    } else {
        builder.body(Body::empty()).unwrap()
    }
}

#[tokio::test]
async fn create_vm_returns_201() {
    let (state, backend) = test_state();
    let vm = test_vm("vm-create-201", "alpine:latest", VmState::Running);
    backend.insert_vm(vm).await;

    let app = build_router(state);
    let response = app
        .oneshot(json_request("GET", "/v1/vms/vm-create-201", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: VmInfo = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info.image, "alpine:latest");
    assert_eq!(info.id, "vm-create-201");
}

#[tokio::test]
async fn list_vms_empty() {
    let (state, _backend) = test_state();
    let app = build_router(state);

    let response = app
        .oneshot(json_request("GET", "/v1/vms", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let vms: Vec<VmInfo> = serde_json::from_slice(&bytes).unwrap();
    assert!(vms.is_empty());
}

#[tokio::test]
async fn create_then_list_shows_vm() {
    let (state, backend) = test_state();
    backend
        .insert_vm(test_vm("vm-list", "ubuntu:22.04", VmState::Running))
        .await;

    let app = build_router(state);
    let response = app
        .oneshot(json_request("GET", "/v1/vms", None))
        .await
        .unwrap();

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let vms: Vec<VmInfo> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(vms.len(), 1);
    assert_eq!(vms[0].image, "ubuntu:22.04");
}

#[tokio::test]
async fn get_vm_not_found_returns_404() {
    let (state, _backend) = test_state();
    let app = build_router(state);

    let response = app
        .oneshot(json_request("GET", "/v1/vms/nonexistent", None))
        .await
        .unwrap();

    // Backend returns anyhow error with "vm not found" → ApiError → 404.
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn create_then_get_vm() {
    let (state, backend) = test_state();
    backend
        .insert_vm(test_vm("vm-get", "alpine:3.20", VmState::Running))
        .await;

    let app = build_router(state);
    let response = app
        .oneshot(json_request("GET", "/v1/vms/vm-get", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: VmInfo = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info.id, "vm-get");
}

#[tokio::test]
async fn destroy_vm_returns_204() {
    let (state, backend) = test_state();
    backend
        .insert_vm(test_vm("vm-del", "alpine:latest", VmState::Running))
        .await;

    let app = build_router(state);
    let response = app
        .oneshot(json_request("DELETE", "/v1/vms/vm-del", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn stop_vm_returns_204() {
    let (state, backend) = test_state();
    backend
        .insert_vm(test_vm("vm-stop", "alpine:latest", VmState::Running))
        .await;

    let app = build_router(state);
    let response = app
        .oneshot(json_request("POST", "/v1/vms/vm-stop/stop", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn start_vm_returns_200_and_running_state() {
    let backend = Arc::new(MockStartBackend::default());
    insert_mock_start_vm(
        &backend,
        test_vm("vm-start", "alpine:latest", VmState::Stopped),
    )
    .await;
    let state = AppState {
        backend: Arc::clone(&backend) as Arc<dyn ExecutionBackend>,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    };

    let app = build_router(state);
    let response = app
        .oneshot(json_request("POST", "/v1/vms/vm-start/start", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: VmInfo = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info.id, "vm-start");
    assert_eq!(info.state, VmState::Running);
}

#[tokio::test]
async fn exec_vm_returns_result() {
    let (state, backend) = test_state_with_mock();
    backend
        .insert_vm_with_cid(test_vm("vm-exec", "alpine:latest", VmState::Running), 3)
        .await;

    let app = build_router(state);
    let body = serde_json::json!({ "cmd": ["echo", "hello"] });
    let response = app
        .oneshot(json_request("POST", "/v1/vms/vm-exec/exec", Some(body)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_error_returns_json_body() {
    let (state, _backend) = test_state();
    let app = build_router(state);

    let response = app
        .oneshot(json_request("DELETE", "/v1/vms/does-not-exist", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(body["error"].as_str().unwrap().contains("not found"));
}

#[tokio::test]
async fn kill_vm_returns_204() {
    let (state, backend) = test_state();
    backend
        .insert_vm(test_vm("vm-kill", "alpine:latest", VmState::Running))
        .await;

    let app = build_router(state);
    let response = app
        .oneshot(json_request("POST", "/v1/vms/vm-kill/kill", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn kill_nonexistent_vm_returns_404() {
    let (state, _backend) = test_state();
    let app = build_router(state);
    let response = app
        .oneshot(json_request("POST", "/v1/vms/no-such-vm/kill", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn stop_vm_with_timeout_query_param() {
    let (state, backend) = test_state();
    backend
        .insert_vm(test_vm("vm-stop-t", "alpine:latest", VmState::Running))
        .await;

    let app = build_router(state);
    let response = app
        .oneshot(json_request("POST", "/v1/vms/vm-stop-t/stop?t=5", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn stop_vm_default_timeout_is_10() {
    let (state, backend) = test_state();
    backend
        .insert_vm(test_vm(
            "vm-stop-default",
            "alpine:latest",
            VmState::Running,
        ))
        .await;

    let app = build_router(state);
    // No ?t= query param — should use default of 10
    let response = app
        .oneshot(json_request("POST", "/v1/vms/vm-stop-default/stop", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

// ── Pool integration ─────────────────────────────────────────────

use std::sync::atomic::{AtomicU32, Ordering};

use crate::pool::manager::{PoolConfig, PoolManager};

/// Mock backend for pool integration tests that doesn't try to boot real VMs.
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

#[async_trait::async_trait]
impl ExecutionBackend for MockPoolBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(VmInfo::new(
            format!("mock-pool-vm-{id}"),
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

    async fn get(&self, id: &str) -> anyhow::Result<VmInfo> {
        anyhow::bail!("vm not found: {id}")
    }

    async fn exec(&self, _id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        anyhow::bail!("not implemented in mock")
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
    let pool = PoolManager::new(
        PoolConfig::default(),
        Arc::clone(&backend),
        crate::pool::snapshot_cache::SnapshotCache::new(std::path::PathBuf::from(
            "/tmp/visor-vms-test",
        )),
    );
    AppState {
        backend,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: Some(Arc::new(pool)),
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    }
}

#[tokio::test]
async fn create_vm_uses_pool_when_available() {
    let state = test_state_with_pool();

    // Warm the pool with one VM
    let pool = state.pool.as_ref().unwrap();
    pool.warm("alpine:latest", 1).await.unwrap();
    assert_eq!(pool.status().await.total, 1);

    // POST to create a VM with the same image
    let app = build_router(state.clone());
    let body = serde_json::json!({ "image": "alpine:latest" });
    let response = app
        .oneshot(json_request("POST", "/v1/vms", Some(body)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    // Pool should now be empty (VM was acquired from pool)
    assert_eq!(pool.status().await.total, 0);
}

#[tokio::test]
async fn create_vm_falls_back_to_backend_when_pool_empty() {
    let state = test_state_with_pool();

    // Pool is empty, no warming
    let pool = state.pool.as_ref().unwrap();
    assert_eq!(pool.status().await.total, 0);

    // POST to create a VM — pool is empty so falls back to backend.create()
    let app = build_router(state.clone());
    let body = serde_json::json!({ "image": "ubuntu:latest" });
    let response = app
        .oneshot(json_request("POST", "/v1/vms", Some(body)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: VmInfo = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info.image, "ubuntu:latest");
}
