use std::sync::Arc;
use std::time::Instant;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use crate::api::router::{AppState, build_router};
use crate::api::sse::EventBroadcaster;
use crate::backend::{ExecutionBackend, VmmBackend};

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

#[tokio::test]
async fn health_check_returns_200() {
    let app = build_router(test_state());
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

#[tokio::test]
async fn get_info_returns_system_info() {
    let app = build_router(test_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/info")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let body = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: SystemInfo = serde_json::from_slice(&body).unwrap();

    let expected_mode = if cfg!(target_os = "macos") {
        "hvf"
    } else {
        "kvm"
    };
    assert_eq!(info.mode, expected_mode);
    assert_eq!(info.vm_count, 0);
    assert!(!info.version.is_empty());
    assert!(!info.kernel_version.is_empty());
    assert!(info.kernel_size_bytes > 0);
    assert_eq!(info.kernel_sha256.len(), 64);
    assert_eq!(
        info.capabilities.guest.networking,
        cfg!(target_os = "linux")
    );
    assert!(info.capabilities.observability.metrics);
    assert!(!info.capabilities.observability.vm_runtime_metrics);
}

#[test]
fn system_info_serialization() {
    let info = SystemInfo {
        version: "0.1.0".to_owned(),
        mode: "kvm".to_owned(),
        uptime_secs: 42,
        vm_count: 3,
        kernel_version: "Linux version 7.0.0-rc1-test".to_owned(),
        kernel_size_bytes: 32_000_000,
        kernel_sha256: "a".repeat(64),
        capabilities: SystemCapabilities {
            guest: GuestCapabilities {
                networking: true,
                volume_mounts: true,
                snapshot_restore: true,
            },
            lifecycle: LifecycleCapabilities {
                warm_pool: true,
                health_monitoring: true,
            },
            observability: ObservabilityCapabilities {
                metrics: true,
                vm_runtime_metrics: false,
                seccomp_sandbox: true,
            },
        },
    };
    let json = serde_json::to_value(&info).unwrap();
    assert_eq!(json["version"], "0.1.0");
    assert_eq!(json["mode"], "kvm");
    assert_eq!(json["uptime_secs"], 42);
    assert_eq!(json["vm_count"], 3);
    assert_eq!(json["kernel_version"], "Linux version 7.0.0-rc1-test");
    assert_eq!(json["kernel_size_bytes"], 32_000_000);
    assert_eq!(json["kernel_sha256"], "a".repeat(64));
    assert_eq!(json["capabilities"]["networking"], true);
    assert_eq!(json["capabilities"]["warm_pool"], true);
    assert_eq!(json["capabilities"]["vm_runtime_metrics"], false);
}
