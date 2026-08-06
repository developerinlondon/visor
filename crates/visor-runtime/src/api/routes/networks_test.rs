use axum::body::Body;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use crate::api::routes::networks::{NetworkConfig, NetworkInfo, NetworkState};

/// Helper to build a test router with an empty network manager.
fn test_app() -> axum::Router {
    let state = crate::api::router::AppState {
        backend: std::sync::Arc::new(crate::backend::VmmBackend::new()),
        events: std::sync::Arc::new(crate::api::sse::EventBroadcaster::new(16)),
        start_time: std::time::Instant::now(),
        shutdown: std::sync::Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    };

    crate::api::routes::networks::network_routes().with_state(state)
}

// ── NetworkConfig type tests ──────────────────────────────────────

#[test]
fn test_network_config_defaults() {
    let config: NetworkConfig = serde_json::from_str(r#"{"name": "mynet"}"#).expect("should parse");
    assert_eq!(config.name, "mynet");
    assert_eq!(config.subnet, None);
    assert_eq!(config.gateway, None);
}

#[test]
fn test_network_config_with_subnet() {
    let config: NetworkConfig = serde_json::from_str(
        r#"{"name": "mynet", "subnet": "10.0.0.0/24", "gateway": "10.0.0.1"}"#,
    )
    .expect("should parse");
    assert_eq!(config.name, "mynet");
    assert_eq!(config.subnet.as_deref(), Some("10.0.0.0/24"));
    assert_eq!(config.gateway.as_deref(), Some("10.0.0.1"));
}

#[test]
fn test_network_info_state_default() {
    let info = NetworkInfo {
        id: "net-1".to_owned(),
        name: "test".to_owned(),
        subnet: "172.20.0.0/24".to_owned(),
        gateway: "172.20.0.1".to_owned(),
        state: NetworkState::default(),
        connected_vms: Vec::new(),
    };
    assert_eq!(info.state, NetworkState::Active);
}

// ── REST endpoint tests ───────────────────────────────────────────

#[tokio::test]
async fn test_create_network() {
    let app = test_app();

    let body = serde_json::json!({
        "name": "test-net",
        "subnet": "10.0.0.0/24",
        "gateway": "10.0.0.1"
    });

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/networks")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::CREATED);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
        .await
        .unwrap();
    let info: NetworkInfo = serde_json::from_slice(&body).unwrap();
    assert_eq!(info.name, "test-net");
    assert_eq!(info.subnet, "10.0.0.0/24");
    assert_eq!(info.gateway, "10.0.0.1");
    assert_eq!(info.state, NetworkState::Active);
    assert!(info.connected_vms.is_empty());
}

#[tokio::test]
async fn test_list_networks_empty() {
    let app = test_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/networks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
        .await
        .unwrap();
    let networks: Vec<NetworkInfo> = serde_json::from_slice(&body).unwrap();
    assert!(networks.is_empty());
}

#[tokio::test]
async fn test_create_and_list_network() {
    let state = crate::api::router::AppState {
        backend: std::sync::Arc::new(crate::backend::VmmBackend::new()),
        events: std::sync::Arc::new(crate::api::sse::EventBroadcaster::new(16)),
        start_time: std::time::Instant::now(),
        shutdown: std::sync::Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    };

    let app = crate::api::routes::networks::network_routes().with_state(state);

    // Create a network
    let create_body = serde_json::json!({"name": "net1"});
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/networks")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&create_body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // List networks
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/networks")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), 1024 * 64)
        .await
        .unwrap();
    let networks: Vec<NetworkInfo> = serde_json::from_slice(&body).unwrap();
    assert_eq!(networks.len(), 1);
    assert_eq!(networks[0].name, "net1");
}

#[tokio::test]
async fn test_get_network_not_found() {
    let app = test_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::GET)
                .uri("/v1/networks/nonexistent-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // Not found returns 404 (via ApiError) since the error chain contains "not found"
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_delete_network_not_found() {
    let app = test_app();

    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/v1/networks/nonexistent-id")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn test_create_duplicate_network_name() {
    let state = crate::api::router::AppState {
        backend: std::sync::Arc::new(crate::backend::VmmBackend::new()),
        events: std::sync::Arc::new(crate::api::sse::EventBroadcaster::new(16)),
        start_time: std::time::Instant::now(),
        shutdown: std::sync::Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new())),
    };

    let app = crate::api::routes::networks::network_routes().with_state(state);

    let body = serde_json::json!({"name": "dup-net"});
    let body_str = serde_json::to_string(&body).unwrap();

    // First create succeeds
    let resp = app
        .clone()
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/networks")
                .header("content-type", "application/json")
                .body(Body::from(body_str.clone()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::CREATED);

    // Second create fails (duplicate name)
    let resp = app
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/v1/networks")
                .header("content-type", "application/json")
                .body(Body::from(body_str))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::INTERNAL_SERVER_ERROR);
}

// ── NetworkManager unit tests ─────────────────────────────────────

#[test]
fn test_network_manager_create() {
    use crate::api::routes::networks::NetworkManager;

    let mut mgr = NetworkManager::new();
    let config = NetworkConfig {
        name: "test".to_owned(),
        subnet: Some("10.0.0.0/24".to_owned()),
        gateway: Some("10.0.0.1".to_owned()),
    };
    let info = mgr.create(config).expect("should create");
    assert_eq!(info.name, "test");
    assert_eq!(info.subnet, "10.0.0.0/24");
    assert!(!info.id.is_empty());
}

#[test]
fn test_network_manager_list_empty() {
    use crate::api::routes::networks::NetworkManager;

    let mgr = NetworkManager::new();
    assert!(mgr.list().is_empty());
}

#[test]
fn test_network_manager_get() {
    use crate::api::routes::networks::NetworkManager;

    let mut mgr = NetworkManager::new();
    let config = NetworkConfig {
        name: "test".to_owned(),
        subnet: None,
        gateway: None,
    };
    let info = mgr.create(config).unwrap();
    let id = info.id.clone();

    let fetched = mgr.get(&id).expect("should find");
    assert_eq!(fetched.name, "test");
}

#[test]
fn test_network_manager_delete() {
    use crate::api::routes::networks::NetworkManager;

    let mut mgr = NetworkManager::new();
    let config = NetworkConfig {
        name: "test".to_owned(),
        subnet: None,
        gateway: None,
    };
    let info = mgr.create(config).unwrap();
    let id = info.id.clone();

    mgr.delete(&id).expect("should delete");
    assert!(mgr.get(&id).is_err());
}

#[test]
fn test_network_manager_connect_vm() {
    use crate::api::routes::networks::NetworkManager;

    let mut mgr = NetworkManager::new();
    let config = NetworkConfig {
        name: "test".to_owned(),
        subnet: None,
        gateway: None,
    };
    let info = mgr.create(config).unwrap();
    let id = info.id.clone();

    mgr.connect_vm(&id, "vm-123").expect("should connect");

    let info = mgr.get(&id).unwrap();
    assert_eq!(info.connected_vms, vec!["vm-123"]);
}

#[test]
fn test_network_manager_disconnect_vm() {
    use crate::api::routes::networks::NetworkManager;

    let mut mgr = NetworkManager::new();
    let config = NetworkConfig {
        name: "test".to_owned(),
        subnet: None,
        gateway: None,
    };
    let info = mgr.create(config).unwrap();
    let id = info.id.clone();

    mgr.connect_vm(&id, "vm-123").unwrap();
    mgr.disconnect_vm(&id, "vm-123").expect("should disconnect");

    let info = mgr.get(&id).unwrap();
    assert!(info.connected_vms.is_empty());
}
