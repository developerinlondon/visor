//! Integration tests for the real build service path.
//!
//! These tests exercise the same VM-backed build executor used by the Docker
//! `/build` endpoint, without going through the HTTP compatibility layer.

use std::sync::Arc;

use serial_test::serial;
use visor_types::{BuildRequest, BuildService, ExecutionBackend};

fn build_test_tempdir() -> std::io::Result<tempfile::TempDir> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("visor-runtime-build-tests");
    std::fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix("visor-runtime-build-")
        .tempdir_in(root)
        .map_err(std::io::Error::from)
}

#[tokio::test]
#[serial]
async fn build_service_builds_scratch_copy_image() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(visor_runtime::backend::VmmBackend::new());
    let image_store_dir = build_test_tempdir().expect("create image store dir");
    let service = visor_runtime::vsock::build_service::VmmBuildService::new(
        backend,
        image_store_dir.path().to_path_buf(),
    );

    let context_dir = build_test_tempdir().expect("create build context dir");
    std::fs::write(context_dir.path().join("hello.txt"), "hello\n").expect("write build input");

    let mut request = BuildRequest::new("FROM scratch\nCOPY hello.txt /hello.txt\n".to_owned());
    request.context_dir = context_dir.path().to_path_buf();
    request.tag = Some("visor-test:scratch".to_owned());

    let result = service.build_image(request).await;
    match result {
        Ok(output) => {
            assert!(
                !output.image_id.is_empty(),
                "built image should have a manifest digest"
            );
        }
        Err(error) => {
            panic!("real build should succeed:\n{error:#}");
        }
    }
}

#[tokio::test]
#[serial]
async fn build_service_runs_alpine_command_layer() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(visor_runtime::backend::VmmBackend::new());
    let image_store_dir = build_test_tempdir().expect("create image store dir");
    let service = visor_runtime::vsock::build_service::VmmBuildService::new(
        backend,
        image_store_dir.path().to_path_buf(),
    );

    let context_dir = build_test_tempdir().expect("create build context dir");
    let mut request =
        BuildRequest::new("FROM alpine:latest\nRUN printf 'run-ok\\n' >/run-ok.txt\n".to_owned());
    request.context_dir = context_dir.path().to_path_buf();
    request.tag = Some("visor-test:run".to_owned());

    let result = service.build_image(request).await;
    match result {
        Ok(output) => {
            assert!(
                !output.image_id.is_empty(),
                "built image should have a manifest digest"
            );
        }
        Err(error) => {
            panic!("real RUN build should succeed:\n{error:#}");
        }
    }
}
