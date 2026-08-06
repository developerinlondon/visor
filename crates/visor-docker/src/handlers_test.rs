use super::*;
use std::net::Ipv4Addr;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use visor_types::{
    BuildOutput, BuildProgress, BuildRequest, BuildService, ExecRequest, ExecResult,
    ExecutionBackend, GuestNetworkLink, ImageInfo, ImageManager, PortMapping, VmConfig, VmInfo,
    VmState,
};

use crate::docker_router;

// ── Mock Backend ────────────────────────────────────────────────────

#[derive(Debug)]
struct MockBackend {
    vms: tokio::sync::Mutex<Vec<VmInfo>>,
    created_configs: tokio::sync::Mutex<Vec<VmConfig>>,
    exec_requests: tokio::sync::Mutex<Vec<(String, ExecRequest)>>,
    copied_archives: tokio::sync::Mutex<Vec<(String, String, Vec<u8>)>>,
    next_cid: std::sync::atomic::AtomicU32,
}

#[derive(Debug, Default)]
struct MockImageManager {
    images: tokio::sync::Mutex<std::collections::HashMap<String, ImageInfo>>,
}

impl MockBackend {
    fn with_vm(vm: VmInfo) -> Self {
        Self {
            vms: tokio::sync::Mutex::new(vec![vm]),
            created_configs: tokio::sync::Mutex::new(Vec::new()),
            exec_requests: tokio::sync::Mutex::new(Vec::new()),
            copied_archives: tokio::sync::Mutex::new(Vec::new()),
            next_cid: std::sync::atomic::AtomicU32::new(3),
        }
    }
}

impl Default for MockBackend {
    fn default() -> Self {
        Self {
            vms: tokio::sync::Mutex::new(Vec::new()),
            created_configs: tokio::sync::Mutex::new(Vec::new()),
            exec_requests: tokio::sync::Mutex::new(Vec::new()),
            copied_archives: tokio::sync::Mutex::new(Vec::new()),
            next_cid: std::sync::atomic::AtomicU32::new(3),
        }
    }
}

#[derive(Debug, Default)]
struct MockServiceDiscovery {
    registrations: std::sync::Mutex<Vec<(String, Ipv4Addr)>>,
    unregistrations: std::sync::Mutex<Vec<String>>,
    snapshots: std::sync::Mutex<Vec<(String, Ipv4Addr)>>,
}

#[async_trait]
impl crate::ServiceDiscovery for MockServiceDiscovery {
    async fn register_name(&self, name: &str, ip: Ipv4Addr) {
        self.registrations
            .lock()
            .unwrap()
            .push((name.to_owned(), ip));
    }

    async fn unregister_name(&self, name: &str) {
        self.unregistrations.lock().unwrap().push(name.to_owned());
    }

    async fn snapshot_names(&self) -> Vec<(String, Ipv4Addr)> {
        self.snapshots.lock().unwrap().clone()
    }
}

#[async_trait]
impl ImageManager for MockImageManager {
    async fn list_images(&self) -> anyhow::Result<Vec<ImageInfo>> {
        Ok(self.images.lock().await.values().cloned().collect())
    }

    async fn pull_image(&self, reference: &str) -> anyhow::Result<ImageInfo> {
        let image = ImageInfo::new(
            format!("sha256:{}", reference.replace(['/', ':'], "-")),
            vec![reference.to_owned()],
        );
        self.images
            .lock()
            .await
            .insert(reference.to_owned(), image.clone());
        Ok(image)
    }

    async fn inspect_image(&self, reference: &str) -> anyhow::Result<ImageInfo> {
        self.images
            .lock()
            .await
            .get(reference)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("not found"))
    }

    async fn remove_image(&self, reference: &str) -> anyhow::Result<()> {
        let removed = self.images.lock().await.remove(reference);
        anyhow::ensure!(removed.is_some(), "not found");
        Ok(())
    }
}

#[async_trait]
impl ExecutionBackend for MockBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        self.created_configs.lock().await.push(config.clone());
        let mut vm = VmInfo::new(
            uuid::Uuid::new_v4().to_string(),
            config.image.clone(),
            VmState::Running,
            "2024-01-01T00:00:00Z".to_owned(),
            config.memory_mib,
            config.vcpus,
        );
        vm.name = config.name.clone();
        vm.ports = config.ports.clone();
        vm.cid = Some(
            self.next_cid
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst),
        );
        self.vms.lock().await.push(vm.clone());
        Ok(vm)
    }

    async fn create_from_snapshot(
        &self,
        config: VmConfig,
        _snapshot_dir: &Path,
    ) -> anyhow::Result<VmInfo> {
        self.create(config).await
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(self.vms.lock().await.clone())
    }

    async fn get(&self, id: &str) -> anyhow::Result<VmInfo> {
        self.vms
            .lock()
            .await
            .iter()
            .find(|v| v.id == id)
            .cloned()
            .ok_or_else(|| anyhow::anyhow!("not found"))
    }

    async fn exec(&self, id: &str, req: ExecRequest) -> anyhow::Result<ExecResult> {
        self.exec_requests
            .lock()
            .await
            .push((id.to_owned(), req.clone()));
        Ok(ExecResult::new(
            0,
            format!("ran: {}", req.cmd.join(" ")),
            String::new(),
        ))
    }

    async fn copy_to_guest(&self, id: &str, archive: Vec<u8>, dest: &str) -> anyhow::Result<()> {
        self.copied_archives
            .lock()
            .await
            .push((id.to_owned(), dest.to_owned(), archive));
        Ok(())
    }

    async fn stop(&self, id: &str, _timeout: u64) -> anyhow::Result<()> {
        let mut vms = self.vms.lock().await;
        let vm = vms
            .iter_mut()
            .find(|v| v.id == id)
            .ok_or_else(|| anyhow::anyhow!("not found"))?;
        vm.state = VmState::Stopped;
        vm.exit_code = Some(0);
        Ok(())
    }

    async fn kill(&self, id: &str) -> anyhow::Result<()> {
        let mut vms = self.vms.lock().await;
        let vm = vms
            .iter_mut()
            .find(|v| v.id == id)
            .ok_or_else(|| anyhow::anyhow!("not found"))?;
        vm.state = VmState::Stopped;
        vm.exit_code = Some(137);
        Ok(())
    }

    async fn destroy(&self, id: &str) -> anyhow::Result<()> {
        let mut vms = self.vms.lock().await;
        let idx = vms
            .iter()
            .position(|v| v.id == id)
            .ok_or_else(|| anyhow::anyhow!("not found"))?;
        vms.remove(idx);
        Ok(())
    }

    async fn console_output(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let vms = self.vms.lock().await;
        let vm = vms
            .iter()
            .find(|vm| vm.id == id)
            .ok_or_else(|| anyhow::anyhow!("not found"))?;
        anyhow::ensure!(
            vm.state == VmState::Running,
            "console unavailable for non-running vm"
        );
        Ok(b"hello world\n".to_vec())
    }
}

// ── Helper ──────────────────────────────────────────────────────────

fn test_router_with(backend: Arc<dyn ExecutionBackend>) -> axum::Router {
    docker_router(backend, None, None)
}

fn test_router_with_image_manager(
    backend: Arc<dyn ExecutionBackend>,
    image_manager: Arc<dyn ImageManager>,
) -> axum::Router {
    crate::docker_router_with_image_manager(backend, None, None, Some(image_manager))
}

fn test_router_with_service_discovery(
    backend: Arc<dyn ExecutionBackend>,
    service_discovery: Arc<dyn crate::ServiceDiscovery>,
) -> axum::Router {
    crate::docker_router_with_service_discovery(backend, None, None, None, Some(service_discovery))
}

fn make_running_vm(id: &str) -> VmInfo {
    let mut vm = VmInfo::new(
        id.to_owned(),
        "alpine:latest".to_owned(),
        VmState::Running,
        "2024-01-01T00:00:00Z".to_owned(),
        512,
        1,
    );
    vm.name = Some("test-container".to_owned());
    vm.ports = vec![PortMapping::new(8080, 80)];
    vm
}

fn make_stopped_vm(id: &str, stdout: &str) -> VmInfo {
    let mut vm = make_running_vm(id);
    vm.state = VmState::Stopped;
    vm.exit_code = Some(0);
    vm.stdout = Some(stdout.to_owned());
    vm
}

fn expected_log_frame(stream_type: u8, payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(payload.len()).unwrap();
    let size = len.to_be_bytes();
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&[stream_type, 0, 0, 0, size[0], size[1], size[2], size[3]]);
    frame.extend_from_slice(payload);
    frame
}

async fn create_container(app: axum::Router, uri: &str, body: serde_json::Value) -> String {
    let resp = app
        .oneshot(
            Request::post(uri)
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    json["Id"].as_str().unwrap().to_owned()
}

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn ping_returns_ok_with_headers() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(Request::get("/_ping").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("Api-Version").unwrap(),
        crate::API_VERSION
    );
    assert_eq!(resp.headers().get("OSType").unwrap(), "linux");

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], b"OK");
}

#[tokio::test]
async fn version_returns_valid_json() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(Request::get("/version").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ApiVersion"], crate::API_VERSION);
    assert!(json["Version"].as_str().is_some());
}

#[tokio::test]
async fn versioned_version_path_works() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(Request::get("/v1.45/version").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["ApiVersion"], crate::API_VERSION);
}

#[tokio::test]
async fn info_returns_container_counts() {
    let vm = make_running_vm("vm-1");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(Request::get("/info").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["Containers"], 1);
    assert_eq!(json["ContainersRunning"], 1);
    assert_eq!(json["ContainersStopped"], 0);
}

#[tokio::test]
async fn container_create_returns_201_with_id() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let body = serde_json::json!({
        "Image": "alpine:latest",
        "Cmd": ["echo", "hello"]
    });

    let resp = app
        .oneshot(
            Request::post("/containers/create")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["Id"].as_str().is_some());
    assert!(json["Warnings"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn container_create_with_name_query() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let body = serde_json::json!({"Image": "nginx"});

    let resp = app
        .oneshot(
            Request::post("/containers/create?name=web")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn container_create_defers_backend_boot_until_start() {
    let backend = Arc::new(MockBackend::default());
    let app = test_router_with(Arc::clone(&backend) as Arc<dyn ExecutionBackend>);

    let id = create_container(
        app.clone(),
        "/containers/create?name=deferred-start",
        serde_json::json!({
            "Image": "alpine:latest",
            "Cmd": ["echo", "hello"]
        }),
    )
    .await;

    assert!(backend.created_configs.lock().await.is_empty());

    let inspect_before_start = app
        .clone()
        .oneshot(
            Request::get(format!("/containers/{id}/json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(inspect_before_start.status(), StatusCode::OK);
    let inspect_body = axum::body::to_bytes(inspect_before_start.into_body(), usize::MAX)
        .await
        .unwrap();
    let inspect_json: serde_json::Value = serde_json::from_slice(&inspect_body).unwrap();
    assert_eq!(inspect_json["State"]["Status"], "created");

    let start_resp = app
        .oneshot(
            Request::post(format!("/containers/{id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start_resp.status(), StatusCode::NO_CONTENT);

    let created_configs = backend.created_configs.lock().await;
    assert_eq!(created_configs.len(), 1);
    assert_eq!(created_configs[0].name.as_deref(), Some("deferred-start"));
}

#[tokio::test]
async fn container_create_registers_service_names_with_guest_ip() {
    let backend = Arc::new(MockBackend::default());
    let service_discovery = Arc::new(MockServiceDiscovery::default());
    let app = test_router_with_service_discovery(
        Arc::clone(&backend) as Arc<dyn ExecutionBackend>,
        Arc::clone(&service_discovery) as Arc<dyn crate::ServiceDiscovery>,
    );

    let body = serde_json::json!({
        "Image": "alpine:latest",
        "Hostname": "api-host",
        "ExposedPorts": {"8080/tcp": {}},
        "NetworkingConfig": {
            "EndpointsConfig": {
                "visor-compose_default": {
                    "Aliases": ["api", "backend"]
                }
            }
        }
    });

    let id = create_container(
        app.clone(),
        "/containers/create?name=visor-compose-api-1",
        body,
    )
    .await;
    let start_resp = app
        .oneshot(
            Request::post(format!("/containers/{id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start_resp.status(), StatusCode::NO_CONTENT);
    let registrations = service_discovery.registrations.lock().unwrap().clone();

    let guest_ip = GuestNetworkLink::for_named_network("visor-compose_default", 3).guest_ip;
    assert!(registrations.contains(&("api".to_owned(), guest_ip)));
    assert!(registrations.contains(&("backend".to_owned(), guest_ip)));
    assert!(registrations.contains(&("api-host".to_owned(), guest_ip)));
}

#[tokio::test]
async fn container_stop_unregisters_service_names() {
    let backend = Arc::new(MockBackend::default());
    let service_discovery = Arc::new(MockServiceDiscovery::default());
    let app = test_router_with_service_discovery(
        Arc::clone(&backend) as Arc<dyn ExecutionBackend>,
        Arc::clone(&service_discovery) as Arc<dyn crate::ServiceDiscovery>,
    );

    let body = serde_json::json!({
        "Image": "alpine:latest",
        "ExposedPorts": {"8080/tcp": {}},
        "NetworkingConfig": {
            "EndpointsConfig": {
                "visor-compose_default": {
                    "Aliases": ["api"]
                }
            }
        }
    });

    let id = create_container(
        app.clone(),
        "/containers/create?name=visor-compose-api-1",
        body,
    )
    .await;
    let start_resp = app
        .clone()
        .oneshot(
            Request::post(format!("/containers/{id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start_resp.status(), StatusCode::NO_CONTENT);
    let resp = app
        .oneshot(
            Request::post(format!("/containers/{id}/stop"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let unregistrations = service_discovery.unregistrations.lock().unwrap().clone();
    assert!(unregistrations.contains(&"api".to_owned()));
}

#[tokio::test]
async fn compose_container_start_registers_project_qualified_service_names() {
    let backend = Arc::new(MockBackend::default());
    let service_discovery = Arc::new(MockServiceDiscovery::default());
    let app = test_router_with_service_discovery(
        Arc::clone(&backend) as Arc<dyn ExecutionBackend>,
        Arc::clone(&service_discovery) as Arc<dyn crate::ServiceDiscovery>,
    );

    let body = serde_json::json!({
        "Image": "alpine:latest",
        "Hostname": "api-host",
        "Labels": {
            "com.docker.compose.project": "alpha",
            "com.docker.compose.service": "api"
        },
        "NetworkingConfig": {
            "EndpointsConfig": {
                "alpha_default": {
                    "Aliases": ["api", "backend"]
                }
            }
        }
    });

    let id = create_container(app.clone(), "/containers/create?name=alpha-api-1", body).await;
    let start_resp = app
        .oneshot(
            Request::post(format!("/containers/{id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start_resp.status(), StatusCode::NO_CONTENT);

    let registrations = service_discovery.registrations.lock().unwrap().clone();
    let guest_ip = GuestNetworkLink::for_named_network("alpha_default", 3).guest_ip;
    assert!(registrations.contains(&("api.alpha".to_owned(), guest_ip)));
    assert!(registrations.contains(&("backend.alpha".to_owned(), guest_ip)));
    assert!(registrations.contains(&("api-host.alpha".to_owned(), guest_ip)));
    assert!(
        !registrations.iter().any(|(name, _)| name == "api"),
        "compose-managed services should not publish bare global aliases"
    );
}

#[tokio::test]
async fn compose_container_stop_unregisters_project_qualified_service_names() {
    let backend = Arc::new(MockBackend::default());
    let service_discovery = Arc::new(MockServiceDiscovery::default());
    let app = test_router_with_service_discovery(
        Arc::clone(&backend) as Arc<dyn ExecutionBackend>,
        Arc::clone(&service_discovery) as Arc<dyn crate::ServiceDiscovery>,
    );

    let body = serde_json::json!({
        "Image": "alpine:latest",
        "Labels": {
            "com.docker.compose.project": "alpha",
            "com.docker.compose.service": "api"
        },
        "NetworkingConfig": {
            "EndpointsConfig": {
                "alpha_default": {
                    "Aliases": ["api"]
                }
            }
        }
    });

    let id = create_container(app.clone(), "/containers/create?name=alpha-api-1", body).await;
    let start_resp = app
        .clone()
        .oneshot(
            Request::post(format!("/containers/{id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start_resp.status(), StatusCode::NO_CONTENT);

    let stop_resp = app
        .oneshot(
            Request::post(format!("/containers/{id}/stop"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(stop_resp.status(), StatusCode::NO_CONTENT);

    let unregistrations = service_discovery.unregistrations.lock().unwrap().clone();
    assert!(unregistrations.contains(&"api.alpha".to_owned()));
    assert!(
        !unregistrations.iter().any(|name| name == "api"),
        "compose-managed services should not unregister bare global aliases"
    );
}

#[tokio::test]
async fn container_create_snapshots_service_names_into_extra_hosts() {
    let backend = Arc::new(MockBackend::default());
    let service_discovery = Arc::new(MockServiceDiscovery::default());
    service_discovery
        .snapshots
        .lock()
        .unwrap()
        .push(("api".to_owned(), Ipv4Addr::new(172, 20, 0, 2)));
    let app = test_router_with_service_discovery(
        Arc::clone(&backend) as Arc<dyn ExecutionBackend>,
        Arc::clone(&service_discovery) as Arc<dyn crate::ServiceDiscovery>,
    );

    let body = serde_json::json!({
        "Image": "alpine:latest",
        "NetworkingConfig": {
            "EndpointsConfig": {
                "visor-compose_default": {
                    "Aliases": ["probe"]
                }
            }
        }
    });

    let id = create_container(
        app.clone(),
        "/containers/create?name=visor-compose-probe-1",
        body,
    )
    .await;
    let start_resp = app
        .oneshot(
            Request::post(format!("/containers/{id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start_resp.status(), StatusCode::NO_CONTENT);
    let configs = backend.created_configs.lock().await;
    let config = configs.last().expect("container config should be recorded");

    assert_eq!(
        config.extra_hosts,
        vec![visor_types::HostEntry::new("api", "172.20.0.2")]
    );
}

#[tokio::test]
async fn compose_container_start_scopes_peer_hosts_to_the_same_project() {
    let backend = Arc::new(MockBackend::default());
    let app = test_router_with(Arc::clone(&backend) as Arc<dyn ExecutionBackend>);

    let alpha_api = serde_json::json!({
        "Image": "alpine:latest",
        "Labels": {
            "com.docker.compose.project": "alpha",
            "com.docker.compose.service": "api"
        },
        "NetworkingConfig": {
            "EndpointsConfig": {
                "alpha_default": {
                    "Aliases": ["api"]
                }
            }
        }
    });
    let alpha_api_id = create_container(
        app.clone(),
        "/containers/create?name=alpha-api-1",
        alpha_api,
    )
    .await;
    let alpha_api_start = app
        .clone()
        .oneshot(
            Request::post(format!("/containers/{alpha_api_id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(alpha_api_start.status(), StatusCode::NO_CONTENT);

    let beta_api = serde_json::json!({
        "Image": "alpine:latest",
        "Labels": {
            "com.docker.compose.project": "beta",
            "com.docker.compose.service": "api"
        },
        "NetworkingConfig": {
            "EndpointsConfig": {
                "beta_default": {
                    "Aliases": ["api"]
                }
            }
        }
    });
    let beta_api_id =
        create_container(app.clone(), "/containers/create?name=beta-api-1", beta_api).await;
    let beta_api_start = app
        .clone()
        .oneshot(
            Request::post(format!("/containers/{beta_api_id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(beta_api_start.status(), StatusCode::NO_CONTENT);

    let alpha_probe = serde_json::json!({
        "Image": "alpine:latest",
        "Labels": {
            "com.docker.compose.project": "alpha",
            "com.docker.compose.service": "probe"
        },
        "NetworkingConfig": {
            "EndpointsConfig": {
                "alpha_default": {
                    "Aliases": ["probe"]
                }
            }
        }
    });
    let alpha_probe_id = create_container(
        app.clone(),
        "/containers/create?name=alpha-probe-1",
        alpha_probe,
    )
    .await;
    let alpha_probe_start = app
        .oneshot(
            Request::post(format!("/containers/{alpha_probe_id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(alpha_probe_start.status(), StatusCode::NO_CONTENT);

    let configs = backend.created_configs.lock().await;
    let config = configs.last().expect("container config should be recorded");
    let alpha_guest_ip = GuestNetworkLink::for_named_network("alpha_default", 3)
        .guest_ip
        .to_string();
    let beta_guest_ip = GuestNetworkLink::for_named_network("beta_default", 4)
        .guest_ip
        .to_string();

    assert!(
        config
            .extra_hosts
            .contains(&visor_types::HostEntry::new("api", alpha_guest_ip.clone())),
        "probe should resolve the alpha api by short name"
    );
    assert!(
        config
            .extra_hosts
            .contains(&visor_types::HostEntry::new("api.alpha", alpha_guest_ip)),
        "probe should also resolve the alpha api by project-qualified name"
    );
    assert!(
        !config.extra_hosts.contains(&visor_types::HostEntry::new(
            "api.beta",
            beta_guest_ip.clone()
        )),
        "probe should not see beta project aliases"
    );
    assert!(
        !config
            .extra_hosts
            .contains(&visor_types::HostEntry::new("api", beta_guest_ip)),
        "probe should not receive a conflicting api short name from beta"
    );
}

#[tokio::test]
async fn compose_container_start_scopes_peer_hosts_to_shared_networks() {
    let backend = Arc::new(MockBackend::default());
    let app = test_router_with(Arc::clone(&backend) as Arc<dyn ExecutionBackend>);

    let frontend_api = serde_json::json!({
        "Image": "alpine:latest",
        "Labels": {
            "com.docker.compose.project": "alpha",
            "com.docker.compose.service": "api"
        },
        "NetworkingConfig": {
            "EndpointsConfig": {
                "alpha_frontend": {
                    "Aliases": ["api"]
                }
            }
        }
    });
    let frontend_api_id = create_container(
        app.clone(),
        "/containers/create?name=alpha-api-1",
        frontend_api,
    )
    .await;
    let frontend_api_start = app
        .clone()
        .oneshot(
            Request::post(format!("/containers/{frontend_api_id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(frontend_api_start.status(), StatusCode::NO_CONTENT);

    let backend_db = serde_json::json!({
        "Image": "alpine:latest",
        "Labels": {
            "com.docker.compose.project": "alpha",
            "com.docker.compose.service": "db"
        },
        "NetworkingConfig": {
            "EndpointsConfig": {
                "alpha_backend": {
                    "Aliases": ["db"]
                }
            }
        }
    });
    let backend_db_id = create_container(
        app.clone(),
        "/containers/create?name=alpha-db-1",
        backend_db,
    )
    .await;
    let backend_db_start = app
        .clone()
        .oneshot(
            Request::post(format!("/containers/{backend_db_id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(backend_db_start.status(), StatusCode::NO_CONTENT);

    let bridge = serde_json::json!({
        "Image": "alpine:latest",
        "Labels": {
            "com.docker.compose.project": "alpha",
            "com.docker.compose.service": "bridge"
        },
        "NetworkingConfig": {
            "EndpointsConfig": {
                "alpha_frontend": {
                    "Aliases": ["bridge"]
                },
                "alpha_backend": {
                    "Aliases": ["bridge"]
                }
            }
        }
    });
    let bridge_id = create_container(
        app.clone(),
        "/containers/create?name=alpha-bridge-1",
        bridge,
    )
    .await;
    let bridge_start = app
        .clone()
        .oneshot(
            Request::post(format!("/containers/{bridge_id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(bridge_start.status(), StatusCode::NO_CONTENT);

    let frontend_probe = serde_json::json!({
        "Image": "alpine:latest",
        "Labels": {
            "com.docker.compose.project": "alpha",
            "com.docker.compose.service": "probe"
        },
        "NetworkingConfig": {
            "EndpointsConfig": {
                "alpha_frontend": {
                    "Aliases": ["probe"]
                }
            }
        }
    });
    let frontend_probe_id = create_container(
        app.clone(),
        "/containers/create?name=alpha-probe-1",
        frontend_probe,
    )
    .await;
    let frontend_probe_start = app
        .oneshot(
            Request::post(format!("/containers/{frontend_probe_id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(frontend_probe_start.status(), StatusCode::NO_CONTENT);

    let configs = backend.created_configs.lock().await;
    let config = configs.last().expect("container config should be recorded");
    let api_guest_ip = GuestNetworkLink::for_named_network("alpha_frontend", 3)
        .guest_ip
        .to_string();
    let db_guest_ip = GuestNetworkLink::for_named_network("alpha_backend", 4)
        .guest_ip
        .to_string();
    let bridge_frontend_alias = visor_types::HostEntry::new(
        "bridge",
        GuestNetworkLink::for_named_network("alpha_frontend", 5)
            .guest_ip
            .to_string(),
    );

    assert!(
        config
            .extra_hosts
            .contains(&visor_types::HostEntry::new("api", api_guest_ip.clone())),
        "frontend probe should see frontend peers"
    );
    assert!(
        config
            .extra_hosts
            .contains(&visor_types::HostEntry::new("api.alpha", api_guest_ip)),
        "frontend probe should see qualified frontend peers"
    );
    assert!(
        config.extra_hosts.contains(&bridge_frontend_alias),
        "frontend probe should see bridge service on the shared frontend network"
    );
    assert!(
        !config
            .extra_hosts
            .contains(&visor_types::HostEntry::new("db", db_guest_ip.clone())),
        "frontend probe should not see backend-only peers"
    );
    assert!(
        !config
            .extra_hosts
            .contains(&visor_types::HostEntry::new("db.alpha", db_guest_ip)),
        "frontend probe should not see backend-only qualified peers"
    );
}

#[tokio::test]
async fn container_list_returns_array() {
    let vm = make_running_vm("vm-list");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::get("/containers/json?all=true")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.as_array().unwrap().len() >= 1);
}

#[tokio::test]
async fn container_list_accepts_numeric_all_query_flag() {
    let vm = make_running_vm("vm-list-numeric");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::get("/containers/json?all=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn container_list_filters_by_compose_label() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let compose_id = create_container(
        app.clone(),
        "/containers/create?name=compose-app",
        serde_json::json!({
            "Image": "alpine:latest",
            "Labels": {
                "com.docker.compose.project": "visor-compose",
                "com.docker.compose.service": "app"
            }
        }),
    )
    .await;

    let _other_id = create_container(
        app.clone(),
        "/containers/create?name=other-app",
        serde_json::json!({
            "Image": "alpine:latest",
            "Labels": {
                "com.example.group": "other"
            }
        }),
    )
    .await;

    let resp = app
        .oneshot(
            Request::get(
                "/containers/json?all=1&filters=%7B%22label%22%3A%5B%22com.docker.compose.project%3Dvisor-compose%22%5D%7D",
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let entries = json.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["Id"], compose_id);
    assert_eq!(
        entries[0]["Labels"]["com.docker.compose.project"],
        "visor-compose"
    );
}

#[tokio::test]
async fn container_list_filters_by_compose_label_map_form() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let compose_id = create_container(
        app.clone(),
        "/containers/create?name=compose-app-map",
        serde_json::json!({
            "Image": "alpine:latest",
            "Labels": {
                "com.docker.compose.project": "visor-compose-map",
                "com.docker.compose.service": "app"
            }
        }),
    )
    .await;

    let _other_id = create_container(
        app.clone(),
        "/containers/create?name=other-app-map",
        serde_json::json!({
            "Image": "alpine:latest",
            "Labels": {
                "com.example.group": "other"
            }
        }),
    )
    .await;

    let resp = app
        .oneshot(
            Request::get(
                "/containers/json?all=1&filters=%7B%22label%22%3A%7B%22com.docker.compose.project%3Dvisor-compose-map%22%3Atrue%7D%7D",
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let entries = json.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["Id"], compose_id);
}

#[tokio::test]
async fn container_inspect_returns_200() {
    let vm = make_running_vm("vm-inspect");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::get("/containers/vm-inspect/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["Id"], "vm-inspect");
    assert_eq!(json["State"]["Running"], true);
}

#[tokio::test]
async fn container_inspect_includes_create_labels() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let id = create_container(
        app.clone(),
        "/containers/create?name=compose-inspect",
        serde_json::json!({
            "Image": "alpine:latest",
            "Labels": {
                "com.docker.compose.project": "visor-compose",
                "com.docker.compose.service": "app"
            }
        }),
    )
    .await;

    let resp = app
        .oneshot(
            Request::get(format!("/containers/{id}/json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        json["Config"]["Labels"]["com.docker.compose.project"],
        "visor-compose"
    );
    assert_eq!(
        json["Config"]["Labels"]["com.docker.compose.service"],
        "app"
    );
}

#[tokio::test]
async fn container_inspect_not_found_returns_404() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(
            Request::get("/containers/nonexistent/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn container_stop_returns_204() {
    let vm = make_running_vm("vm-stop");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::post("/containers/vm-stop/stop?t=5")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn container_start_running_vm_returns_304() {
    let vm = make_running_vm("vm-start");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::post("/containers/vm-start/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn container_start_recreates_stopped_vm() {
    let mut vm = make_running_vm("vm-recreate");
    vm.state = VmState::Stopped;
    vm.exit_code = Some(0);
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let start_resp = app
        .clone()
        .oneshot(
            Request::post("/containers/vm-recreate/start")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(start_resp.status(), StatusCode::NO_CONTENT);

    let list_resp = app
        .oneshot(
            Request::get("/containers/json?all=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(list_resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(list_resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let entries = json.as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["State"], "running");
    assert_eq!(entries[0]["Image"], "alpine:latest");
}

#[tokio::test]
async fn container_kill_returns_204() {
    let vm = make_running_vm("vm-kill");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::post("/containers/vm-kill/kill")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn container_remove_returns_204() {
    let vm = make_running_vm("vm-rm");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::delete("/containers/vm-rm")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn container_logs_returns_output() {
    let vm = make_running_vm("vm-logs");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::get("/containers/vm-logs/logs")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert_eq!(&body[..], expected_log_frame(1, b"hello world\n"));
}

#[tokio::test]
async fn container_logs_returns_stored_output_for_stopped_vm() {
    let mut vm = make_stopped_vm("vm-logs-stopped", "cached logs\n");
    vm.stderr = Some("cached err\n".to_owned());
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::get("/containers/vm-logs-stopped/logs?stdout=1&stderr=1")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let mut expected = expected_log_frame(1, b"cached logs\n");
    expected.extend_from_slice(&expected_log_frame(2, b"cached err\n"));
    assert_eq!(&body[..], expected);
}

#[tokio::test]
async fn container_archive_put_copies_tar_into_guest() {
    let vm = make_running_vm("vm-archive");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend.clone());
    let tar_body = make_tar(&[("buildkitd.toml", b"debug = true\n")]);

    let resp = app
        .oneshot(
            Request::put("/containers/vm-archive/archive?path=%2Fetc&noOverwriteDirNonDir=true")
                .body(Body::from(tar_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let copies = backend.copied_archives.lock().await;
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].0, "vm-archive");
    assert_eq!(copies[0].1, "/etc");
    assert_eq!(copies[0].2, tar_body);
}

#[tokio::test]
async fn container_archive_put_stages_pending_copy_until_start() {
    let backend = Arc::new(MockBackend::default());
    let app = test_router_with(Arc::clone(&backend) as Arc<dyn ExecutionBackend>);
    let id = create_container(
        app.clone(),
        "/containers/create?name=vm-archive-pending",
        serde_json::json!({
            "Image": "alpine:latest"
        }),
    )
    .await;
    let tar_body = make_tar(&[("buildkitd.toml", b"debug = true\n")]);

    let archive_resp = app
        .clone()
        .oneshot(
            Request::put("/containers/vm-archive-pending/archive?path=%2Fetc")
                .body(Body::from(tar_body.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(archive_resp.status(), StatusCode::OK);
    assert!(backend.copied_archives.lock().await.is_empty());

    let start_resp = app
        .oneshot(
            Request::post(format!("/containers/{id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start_resp.status(), StatusCode::NO_CONTENT);

    let copies = backend.copied_archives.lock().await;
    assert_eq!(copies.len(), 1);
    assert_eq!(copies[0].1, "/etc");
    assert_eq!(copies[0].2, tar_body);
}

#[tokio::test]
async fn container_archive_put_overrides_loopback_resolv_conf() {
    let vm = make_running_vm("vm-archive-resolv");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend.clone());
    let tar_body = make_tar(&[("resolv.conf", b"nameserver ::1\n")]);

    let resp = app
        .oneshot(
            Request::put("/containers/vm-archive-resolv/archive?path=%2Fetc")
                .body(Body::from(tar_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let copies = backend.copied_archives.lock().await;
    assert_eq!(copies.len(), 2);
    assert_eq!(copies[1].0, "vm-archive-resolv");
    assert_eq!(copies[1].1, "/etc");

    let resolv_conf = tar_file_contents(&copies[1].2, "resolv.conf").unwrap();
    assert_eq!(resolv_conf, "nameserver 1.1.1.1\nnameserver 8.8.8.8\n");
}

// ── Build endpoint tests ─────────────────────────────────────────

/// Creates an in-memory tar archive containing the given files.
/// Each entry is a `(path, content)` pair.
fn make_tar(files: &[(&str, &[u8])]) -> Vec<u8> {
    let buf = Vec::new();
    let mut builder = tar::Builder::new(buf);
    for (path, content) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append_data(&mut header, path, *content).unwrap();
    }
    builder.into_inner().unwrap()
}

fn make_docker_image_archive() -> Vec<u8> {
    let layer_tar = make_tar(&[("hello.txt", b"hello from loaded image\n")]);
    let config = serde_json::json!({
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "Cmd": ["cat", "/hello.txt"],
            "Entrypoint": ["/bin/sh", "-lc"],
            "Env": ["HELLO=world"],
            "WorkingDir": "/"
        }
    });
    let manifest = serde_json::json!([
        {
            "Config": "config.json",
            "RepoTags": ["loaded:test"],
            "Layers": ["layer.tar"]
        }
    ]);

    make_tar(&[
        (
            "manifest.json",
            serde_json::to_string(&manifest).unwrap().as_bytes(),
        ),
        (
            "config.json",
            serde_json::to_string(&config).unwrap().as_bytes(),
        ),
        ("layer.tar", &layer_tar),
    ])
}

fn tar_file_contents(archive_bytes: &[u8], path: &str) -> Option<String> {
    let mut archive = tar::Archive::new(archive_bytes);
    for entry_result in archive.entries().ok()? {
        let mut entry = entry_result.ok()?;
        let entry_path = entry.path().ok()?.into_owned();
        if entry_path == Path::new(path) {
            let mut contents = String::new();
            std::io::Read::read_to_string(&mut entry, &mut contents).ok()?;
            return Some(contents);
        }
    }
    None
}

fn test_tempdir() -> tempfile::TempDir {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("visor-docker-tests");
    std::fs::create_dir_all(&root).unwrap();
    tempfile::Builder::new()
        .prefix("visor-docker-")
        .tempdir_in(root)
        .unwrap()
}

#[tokio::test]
async fn build_accepts_tar_body() {
    let app = test_router_with(Arc::new(MockBackend::default()));
    let dockerfile = b"FROM alpine\nRUN echo hello\n";
    let tar_body = make_tar(&[("Dockerfile", dockerfile)]);

    let resp = app
        .oneshot(Request::post("/build").body(Body::from(tar_body)).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn build_parses_dockerfile_param() {
    let app = test_router_with(Arc::new(MockBackend::default()));
    let dockerfile = b"FROM ubuntu\nCMD echo hi\n";
    let tar_body = make_tar(&[("custom.Dockerfile", dockerfile)]);

    let resp = app
        .oneshot(
            Request::post("/build?dockerfile=custom.Dockerfile")
                .body(Body::from(tar_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("FROM ubuntu"));
}

#[tokio::test]
async fn build_parses_tag_param() {
    let app = test_router_with(Arc::new(MockBackend::default()));
    let dockerfile = b"FROM alpine\n";
    let tar_body = make_tar(&[("Dockerfile", dockerfile)]);

    let resp = app
        .oneshot(
            Request::post("/build?t=myapp:latest")
                .body(Body::from(tar_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("myapp:latest"));
}

#[tokio::test]
async fn build_parses_buildargs() {
    let app = test_router_with(Arc::new(MockBackend::default()));
    let dockerfile = b"FROM alpine\nARG VERSION\nRUN echo $VERSION\n";
    let tar_body = make_tar(&[("Dockerfile", dockerfile)]);

    let resp = app
        .oneshot(
            Request::post("/build?buildargs=%7B%22VERSION%22%3A%221.0%22%7D")
                .body(Body::from(tar_body))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn build_returns_step_stream() {
    let app = test_router_with(Arc::new(MockBackend::default()));
    let dockerfile = b"FROM alpine\nRUN echo hello\nCMD echo hi\n";
    let tar_body = make_tar(&[("Dockerfile", dockerfile)]);

    let resp = app
        .oneshot(Request::post("/build").body(Body::from(tar_body)).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    assert!(text.contains("Step 1/3"), "expected Step 1/3 in: {text}");
    assert!(text.contains("Step 2/3"), "expected Step 2/3 in: {text}");
    assert!(text.contains("Step 3/3"), "expected Step 3/3 in: {text}");
    assert!(text.contains("FROM alpine"));
    assert!(text.contains("Successfully built"));
}

#[tokio::test]
async fn build_with_invalid_tar() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(
            Request::post("/build")
                .body(Body::from(b"not-a-tar-file".to_vec()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"].as_str().is_some());
}

#[tokio::test]
async fn build_with_missing_dockerfile() {
    let app = test_router_with(Arc::new(MockBackend::default()));
    let tar_body = make_tar(&[("README.md", b"hello")]);

    let resp = app
        .oneshot(Request::post("/build").body(Body::from(tar_body)).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["error"].as_str().unwrap().contains("Dockerfile"));
}

#[tokio::test]
async fn build_returns_json_content_type() {
    let app = test_router_with(Arc::new(MockBackend::default()));
    let dockerfile = b"FROM alpine\n";
    let tar_body = make_tar(&[("Dockerfile", dockerfile)]);

    let resp = app
        .oneshot(Request::post("/build").body(Body::from(tar_body)).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let ct = resp.headers().get("Content-Type").unwrap();
    assert_eq!(ct, "application/json");
}

#[tokio::test]
async fn image_list_returns_empty_array() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(Request::get("/images/json").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn image_create_persists_image_for_inspect_and_list() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let image_manager = Arc::new(MockImageManager::default());
    let app = test_router_with_image_manager(backend, image_manager);

    let resp = app
        .clone()
        .oneshot(
            Request::post("/images/create?fromImage=alpine&tag=latest")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .clone()
        .oneshot(
            Request::get("/images/alpine:latest/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(Request::get("/images/json").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn network_list_returns_empty_array() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(Request::get("/networks").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn network_create_returns_201() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let body = serde_json::json!({"Name": "mynet"});
    let resp = app
        .oneshot(
            Request::post("/networks/create")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);
}

#[tokio::test]
async fn network_create_is_visible_to_inspect_and_list() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let body = serde_json::json!({
        "Name": "compose_default",
        "Driver": "bridge"
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/networks/create")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let network_id = json["Id"].as_str().unwrap().to_owned();

    let resp = app
        .clone()
        .oneshot(
            Request::get(format!("/networks/{network_id}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["Name"], "compose_default");
    assert_eq!(json["Driver"], "bridge");

    let resp = app
        .oneshot(Request::get("/networks").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json.as_array().unwrap().len(), 1);
}

#[tokio::test]
async fn volume_list_returns_empty() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(Request::get("/volumes").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["Volumes"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn volume_create_is_visible_to_list_and_remove() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let body = serde_json::json!({
        "Name": "compose-data",
        "Driver": "local"
    });
    let resp = app
        .clone()
        .oneshot(
            Request::post("/volumes/create")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let resp = app
        .clone()
        .oneshot(Request::get("/volumes").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["Volumes"].as_array().unwrap().len(), 1);

    let resp = app
        .clone()
        .oneshot(
            Request::delete("/volumes/compose-data")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NO_CONTENT);

    let resp = app
        .oneshot(Request::get("/volumes").body(Body::empty()).unwrap())
        .await
        .unwrap();

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json["Volumes"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn versioned_path_works() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(
            Request::get("/v1.45/containers/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn api_version_header_on_all_responses() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(Request::get("/networks").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(
        resp.headers().get("Api-Version").unwrap(),
        crate::API_VERSION
    );
    assert_eq!(resp.headers().get("Server").unwrap(), "visor");
}

#[tokio::test]
async fn exec_create_start_inspect_flow() {
    let vm = make_running_vm("vm-exec");
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::with_vm(vm));
    let app = docker_router(Arc::clone(&backend), None, None);

    // Step 1: Create exec
    let create_body = serde_json::json!({
        "Cmd": ["echo", "test"],
        "AttachStdout": true
    });

    let resp = app
        .clone()
        .oneshot(
            Request::post("/containers/vm-exec/exec")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let exec_id = json["Id"].as_str().unwrap().to_owned();

    // Step 2: Start exec
    let start_body = serde_json::json!({"Detach": false});
    let resp = app
        .clone()
        .oneshot(
            Request::post(format!("/exec/{exec_id}/start"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&start_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(String::from_utf8_lossy(&body).contains("ran: echo test"));

    // Step 3: Inspect exec
    let resp = app
        .oneshot(
            Request::get(format!("/exec/{exec_id}/json"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["Running"], false);
    assert_eq!(json["ExitCode"], 0);
}

#[tokio::test]
async fn exec_start_preserves_tty_from_create_request() {
    let vm = make_running_vm("vm-exec");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = docker_router(backend.clone(), None, None);

    let create_body = serde_json::json!({
        "Cmd": ["sh", "-lc", "tty >/dev/null && printf tty-ok"],
        "Tty": true,
    });

    let resp = app
        .clone()
        .oneshot(
            Request::post("/containers/vm-exec/exec")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let exec_id = json["Id"].as_str().unwrap().to_owned();

    let start_body = serde_json::json!({"Detach": false});
    let resp = app
        .oneshot(
            Request::post(format!("/exec/{exec_id}/start"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&start_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let exec_requests = backend.exec_requests.lock().await;
    assert_eq!(exec_requests.len(), 1);
    assert_eq!(exec_requests[0].0, "vm-exec");
    assert!(exec_requests[0].1.tty);
}

#[tokio::test]
async fn exec_start_body_tty_overrides_create_request() {
    let vm = make_running_vm("vm-exec");
    let backend = Arc::new(MockBackend::with_vm(vm));
    let app = docker_router(backend.clone(), None, None);

    let create_body = serde_json::json!({
        "Cmd": ["sh", "-lc", "printf tty-ok"],
        "Tty": false,
    });

    let resp = app
        .clone()
        .oneshot(
            Request::post("/containers/vm-exec/exec")
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&create_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let exec_id = json["Id"].as_str().unwrap().to_owned();

    let start_body = serde_json::json!({"Detach": false, "Tty": true});
    let resp = app
        .oneshot(
            Request::post(format!("/exec/{exec_id}/start"))
                .header("Content-Type", "application/json")
                .body(Body::from(serde_json::to_vec(&start_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let exec_requests = backend.exec_requests.lock().await;
    assert_eq!(exec_requests.len(), 1);
    assert!(exec_requests[0].1.tty);
}

#[tokio::test]
async fn write_raw_stream_frame_prefixes_payload_with_docker_header() {
    let (mut writer, mut reader) = tokio::io::duplex(128);

    let write_task = tokio::spawn(async move {
        write_raw_stream_frame(&mut writer, 2, b"boom\n")
            .await
            .unwrap();
    });

    let mut header = [0u8; 8];
    tokio::io::AsyncReadExt::read_exact(&mut reader, &mut header)
        .await
        .unwrap();
    let size = u32::from_be_bytes([header[4], header[5], header[6], header[7]]);
    let mut payload = vec![0u8; usize::try_from(size).unwrap()];
    tokio::io::AsyncReadExt::read_exact(&mut reader, &mut payload)
        .await
        .unwrap();

    write_task.await.unwrap();

    assert_eq!(header[0], 2);
    assert_eq!(String::from_utf8(payload).unwrap(), "boom\n");
}

#[tokio::test]
async fn bridge_exec_stream_forwards_input_and_output() {
    let (client_side, mut client_peer) = tokio::io::duplex(128);
    let (guest_side, mut guest_peer) = tokio::io::duplex(128);
    let expected_input = b"hello from client\n";
    let expected_output = b"hello from guest\n";

    let bridge_task =
        tokio::spawn(async move { bridge_exec_stream(client_side, guest_side).await });

    tokio::io::AsyncWriteExt::write_all(&mut client_peer, expected_input)
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut client_peer)
        .await
        .unwrap();

    let mut guest_input = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut guest_peer, &mut guest_input)
        .await
        .unwrap();
    assert_eq!(guest_input, expected_input);

    tokio::io::AsyncWriteExt::write_all(&mut guest_peer, expected_output)
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut guest_peer)
        .await
        .unwrap();

    let mut client_output = Vec::new();
    tokio::io::AsyncReadExt::read_to_end(&mut client_peer, &mut client_output)
        .await
        .unwrap();
    assert_eq!(client_output, expected_output);

    let (to_guest, from_guest) = bridge_task.await.unwrap().unwrap();
    assert_eq!(to_guest, expected_input.len() as u64);
    assert_eq!(from_guest, expected_output.len() as u64);
}

#[tokio::test]
async fn bridge_exec_stream_returns_after_guest_closes_with_idle_client_stdin() {
    let (client_side, mut client_peer) = tokio::io::duplex(128);
    let (guest_side, mut guest_peer) = tokio::io::duplex(128);
    let expected_output = b"resolve-ok";

    let bridge_task =
        tokio::spawn(async move { bridge_exec_stream(client_side, guest_side).await });

    tokio::io::AsyncWriteExt::write_all(&mut guest_peer, expected_output)
        .await
        .unwrap();
    tokio::io::AsyncWriteExt::shutdown(&mut guest_peer)
        .await
        .unwrap();

    let mut client_output = Vec::new();
    tokio::time::timeout(
        std::time::Duration::from_secs(1),
        tokio::io::AsyncReadExt::read_to_end(&mut client_peer, &mut client_output),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(client_output, expected_output);

    let (to_guest, from_guest) =
        tokio::time::timeout(std::time::Duration::from_secs(1), bridge_task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    assert_eq!(to_guest, 0);
    assert_eq!(from_guest, expected_output.len() as u64);
}

// ── Build with BuildService tests ──────────────────────────────

struct MockBuildService;

#[async_trait]
impl BuildService for MockBuildService {
    async fn build_image(&self, _req: BuildRequest) -> anyhow::Result<BuildOutput> {
        Ok(BuildOutput::new(
            "sha256:mock123".to_owned(),
            vec![
                BuildProgress::new(1, 2, "FROM alpine".to_owned()),
                BuildProgress::new(2, 2, "RUN echo hello".to_owned()),
            ],
        ))
    }
}

#[tokio::test]
async fn build_with_build_service_returns_real_progress() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let build_service: Arc<dyn BuildService> = Arc::new(MockBuildService);
    let app = docker_router(backend, Some(build_service), None);

    let dockerfile = b"FROM alpine\nRUN echo hello\n";
    let tar_body = make_tar(&[("Dockerfile", dockerfile)]);

    let resp = app
        .oneshot(Request::post("/build").body(Body::from(tar_body)).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8_lossy(&body);
    // Should contain real step output, not fake "pending" messages
    assert!(text.contains("Step 1/2"), "expected Step 1/2 in: {text}");
    assert!(text.contains("Step 2/2"), "expected Step 2/2 in: {text}");
    assert!(
        text.contains("Successfully built sha256:mock123"),
        "expected image id in: {text}"
    );
}

// ── Image store integration tests ──────────────────────────────

fn test_router_with_image_store(
    backend: Arc<dyn ExecutionBackend>,
    store: Arc<visor_build::ImageStore>,
) -> axum::Router {
    docker_router(backend, None, Some(store))
}

#[tokio::test]
async fn image_list_returns_entries_from_store() {
    let dir = test_tempdir();
    let store = Arc::new(visor_build::ImageStore::new(dir.path().to_path_buf()));
    store.tag("myapp:latest", "sha256:abc123").unwrap();
    store.tag("myapp:v2", "sha256:def456").unwrap();

    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let app = test_router_with_image_store(backend, store);

    let resp = app
        .oneshot(Request::get("/images/json").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    let images = json.as_array().unwrap();
    assert_eq!(images.len(), 2, "expected 2 images from store: {json}");
}

#[tokio::test]
async fn image_load_persists_loaded_archive_in_store() {
    let dir = test_tempdir();
    let store = Arc::new(visor_build::ImageStore::new(dir.path().to_path_buf()));
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let app = test_router_with_image_store(backend, store);

    let resp = app
        .clone()
        .oneshot(
            Request::post("/images/load")
                .body(Body::from(make_docker_image_archive()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let resp = app
        .oneshot(
            Request::get("/images/loaded:test/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}

#[tokio::test]
async fn image_load_quiet_returns_empty_body() {
    let dir = test_tempdir();
    let store = Arc::new(visor_build::ImageStore::new(dir.path().to_path_buf()));
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let app = test_router_with_image_store(backend, store);

    let resp = app
        .oneshot(
            Request::post("/images/load?quiet=1")
                .body(Body::from(make_docker_image_archive()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    assert!(body.is_empty(), "quiet load should not emit stream output");
}

#[tokio::test]
async fn image_load_rejects_archive_without_manifest_json() {
    let dir = test_tempdir();
    let store = Arc::new(visor_build::ImageStore::new(dir.path().to_path_buf()));
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let app = test_router_with_image_store(backend, store);

    let resp = app
        .oneshot(
            Request::post("/images/load")
                .body(Body::from(make_tar(&[("readme.txt", b"missing manifest")])))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::BAD_REQUEST);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    assert!(text.contains("docker archive missing manifest.json"));
}

#[tokio::test]
async fn image_list_returns_empty_when_no_store() {
    // docker_router with None for image_store should still return empty
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let app = docker_router(backend, None, None);

    let resp = app
        .oneshot(Request::get("/images/json").body(Body::empty()).unwrap())
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert!(json.as_array().unwrap().is_empty());
}

#[tokio::test]
async fn image_inspect_returns_data_for_tagged_image() {
    let dir = test_tempdir();
    let store = Arc::new(visor_build::ImageStore::new(dir.path().to_path_buf()));
    store.tag("myapp:latest", "sha256:abc123").unwrap();

    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let app = test_router_with_image_store(backend, store);

    let resp = app
        .oneshot(
            Request::get("/images/myapp:latest/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["Id"], "sha256:abc123");
}

#[tokio::test]
async fn image_inspect_returns_404_when_not_found() {
    let dir = test_tempdir();
    let store = Arc::new(visor_build::ImageStore::new(dir.path().to_path_buf()));

    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let app = test_router_with_image_store(backend, store);

    let resp = app
        .oneshot(
            Request::get("/images/nonexistent:v1/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

// ── container_wait ──────────────────────────────────────────────────

#[tokio::test]
async fn wait_stopped_container_returns_immediately() {
    let mut vm = make_running_vm("vm-wait-1");
    vm.state = VmState::Stopped;
    vm.exit_code = Some(0);
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::with_vm(vm));
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::post("/containers/vm-wait-1/wait")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["StatusCode"], 0);
}

#[tokio::test]
async fn wait_running_container_sends_headers_before_exit() {
    let backend = Arc::new(MockBackend::with_vm(make_running_vm("vm-wait-2")));
    let app = test_router_with(backend.clone() as Arc<dyn ExecutionBackend>);

    // Send the /wait request — it should return 200 headers immediately
    // even though the container is still running.
    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.oneshot(
            Request::post("/containers/vm-wait-2/wait")
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("wait should return headers within 2s")
    .unwrap();

    // Headers should arrive immediately with 200 status.
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        resp.headers().get("content-type").unwrap(),
        "application/json"
    );

    // Stop the container so the body can be sent.
    {
        let mut vms = backend.vms.lock().await;
        if let Some(vm) = vms.iter_mut().find(|v| v.id == "vm-wait-2") {
            vm.state = VmState::Stopped;
            vm.exit_code = Some(42);
        }
    }

    // Now read the body — it should arrive after the state change.
    let body = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        axum::body::to_bytes(resp.into_body(), usize::MAX),
    )
    .await
    .expect("body should arrive after container stops")
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["StatusCode"], 42);
}

#[tokio::test]
async fn wait_created_container_unblocks_after_start_and_exit() {
    let backend = Arc::new(MockBackend::default());
    let app = test_router_with(Arc::clone(&backend) as Arc<dyn ExecutionBackend>);
    let id = create_container(
        app.clone(),
        "/containers/create?name=vm-wait-created",
        serde_json::json!({
            "Image": "alpine:latest",
            "Cmd": ["echo", "hello"]
        }),
    )
    .await;

    let resp = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        app.clone().oneshot(
            Request::post(format!("/containers/{id}/wait"))
                .body(Body::empty())
                .unwrap(),
        ),
    )
    .await
    .expect("wait should return headers within 2s")
    .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let start_resp = app
        .clone()
        .oneshot(
            Request::post(format!("/containers/{id}/start"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(start_resp.status(), StatusCode::NO_CONTENT);

    {
        let mut vms = backend.vms.lock().await;
        let vm = vms.last_mut().expect("started VM should exist");
        vm.state = VmState::Stopped;
        vm.exit_code = Some(7);
    }

    let body = tokio::time::timeout(
        std::time::Duration::from_secs(3),
        axum::body::to_bytes(resp.into_body(), usize::MAX),
    )
    .await
    .expect("body should arrive after container stops")
    .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["StatusCode"], 7);
}

#[tokio::test]
async fn wait_nonexistent_container_returns_404() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBackend::default());
    let app = test_router_with(backend);

    let resp = app
        .oneshot(
            Request::post("/containers/no-such-id/wait")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn events_route_exists_for_compose_filters() {
    let app = test_router_with(Arc::new(MockBackend::default()));

    let resp = app
        .oneshot(
            Request::get(
                "/events?filters=%7B%22label%22%3A%7B%22com.docker.compose.project%3Dvisor-compose-filtered%22%3Atrue%7D%2C%22type%22%3A%7B%22container%22%3Atrue%7D%7D",
            )
            .body(Body::empty())
            .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);
}
