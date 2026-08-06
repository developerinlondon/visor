//! E2E tests for volume lifecycle management.
//!
//! Tests volume CRUD via standalone Axum routes with a temporary volume
//! directory. Uses `tower::ServiceExt::oneshot` for in-process requests.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use visor_runtime::api::routes::volumes::{
    VolumeState, create_volume, delete_volume, list_volumes,
};
use visor_runtime::volume::{VolumeInfo, VolumeManager};

// ── Helpers ─────────────────────────────────────────────────────────

/// Creates a temporary test directory under the workspace instead of `/tmp`.
fn workspace_tempdir() -> std::io::Result<tempfile::TempDir> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("integration-tests");
    std::fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix("visor-integration-volume-")
        .tempdir_in(root)
        .map_err(std::io::Error::from)
}

/// Builds a standalone volume router backed by a temporary directory.
fn test_volume_app(base_dir: &std::path::Path) -> axum::Router {
    let state = VolumeState::new(Arc::new(
        VolumeManager::new(base_dir).expect("create test volume manager"),
    ));
    axum::Router::new()
        .route(
            "/v1/volumes",
            axum::routing::get(list_volumes).post(create_volume),
        )
        .route("/v1/volumes/{name}", axum::routing::delete(delete_volume))
        .with_state(state)
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

// ── Tests ───────────────────────────────────────────────────────────

#[tokio::test]
async fn test_volume_list_empty() {
    let dir = workspace_tempdir().expect("create temp dir");
    let app = test_volume_app(dir.path());

    let response = app
        .oneshot(json_request("GET", "/v1/volumes", None))
        .await
        .expect("list volumes request should succeed");

    assert_eq!(
        response.status(),
        StatusCode::OK,
        "GET /v1/volumes should return 200"
    );

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .expect("read list response body");
    let volumes: Vec<VolumeInfo> =
        serde_json::from_slice(&bytes).expect("list response should deserialize");
    assert!(
        volumes.is_empty(),
        "fresh volume directory should list zero volumes"
    );
}

#[tokio::test]
async fn test_volume_create_and_list() {
    let dir = workspace_tempdir().expect("create temp dir");
    let manager = Arc::new(VolumeManager::new(dir.path()).expect("create test volume manager"));
    let state = VolumeState::new(manager.clone());

    // Create a volume via POST.
    let create_app = axum::Router::new()
        .route("/v1/volumes", axum::routing::post(create_volume))
        .with_state(state.clone());

    let body = serde_json::json!({ "name": "testvol", "size_mib": 10 });
    let create_response = create_app
        .oneshot(json_request("POST", "/v1/volumes", Some(body)))
        .await
        .expect("create volume request should succeed");

    assert_eq!(
        create_response.status(),
        StatusCode::CREATED,
        "POST /v1/volumes should return 201"
    );

    let bytes = axum::body::to_bytes(create_response.into_body(), 1024 * 1024)
        .await
        .expect("read create response body");
    let created: VolumeInfo =
        serde_json::from_slice(&bytes).expect("create response should deserialize");
    assert_eq!(created.name, "testvol");
    assert_eq!(created.size_mib, 10);

    // List volumes via GET — should show the created volume.
    let list_app = axum::Router::new()
        .route("/v1/volumes", axum::routing::get(list_volumes))
        .with_state(state);

    let list_response = list_app
        .oneshot(json_request("GET", "/v1/volumes", None))
        .await
        .expect("list volumes request should succeed");

    let bytes = axum::body::to_bytes(list_response.into_body(), 1024 * 1024)
        .await
        .expect("read list response body");
    let volumes: Vec<VolumeInfo> =
        serde_json::from_slice(&bytes).expect("list response should deserialize");
    assert_eq!(volumes.len(), 1, "should list exactly one volume");
    assert_eq!(volumes[0].name, "testvol");
}

#[tokio::test]
async fn test_volume_delete() {
    let dir = workspace_tempdir().expect("create temp dir");
    let manager = Arc::new(VolumeManager::new(dir.path()).expect("create test volume manager"));

    // Pre-create a volume using the manager directly.
    manager
        .create("deleteme", 10)
        .expect("pre-create volume for delete test");

    let state = VolumeState::new(manager);
    let app = axum::Router::new()
        .route("/v1/volumes/{name}", axum::routing::delete(delete_volume))
        .with_state(state);

    let response = app
        .oneshot(json_request("DELETE", "/v1/volumes/deleteme", None))
        .await
        .expect("delete volume request should succeed");

    assert_eq!(
        response.status(),
        StatusCode::NO_CONTENT,
        "DELETE /v1/volumes/deleteme should return 204"
    );
}
