use std::sync::Arc;

use axum::{Json, Router, extract::State, routing::post};
use serde_json::{Value, json};
use tokio::sync::{Mutex, oneshot};

use super::execute;
use crate::cli::RunArgs;

#[derive(Clone)]
struct CaptureState {
    sender: Arc<Mutex<Option<oneshot::Sender<Value>>>>,
}

async fn capture_run_request(
    State(state): State<CaptureState>,
    Json(body): Json<Value>,
) -> Json<Value> {
    if let Some(sender) = state.sender.lock().await.take() {
        let _ = sender.send(body);
    }
    Json(json!({
        "id": "vm-test-id",
        "name": "vm-test-name"
    }))
}

async fn capture_run_config(args: RunArgs) -> Value {
    let (tx, rx) = oneshot::channel();
    let state = CaptureState {
        sender: Arc::new(Mutex::new(Some(tx))),
    };
    let app = Router::new()
        .route("/v1/vms", post(capture_run_request))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind test listener");
    let addr = listener.local_addr().expect("get test listener address");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.expect("serve test app");
    });

    execute(&format!("http://{addr}"), args)
        .await
        .expect("run::execute should succeed");

    let body = rx.await.expect("capture run request body");
    server.abort();
    body
}

fn detached_run_args(image: &str) -> RunArgs {
    RunArgs {
        image: image.to_owned(),
        cmd: vec!["sleep".to_owned(), "1".to_owned()],
        env: Vec::new(),
        memory: 512,
        cpus: 1,
        name: Some("test-vm".to_owned()),
        port: Vec::new(),
        network: false,
        no_network: false,
        detach: true,
        nested_virt: false,
        workdir: None,
        volume: Vec::new(),
    }
}

#[tokio::test]
async fn execute_posts_network_and_nested_virtualization_flags() {
    let mut args = detached_run_args("alpine:latest");
    args.network = true;
    args.nested_virt = true;

    let body = capture_run_config(args).await;

    assert_eq!(
        body.get("image").and_then(Value::as_str),
        Some("alpine:latest")
    );
    assert_eq!(
        body.get("network_enabled").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        body.get("guest_virtualization").and_then(Value::as_str),
        Some("nested")
    );
}

#[tokio::test]
async fn execute_keeps_network_disabled_by_default() {
    let body = capture_run_config(detached_run_args("alpine:latest")).await;

    assert_eq!(
        body.get("network_enabled").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        body.get("guest_virtualization").and_then(Value::as_str),
        Some("standard")
    );
}

#[tokio::test]
async fn execute_honors_no_network_override() {
    let mut args = detached_run_args("alpine:latest");
    args.no_network = true;

    let body = capture_run_config(args).await;

    assert_eq!(
        body.get("network_enabled").and_then(Value::as_bool),
        Some(false)
    );
}
