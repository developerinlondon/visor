//! End-to-end integration tests for the visor pipeline.
//!
//! Covers six test areas:
//!
//! 1. **Backend** — `VmmBackend` state management CRUD, `VmConfig` serialization
//! 2. **OCI** — image reference parsing, layer merging, rootfs building
//! 3. **CLI** — all subcommand argument parsing via `try_parse_from`
//! 4. **API** — HTTP route responses via `tower::ServiceExt::oneshot`
//! 5. **SSE** — event broadcasting with `EventBroadcaster`
//! 6. **Audit** — structured event creation and tracing emission

use std::sync::{Arc, Mutex};

use anyhow::Context;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use clap::Parser as _;
#[cfg(target_os = "linux")]
use serial_test::serial;
use tower::ServiceExt as _;

use visor_runtime::api::router::{AppState, build_router};
use visor_runtime::api::sse::{EventBroadcaster, VmEvent};
use visor_runtime::audit::{AuditAction, AuditEvent, AuditOutcome};
use visor_runtime::backend::{ExecutionBackend, VmConfig, VmState, VmmBackend};
use visor_runtime::cli::{Cli, Command};
use visor_runtime::oci::layers::LayerMerger;
use visor_runtime::oci::reference::ImageReference;
use visor_runtime::oci::rootfs::RootfsBuilder;
#[cfg(target_os = "linux")]
use visor_vmm::platform::{KvmPlatform, Platform};
// ── Helpers ─────────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
/// Returns `true` if `/dev/kvm` is accessible on this machine.
#[must_use]
fn has_kvm() -> bool {
    std::path::Path::new("/dev/kvm").exists()
}

/// Builds an [`AppState`] with a fresh [`VmmBackend`] for API testing.
///
/// Uses a small SSE buffer (16) and a fresh shutdown notifier.
#[must_use]
fn test_app_state() -> AppState {
    AppState {
        backend: Arc::new(VmmBackend::new()) as Arc<dyn ExecutionBackend>,
        events: Arc::new(EventBroadcaster::new(16)),
        start_time: std::time::Instant::now(),
        shutdown: Arc::new(tokio::sync::Notify::new()),
        health: None,
        pool: None,
        networks: std::sync::Arc::new(tokio::sync::RwLock::new(
            visor_runtime::api::routes::networks::NetworkManager::new(),
        )),
        dns: std::sync::Arc::new(tokio::sync::RwLock::new(
            visor_runtime::net::dns::DnsRegistry::new(),
        )),
    }
}

/// Creates a minimal gzipped tar archive containing a single text file.
///
/// The archive contains `hello.txt` with content `"Hello from test layer\n"`.
///
/// # Errors
///
/// Returns an error if the tar/gzip file cannot be created.
fn create_test_tar_gz(path: &std::path::Path) -> anyhow::Result<()> {
    use flate2::Compression;
    use flate2::write::GzEncoder;

    let file = std::fs::File::create(path).context("create tar.gz file")?;
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = tar::Builder::new(encoder);

    let content = b"Hello from test layer\n";
    let mut header = tar::Header::new_gnu();
    header.set_path("hello.txt").context("set tar entry path")?;
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();

    builder
        .append(&header, &content[..])
        .context("append tar entry")?;
    let encoder = builder.into_inner().context("finish tar archive")?;
    encoder.finish().context("finish gzip encoding")?;

    Ok(())
}

fn workspace_tempdir() -> std::io::Result<tempfile::TempDir> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("integration-tests");
    std::fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix("visor-e2e-")
        .tempdir_in(root)
        .map_err(std::io::Error::from)
}

/// A tracing layer that captures formatted log output for assertions.
struct CapturingLayer {
    lines: Arc<Mutex<Vec<String>>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturingLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = StringVisitor::default();
        event.record(&mut visitor);
        if let Ok(mut lines) = self.lines.lock() {
            lines.push(visitor.output);
        }
    }
}

/// Field visitor that concatenates tracing fields into a single string.
#[derive(Default)]
struct StringVisitor {
    output: String,
}

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let _ = write!(self.output, "{}={:?} ", field.name(), value);
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write;
        let _ = write!(self.output, "{}={} ", field.name(), value);
    }
}

/// Creates a tracing subscriber that captures all events into a shared vec.
///
/// Returns `(subscriber, captured_lines)` — use `tracing::subscriber::with_default`
/// to install the subscriber, then inspect `captured_lines` after the test.
fn capturing_subscriber() -> (impl tracing::Subscriber, Arc<Mutex<Vec<String>>>) {
    use tracing_subscriber::layer::SubscriberExt as _;

    let lines: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let layer = CapturingLayer {
        lines: Arc::clone(&lines),
    };
    let subscriber = tracing_subscriber::registry().with(layer);
    (subscriber, lines)
}

// ═════════════════════════════════════════════════════════════════════
// Test 1: Backend — VmmBackend state management + VmConfig
// ═════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn backend_new_starts_empty() {
    let backend = VmmBackend::new();
    let vms = backend.list().await.unwrap();
    assert!(vms.is_empty(), "fresh backend should have no VMs");
}

#[tokio::test]
async fn backend_get_nonexistent_returns_error() {
    let backend = VmmBackend::new();
    let result = backend.get("nonexistent-vm-id").await;
    assert!(result.is_err());
    let msg = format!("{}", result.unwrap_err());
    assert!(msg.contains("vm not found"), "error was: {msg}");
}

#[tokio::test]
async fn backend_stop_nonexistent_returns_error() {
    let backend = VmmBackend::new();
    let result = backend.stop("nonexistent-vm-id", 10).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn backend_destroy_nonexistent_returns_error() {
    let backend = VmmBackend::new();
    let result = backend.destroy("nonexistent-vm-id").await;
    assert!(result.is_err());
}

#[test]
fn vm_config_deserializes_with_defaults() {
    let config: VmConfig = serde_json::from_str(r#"{"image": "alpine:latest"}"#).unwrap();
    assert_eq!(config.image, "alpine:latest");
    assert_eq!(config.memory_mib, 512);
    assert_eq!(config.vcpus, 1);
    assert!(config.cmd.is_empty());
    assert!(config.env.is_empty());
    assert!(config.ports.is_empty());
    assert!(config.volumes.is_empty());
}

#[test]
fn vm_config_round_trips_through_json() {
    let json = r#"{
        "image": "ubuntu:22.04",
        "cmd": ["/bin/bash"],
        "env": ["FOO=bar"],
        "memory_mib": 1024,
        "vcpus": 2,
        "name": "integration-test-vm"
    }"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    let serialized = serde_json::to_string(&config).unwrap();
    let roundtripped: VmConfig = serde_json::from_str(&serialized).unwrap();
    assert_eq!(roundtripped.image, "ubuntu:22.04");
    assert_eq!(roundtripped.memory_mib, 1024);
    assert_eq!(roundtripped.vcpus, 2);
    assert_eq!(roundtripped.name.as_deref(), Some("integration-test-vm"),);
}

#[test]
fn vm_state_default_is_creating() {
    assert_eq!(VmState::default(), VmState::Creating);
}

#[test]
fn vm_state_serializes_all_variants() {
    assert_eq!(
        serde_json::to_string(&VmState::Creating).unwrap(),
        r#""creating""#,
    );
    assert_eq!(
        serde_json::to_string(&VmState::Running).unwrap(),
        r#""running""#,
    );
    assert_eq!(
        serde_json::to_string(&VmState::Stopped).unwrap(),
        r#""stopped""#,
    );
    assert_eq!(
        serde_json::to_string(&VmState::Failed).unwrap(),
        r#""failed""#,
    );
}

#[cfg(target_os = "linux")]
#[test]
fn kvm_platform_can_be_created() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let platform = KvmPlatform::new();
    assert!(
        platform.is_ok(),
        "KVM platform creation should succeed: {:?}",
        platform.err(),
    );

    let platform = platform.unwrap();
    assert_eq!(
        platform.kvm().get_api_version(),
        12,
        "KVM API version should be 12"
    );
}

#[cfg(target_os = "linux")]
#[test]
fn kvm_platform_can_create_vm() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let platform = KvmPlatform::new().unwrap();
    let vm_fd = platform.create_vm();
    assert!(
        vm_fd.is_ok(),
        "VM creation should succeed: {:?}",
        vm_fd.err(),
    );
}

// ═════════════════════════════════════════════════════════════════════
// Test 2: OCI pipeline — reference parsing, layer merge, rootfs build
// ═════════════════════════════════════════════════════════════════════

#[test]
fn oci_parse_bare_image_name() {
    let r = ImageReference::parse("alpine").unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "library/alpine");
    assert_eq!(r.tag().unwrap().as_ref(), "latest");
}

#[test]
fn oci_parse_tagged_image() {
    let r = ImageReference::parse("alpine:3.20").unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "library/alpine");
    assert_eq!(r.tag().unwrap().as_ref(), "3.20");
}

#[test]
fn oci_parse_fully_qualified_reference() {
    let r = ImageReference::parse("ghcr.io/owner/repo:v1.0").unwrap();
    assert_eq!(r.registry().as_ref(), "ghcr.io");
    assert_eq!(r.repository().as_ref(), "owner/repo");
    assert_eq!(r.tag().unwrap().as_ref(), "v1.0");
}

#[test]
fn oci_parse_registry_with_port() {
    let r = ImageReference::parse("registry.local:5000/myapp:2.0").unwrap();
    assert_eq!(r.registry().as_ref(), "registry.local:5000");
    assert_eq!(r.repository().as_ref(), "myapp");
    assert_eq!(r.tag().unwrap().as_ref(), "2.0");
}

#[test]
fn oci_parse_digest_reference() {
    let digest = "sha256:abcdef1234567890";
    let input = format!("alpine@{digest}");
    let r = ImageReference::parse(&input).unwrap();
    assert_eq!(r.registry().as_ref(), "docker.io");
    assert_eq!(r.repository().as_ref(), "library/alpine");
    assert!(r.tag().is_none(), "digest ref should have no tag");
    assert_eq!(r.digest().unwrap().as_ref(), digest);
}

#[test]
fn oci_parse_empty_reference_fails() {
    let result = ImageReference::parse("");
    assert!(result.is_err());
}

#[test]
fn oci_parse_display_round_trip() {
    let r = ImageReference::parse("ghcr.io/owner/repo:v2.0").unwrap();
    let displayed = r.to_string();
    assert_eq!(displayed, "ghcr.io/owner/repo:v2.0");
}

#[test]
fn oci_layer_merger_creates_target_and_unpacks() {
    let tmp = workspace_tempdir().unwrap();
    let target = tmp.path().join("merged");

    let tar_path = tmp.path().join("test-layer.tar.gz");
    create_test_tar_gz(&tar_path).unwrap();

    let merger = LayerMerger::new(&target).unwrap();
    merger.unpack_layer(&tar_path).unwrap();

    assert!(
        target.join("hello.txt").exists(),
        "hello.txt should be extracted",
    );
    let content = std::fs::read_to_string(target.join("hello.txt")).unwrap();
    assert_eq!(content, "Hello from test layer\n");
}

#[test]
fn oci_layer_merger_multiple_layers_overlay() {
    let tmp = workspace_tempdir().unwrap();
    let target = tmp.path().join("merged");

    // Layer 1: base file
    let layer1_path = tmp.path().join("layer1.tar.gz");
    create_test_tar_gz(&layer1_path).unwrap();

    // Layer 2: overwrite with different content
    let layer2_path = tmp.path().join("layer2.tar.gz");
    {
        use flate2::Compression;
        use flate2::write::GzEncoder;

        let file = std::fs::File::create(&layer2_path).unwrap();
        let encoder = GzEncoder::new(file, Compression::default());
        let mut builder = tar::Builder::new(encoder);

        let content = b"Overwritten by layer 2\n";
        let mut header = tar::Header::new_gnu();
        header.set_path("hello.txt").unwrap();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder.append(&header, &content[..]).unwrap();

        let encoder = builder.into_inner().unwrap();
        encoder.finish().unwrap();
    }

    let merger = LayerMerger::new(&target).unwrap();
    merger.merge_layers(&[layer1_path, layer2_path]).unwrap();

    let content = std::fs::read_to_string(target.join("hello.txt")).unwrap();
    assert_eq!(content, "Overwritten by layer 2\n");
}

#[test]
fn oci_rootfs_builder_rejects_missing_source() {
    let tmp = workspace_tempdir().unwrap();
    let nonexistent = tmp.path().join("does-not-exist");
    let output = tmp.path().join("rootfs.ext4");
    let builder = RootfsBuilder::new(&nonexistent, &output);
    let result = builder.build();
    assert!(result.is_err(), "should fail for nonexistent source dir");
}

#[test]
fn oci_rootfs_builder_creates_ext4_from_directory() {
    let tmp = workspace_tempdir().unwrap();
    let source = tmp.path().join("source");
    std::fs::create_dir_all(&source).unwrap();

    // Populate with test files.
    std::fs::write(source.join("hello.txt"), "Hello from visor e2e test").unwrap();
    std::fs::create_dir_all(source.join("subdir")).unwrap();
    std::fs::write(source.join("subdir/nested.txt"), "Nested file").unwrap();

    let output = tmp.path().join("rootfs.ext4");
    let builder = RootfsBuilder::new(&source, &output);
    let result = builder.build();

    // mke2fs must be available. If it is, verify the image; otherwise
    // confirm the error is about mke2fs (not something else).
    match result {
        Ok(path) => {
            assert!(path.exists(), "rootfs image should exist");
            let metadata = std::fs::metadata(&path).unwrap();
            assert!(metadata.len() > 0, "rootfs image should not be empty");
        }
        Err(e) => {
            let msg = format!("{e:#}");
            assert!(
                msg.contains("mke2fs"),
                "failure should be about mke2fs, got: {msg}",
            );
        }
    }
}

// ═════════════════════════════════════════════════════════════════════
// Test 3: CLI argument parsing — all subcommands
// ═════════════════════════════════════════════════════════════════════

#[test]
fn cli_parse_start_defaults() {
    let cli = Cli::try_parse_from(["visor", "start"]).unwrap();
    match cli.command {
        Command::Start(args) => {
            assert_eq!(args.listen, "0.0.0.0:7800");
            assert!(!args.foreground);
        }
        other => panic!("expected Start, got {other:?}"),
    }
}

#[test]
fn cli_parse_start_with_listen_and_foreground() {
    let cli = Cli::try_parse_from([
        "visor",
        "start",
        "--listen",
        "127.0.0.1:9000",
        "--foreground",
    ])
    .unwrap();
    match cli.command {
        Command::Start(args) => {
            assert_eq!(args.listen, "127.0.0.1:9000");
            assert!(args.foreground);
        }
        other => panic!("expected Start, got {other:?}"),
    }
}

#[test]
fn cli_parse_run_minimal() {
    let cli = Cli::try_parse_from(["visor", "run", "alpine:latest"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.image, "alpine:latest");
            assert!(args.cmd.is_empty());
            assert_eq!(args.memory, 512);
            assert_eq!(args.cpus, 1);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn cli_parse_run_with_all_options() {
    let cli = Cli::try_parse_from([
        "visor",
        "run",
        "-m",
        "1024",
        "--cpus",
        "2",
        "--name",
        "test-vm",
        "-e",
        "FOO=bar",
        "-w",
        "/app",
        "-p",
        "8080:80",
        "alpine:latest",
        "echo",
        "hello",
    ])
    .unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.image, "alpine:latest");
            assert_eq!(args.memory, 1024);
            assert_eq!(args.cpus, 2);
            assert_eq!(args.name.as_deref(), Some("test-vm"));
            assert_eq!(args.env, vec!["FOO=bar"]);
            assert_eq!(args.workdir.as_deref(), Some("/app"));
            assert_eq!(args.port, vec!["8080:80"]);
            assert_eq!(args.cmd, vec!["echo", "hello"]);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn cli_parse_exec() {
    let cli = Cli::try_parse_from(["visor", "exec", "vm-123", "ls", "-la"]).unwrap();
    match cli.command {
        Command::Exec(args) => {
            assert_eq!(args.vm_id, "vm-123");
            assert_eq!(args.cmd, vec!["ls", "-la"]);
        }
        other => panic!("expected Exec, got {other:?}"),
    }
}

#[test]
fn cli_parse_ps() {
    let cli = Cli::try_parse_from(["visor", "ps"]).unwrap();
    assert!(matches!(cli.command, Command::Ps));
}

#[test]
fn cli_parse_stop_with_vm_id() {
    let cli = Cli::try_parse_from(["visor", "stop", "vm-456"]).unwrap();
    match cli.command {
        Command::Stop(args) => {
            assert_eq!(args.vm_id.as_deref(), Some("vm-456"));
        }
        other => panic!("expected Stop, got {other:?}"),
    }
}

#[test]
fn cli_parse_stop_daemon() {
    let cli = Cli::try_parse_from(["visor", "stop"]).unwrap();
    match cli.command {
        Command::Stop(args) => {
            assert!(args.vm_id.is_none(), "stop without vm_id stops daemon");
        }
        other => panic!("expected Stop, got {other:?}"),
    }
}

#[test]
fn cli_parse_shell() {
    let cli = Cli::try_parse_from(["visor", "shell", "vm-789"]).unwrap();
    match cli.command {
        Command::Shell(args) => {
            assert_eq!(args.vm_id, "vm-789");
        }
        other => panic!("expected Shell, got {other:?}"),
    }
}

#[test]
fn cli_parse_info() {
    let cli = Cli::try_parse_from(["visor", "info"]).unwrap();
    assert!(matches!(cli.command, Command::Info));
}

#[test]
fn cli_parse_global_addr_option() {
    let cli = Cli::try_parse_from(["visor", "--addr", "http://192.168.1.1:8080", "ps"]).unwrap();
    assert_eq!(cli.addr, "http://192.168.1.1:8080");
}

#[test]
fn cli_parse_default_addr() {
    let cli = Cli::try_parse_from(["visor", "ps"]).unwrap();
    assert_eq!(cli.addr, "http://127.0.0.1:7800");
}

#[test]
fn cli_parse_invalid_subcommand_fails() {
    let result = Cli::try_parse_from(["visor", "nonexistent"]);
    assert!(result.is_err());
}

#[test]
fn cli_parse_port_mapping_valid() {
    let mapping = visor_runtime::cli::parse_port_mapping("8080:80").unwrap();
    assert_eq!(mapping.host_port, 8080);
    assert_eq!(mapping.guest_port, 80);
    assert_eq!(mapping.protocol, "tcp");
}

#[test]
fn cli_parse_port_mapping_invalid_fails() {
    let result = visor_runtime::cli::parse_port_mapping("invalid");
    assert!(result.is_err());
}

// ═════════════════════════════════════════════════════════════════════
// Test 4: API routes — via tower::ServiceExt::oneshot
// ═════════════════════════════════════════════════════════════════════

#[tokio::test]
async fn api_health_returns_200() {
    let app = build_router(test_app_state());
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
async fn api_info_returns_json_with_version_and_mode() {
    let app = build_router(test_app_state());
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

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let info: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(info["version"].is_string(), "should have version");
    let mode = info["mode"].as_str().expect("should have mode");
    assert!(
        mode == "kvm" || mode == "hvf",
        "mode should be 'kvm' or 'hvf', got: {mode}",
    );
    assert_eq!(info["vm_count"], 0);
    assert!(info["uptime_secs"].is_number(), "should have uptime_secs",);
}

#[tokio::test]
async fn api_list_vms_returns_empty_array() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/vms")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let vms: Vec<serde_json::Value> = serde_json::from_slice(&bytes).unwrap();
    assert!(vms.is_empty(), "fresh backend should list zero VMs");
}

#[tokio::test]
async fn api_get_nonexistent_vm_returns_404() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/vms/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_destroy_nonexistent_vm_returns_404() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .method("DELETE")
                .uri("/v1/vms/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_stop_nonexistent_vm_returns_404() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/vms/does-not-exist/stop")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_shutdown_returns_200() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/shutdown")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        body["status"].is_string(),
        "shutdown response should have status",
    );
}

#[tokio::test]
async fn api_unknown_route_returns_404() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/nonexistent")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn api_openapi_spec_is_valid() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api-docs/openapi.json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let bytes = axum::body::to_bytes(response.into_body(), 2 * 1024 * 1024)
        .await
        .unwrap();
    let spec: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert!(
        spec["openapi"].is_string(),
        "should have openapi version field",
    );
    assert!(spec["paths"].is_object(), "should have paths object");
}

#[tokio::test]
async fn api_create_vm_without_body_returns_error() {
    let app = build_router(test_app_state());
    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/vms")
                .header("content-type", "application/json")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    // Missing/empty body should be rejected (422 or 400).
    assert_ne!(
        response.status(),
        StatusCode::OK,
        "empty body should not succeed",
    );
    assert_ne!(
        response.status(),
        StatusCode::CREATED,
        "empty body should not create VM",
    );
}

// ═════════════════════════════════════════════════════════════════════
// Test 5: SSE event broadcasting
// ═════════════════════════════════════════════════════════════════════

#[test]
fn sse_broadcaster_reports_capacity() {
    let bc = EventBroadcaster::new(64);
    assert_eq!(bc.capacity(), 64);
}

#[test]
fn sse_send_without_receivers_does_not_panic() {
    let bc = EventBroadcaster::new(16);
    bc.send(VmEvent::new("vm.created", "orphan-event"));
    // No panic = success.
}

#[tokio::test]
async fn sse_send_and_receive_single_event() {
    let bc = EventBroadcaster::new(16);
    let mut rx = bc.subscribe();

    bc.send(VmEvent::new("vm.created", "vm-e2e-001"));

    let event = rx.recv().await.expect("should receive event");
    assert_eq!(event.event_type, "vm.created");
    assert_eq!(event.vm_id, "vm-e2e-001");
}

#[tokio::test]
async fn sse_multiple_receivers_all_get_event() {
    let bc = EventBroadcaster::new(16);
    let mut rx1 = bc.subscribe();
    let mut rx2 = bc.subscribe();

    bc.send(VmEvent::new("vm.stopped", "vm-multi"));

    let e1 = rx1.recv().await.expect("rx1 should receive");
    let e2 = rx2.recv().await.expect("rx2 should receive");
    assert_eq!(e1.event_type, "vm.stopped");
    assert_eq!(e2.event_type, "vm.stopped");
    assert_eq!(e1.vm_id, "vm-multi");
    assert_eq!(e2.vm_id, "vm-multi");
}

#[tokio::test]
async fn sse_multiple_events_arrive_in_order() {
    let bc = EventBroadcaster::new(16);
    let mut rx = bc.subscribe();

    bc.send(VmEvent::new("vm.created", "vm-order"));
    bc.send(VmEvent::new("vm.stopped", "vm-order"));

    let e1 = rx.recv().await.expect("first event");
    let e2 = rx.recv().await.expect("second event");
    assert_eq!(e1.event_type, "vm.created");
    assert_eq!(e2.event_type, "vm.stopped");
}

#[test]
fn sse_event_serializes_to_json() {
    let event = VmEvent::new("vm.destroyed", "vm-ser")
        .with_data(serde_json::json!({"reason": "user request"}));
    let json = serde_json::to_string(&event).unwrap();
    let decoded: VmEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(decoded.event_type, "vm.destroyed");
    assert_eq!(decoded.vm_id, "vm-ser");
    assert_eq!(decoded.data["reason"], "user request");
}

#[test]
fn sse_event_fields_set_correctly() {
    let event = VmEvent::new("vm.created", "vm-fields");
    assert_eq!(event.event_type, "vm.created");
    assert_eq!(event.vm_id, "vm-fields");
    assert_eq!(event.data, serde_json::Value::Null);
}

// ═════════════════════════════════════════════════════════════════════
// Test 6: Audit logging — structured events + tracing emission
// ═════════════════════════════════════════════════════════════════════

#[test]
fn audit_event_has_valid_iso8601_timestamp() {
    let event = AuditEvent::new(AuditAction::VmCreate, AuditOutcome::Success);
    let ts = &event.timestamp;
    assert_eq!(ts.len(), 20, "ISO 8601 should be 20 chars: {ts}");
    assert!(ts.contains('T'), "should contain 'T': {ts}");
    assert!(ts.ends_with('Z'), "should end with 'Z': {ts}");
}

#[test]
fn audit_event_builder_sets_target_and_detail() {
    let event = AuditEvent::new(AuditAction::VmDestroy, AuditOutcome::Failure)
        .with_target("vm-audit-test")
        .with_detail("test detail");
    assert_eq!(event.target.as_deref(), Some("vm-audit-test"));
    assert_eq!(event.detail.as_deref(), Some("test detail"));
    assert_eq!(event.action, AuditAction::VmDestroy);
    assert_eq!(event.outcome, AuditOutcome::Failure);
}

#[test]
fn audit_event_json_round_trip() {
    let event = AuditEvent::new(AuditAction::VmExec, AuditOutcome::Success)
        .with_target("vm-roundtrip")
        .with_detail("executed /bin/ls");

    let json = serde_json::to_string(&event).unwrap();
    let decoded: AuditEvent = serde_json::from_str(&json).unwrap();

    assert_eq!(decoded.action, AuditAction::VmExec);
    assert_eq!(decoded.outcome, AuditOutcome::Success);
    assert_eq!(decoded.target.as_deref(), Some("vm-roundtrip"));
    assert_eq!(decoded.detail.as_deref(), Some("executed /bin/ls"));
    assert_eq!(decoded.timestamp, event.timestamp);
}

#[test]
fn audit_action_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditAction::VmCreate).unwrap(),
        r#""vm_create""#,
    );
    assert_eq!(
        serde_json::to_string(&AuditAction::DaemonStart).unwrap(),
        r#""daemon_start""#,
    );
    assert_eq!(
        serde_json::to_string(&AuditAction::DaemonStop).unwrap(),
        r#""daemon_stop""#,
    );
}

#[test]
fn audit_outcome_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditOutcome::Success).unwrap(),
        r#""success""#,
    );
    assert_eq!(
        serde_json::to_string(&AuditOutcome::Failure).unwrap(),
        r#""failure""#,
    );
}

#[test]
fn audit_emit_writes_structured_tracing_event() {
    let (subscriber, lines) = capturing_subscriber();
    tracing::subscriber::with_default(subscriber, || {
        let event = AuditEvent::new(AuditAction::DaemonStart, AuditOutcome::Success)
            .with_detail("e2e integration test");
        visor_runtime::audit::emit(&event);
    });

    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 1, "expected one tracing event");
    let line = &captured[0];
    assert!(line.contains("daemon_start"), "missing action in: {line}",);
    assert!(line.contains("success"), "missing outcome in: {line}");
    assert!(
        line.contains("e2e integration test"),
        "missing detail in: {line}",
    );
}

#[test]
fn audit_log_success_emits_success_event() {
    let (subscriber, lines) = capturing_subscriber();
    tracing::subscriber::with_default(subscriber, || {
        visor_runtime::audit::log_success(
            AuditAction::VmCreate,
            Some("vm-success"),
            Some("created from alpine"),
        );
    });

    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 1, "expected one tracing event");
    let line = &captured[0];
    assert!(line.contains("vm_create"), "missing action in: {line}",);
    assert!(line.contains("success"), "missing outcome in: {line}");
    assert!(line.contains("vm-success"), "missing target in: {line}",);
}

#[test]
fn audit_log_failure_emits_failure_event() {
    let (subscriber, lines) = capturing_subscriber();
    tracing::subscriber::with_default(subscriber, || {
        visor_runtime::audit::log_failure(
            AuditAction::VmDestroy,
            Some("vm-fail"),
            Some("vm not found"),
        );
    });

    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 1, "expected one tracing event");
    let line = &captured[0];
    assert!(line.contains("vm_destroy"), "missing action in: {line}",);
    assert!(line.contains("failure"), "missing outcome in: {line}");
    assert!(line.contains("vm-fail"), "missing target in: {line}",);
    assert!(line.contains("vm not found"), "missing detail in: {line}",);
}

// ═════════════════════════════════════════════════════════════════════
// Test 7: Real E2E — OCI pull + KVM boot + output capture
// ═════════════════════════════════════════════════════════════════════
//
// These tests run the full pipeline: OCI image pull → rootfs build →
// KVM microVM boot → visor-init → command execution → output capture.
// They require /dev/kvm and network access (for OCI registry).

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_run_alpine_echo_hello() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let backend = VmmBackend::new();
    let config: VmConfig = serde_json::from_value(serde_json::json!({
        "image": "alpine",
        "cmd": ["echo", "hello"]
    }))
    .unwrap();

    let result = backend.create(config).await;
    assert!(
        result.is_ok(),
        "VM creation should succeed: {:?}",
        result.err()
    );

    let vm = result.unwrap();
    assert_eq!(
        vm.state,
        VmState::Stopped,
        "VM should be stopped after completion"
    );
    assert_eq!(vm.exit_code, Some(0), "exit code should be 0");
    assert!(
        vm.stdout.as_deref().unwrap_or("").contains("hello"),
        "stdout should contain 'hello', got: {:?}",
        vm.stdout,
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_run_alpine_with_env_vars() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let backend = VmmBackend::new();
    let config: VmConfig = serde_json::from_value(serde_json::json!({
        "image": "alpine",
        "cmd": ["sh", "-c", "echo $VISOR_TEST_VAR"],
        "env": ["VISOR_TEST_VAR=e2e_works"]
    }))
    .unwrap();

    let result = backend.create(config).await;
    assert!(
        result.is_ok(),
        "VM creation should succeed: {:?}",
        result.err()
    );

    let vm = result.unwrap();
    assert_eq!(
        vm.exit_code,
        Some(0),
        "expected exit 0, got {:?}; stdout={:?}",
        vm.exit_code,
        vm.stdout,
    );
    assert!(
        vm.stdout.as_deref().unwrap_or("").contains("e2e_works"),
        "stdout should contain env var value, got: {:?}",
        vm.stdout,
    );
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_run_alpine_with_env_vars_reports_exit_code_reliably() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    for attempt in 1..=10 {
        let backend = VmmBackend::new();
        let config: VmConfig = serde_json::from_value(serde_json::json!({
            "image": "alpine",
            "cmd": ["sh", "-c", "echo $VISOR_TEST_VAR"],
            "env": ["VISOR_TEST_VAR=e2e_works"]
        }))
        .unwrap();

        let result = backend.create(config).await;
        assert!(
            result.is_ok(),
            "VM creation should succeed on attempt {attempt}: {:?}",
            result.err(),
        );

        let vm = result.unwrap();
        assert_eq!(
            vm.exit_code,
            Some(0),
            "expected exit 0 on attempt {attempt}, got {:?}; stdout={:?}",
            vm.exit_code,
            vm.stdout,
        );
        assert!(
            vm.stdout.as_deref().unwrap_or("").contains("e2e_works"),
            "stdout should contain env var value on attempt {attempt}, got: {:?}",
            vm.stdout,
        );
    }
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_run_alpine_exit_code_nonzero() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let backend = VmmBackend::new();
    let config: VmConfig = serde_json::from_value(serde_json::json!({
        "image": "alpine",
        "cmd": ["sh", "-c", "exit 42"]
    }))
    .unwrap();

    let result = backend.create(config).await;
    assert!(
        result.is_ok(),
        "VM creation should succeed even with non-zero exit: {:?}",
        result.err()
    );

    let vm = result.unwrap();
    assert_eq!(vm.state, VmState::Stopped);
    assert_eq!(vm.exit_code, Some(42), "exit code should be 42");
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_run_alpine_multiline_output() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let backend = VmmBackend::new();
    let config: VmConfig = serde_json::from_value(serde_json::json!({
        "image": "alpine",
        "cmd": ["sh", "-c", "echo line1; echo line2; echo line3"]
    }))
    .unwrap();

    let result = backend.create(config).await;
    assert!(
        result.is_ok(),
        "VM creation should succeed: {:?}",
        result.err()
    );

    let vm = result.unwrap();
    let stdout = vm.stdout.as_deref().unwrap_or("");
    assert!(stdout.contains("line1"), "should contain line1");
    assert!(stdout.contains("line2"), "should contain line2");
    assert!(stdout.contains("line3"), "should contain line3");
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_list_shows_created_vms() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let backend = VmmBackend::new();
    let config: VmConfig = serde_json::from_value(serde_json::json!({
        "image": "alpine",
        "cmd": ["true"],
        "name": "e2e-list-test"
    }))
    .unwrap();

    let vm = backend.create(config).await.unwrap();
    let list = backend.list().await.unwrap();

    assert!(!list.is_empty(), "list should not be empty after create");
    let found = list.iter().find(|v| v.id == vm.id);
    assert!(found.is_some(), "created VM should appear in list");

    let found = found.unwrap();
    assert_eq!(found.state, VmState::Stopped);
    assert_eq!(found.name.as_deref(), Some("e2e-list-test"));
    assert_eq!(found.image, "alpine");
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_detached_vm_accepts_exec_commands() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let backend = VmmBackend::new();
    let mut config = VmConfig::new("alpine:latest");
    config.cmd = vec!["sleep".to_owned(), "60".to_owned()];
    config.detach = true;
    config.name = Some("e2e-detached-exec".to_owned());

    let vm = backend.create(config).await.unwrap();
    assert_eq!(vm.state, VmState::Running);

    let exec_result = backend
        .exec(
            &vm.id,
            visor_runtime::backend::ExecRequest::new(vec![
                "sh".to_owned(),
                "-lc".to_owned(),
                "echo detached-exec-ok".to_owned(),
            ]),
        )
        .await;

    let result = match exec_result {
        Ok(result) => result,
        Err(error) => {
            let serial = backend
                .console_output(&vm.id)
                .await
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|| "<serial unavailable>".to_owned());
            panic!("detached VM exec should succeed: {error}\nserial output:\n{serial}");
        }
    };

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.trim(), "detached-exec-ok");

    backend.stop(&vm.id, 10).await.unwrap();
    backend.destroy(&vm.id).await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_nested_builder_vm_reaches_alpine_mirrors_and_runs_qemu_img() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let backend = VmmBackend::new();
    let config: VmConfig = serde_json::from_value(serde_json::json!({
        "image": "alpine:latest",
        "cmd": ["sleep", "600"],
        "detach": true,
        "guest_virtualization": "nested",
        "name": "e2e-nested-builder"
    }))
    .unwrap();

    let vm = backend.create(config).await.unwrap();
    assert_eq!(vm.state, VmState::Running);

    let exec_result = backend
        .exec(
            &vm.id,
            visor_runtime::backend::ExecRequest::new(vec![
                "sh".to_owned(),
                "-lc".to_owned(),
                "test -c /dev/kvm && apk add --no-cache qemu-img >/dev/null && qemu-img create -f qcow2 /tmp/test.qcow2 64M && qemu-img info /tmp/test.qcow2"
                    .to_owned(),
            ]),
        )
        .await;

    let result = match exec_result {
        Ok(result) => result,
        Err(error) => {
            let serial = backend
                .console_output(&vm.id)
                .await
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|| "<serial unavailable>".to_owned());
            let _ = backend.stop(&vm.id, 10).await;
            let _ = backend.destroy(&vm.id).await;
            panic!("nested builder exec should succeed: {error}\nserial output:\n{serial}");
        }
    };

    assert_eq!(
        result.exit_code, 0,
        "nested builder guest should complete successfully:\nstdout:\n{}\n\nstderr:\n{}",
        result.stdout, result.stderr
    );
    assert!(
        result.stdout.contains("file format: qcow2"),
        "stdout should contain qemu-img info output:\n{}",
        result.stdout
    );
    assert!(
        result.stdout.contains("virtual size: 64 MiB"),
        "stdout should show the created qcow2 size:\n{}",
        result.stdout
    );

    backend.stop(&vm.id, 10).await.unwrap();
    backend.destroy(&vm.id).await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_detached_vm_streaming_exec_returns_docker_frames() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let backend = VmmBackend::new();
    let mut config = VmConfig::new("alpine:latest");
    config.cmd = vec!["sleep".to_owned(), "60".to_owned()];
    config.detach = true;
    config.name = Some("e2e-detached-stream-exec".to_owned());

    let vm = backend.create(config).await.unwrap();
    assert_eq!(vm.state, VmState::Running);

    let stream_result = backend
        .exec_stream(
            &vm.id,
            visor_runtime::backend::ExecRequest::new(vec![
                "sh".to_owned(),
                "-lc".to_owned(),
                "echo detached-stream-ok".to_owned(),
            ]),
        )
        .await;

    let mut stream = match stream_result {
        Ok(stream) => stream,
        Err(error) => {
            let serial = backend
                .console_output(&vm.id)
                .await
                .ok()
                .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                .unwrap_or_else(|| "<serial unavailable>".to_owned());
            panic!("detached VM streaming exec should succeed: {error}\nserial output:\n{serial}");
        }
    };

    let mut header = [0u8; 8];
    tokio::io::AsyncReadExt::read_exact(&mut stream, &mut header)
        .await
        .expect("stream should include Docker frame header");
    assert_eq!(header[0], 1, "stdout frame type should be 1");
    let payload_len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let mut payload = vec![0u8; payload_len];
    tokio::io::AsyncReadExt::read_exact(&mut stream, &mut payload)
        .await
        .expect("stream should include Docker frame payload");
    assert_eq!(
        String::from_utf8(payload).unwrap().trim(),
        "detached-stream-ok"
    );

    backend.stop(&vm.id, 10).await.unwrap();
    backend.destroy(&vm.id).await.unwrap();
}

#[cfg(target_os = "linux")]
#[tokio::test]
#[serial]
async fn e2e_api_create_vm_returns_output() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let app = build_router(test_app_state());
    let body = serde_json::json!({
        "image": "alpine",
        "cmd": ["echo", "api-e2e-test"]
    });

    let response = app
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/v1/vms")
                .header("content-type", "application/json")
                .body(Body::from(serde_json::to_string(&body).unwrap()))
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);

    let bytes = axum::body::to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap();
    let vm: serde_json::Value = serde_json::from_slice(&bytes).unwrap();

    assert_eq!(vm["state"], "stopped");
    assert_eq!(vm["exit_code"], 0, "expected exit 0, got payload: {vm}",);
    assert!(
        vm["stdout"].as_str().unwrap_or("").contains("api-e2e-test"),
        "stdout should contain command output, got: {}",
        vm["stdout"],
    );
}
