use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Instant;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use crate::api::router::{AppState, build_router};
use crate::api::sse::EventBroadcaster;
use crate::backend::{ExecutionBackend, VmInfo, VmState, VmmBackend};
use crate::pool::health::{HealthCheckConfig, HealthCheckLoop, HealthChecker, VsockHealthPinger};
use crate::pool::manager::{ImagePoolConfig, PoolConfig, PoolManager};

use super::*;

fn test_state() -> AppState {
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

fn test_state_with_backend(backend: VmmBackend) -> AppState {
    AppState {
        backend: Arc::new(backend) as Arc<dyn ExecutionBackend>,
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

struct MetricsPoolBackend {
    next_id: AtomicU32,
}

impl MetricsPoolBackend {
    fn new() -> Self {
        Self {
            next_id: AtomicU32::new(1),
        }
    }
}

#[async_trait]
impl ExecutionBackend for MetricsPoolBackend {
    async fn create(&self, config: crate::backend::VmConfig) -> anyhow::Result<VmInfo> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(VmInfo::new(
            format!("pool-vm-{id}"),
            config.image,
            VmState::Running,
            "2026-01-01T00:00:00Z".to_owned(),
            config.memory_mib,
            config.vcpus,
        ))
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(Vec::new())
    }

    async fn get(&self, _id: &str) -> anyhow::Result<VmInfo> {
        anyhow::bail!("not implemented")
    }

    async fn exec(
        &self,
        _id: &str,
        _req: crate::backend::ExecRequest,
    ) -> anyhow::Result<crate::backend::ExecResult> {
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

struct MetricsHealthPinger;

#[async_trait]
impl VsockHealthPinger for MetricsHealthPinger {
    async fn ping(&self, cid: u32, _timeout: std::time::Duration) -> anyhow::Result<()> {
        match cid {
            7 => Ok(()),
            _ => anyhow::bail!("mock health failure for cid {cid}"),
        }
    }
}

async fn fetch_metrics(state: AppState) -> (StatusCode, String) {
    let app = build_router(state);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let status = response.status();
    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let text = String::from_utf8(body.to_vec()).unwrap();
    (status, text)
}

#[tokio::test]
async fn returns_200_with_prometheus_content_type() {
    let app = build_router(test_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/metrics")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let content_type = response
        .headers()
        .get("content-type")
        .expect("content-type header missing")
        .to_str()
        .unwrap();
    assert!(
        content_type.starts_with("text/plain; version=0.0.4"),
        "unexpected content type: {content_type}"
    );
}

#[tokio::test]
async fn contains_visor_vms_total_with_zero_vms() {
    let (status, text) = fetch_metrics(test_state()).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("# HELP visor_vms_total"),
        "missing HELP for visor_vms_total"
    );
    assert!(
        text.contains("# TYPE visor_vms_total gauge"),
        "missing TYPE for visor_vms_total"
    );
    assert!(
        text.contains("visor_vms_total 0"),
        "expected visor_vms_total 0, body:\n{text}"
    );
}

#[tokio::test]
async fn includes_all_metric_help_and_type_headers() {
    let (status, text) = fetch_metrics(test_state()).await;

    assert_eq!(status, StatusCode::OK);

    let expected = [
        ("visor_vms_total", "gauge"),
        ("visor_vms_running", "gauge"),
        ("visor_pool_available_total", "gauge"),
        ("visor_pool_target_total", "gauge"),
        ("visor_vm_health_healthy", "gauge"),
        ("visor_vm_health_unhealthy", "gauge"),
        ("visor_vm_health_unknown", "gauge"),
        ("visor_vm_runtime_metrics_available", "gauge"),
    ];

    for (metric, mtype) in &expected {
        assert!(
            text.contains(&format!("# HELP {metric}")),
            "missing HELP for {metric}"
        );
        assert!(
            text.contains(&format!("# TYPE {metric} {mtype}")),
            "missing TYPE {mtype} for {metric}"
        );
    }
}

#[tokio::test]
async fn runtime_vm_metrics_availability_is_explicit_and_placeholder_metrics_are_absent() {
    let backend = VmmBackend::new();
    backend
        .insert_vm({
            let mut info = VmInfo::new(
                "vm-abc-123".to_owned(),
                "alpine:latest".to_owned(),
                VmState::Running,
                "2025-01-01T00:00:00Z".to_owned(),
                512,
                1,
            );
            info.name = Some("test-vm".to_owned());
            info
        })
        .await;

    let (status, text) = fetch_metrics(test_state_with_backend(backend)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("visor_vms_total 1"),
        "expected visor_vms_total 1, body:\n{text}"
    );
    assert!(
        text.contains("visor_vm_runtime_metrics_available 0"),
        "expected explicit runtime metrics availability gauge, body:\n{text}"
    );
    assert!(
        !text.contains("visor_vm_cpu_time_us"),
        "unexpected placeholder cpu metric, body:\n{text}"
    );
    assert!(
        !text.contains("visor_vm_memory_rss_bytes"),
        "unexpected placeholder memory metric, body:\n{text}"
    );
}

#[tokio::test]
async fn multiple_vms_still_report_real_aggregate_counts() {
    let backend = VmmBackend::new();
    for i in 1..=3 {
        backend
            .insert_vm(VmInfo::new(
                format!("vm-{i}"),
                "alpine:latest".to_owned(),
                VmState::Running,
                "2025-01-01T00:00:00Z".to_owned(),
                256,
                1,
            ))
            .await;
    }

    let (status, text) = fetch_metrics(test_state_with_backend(backend)).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("visor_vms_total 3"),
        "expected visor_vms_total 3, body:\n{text}"
    );
    assert!(
        text.contains("visor_vms_running 3"),
        "expected running VM gauge, body:\n{text}"
    );
}

#[tokio::test]
async fn includes_pool_gauges_from_pool_manager() {
    let backend = VmmBackend::new();
    let mut image_configs = std::collections::HashMap::new();
    image_configs.insert(
        "alpine:latest".to_owned(),
        ImagePoolConfig {
            size: 2,
            memory_mib: 256,
        },
    );
    let pool = Arc::new(PoolManager::new(
        PoolConfig {
            default_size: 0,
            image_configs,
        },
        Arc::new(MetricsPoolBackend::new()),
        crate::pool::snapshot_cache::SnapshotCache::new(
            std::env::temp_dir().join("visor-metrics-pool-cache"),
        ),
    ));
    pool.warm("alpine:latest", 1).await.unwrap();

    let mut state = test_state_with_backend(backend);
    state.pool = Some(pool);
    let (status, text) = fetch_metrics(state).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("visor_pool_available_total 1"),
        "expected warm pool availability gauge, body:\n{text}"
    );
    assert!(
        text.contains("visor_pool_target_total 2"),
        "expected warm pool target gauge, body:\n{text}"
    );
}

#[tokio::test]
async fn includes_health_gauges_from_health_loop() {
    let backend = VmmBackend::new();
    let checker = HealthChecker::new(Arc::new(MetricsHealthPinger), HealthCheckConfig::default());
    let health = Arc::new(HealthCheckLoop::new(
        checker,
        Arc::new(EventBroadcaster::new(16)),
        HealthCheckConfig::default(),
    ));
    health
        .check_all(&[("vm-healthy".to_owned(), 7), ("vm-unhealthy".to_owned(), 8)])
        .await;

    let mut state = test_state_with_backend(backend);
    state.health = Some(health);
    let (status, text) = fetch_metrics(state).await;

    assert_eq!(status, StatusCode::OK);
    assert!(
        text.contains("visor_vm_health_healthy 1"),
        "expected healthy gauge, body:\n{text}"
    );
    assert!(
        text.contains("visor_vm_health_unhealthy 1"),
        "expected unhealthy gauge, body:\n{text}"
    );
    assert!(
        text.contains("visor_vm_health_unknown 0"),
        "expected unknown gauge, body:\n{text}"
    );
}

#[test]
fn runtime_vm_metrics_availability_constant_is_disabled() {
    assert_eq!(RUNTIME_VM_METRICS_AVAILABLE, 0);
}
