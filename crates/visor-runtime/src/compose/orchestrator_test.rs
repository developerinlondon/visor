use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use async_trait::async_trait;
use tokio::sync::{Mutex, RwLock};

use super::{Orchestrator, build_vm_config, dependency_sort, needs_health_wait};
use crate::api::routes::networks::NetworkManager;
use crate::backend::{ExecRequest, ExecResult, ExecutionBackend, VmConfig, VmInfo, VmState};
use crate::compose::{
    ComposeDependsOn, ComposeEnvironment, ComposeNetwork, ComposeProject, ComposeService,
    DependsOnCondition,
};
use crate::pool::health::VsockHealthPinger;
use std::time::Duration;

// ── Mock backend ────────────────────────────────────────────────

struct MockBackend {
    vms: RwLock<HashMap<String, VmInfo>>,
    create_order: Mutex<Vec<String>>,
    stopped_ids: Mutex<Vec<String>>,
    destroyed_ids: Mutex<Vec<String>>,
    next_id: AtomicU32,
}

impl MockBackend {
    fn new() -> Self {
        Self {
            vms: RwLock::new(HashMap::new()),
            create_order: Mutex::new(Vec::new()),
            stopped_ids: Mutex::new(Vec::new()),
            destroyed_ids: Mutex::new(Vec::new()),
            next_id: AtomicU32::new(1),
        }
    }
}

#[async_trait]
impl ExecutionBackend for MockBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        let id = format!("vm-{}", self.next_id.fetch_add(1, Ordering::Relaxed));
        if let Some(ref name) = config.name {
            self.create_order.lock().await.push(name.clone());
        }
        let mut info = VmInfo::new(
            id.clone(),
            config.image,
            VmState::Running,
            "2026-01-01T00:00:00Z".to_owned(),
            config.memory_mib,
            config.vcpus,
        );
        info.name = config.name;
        let cid = id
            .strip_prefix("vm-")
            .and_then(|n| n.parse::<u32>().ok())
            .unwrap_or(0)
            + 2;
        info.cid = Some(cid);
        info.ports = config.ports;
        self.vms.write().await.insert(id, info.clone());
        Ok(info)
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

    async fn exec(&self, id: &str, req: ExecRequest) -> anyhow::Result<ExecResult> {
        // Consume parameters to satisfy interface contract.
        let _vm_id = id;
        let _exec_req = req;
        Ok(ExecResult::new(0, String::new(), String::new()))
    }

    async fn stop(&self, id: &str, _timeout_secs: u64) -> anyhow::Result<()> {
        self.stopped_ids.lock().await.push(id.to_owned());
        if let Some(vm) = self.vms.write().await.get_mut(id) {
            vm.state = VmState::Stopped;
        }
        Ok(())
    }

    async fn kill(&self, id: &str) -> anyhow::Result<()> {
        self.stop(id, 0).await
    }

    async fn destroy(&self, id: &str) -> anyhow::Result<()> {
        self.destroyed_ids.lock().await.push(id.to_owned());
        self.vms.write().await.remove(id);
        Ok(())
    }

    async fn console_output(&self, _id: &str) -> anyhow::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

// ── Helpers ─────────────────────────────────────────────────────

fn make_service(image: &str, depends_on: Vec<&str>) -> ComposeService {
    make_service_with_ports(image, depends_on, vec![])
}

fn make_service_with_ports(
    image: &str,
    depends_on: Vec<&str>,
    ports: Vec<String>,
) -> ComposeService {
    let deps = if depends_on.is_empty() {
        ComposeDependsOn::Empty
    } else {
        ComposeDependsOn::Simple(depends_on.into_iter().map(String::from).collect())
    };

    ComposeService {
        image: image.to_owned(),
        command: None,
        environment: ComposeEnvironment::Empty,
        ports: ports
            .into_iter()
            .map(|p| crate::compose::types::ComposePort::Short(p))
            .collect(),
        volumes: Vec::new(),
        depends_on: deps,
        networks: Vec::new(),
        mem_limit: None,
        cpus: None,
        hostname: None,
        working_dir: None,
        labels: HashMap::new(),
    }
}

/// Always-healthy mock pinger for tests that don't need health logic.
struct AlwaysHealthyPinger;

#[async_trait]
impl VsockHealthPinger for AlwaysHealthyPinger {
    async fn ping(&self, _cid: u32, _timeout: Duration) -> anyhow::Result<()> {
        Ok(())
    }
}

/// Mock pinger that tracks ping attempts and only succeeds after N calls per CID.
struct CountdownPinger {
    /// Number of calls remaining before returning healthy, per CID.
    remaining: Mutex<HashMap<u32, u32>>,
}

impl CountdownPinger {
    fn new(calls_before_healthy: HashMap<u32, u32>) -> Self {
        Self {
            remaining: Mutex::new(calls_before_healthy),
        }
    }
}

#[async_trait]
impl VsockHealthPinger for CountdownPinger {
    async fn ping(&self, cid: u32, _timeout: Duration) -> anyhow::Result<()> {
        let mut map = self.remaining.lock().await;
        let count = map.entry(cid).or_insert(0);
        if *count > 0 {
            *count -= 1;
            anyhow::bail!("mock ping failed for CID {cid}, {} calls remaining", *count)
        }
        Ok(())
    }
}

fn make_orchestrator_with_mock() -> (Orchestrator, Arc<RwLock<NetworkManager>>, Arc<MockBackend>) {
    make_orchestrator_with_pinger(Arc::new(AlwaysHealthyPinger))
}

fn make_orchestrator_with_pinger(
    pinger: Arc<dyn VsockHealthPinger>,
) -> (Orchestrator, Arc<RwLock<NetworkManager>>, Arc<MockBackend>) {
    let mock = Arc::new(MockBackend::new());
    let networks = Arc::new(RwLock::new(NetworkManager::new()));
    let dns = Arc::new(RwLock::new(crate::net::dns::DnsRegistry::new()));
    let orch = Orchestrator::new(
        Arc::clone(&mock) as Arc<dyn ExecutionBackend>,
        Arc::clone(&networks),
        dns,
        pinger,
    )
    .unwrap();
    (orch, networks, mock)
}

fn make_service_with_health_dep(image: &str, dep_name: &str) -> ComposeService {
    let mut deps = HashMap::new();
    deps.insert(
        dep_name.to_owned(),
        DependsOnCondition {
            condition: Some("service_healthy".to_owned()),
        },
    );
    ComposeService {
        image: image.to_owned(),
        command: None,
        environment: ComposeEnvironment::Empty,
        ports: Vec::new(),
        volumes: Vec::new(),
        depends_on: ComposeDependsOn::Extended(deps),
        networks: Vec::new(),
        mem_limit: None,
        cpus: None,
        hostname: None,
        working_dir: None,
        labels: HashMap::new(),
    }
}

// ── dependency_sort tests ───────────────────────────────────────

#[test]
fn test_dependency_sort_empty() {
    let services = HashMap::new();
    let result = dependency_sort(&services).unwrap();
    assert!(result.is_empty());
}

#[test]
fn test_dependency_sort_single() {
    let services = HashMap::from([("a".to_owned(), make_service("img:latest", vec![]))]);
    let result = dependency_sort(&services).unwrap();
    assert_eq!(result, vec!["a"]);
}

#[test]
fn test_dependency_sort_linear() {
    let services = HashMap::from([
        ("a".to_owned(), make_service("img:latest", vec!["b"])),
        ("b".to_owned(), make_service("img:latest", vec!["c"])),
        ("c".to_owned(), make_service("img:latest", vec![])),
    ]);
    let result = dependency_sort(&services).unwrap();
    assert_eq!(result, vec!["c", "b", "a"]);
}

#[test]
fn test_dependency_sort_parallel() {
    let services = HashMap::from([
        ("a".to_owned(), make_service("img:latest", vec![])),
        ("b".to_owned(), make_service("img:latest", vec![])),
    ]);
    let result = dependency_sort(&services).unwrap();
    assert_eq!(result.len(), 2);
    assert!(result.contains(&"a".to_owned()));
    assert!(result.contains(&"b".to_owned()));
}

#[test]
fn test_dependency_sort_diamond() {
    let services = HashMap::from([
        ("a".to_owned(), make_service("img:latest", vec!["b", "c"])),
        ("b".to_owned(), make_service("img:latest", vec!["d"])),
        ("c".to_owned(), make_service("img:latest", vec!["d"])),
        ("d".to_owned(), make_service("img:latest", vec![])),
    ]);
    let result = dependency_sort(&services).unwrap();
    assert_eq!(result[0], "d", "D must come first");
    assert_eq!(result[result.len() - 1], "a", "A must come last");
}

#[test]
fn test_dependency_sort_cycle_detected() {
    let services = HashMap::from([
        ("a".to_owned(), make_service("img:latest", vec!["b"])),
        ("b".to_owned(), make_service("img:latest", vec!["a"])),
    ]);
    let result = dependency_sort(&services);
    assert!(result.is_err());
    let msg = result.unwrap_err().to_string();
    assert!(msg.contains("cycle"), "expected 'cycle' in error: {msg}");
}

// ── Orchestrator tests ──────────────────────────────────────────

#[tokio::test]
async fn test_compose_up_creates_project() {
    let (orch, _networks, _mock) = make_orchestrator_with_mock();

    let project = ComposeProject {
        name: Some("myapp".to_owned()),
        services: HashMap::from([("web".to_owned(), make_service("nginx:latest", vec![]))]),
        networks: HashMap::new(),
        volumes: HashMap::new(),
    };

    let instance = orch.up(&project).await.unwrap();
    assert_eq!(instance.name, "myapp");
    assert_eq!(instance.services.len(), 1);
    assert!(instance.services.contains_key("web"));

    let web = &instance.services["web"];
    assert_eq!(web.state, VmState::Running);
    assert_eq!(web.image, "nginx:latest");
}

#[tokio::test]
async fn test_compose_up_creates_networks() {
    let (orch, networks, _mock) = make_orchestrator_with_mock();

    let project = ComposeProject {
        name: Some("myapp".to_owned()),
        services: HashMap::from([("web".to_owned(), make_service("nginx:latest", vec![]))]),
        networks: HashMap::from([
            ("frontend".to_owned(), ComposeNetwork::default()),
            ("backend".to_owned(), ComposeNetwork::default()),
        ]),
        volumes: HashMap::new(),
    };

    let instance = orch.up(&project).await.unwrap();
    assert_eq!(instance.networks.len(), 2);

    // Verify networks were created in the manager with correct names.
    let mgr = networks.read().await;
    let all_nets = mgr.list();
    assert_eq!(all_nets.len(), 2);
    let names: Vec<&str> = all_nets.iter().map(|n| n.name.as_str()).collect();
    assert!(
        names.contains(&"myapp_frontend"),
        "expected 'myapp_frontend' in {names:?}"
    );
    assert!(
        names.contains(&"myapp_backend"),
        "expected 'myapp_backend' in {names:?}"
    );
}

#[tokio::test]
async fn test_compose_down_stops_all() {
    let (orch, _networks, mock) = make_orchestrator_with_mock();

    let project = ComposeProject {
        name: Some("myapp".to_owned()),
        services: HashMap::from([
            ("web".to_owned(), make_service("nginx:latest", vec![])),
            ("db".to_owned(), make_service("postgres:16", vec![])),
        ]),
        networks: HashMap::new(),
        volumes: HashMap::new(),
    };

    let instance = orch.up(&project).await.unwrap();
    orch.down(&instance).await.unwrap();

    let stopped = mock.stopped_ids.lock().await;
    assert_eq!(stopped.len(), 2, "expected 2 services stopped");

    let destroyed = mock.destroyed_ids.lock().await;
    assert_eq!(destroyed.len(), 2, "expected 2 services destroyed");
}

#[tokio::test]
async fn test_compose_ps_lists_services() {
    let (orch, _networks, _mock) = make_orchestrator_with_mock();

    let project = ComposeProject {
        name: Some("myapp".to_owned()),
        services: HashMap::from([
            ("web".to_owned(), make_service("nginx:latest", vec![])),
            ("db".to_owned(), make_service("postgres:16", vec![])),
        ]),
        networks: HashMap::new(),
        volumes: HashMap::new(),
    };

    let instance = orch.up(&project).await.unwrap();
    let statuses = orch.ps(&instance);

    assert_eq!(statuses.len(), 2);
    let names: Vec<&str> = statuses.iter().map(|s| s.name.as_str()).collect();
    assert!(names.contains(&"web"));
    assert!(names.contains(&"db"));

    for status in &statuses {
        assert_eq!(status.state, "running");
        assert!(status.vm_id.is_some());
    }
}

#[tokio::test]
async fn test_compose_up_respects_order() {
    let (orch, _networks, mock) = make_orchestrator_with_mock();

    let project = ComposeProject {
        name: Some("myapp".to_owned()),
        services: HashMap::from([
            ("web".to_owned(), make_service("nginx:latest", vec!["api"])),
            ("api".to_owned(), make_service("node:20", vec!["db"])),
            ("db".to_owned(), make_service("postgres:16", vec![])),
        ]),
        networks: HashMap::new(),
        volumes: HashMap::new(),
    };

    orch.up(&project).await.unwrap();

    let order = mock.create_order.lock().await;
    assert_eq!(order.len(), 3);
    assert_eq!(order[0], "db");
    assert_eq!(order[1], "api");
    assert_eq!(order[2], "web");
}

// ── build_vm_config port wiring tests ──────────────────────────────

#[test]
fn test_build_vm_config_with_simple_port() {
    let svc = make_service_with_ports("nginx:latest", vec![], vec!["8080:80".to_owned()]);
    let config = build_vm_config("web", &svc);

    assert_eq!(config.ports.len(), 1);
    assert_eq!(config.ports[0].host_port, 8080);
    assert_eq!(config.ports[0].guest_port, 80);
    assert_eq!(config.ports[0].protocol, "tcp");
}

#[test]
fn test_build_vm_config_with_multiple_ports() {
    let svc = make_service_with_ports(
        "nginx:latest",
        vec![],
        vec!["8080:80".to_owned(), "443:443".to_owned()],
    );
    let config = build_vm_config("web", &svc);

    assert_eq!(config.ports.len(), 2);
    assert_eq!(config.ports[0].host_port, 8080);
    assert_eq!(config.ports[0].guest_port, 80);
    assert_eq!(config.ports[1].host_port, 443);
    assert_eq!(config.ports[1].guest_port, 443);
}

#[test]
fn test_build_vm_config_with_port_and_protocol() {
    let svc = make_service_with_ports("nginx:latest", vec![], vec!["8080:80/tcp".to_owned()]);
    let config = build_vm_config("web", &svc);

    assert_eq!(config.ports.len(), 1);
    assert_eq!(config.ports[0].host_port, 8080);
    assert_eq!(config.ports[0].guest_port, 80);
}

#[test]
fn test_build_vm_config_with_empty_ports() {
    let svc = make_service_with_ports("nginx:latest", vec![], vec![]);
    let config = build_vm_config("web", &svc);

    assert_eq!(config.ports.len(), 0);
}

#[test]
fn test_build_vm_config_copies_declared_networks() {
    let mut svc = make_service_with_ports("nginx:latest", vec![], vec![]);
    svc.networks = vec!["frontend".to_owned(), "backend".to_owned()];

    let config = build_vm_config("web", &svc);

    assert_eq!(
        config.networks,
        vec!["frontend".to_owned(), "backend".to_owned()]
    );
}

#[test]
fn test_build_vm_config_skips_invalid_ports() {
    let svc = make_service_with_ports(
        "nginx:latest",
        vec![],
        vec![
            "8080:80".to_owned(),
            "invalid".to_owned(),
            "443:443".to_owned(),
        ],
    );
    let config = build_vm_config("web", &svc);

    // Invalid port should be skipped, valid ones should remain
    assert_eq!(config.ports.len(), 2);
    assert_eq!(config.ports[0].host_port, 8080);
    assert_eq!(config.ports[1].host_port, 443);
}

// ── needs_health_wait tests ─────────────────────────────────────

#[test]
fn needs_health_wait_true_for_service_healthy_condition() {
    let services = HashMap::from([
        ("db".to_owned(), make_service("postgres:16", vec![])),
        (
            "web".to_owned(),
            make_service_with_health_dep("nginx:latest", "db"),
        ),
    ]);
    assert!(
        needs_health_wait("db", &services),
        "db should need health wait because web depends on it with service_healthy"
    );
}

#[test]
fn needs_health_wait_false_for_simple_depends_on() {
    let services = HashMap::from([
        ("db".to_owned(), make_service("postgres:16", vec![])),
        ("web".to_owned(), make_service("nginx:latest", vec!["db"])),
    ]);
    assert!(
        !needs_health_wait("db", &services),
        "db should NOT need health wait for simple depends_on"
    );
}

#[test]
fn needs_health_wait_false_for_service_started_condition() {
    let mut deps = HashMap::new();
    deps.insert(
        "db".to_owned(),
        DependsOnCondition {
            condition: Some("service_started".to_owned()),
        },
    );
    let web = ComposeService {
        image: "nginx:latest".to_owned(),
        command: None,
        environment: ComposeEnvironment::Empty,
        ports: Vec::new(),
        volumes: Vec::new(),
        depends_on: ComposeDependsOn::Extended(deps),
        networks: Vec::new(),
        mem_limit: None,
        cpus: None,
        hostname: None,
        working_dir: None,
        labels: HashMap::new(),
    };
    let services = HashMap::from([
        ("db".to_owned(), make_service("postgres:16", vec![])),
        ("web".to_owned(), web),
    ]);
    assert!(
        !needs_health_wait("db", &services),
        "db should NOT need health wait for service_started condition"
    );
}

#[test]
fn needs_health_wait_false_for_none_condition() {
    let mut deps = HashMap::new();
    deps.insert("db".to_owned(), DependsOnCondition { condition: None });
    let web = ComposeService {
        image: "nginx:latest".to_owned(),
        command: None,
        environment: ComposeEnvironment::Empty,
        ports: Vec::new(),
        volumes: Vec::new(),
        depends_on: ComposeDependsOn::Extended(deps),
        networks: Vec::new(),
        mem_limit: None,
        cpus: None,
        hostname: None,
        working_dir: None,
        labels: HashMap::new(),
    };
    let services = HashMap::from([
        ("db".to_owned(), make_service("postgres:16", vec![])),
        ("web".to_owned(), web),
    ]);
    assert!(
        !needs_health_wait("db", &services),
        "db should NOT need health wait when condition is None"
    );
}

#[test]
fn needs_health_wait_false_for_unrelated_service() {
    let services = HashMap::from([
        ("db".to_owned(), make_service("postgres:16", vec![])),
        (
            "web".to_owned(),
            make_service_with_health_dep("nginx:latest", "db"),
        ),
    ]);
    assert!(
        !needs_health_wait("web", &services),
        "web itself should NOT need health wait (nobody depends on web with service_healthy)"
    );
}

// ── Health wait integration test ────────────────────────────────

#[tokio::test]
async fn compose_up_waits_for_health_before_starting_dependent() {
    // CID 3 assigned to first VM (db), needs 2 pings before healthy
    let pinger = Arc::new(CountdownPinger::new(HashMap::from([(3, 2)])));
    let (orch, _networks, mock) = make_orchestrator_with_pinger(pinger);

    let project = ComposeProject {
        name: Some("healthtest".to_owned()),
        services: HashMap::from([
            ("db".to_owned(), make_service("postgres:16", vec![])),
            (
                "web".to_owned(),
                make_service_with_health_dep("nginx:latest", "db"),
            ),
        ]),
        networks: HashMap::new(),
        volumes: HashMap::new(),
    };

    let instance = orch.up(&project).await.unwrap();
    assert_eq!(instance.services.len(), 2);

    // Verify db was created before web
    let order = mock.create_order.lock().await;
    assert_eq!(order[0], "db");
    assert_eq!(order[1], "web");
}

#[tokio::test]
async fn compose_up_no_health_wait_for_simple_depends_on() {
    let pinger = Arc::new(AlwaysHealthyPinger);
    let (orch, _networks, mock) = make_orchestrator_with_pinger(pinger);

    let project = ComposeProject {
        name: Some("simpletest".to_owned()),
        services: HashMap::from([
            ("db".to_owned(), make_service("postgres:16", vec![])),
            ("web".to_owned(), make_service("nginx:latest", vec!["db"])),
        ]),
        networks: HashMap::new(),
        volumes: HashMap::new(),
    };

    let instance = orch.up(&project).await.unwrap();
    assert_eq!(instance.services.len(), 2);

    let order = mock.create_order.lock().await;
    assert_eq!(order[0], "db");
    assert_eq!(order[1], "web");
}
