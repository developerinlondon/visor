//! Tests for [`VmmBuildService`].

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use visor_build::{ImageAssembler, ImageMetadata, ImageStore, LayerCreator, ProcessedLayer};
use visor_types::{
    BuildRequest, BuildService, ExecRequest, ExecResult, ExecutionBackend, VmConfig, VmInfo,
    VmState,
};

use super::VmmBuildService;

// ── Mock Backend ────────────────────────────────────────────────────

/// Mock backend that simulates VM lifecycle for build tests.
///
/// Records calls to `create` and `destroy` and can be configured to
/// return specific CIDs for vsock connection.
#[derive(Debug)]
struct MockBuildBackend {
    /// Records the VM IDs of created VMs.
    created: tokio::sync::Mutex<Vec<String>>,
    /// Records the VM IDs of destroyed VMs.
    destroyed: tokio::sync::Mutex<Vec<String>>,
    /// Records the VmConfigs passed to create.
    configs: tokio::sync::Mutex<Vec<VmConfig>>,
}

impl MockBuildBackend {
    fn new() -> Self {
        Self {
            created: tokio::sync::Mutex::new(Vec::new()),
            destroyed: tokio::sync::Mutex::new(Vec::new()),
            configs: tokio::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl ExecutionBackend for MockBuildBackend {
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        let id = format!("build-vm-{}", uuid::Uuid::new_v4());
        self.created.lock().await.push(id.clone());
        self.configs.lock().await.push(config.clone());

        let mut info = VmInfo::new(
            id,
            config.image,
            VmState::Running,
            "2024-01-01T00:00:00Z".to_owned(),
            config.memory_mib,
            config.vcpus,
        );
        info.cid = Some(42);
        Ok(info)
    }

    async fn create_from_snapshot(
        &self,
        config: VmConfig,
        _snapshot_dir: &Path,
    ) -> anyhow::Result<VmInfo> {
        self.create(config).await
    }

    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        Ok(Vec::new())
    }

    async fn get(&self, _id: &str) -> anyhow::Result<VmInfo> {
        anyhow::bail!("not found")
    }

    async fn exec(&self, _id: &str, _req: ExecRequest) -> anyhow::Result<ExecResult> {
        Ok(ExecResult::new(0, String::new(), String::new()))
    }

    async fn stop(&self, _id: &str, _timeout: u64) -> anyhow::Result<()> {
        Ok(())
    }

    async fn kill(&self, _id: &str) -> anyhow::Result<()> {
        Ok(())
    }

    async fn destroy(&self, id: &str) -> anyhow::Result<()> {
        self.destroyed.lock().await.push(id.to_owned());
        Ok(())
    }

    async fn console_output(&self, _id: &str) -> anyhow::Result<Vec<u8>> {
        Ok(Vec::new())
    }
}

fn make_test_layer(path: &str, contents: &[u8]) -> ProcessedLayer {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, path, contents).unwrap();
    let tar_data = builder.into_inner().unwrap();

    LayerCreator::from_tar(&tar_data, &[]).unwrap()
}

fn store_test_image(store_dir: &Path, tag: &str) {
    std::fs::create_dir_all(store_dir).unwrap();

    let layer = make_test_layer("usr/local/bin/base-tool", b"base-image\n");
    let mut metadata = ImageMetadata::default();
    metadata.cmd = Some(vec!["sleep".to_owned(), "5".to_owned()]);
    metadata.entrypoint = Some(vec!["/bin/sh".to_owned(), "-c".to_owned()]);
    metadata.env = vec![
        (
            "PATH".to_owned(),
            "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin".to_owned(),
        ),
        ("BASE_ONLY".to_owned(), "1".to_owned()),
    ];
    metadata.working_dir = Some("/workspace".to_owned());
    metadata.user = Some("root".to_owned());
    metadata.exposed_ports = vec![(8080, "tcp".to_owned())];
    metadata.labels = vec![(
        "org.opencontainers.image.title".to_owned(),
        "base".to_owned(),
    )];
    metadata.stop_signal = Some("SIGTERM".to_owned());
    metadata.volumes = vec!["/data".to_owned()];

    let staging_dir = store_dir.join("staging");
    let stored = ImageAssembler::assemble(&[layer], &metadata, &staging_dir).unwrap();
    let digest_hex = stored
        .manifest_digest
        .strip_prefix("sha256:")
        .unwrap_or(&stored.manifest_digest);
    std::fs::rename(&staging_dir, store_dir.join(digest_hex)).unwrap();

    let store = ImageStore::new(store_dir.to_path_buf());
    store.tag(tag, &stored.manifest_digest).unwrap();
    store
        .tag(&stored.manifest_digest, &stored.manifest_digest)
        .unwrap();
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn vmm_build_service_new_creates_instance() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBuildBackend::new());
    let _service =
        VmmBuildService::new(Arc::clone(&backend), std::path::PathBuf::from("/tmp/test"));
    // Construction should not panic.
}

#[test]
fn vmm_build_service_implements_build_service() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBuildBackend::new());
    let service = VmmBuildService::new(backend, std::path::PathBuf::from("/tmp/test"));
    // Must be usable as `Arc<dyn BuildService>`
    let _trait_obj: Arc<dyn BuildService> = Arc::new(service);
}

#[tokio::test]
async fn build_image_creates_and_destroys_vm() {
    let backend = Arc::new(MockBuildBackend::new());
    let service = VmmBuildService::new(
        backend.clone() as Arc<dyn ExecutionBackend>,
        std::path::PathBuf::from("/tmp/test"),
    );

    let request = BuildRequest::new("FROM alpine\nRUN echo hello\n".to_owned());

    // The build will fail at vsock connection (no real VM), but we can
    // verify that create was called. The error is expected.
    let result = service.build_image(request).await;

    // VM should have been created
    let created = backend.created.lock().await;
    assert_eq!(created.len(), 1, "should have created a build VM");

    // VM should have been destroyed (cleanup on failure)
    let destroyed = backend.destroyed.lock().await;
    assert_eq!(
        destroyed.len(),
        1,
        "should have destroyed build VM on failure"
    );
    assert_eq!(destroyed[0], created[0], "should destroy the same VM");

    // Build should have returned an error (can't connect vsock without real VM)
    assert!(result.is_err(), "build should fail without real VM");
}

#[tokio::test]
async fn build_image_returns_error_on_invalid_dockerfile() {
    let backend = Arc::new(MockBuildBackend::new());
    let service = VmmBuildService::new(
        backend.clone() as Arc<dyn ExecutionBackend>,
        std::path::PathBuf::from("/tmp/test"),
    );

    // Empty content is not a valid Dockerfile
    let request = BuildRequest::new(String::new());
    let result = service.build_image(request).await;

    assert!(result.is_err(), "empty Dockerfile should fail");

    // No VM should be created for parse failures
    let created = backend.created.lock().await;
    assert!(
        created.is_empty(),
        "should not create VM for parse failures"
    );
}

#[tokio::test]
async fn build_image_sets_build_vm_config() {
    let backend = Arc::new(MockBuildBackend::new());
    let service = VmmBuildService::new(
        backend.clone() as Arc<dyn ExecutionBackend>,
        std::path::PathBuf::from("/tmp/test"),
    );

    let request = BuildRequest::new("FROM alpine\n".to_owned());
    let _ = service.build_image(request).await;

    // Verify a VM was created (config details are internal)
    let created = backend.created.lock().await;
    assert_eq!(created.len(), 1);
}

#[tokio::test]
async fn build_image_sets_agent_mode_on_vm_config() {
    let backend = Arc::new(MockBuildBackend::new());
    let service = VmmBuildService::new(
        backend.clone() as Arc<dyn ExecutionBackend>,
        std::path::PathBuf::from("/tmp/test"),
    );

    let request = BuildRequest::new("FROM alpine\n".to_owned());
    let _ = service.build_image(request).await;

    let configs = backend.configs.lock().await;
    assert_eq!(configs.len(), 1, "should have created one VM");
    assert_eq!(
        configs[0].mode.as_deref(),
        Some("agent"),
        "build VM must use agent mode for vsock listener"
    );
}

#[tokio::test]
async fn build_image_sets_detach_true() {
    let backend = Arc::new(MockBuildBackend::new());
    let service = VmmBuildService::new(
        backend.clone() as Arc<dyn ExecutionBackend>,
        std::path::PathBuf::from("/tmp/test"),
    );

    let request = BuildRequest::new("FROM alpine\n".to_owned());
    let _ = service.build_image(request).await;

    let configs = backend.configs.lock().await;
    assert_eq!(configs.len(), 1);
    assert!(configs[0].detach, "build VM must be detached");
}

#[tokio::test]
async fn build_image_uses_linux_helper_image() {
    let backend = Arc::new(MockBuildBackend::new());
    let service = VmmBuildService::new(
        backend.clone() as Arc<dyn ExecutionBackend>,
        std::path::PathBuf::from("/tmp/test"),
    );

    let request = BuildRequest::new("FROM scratch\n".to_owned());
    let _ = service.build_image(request).await;

    let configs = backend.configs.lock().await;
    assert_eq!(configs.len(), 1);
    assert_eq!(
        configs[0].image, "alpine:latest",
        "build VM should use a Linux helper image with basic userland tools"
    );
}

// ── BuiltLayer → ProcessedLayer conversion tests ─────────────────

#[test]
fn built_layer_to_processed_layer_conversion() {
    use base64::Engine as _;
    #[allow(unused_imports)]
    // Create minimal compressed data and base64-encode it
    let raw_data = b"test layer data";
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(raw_data);

    let built = visor_build::BuiltLayer::new(
        encoded,
        "sha256:abc123".to_owned(),
        "sha256:def456".to_owned(),
        raw_data.len() as u64,
        false,
    );

    let processed = super::built_layer_to_processed(&built).unwrap();

    assert_eq!(processed.compressed_data, raw_data);
    assert_eq!(processed.digest, "sha256:abc123");
    assert_eq!(processed.diff_id, "sha256:def456");
    assert_eq!(processed.compressed_size, raw_data.len() as u64);
    assert_eq!(
        processed.media_type,
        "application/vnd.oci.image.layer.v1.tar+gzip"
    );
    assert!(!processed.empty);
}

#[test]
fn built_layer_to_processed_layer_empty_layer() {
    use base64::Engine as _;
    #[allow(unused_imports)]
    let encoded = base64::engine::general_purpose::STANDARD_NO_PAD.encode(b"");

    let built = visor_build::BuiltLayer::new(
        encoded,
        "sha256:empty".to_owned(),
        "sha256:empty".to_owned(),
        0,
        true,
    );

    let processed = super::built_layer_to_processed(&built).unwrap();

    assert!(processed.empty);
    assert!(processed.compressed_data.is_empty());
}

#[test]
fn built_layer_to_processed_layer_accepts_padded_base64() {
    use base64::Engine as _;

    let raw_data = b"pad";
    let encoded = base64::engine::general_purpose::STANDARD.encode(raw_data);

    let built = visor_build::BuiltLayer::new(
        encoded,
        "sha256:abc123".to_owned(),
        "sha256:def456".to_owned(),
        raw_data.len() as u64,
        false,
    );

    let processed = super::built_layer_to_processed(&built).unwrap();

    assert_eq!(processed.compressed_data, raw_data);
    assert_eq!(processed.digest, "sha256:abc123");
    assert_eq!(processed.diff_id, "sha256:def456");
}

#[test]
fn vmm_build_service_new_with_store_path() {
    let backend: Arc<dyn ExecutionBackend> = Arc::new(MockBuildBackend::new());
    let _service = VmmBuildService::new(
        Arc::clone(&backend),
        std::path::PathBuf::from("/tmp/test-images"),
    );
    // Construction should not panic.
}

#[test]
fn image_store_candidates_add_latest_for_tagless_reference() {
    let candidates = super::image_store_candidates("alpine");

    assert_eq!(
        candidates,
        vec!["alpine".to_owned(), "alpine:latest".to_owned()]
    );
}

#[test]
fn load_stored_base_image_reads_layers_and_metadata() {
    let tempdir = tempfile::tempdir().unwrap();
    store_test_image(tempdir.path(), "base:test");

    let base = super::load_stored_base_image("base:test", tempdir.path()).unwrap();

    assert_eq!(base.layers.len(), 1);
    assert_eq!(
        base.layers[0].media_type,
        visor_build::layer::OCI_LAYER_MEDIA_TYPE
    );
    assert_eq!(
        base.metadata.entrypoint,
        Some(vec!["/bin/sh".to_owned(), "-c".to_owned()])
    );
    assert_eq!(base.metadata.working_dir.as_deref(), Some("/workspace"));
    assert_eq!(base.metadata.user.as_deref(), Some("root"));
    assert!(
        base.metadata
            .env
            .contains(&(String::from("BASE_ONLY"), String::from("1")))
    );
}

#[test]
fn merge_image_metadata_prefers_overlay_fields_and_keeps_base_env() {
    let mut base = ImageMetadata::default();
    base.cmd = Some(vec!["sleep".to_owned(), "5".to_owned()]);
    base.entrypoint = Some(vec!["/bin/sh".to_owned(), "-c".to_owned()]);
    base.env = vec![
        ("PATH".to_owned(), "/usr/bin:/bin".to_owned()),
        ("BASE_ONLY".to_owned(), "1".to_owned()),
    ];
    base.working_dir = Some("/base".to_owned());
    base.user = Some("root".to_owned());
    base.exposed_ports = vec![(80, "tcp".to_owned())];
    base.labels = vec![("base".to_owned(), "yes".to_owned())];
    base.stop_signal = Some("SIGTERM".to_owned());
    base.volumes = vec!["/data".to_owned()];

    let mut overlay = ImageMetadata::default();
    overlay.entrypoint = Some(vec!["/entrypoint.sh".to_owned()]);
    overlay.env = vec![
        ("PATH".to_owned(), "/custom/bin".to_owned()),
        ("OVERLAY_ONLY".to_owned(), "1".to_owned()),
    ];
    overlay.working_dir = Some("/app".to_owned());
    overlay.exposed_ports = vec![(443, "tcp".to_owned())];
    overlay.labels = vec![("overlay".to_owned(), "yes".to_owned())];
    overlay.volumes = vec!["/cache".to_owned()];

    let merged = super::merge_image_metadata(base, overlay);

    assert_eq!(merged.cmd, Some(vec!["sleep".to_owned(), "5".to_owned()]));
    assert_eq!(merged.entrypoint, Some(vec!["/entrypoint.sh".to_owned()]));
    assert_eq!(merged.working_dir.as_deref(), Some("/app"));
    assert_eq!(merged.user.as_deref(), Some("root"));
    assert!(
        merged
            .env
            .contains(&(String::from("PATH"), String::from("/custom/bin")))
    );
    assert!(
        merged
            .env
            .contains(&(String::from("BASE_ONLY"), String::from("1")))
    );
    assert!(merged.exposed_ports.contains(&(80, "tcp".to_owned())));
    assert!(merged.exposed_ports.contains(&(443, "tcp".to_owned())));
    assert!(merged.volumes.contains(&String::from("/data")));
    assert!(merged.volumes.contains(&String::from("/cache")));
}
