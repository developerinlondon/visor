use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use tower::ServiceExt as _;

use super::*;
use crate::volume::{VolumeInfo, VolumeManager};

fn test_volume_app(base_dir: &std::path::Path) -> axum::Router {
    let state = VolumeState {
        manager: Arc::new(VolumeManager::new(base_dir).unwrap()),
    };
    axum::Router::new()
        .route(
            "/v1/volumes",
            axum::routing::get(list_volumes).post(create_volume),
        )
        .route(
            "/v1/volumes/{name}",
            axum::routing::get(get_volume).delete(delete_volume),
        )
        .route(
            "/v1/volumes/{name}/resize",
            axum::routing::post(resize_volume),
        )
        .with_state(state)
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
async fn list_empty_returns_200() {
    let dir = crate::testutil::tempdir("visor-runtime-route-volume-").unwrap();
    let app = test_volume_app(dir.path());

    let response = app
        .oneshot(json_request("GET", "/v1/volumes", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let volumes: Vec<VolumeInfo> = serde_json::from_slice(&bytes).unwrap();
    assert!(volumes.is_empty());
}

#[tokio::test]
async fn create_returns_201() {
    let dir = crate::testutil::tempdir("visor-runtime-route-volume-").unwrap();
    let app = test_volume_app(dir.path());

    let body = serde_json::json!({ "name": "testvol", "size_mib": 10 });
    let response = app
        .oneshot(json_request("POST", "/v1/volumes", Some(body)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: VolumeInfo = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info.name, "testvol");
    assert_eq!(info.size_mib, 10);
}

#[tokio::test]
async fn create_then_list_shows_volume() {
    let dir = crate::testutil::tempdir("visor-runtime-route-volume-").unwrap();
    let state = VolumeState {
        manager: Arc::new(VolumeManager::new(dir.path()).unwrap()),
    };
    state.manager.create("listed", 10).unwrap();

    let app = axum::Router::new()
        .route("/v1/volumes", axum::routing::get(list_volumes))
        .with_state(state);

    let response = app
        .oneshot(json_request("GET", "/v1/volumes", None))
        .await
        .unwrap();

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let volumes: Vec<VolumeInfo> = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(volumes.len(), 1);
    assert_eq!(volumes[0].name, "listed");
}

#[tokio::test]
async fn get_volume_returns_200() {
    let dir = crate::testutil::tempdir("visor-runtime-route-volume-").unwrap();
    let state = VolumeState {
        manager: Arc::new(VolumeManager::new(dir.path()).unwrap()),
    };
    state.manager.create("getvol", 10).unwrap();

    let app = axum::Router::new()
        .route("/v1/volumes/{name}", axum::routing::get(get_volume))
        .with_state(state);

    let response = app
        .oneshot(json_request("GET", "/v1/volumes/getvol", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: VolumeInfo = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info.name, "getvol");
}

#[tokio::test]
async fn delete_returns_204() {
    let dir = crate::testutil::tempdir("visor-runtime-route-volume-").unwrap();
    let state = VolumeState {
        manager: Arc::new(VolumeManager::new(dir.path()).unwrap()),
    };
    state.manager.create("delvol", 10).unwrap();

    let app = axum::Router::new()
        .route("/v1/volumes/{name}", axum::routing::delete(delete_volume))
        .with_state(state);

    let response = app
        .oneshot(json_request("DELETE", "/v1/volumes/delvol", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn delete_nonexistent_returns_404() {
    let dir = crate::testutil::tempdir("visor-runtime-route-volume-").unwrap();
    let app = test_volume_app(dir.path());

    let response = app
        .oneshot(json_request("DELETE", "/v1/volumes/ghost", None))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn resize_returns_200() {
    let dir = crate::testutil::tempdir("visor-runtime-route-volume-").unwrap();
    let state = VolumeState {
        manager: Arc::new(VolumeManager::new(dir.path()).unwrap()),
    };
    state.manager.create("rsvol", 10).unwrap();

    let app = axum::Router::new()
        .route(
            "/v1/volumes/{name}/resize",
            axum::routing::post(resize_volume),
        )
        .with_state(state);

    let body = serde_json::json!({ "size_mib": 20 });
    let response = app
        .oneshot(json_request("POST", "/v1/volumes/rsvol/resize", Some(body)))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: VolumeInfo = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(info.size_mib, 20);
}
