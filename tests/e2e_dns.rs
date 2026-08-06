//! E2E tests for DNS registry and compose orchestrator.
//!
//! DNS registry tests exercise name registration, resolution, and reverse
//! lookup directly (no server needed). Compose orchestrator tests use a mock
//! [`ExecutionBackend`] to verify project up/down/ps lifecycle.

use std::net::Ipv4Addr;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};

use visor_runtime::api::routes::networks::NetworkManager;
use visor_runtime::backend::{ExecRequest, ExecResult, ExecutionBackend, VmConfig, VmInfo};
use visor_runtime::compose::orchestrator::Orchestrator;
use visor_runtime::compose::types::ComposeProject;
use visor_runtime::net::dns::DnsRegistry;
use visor_runtime::pool::health::VsockHealthPinger;

/// Always-healthy stub pinger for compose tests.
struct StubPinger;

#[async_trait]
impl VsockHealthPinger for StubPinger {
    async fn ping(&self, _cid: u32, _timeout: std::time::Duration) -> anyhow::Result<()> {
        Ok(())
    }
}

// ── DNS Registry tests ──────────────────────────────────────────────

#[test]
fn dns_registry_register_and_resolve() {
    let mut dns = DnsRegistry::new();
    let ip = Ipv4Addr::new(10, 0, 0, 2);
    dns.register("web", ip);

    assert_eq!(
        dns.resolve("web"),
        Some(ip),
        "resolve should return the registered IP"
    );
}

#[test]
fn dns_registry_case_insensitive() {
    let mut dns = DnsRegistry::new();
    let ip = Ipv4Addr::new(10, 0, 0, 3);
    dns.register("Web", ip);

    assert_eq!(
        dns.resolve("web"),
        Some(ip),
        "lowercase lookup should match mixed-case registration"
    );
    assert_eq!(
        dns.resolve("WEB"),
        Some(ip),
        "uppercase lookup should match mixed-case registration"
    );
}

#[test]
fn dns_registry_unregister() {
    let mut dns = DnsRegistry::new();
    let ip = Ipv4Addr::new(10, 0, 0, 4);
    dns.register("db", ip);
    dns.unregister("db");

    assert_eq!(
        dns.resolve("db"),
        None,
        "resolve should return None after unregister"
    );
}

#[test]
fn dns_registry_reverse_lookup() {
    let mut dns = DnsRegistry::new();
    let ip = Ipv4Addr::new(10, 0, 0, 5);
    dns.register("cache", ip);

    assert_eq!(
        dns.reverse_lookup(ip),
        Some("cache"),
        "reverse lookup should return the registered name"
    );
}

#[test]
fn dns_registry_overwrite_updates_ip() {
    let mut dns = DnsRegistry::new();
    let old_ip = Ipv4Addr::new(10, 0, 0, 10);
    let new_ip = Ipv4Addr::new(10, 0, 0, 20);

    dns.register("api", old_ip);
    dns.register("api", new_ip);

    assert_eq!(
        dns.resolve("api"),
        Some(new_ip),
        "resolve should return the updated IP"
    );
    assert_eq!(
        dns.reverse_lookup(old_ip),
        None,
        "old IP reverse lookup should be gone after overwrite"
    );
    assert_eq!(
        dns.reverse_lookup(new_ip),
        Some("api"),
        "new IP reverse lookup should return the name"
    );
}

#[test]
fn dns_registry_count() {
    let mut dns = DnsRegistry::new();
    dns.register("a", Ipv4Addr::new(10, 0, 0, 1));
    dns.register("b", Ipv4Addr::new(10, 0, 0, 2));
    dns.register("c", Ipv4Addr::new(10, 0, 0, 3));

    assert_eq!(dns.count(), 3, "count should be 3 after 3 registrations");

    dns.unregister("b");
    assert_eq!(dns.count(), 2, "count should be 2 after 1 unregister");
}

#[test]
fn dns_registry_all_entries() {
    let mut dns = DnsRegistry::new();
    let ip_a = Ipv4Addr::new(10, 0, 0, 1);
    let ip_b = Ipv4Addr::new(10, 0, 0, 2);
    dns.register("alpha", ip_a);
    dns.register("beta", ip_b);

    let mut entries = dns.all_entries();
    entries.sort_by_key(|(name, _)| name.to_owned());

    assert_eq!(entries.len(), 2, "should have 2 entries");
    assert_eq!(entries[0], ("alpha", ip_a));
    assert_eq!(entries[1], ("beta", ip_b));
}

// ── Mock compose backend ────────────────────────────────────────────

/// A mock execution backend that tracks created/stopped/destroyed VMs
/// for compose orchestrator testing.
struct MockComposeBackend {
    created_vms: Mutex<Vec<VmInfo>>,
    destroyed_ids: Mutex<Vec<String>>,
    next_id: std::sync::atomic::AtomicU32,
}

impl MockComposeBackend {
    fn new() -> Self {
        Self {
            created_vms: Mutex::new(Vec::new()),
            destroyed_ids: Mutex::new(Vec::new()),
            next_id: std::sync::atomic::AtomicU32::new(1),
        }
    }
}

#[async_trait]
impl ExecutionBackend for MockComposeBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        let seq = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let info: VmInfo = serde_json::from_value(serde_json::json!({
            "id": format!("compose-vm-{seq}"),
            "name": config.name,
            "image": config.image,
            "state": "running",
            "created_at": "2026-01-01T00:00:00Z",
            "memory_mib": config.memory_mib,
            "vcpus": config.vcpus,
            "ports": []
        }))
        .expect("mock VmInfo should deserialize");
        self.created_vms.lock().await.push(info.clone());
        Ok(info)
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(self.created_vms.lock().await.clone())
    }

    async fn get(&self, id: &str) -> anyhow::Result<VmInfo> {
        self.created_vms
            .lock()
            .await
            .iter()
            .find(|vm| vm.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("vm not found: {id}"))
    }

    async fn exec(&self, _id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        Ok(serde_json::from_value(serde_json::json!({
            "exit_code": 0,
            "stdout": "",
            "stderr": ""
        }))
        .expect("mock ExecResult should deserialize"))
    }

    async fn stop(&self, _id: &str, _timeout_secs: u64) -> anyhow::Result<()> {
        Ok(())
    }

    async fn kill(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn destroy(&self, id: &str) -> anyhow::Result<()> {
        self.destroyed_ids.lock().await.push(id.to_owned());
        Ok(())
    }

    async fn console_output(&self, _id: &str) -> anyhow::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

// ── Compose helpers ─────────────────────────────────────────────────

/// Build an [`Orchestrator`] backed by a [`MockComposeBackend`].
fn make_compose_orchestrator() -> (Orchestrator, Arc<MockComposeBackend>) {
    let mock = Arc::new(MockComposeBackend::new());
    let networks = Arc::new(RwLock::new(NetworkManager::new()));
    let dns = Arc::new(RwLock::new(DnsRegistry::new()));
    let pinger: Arc<dyn VsockHealthPinger> = Arc::new(StubPinger);
    let orch = Orchestrator::new(
        Arc::clone(&mock) as Arc<dyn ExecutionBackend>,
        networks,
        dns,
        pinger,
    )
    .expect("Orchestrator::new should succeed");
    (orch, mock)
}

/// Build a minimal [`ComposeProject`] via JSON deserialization
/// (bypasses `#[non_exhaustive]`).
fn make_project(name: &str, services: &serde_json::Value) -> ComposeProject {
    serde_json::from_value(serde_json::json!({
        "name": name,
        "services": services
    }))
    .expect("ComposeProject should deserialize from JSON")
}

// ── Compose orchestrator tests ──────────────────────────────────────

#[tokio::test]
async fn compose_orchestrator_creates_successfully() {
    let (orch, _mock) = make_compose_orchestrator();
    // If we got here, Orchestrator::new() succeeded.
    // Verify by running up() with a single-service project, then ps().
    let project = make_project(
        "empty-check",
        &serde_json::json!({
            "svc": { "image": "alpine:latest" }
        }),
    );
    let instance = orch
        .up(&project)
        .await
        .expect("orchestrator up should succeed after new()");
    let statuses = orch.ps(&instance);
    assert_eq!(statuses.len(), 1, "should have 1 service after up()");
}

#[tokio::test]
async fn compose_up_single_service() {
    let (orch, _mock) = make_compose_orchestrator();
    let project = make_project(
        "test-project",
        &serde_json::json!({
            "web": { "image": "nginx:latest" }
        }),
    );

    let instance = orch.up(&project).await.expect("compose up should succeed");

    assert_eq!(instance.name, "test-project");
    assert_eq!(instance.services.len(), 1, "should have 1 service");
    assert!(
        instance.services.contains_key("web"),
        "should contain 'web' service"
    );

    let web = &instance.services["web"];
    assert_eq!(web.image, "nginx:latest");
}

#[tokio::test]
async fn compose_down_stops_and_destroys() {
    let (orch, mock) = make_compose_orchestrator();
    let project = make_project(
        "teardown-test",
        &serde_json::json!({
            "api": { "image": "node:20" },
            "db": { "image": "postgres:16" }
        }),
    );

    let instance = orch.up(&project).await.expect("compose up should succeed");
    assert_eq!(instance.services.len(), 2);

    orch.down(&instance)
        .await
        .expect("compose down should succeed");

    let destroyed = mock.destroyed_ids.lock().await;
    assert_eq!(
        destroyed.len(),
        2,
        "destroy should have been called for each service"
    );
}

#[tokio::test]
async fn compose_ps_lists_services() {
    let (orch, _mock) = make_compose_orchestrator();
    let project = make_project(
        "ps-test",
        &serde_json::json!({
            "web": { "image": "nginx:latest" },
            "worker": { "image": "python:3.12" }
        }),
    );

    let instance = orch.up(&project).await.expect("compose up should succeed");
    let statuses = orch.ps(&instance);

    assert_eq!(statuses.len(), 2, "ps should list 2 services");

    let names: Vec<&str> = statuses.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"web"), "ps should list 'web'");
    assert!(names.contains(&"worker"), "ps should list 'worker'");

    for status in &statuses {
        assert_eq!(status.state, "running", "all services should be running");
        assert!(status.vm_id.is_some(), "all services should have a vm_id");
    }
}
