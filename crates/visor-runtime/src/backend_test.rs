use super::*;
use std::collections::HashMap;
use std::path::Path;

use serde_json;
use visor_build::{ImageAssembler, ImageMetadata, ImageStore, LayerCreator, ProcessedLayer};

fn image_config_with_commands(
    entrypoint: Option<Vec<&str>>,
    cmd: Option<Vec<&str>>,
) -> crate::oci::config::ImageConfig {
    crate::oci::config::ImageConfig {
        cmd: cmd.map(|items| items.into_iter().map(str::to_owned).collect()),
        entrypoint: entrypoint.map(|items| items.into_iter().map(str::to_owned).collect()),
        env: Vec::new(),
        working_dir: None,
        user: None,
        exposed_ports: Vec::new(),
        labels: std::collections::HashMap::new(),
        stop_signal: None,
    }
}

fn make_test_layer(name: &str, content: &[u8]) -> ProcessedLayer {
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(content.len() as u64);
    header.set_mode(0o644);
    header.set_entry_type(tar::EntryType::Regular);
    header.set_cksum();
    builder.append_data(&mut header, name, content).unwrap();
    let tar_data = builder.into_inner().unwrap();

    LayerCreator::from_tar(&tar_data, &[]).unwrap()
}

fn make_docker_image_archive() -> Vec<u8> {
    let mut layer_builder = tar::Builder::new(Vec::new());
    let mut layer_header = tar::Header::new_gnu();
    let layer_contents = b"hello from loaded image\n";
    layer_header.set_size(layer_contents.len() as u64);
    layer_header.set_mode(0o644);
    layer_header.set_entry_type(tar::EntryType::Regular);
    layer_header.set_cksum();
    layer_builder
        .append_data(&mut layer_header, "hello.txt", &layer_contents[..])
        .unwrap();
    let layer_tar = layer_builder.into_inner().unwrap();

    let config = serde_json::json!({
        "architecture": "amd64",
        "os": "linux",
        "config": {
            "Cmd": ["cat", "/hello.txt"]
        }
    });
    let manifest = serde_json::json!([
        {
            "Config": "config.json",
            "RepoTags": ["loaded:test"],
            "Layers": ["layer.tar"]
        }
    ]);

    let mut archive_builder = tar::Builder::new(Vec::new());
    for (path, bytes) in [
        ("manifest.json", serde_json::to_vec(&manifest).unwrap()),
        ("config.json", serde_json::to_vec(&config).unwrap()),
        ("layer.tar", layer_tar),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();
        archive_builder
            .append_data(&mut header, path, bytes.as_slice())
            .unwrap();
    }

    archive_builder.into_inner().unwrap()
}

fn store_local_test_image(store_dir: &Path, tag: &str) -> visor_build::StoredImage {
    std::fs::create_dir_all(store_dir).unwrap();

    let layer = make_test_layer("hello.txt", b"hello from local store\n");
    let mut metadata = ImageMetadata::default();
    metadata.cmd = Some(vec!["cat".to_owned(), "/hello.txt".to_owned()]);

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
    stored
}

// ── VmConfig defaults ──────────────────────────────────────────────

#[test]
fn vm_config_default_memory_is_512() {
    let json = r#"{"image": "alpine:latest"}"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.memory_mib, 512);
}

#[test]
fn vm_config_default_vcpus_is_1() {
    let json = r#"{"image": "alpine:latest"}"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert_eq!(config.vcpus, 1);
}

#[test]
fn rootfs_options_use_requested_writable_size() {
    let requested = rootfs_options(Some(1024));
    let defaulted = rootfs_options(None);

    assert_eq!(requested.extra_size_mb, 1024);
    assert_eq!(defaulted.extra_size_mb, 256);
}

#[test]
fn guest_run_config_receives_the_vm_process_limit() {
    let mut vm_config = VmConfig::new("alpine:latest");
    vm_config.process_limit = Some(256);
    let mut run_config = visor_init::config::RunConfig::default();

    apply_guest_resource_limits(&vm_config, &mut run_config);

    assert_eq!(run_config.process_limit, Some(256));
}

#[test]
fn vm_config_default_cmd_is_empty() {
    let json = r#"{"image": "alpine:latest"}"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert!(config.cmd.is_empty());
}

#[test]
fn vm_config_default_env_is_empty() {
    let json = r#"{"image": "alpine:latest"}"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert!(config.env.is_empty());
}

#[test]
fn vm_config_default_ports_is_empty() {
    let json = r#"{"image": "alpine:latest"}"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert!(config.ports.is_empty());
}

#[test]
fn vm_config_default_volumes_is_empty() {
    let json = r#"{"image": "alpine:latest"}"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert!(config.volumes.is_empty());
}

// ── VmConfig JSON round-trip ───────────────────────────────────────

#[test]
fn vm_config_json_round_trip() {
    let json = r#"{
        "image": "ubuntu:22.04",
        "cmd": ["/bin/bash", "-c", "echo hello"],
        "env": ["FOO=bar"],
        "working_dir": "/app",
        "memory_mib": 1024,
        "vcpus": 4,
        "name": "test-vm",
        "ports": [{"host_port": 8080, "guest_port": 80}],
        "volumes": [{"host_path": "/data", "guest_path": "/mnt/data", "read_only": true}]
    }"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    let serialized = serde_json::to_string(&config).unwrap();
    let deserialized: VmConfig = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.image, "ubuntu:22.04");
    assert_eq!(deserialized.cmd, vec!["/bin/bash", "-c", "echo hello"]);
    assert_eq!(deserialized.env, vec!["FOO=bar"]);
    assert_eq!(deserialized.working_dir.as_deref(), Some("/app"));
    assert_eq!(deserialized.memory_mib, 1024);
    assert_eq!(deserialized.vcpus, 4);
    assert_eq!(deserialized.name.as_deref(), Some("test-vm"));
    assert_eq!(deserialized.ports.len(), 1);
    assert_eq!(deserialized.volumes.len(), 1);
}

// ── VmState serialization ──────────────────────────────────────────

#[test]
fn vm_state_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&VmState::Creating).unwrap(),
        r#""creating""#
    );
    assert_eq!(
        serde_json::to_string(&VmState::Running).unwrap(),
        r#""running""#
    );
    assert_eq!(
        serde_json::to_string(&VmState::Stopped).unwrap(),
        r#""stopped""#
    );
    assert_eq!(
        serde_json::to_string(&VmState::Failed).unwrap(),
        r#""failed""#
    );
}

#[test]
fn vm_state_deserializes_from_snake_case() {
    assert_eq!(
        serde_json::from_str::<VmState>(r#""creating""#).unwrap(),
        VmState::Creating
    );
    assert_eq!(
        serde_json::from_str::<VmState>(r#""running""#).unwrap(),
        VmState::Running
    );
    assert_eq!(
        serde_json::from_str::<VmState>(r#""stopped""#).unwrap(),
        VmState::Stopped
    );
    assert_eq!(
        serde_json::from_str::<VmState>(r#""failed""#).unwrap(),
        VmState::Failed
    );
}

#[test]
fn vm_state_default_is_creating() {
    assert_eq!(VmState::default(), VmState::Creating);
}

// ── PortMapping defaults ───────────────────────────────────────────

#[test]
fn port_mapping_default_protocol_is_tcp() {
    let json = r#"{"host_port": 8080, "guest_port": 80}"#;
    let mapping: PortMapping = serde_json::from_str(json).unwrap();
    assert_eq!(mapping.protocol, "tcp");
}

#[test]
fn port_mapping_custom_protocol() {
    let json = r#"{"host_port": 53, "guest_port": 53, "protocol": "udp"}"#;
    let mapping: PortMapping = serde_json::from_str(json).unwrap();
    assert_eq!(mapping.protocol, "udp");
}

// ── VolumeMount defaults ──────────────────────────────────────────

#[test]
fn volume_mount_default_read_only_is_false() {
    let json = r#"{"host_path": "/data", "guest_path": "/mnt"}"#;
    let volume: VolumeMount = serde_json::from_str(json).unwrap();
    assert!(!volume.read_only);
}

#[test]
fn resolve_vm_storage_stages_read_only_directories_as_data_disks() {
    let dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    std::fs::write(dir.path().join("hello.txt"), b"hello").unwrap();
    let staging = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let volume = VolumeMount::read_only(dir.path().display().to_string(), "/workspace");

    let storage = resolve_vm_storage(&[volume], staging.path()).unwrap();

    assert!(storage.shared_dirs.is_empty());
    assert_eq!(storage.data_disks.len(), 1);
    assert!(storage.data_disks[0].read_only);
    assert!(storage.data_disks[0].path.is_file());
    assert!(storage.data_disks[0].path.starts_with(staging.path()));
    assert_eq!(storage.guest_volumes.len(), 1);
    assert_eq!(storage.guest_volumes[0].device_path, "/dev/vdb");
    assert_eq!(storage.guest_volumes[0].fs_type, "ext4");
    assert!(storage.guest_volumes[0].mount_tag.is_empty());
}

#[test]
fn resolve_vm_storage_rejects_read_write_directory_mounts() {
    let dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let staging = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let volume = VolumeMount::new(dir.path().display().to_string(), "/workspace");

    let err = resolve_vm_storage(&[volume], staging.path()).unwrap_err();

    assert!(
        err.to_string().contains("must be mounted read-only"),
        "unexpected error: {err:?}"
    );
}

#[test]
fn resolve_vm_storage_maps_files_to_guest_block_devices() {
    let dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let disk_path = dir.path().join("named-volume.ext4");
    std::fs::write(&disk_path, b"volume").unwrap();
    let staging = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let volume = VolumeMount::new(disk_path.display().to_string(), "/var/lib/data");

    let storage = resolve_vm_storage(&[volume], staging.path()).unwrap();

    assert!(storage.shared_dirs.is_empty());
    assert_eq!(storage.data_disks.len(), 1);
    assert_eq!(storage.data_disks[0].path, disk_path);
    assert!(!storage.data_disks[0].read_only);
    assert_eq!(storage.guest_volumes.len(), 1);
    assert_eq!(storage.guest_volumes[0].device_path, "/dev/vdb");
    assert_eq!(storage.guest_volumes[0].fs_type, "ext4");
    assert!(storage.guest_volumes[0].mount_tag.is_empty());
}

// ── VmInfo serialization ──────────────────────────────────────────

#[test]
fn vm_info_json_round_trip() {
    let mut info = VmInfo::new(
        "test-id-123".to_owned(),
        "alpine:latest".to_owned(),
        VmState::Running,
        "2026-01-15T10:30:00Z".to_owned(),
        512,
        1,
    );
    info.name = Some("my-vm".to_owned());
    info.ports = vec![PortMapping::new(8080, 80)];

    let serialized = serde_json::to_string(&info).unwrap();
    let deserialized: VmInfo = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.id, "test-id-123");
    assert_eq!(deserialized.name.as_deref(), Some("my-vm"));
    assert_eq!(deserialized.state, VmState::Running);
    assert_eq!(deserialized.memory_mib, 512);
    assert_eq!(deserialized.ports.len(), 1);
    assert!(deserialized.exit_code.is_none());
}

// ── ExecRequest serialization ─────────────────────────────────────

#[test]
fn exec_request_json_round_trip() {
    let mut req = ExecRequest::new(vec!["ls".to_owned(), "-la".to_owned()]);
    req.env = vec!["PATH=/usr/bin".to_owned()];
    req.working_dir = Some("/app".to_owned());
    req.tty = true;

    let serialized = serde_json::to_string(&req).unwrap();
    let deserialized: ExecRequest = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.cmd, vec!["ls", "-la"]);
    assert_eq!(deserialized.env, vec!["PATH=/usr/bin"]);
    assert_eq!(deserialized.working_dir.as_deref(), Some("/app"));
    assert!(deserialized.tty);
}

#[test]
fn exec_request_defaults() {
    let json = r#"{"cmd": ["echo", "hello"]}"#;
    let req: ExecRequest = serde_json::from_str(json).unwrap();
    assert!(req.env.is_empty());
    assert!(req.working_dir.is_none());
    assert!(!req.tty);
}

// ── ExecResult serialization ──────────────────────────────────────

#[test]
fn exec_result_json_round_trip() {
    let result = ExecResult::new(0, "hello world\n".to_owned(), String::new());

    let serialized = serde_json::to_string(&result).unwrap();
    let deserialized: ExecResult = serde_json::from_str(&serialized).unwrap();

    assert_eq!(deserialized.exit_code, 0);
    assert_eq!(deserialized.stdout, "hello world\n");
    assert!(deserialized.stderr.is_empty());
}

// ── Test helpers ─────────────────────────────────────────────────

fn test_vm(id: &str, name: Option<&str>, image: &str, state: VmState) -> VmInfo {
    let mut info = VmInfo::new(
        id.to_owned(),
        image.to_owned(),
        state,
        "1970-01-01T00:00:00Z".to_owned(),
        512,
        1,
    );
    info.name = name.map(ToOwned::to_owned);
    info
}

// ── VmmBackend::insert_vm + state management ────────────────────

#[tokio::test]
async fn vmm_backend_insert_vm_stores_vm() {
    let backend = VmmBackend::new();
    let info = test_vm("vm-001", Some("test-vm"), "alpine:latest", VmState::Running);

    backend.insert_vm(info).await;

    let fetched = backend.get("vm-001").await.unwrap();
    assert_eq!(fetched.id, "vm-001");
    assert_eq!(fetched.name.as_deref(), Some("test-vm"));
    assert_eq!(fetched.image, "alpine:latest");
    assert_eq!(fetched.state, VmState::Running);
    assert_eq!(fetched.memory_mib, 512);
    assert_eq!(fetched.vcpus, 1);
    assert!(fetched.exit_code.is_none());
}

// ── VmmBackend::list ──────────────────────────────────────────────

#[tokio::test]
async fn vmm_backend_list_returns_all_vms() {
    let backend = VmmBackend::new();
    backend
        .insert_vm(test_vm(
            "vm-a",
            Some("vm-a"),
            "alpine:latest",
            VmState::Running,
        ))
        .await;
    backend
        .insert_vm(test_vm(
            "vm-b",
            Some("vm-b"),
            "ubuntu:22.04",
            VmState::Running,
        ))
        .await;

    let vms = backend.list().await.unwrap();
    assert_eq!(vms.len(), 2);
}

// ── VmmBackend::get ───────────────────────────────────────────────

#[tokio::test]
async fn vmm_backend_get_returns_correct_vm() {
    let backend = VmmBackend::new();
    backend
        .insert_vm(test_vm(
            "vm-find",
            Some("find-me"),
            "alpine:latest",
            VmState::Running,
        ))
        .await;

    let fetched = backend.get("vm-find").await.unwrap();
    assert_eq!(fetched.id, "vm-find");
    assert_eq!(fetched.name.as_deref(), Some("find-me"));
}

#[tokio::test]
async fn vmm_backend_get_errors_on_missing_vm() {
    let backend = VmmBackend::new();
    let result = backend.get("nonexistent-id").await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(err_msg.contains("vm not found"), "error was: {err_msg}");
}

// ── VmmBackend::stop ──────────────────────────────────────────────

#[tokio::test]
async fn vmm_backend_stop_changes_state() {
    let backend = VmmBackend::new();
    backend
        .insert_vm(test_vm("vm-stop", None, "alpine:latest", VmState::Running))
        .await;

    backend.stop("vm-stop", 10).await.unwrap();

    let stopped = backend.get("vm-stop").await.unwrap();
    assert_eq!(stopped.state, VmState::Stopped);
}

// ── VmmBackend::destroy ───────────────────────────────────────────

#[tokio::test]
async fn vmm_backend_destroy_removes_vm() {
    let backend = VmmBackend::new();
    backend
        .insert_vm(test_vm(
            "vm-destroy",
            None,
            "alpine:latest",
            VmState::Running,
        ))
        .await;

    backend.destroy("vm-destroy").await.unwrap();

    let result = backend.get("vm-destroy").await;
    assert!(result.is_err());
}

// ── Multiple VMs ──────────────────────────────────────────────────

#[tokio::test]
async fn multiple_vms_managed_independently() {
    let backend = VmmBackend::new();

    for i in 0..3 {
        backend
            .insert_vm(test_vm(
                &format!("vm-{i}"),
                Some(&format!("vm-{i}")),
                &format!("image-{i}:latest"),
                VmState::Running,
            ))
            .await;
    }

    // All stored.
    let vms = backend.list().await.unwrap();
    assert_eq!(vms.len(), 3);

    // Stop one, destroy another — third unaffected.
    backend.stop("vm-0", 10).await.unwrap();
    backend.destroy("vm-1").await.unwrap();

    let vm0 = backend.get("vm-0").await.unwrap();
    assert_eq!(vm0.state, VmState::Stopped);

    let vm1 = backend.get("vm-1").await;
    assert!(vm1.is_err());

    let vm2 = backend.get("vm-2").await.unwrap();
    assert_eq!(vm2.state, VmState::Running);
}

// ── Mock VsockConnector ──────────────────────────────────────────

/// Mock vsock connector that returns canned exec results.
struct MockVsockConnector {
    /// Pre-configured exec result to return.
    exec_result: ExecResult,
    /// Whether shutdown should succeed.
    shutdown_ok: bool,
    /// Recorded guest archive copy calls.
    copy_calls: std::sync::Mutex<Vec<(u32, String, Vec<u8>)>>,
}

impl MockVsockConnector {
    fn new(exit_code: i32, stdout: &str) -> Self {
        Self {
            exec_result: ExecResult::new(exit_code, stdout.to_owned(), String::new()),
            shutdown_ok: true,
            copy_calls: std::sync::Mutex::new(Vec::new()),
        }
    }
}

#[async_trait::async_trait]
impl VsockConnector for MockVsockConnector {
    async fn exec_cmd(&self, _cid: u32, _req: &ExecRequest) -> anyhow::Result<ExecResult> {
        Ok(self.exec_result.clone())
    }

    async fn exec_stream_cmd(
        &self,
        _cid: u32,
        _req: &ExecRequest,
    ) -> anyhow::Result<Box<dyn AsyncIoStream>> {
        let (stream, _peer) = tokio::io::duplex(64);
        Ok(Box::new(stream))
    }

    async fn copy_to_guest(&self, cid: u32, archive: &[u8], dest: &str) -> anyhow::Result<()> {
        self.copy_calls
            .lock()
            .unwrap()
            .push((cid, dest.to_owned(), archive.to_vec()));
        Ok(())
    }

    async fn shutdown(&self, _cid: u32) -> anyhow::Result<()> {
        if self.shutdown_ok {
            Ok(())
        } else {
            anyhow::bail!("mock shutdown failure")
        }
    }
}

fn mock_backend(exit_code: i32, stdout: &str) -> VmmBackend {
    let connector = std::sync::Arc::new(MockVsockConnector::new(exit_code, stdout));
    VmmBackend::with_connector(connector)
}

// ── VmConfig.detach serialization ────────────────────────────────

#[test]
fn vm_config_detach_defaults_to_false() {
    let json = r#"{"image": "alpine:latest"}"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert!(!config.detach);
}

#[test]
fn vm_config_detach_can_be_set_true() {
    let json = r#"{"image": "alpine:latest", "detach": true}"#;
    let config: VmConfig = serde_json::from_str(json).unwrap();
    assert!(config.detach);
}

// ── VmmBackend::exec on running VM ──────────────────────────────

#[tokio::test]
async fn test_exec_on_running_vm_returns_result() {
    let backend = mock_backend(0, "hello world\n");
    let vm = test_vm("vm-exec", None, "alpine:latest", VmState::Running);
    backend.insert_vm_with_cid(vm, 3).await;

    let req = ExecRequest::new(vec!["echo".to_owned(), "hello".to_owned()]);

    let result = backend.exec("vm-exec", req).await.unwrap();
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout, "hello world\n");
    assert!(result.stderr.is_empty());
}

#[tokio::test]
async fn test_exec_stream_on_running_vm_returns_stream() {
    let backend = mock_backend(0, "hello world\n");
    let vm = test_vm("vm-stream", None, "alpine:latest", VmState::Running);
    backend.insert_vm_with_cid(vm, 3).await;

    let req = ExecRequest::new(vec!["cat".to_owned()]);

    let stream = backend.exec_stream("vm-stream", req).await.unwrap();
    drop(stream);
}

// ── VmmBackend::exec on stopped VM ──────────────────────────────

#[tokio::test]
async fn test_exec_on_stopped_vm_returns_error() {
    let backend = mock_backend(0, "");
    let vm = test_vm("vm-stopped", None, "alpine:latest", VmState::Stopped);
    backend.insert_vm(vm).await;

    let req = ExecRequest::new(vec!["echo".to_owned()]);

    let result = backend.exec("vm-stopped", req).await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("not running"),
        "expected 'not running' error, got: {err_msg}"
    );
}

// ── VmmBackend::exec on nonexistent VM ──────────────────────────

#[tokio::test]
async fn test_exec_on_nonexistent_vm_returns_error() {
    let backend = mock_backend(0, "");

    let req = ExecRequest::new(vec!["echo".to_owned()]);

    let result = backend.exec("does-not-exist", req).await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("vm not found"),
        "expected 'vm not found' error, got: {err_msg}"
    );
}

// ── VmmBackend::stop transitions ────────────────────────────────

#[tokio::test]
async fn test_stop_transitions_to_stopped() {
    let backend = mock_backend(0, "");
    let vm = test_vm("vm-live", None, "alpine:latest", VmState::Running);
    backend.insert_vm_with_cid(vm, 3).await;

    backend.stop("vm-live", 10).await.unwrap();

    let stopped = backend.get("vm-live").await.unwrap();
    assert_eq!(stopped.state, VmState::Stopped);
}

// ── VmmBackend::stop on already stopped VM ──────────────────────

#[tokio::test]
async fn test_stop_on_already_stopped_is_noop() {
    let backend = mock_backend(0, "");
    let vm = test_vm(
        "vm-already-stopped",
        None,
        "alpine:latest",
        VmState::Stopped,
    );
    backend.insert_vm(vm).await;

    // Should succeed without error (idempotent).
    backend.stop("vm-already-stopped", 10).await.unwrap();

    let fetched = backend.get("vm-already-stopped").await.unwrap();
    assert_eq!(fetched.state, VmState::Stopped);
}

// ── VmmBackend::stop on nonexistent VM ──────────────────────────

#[tokio::test]
async fn test_stop_on_nonexistent_returns_error() {
    let backend = mock_backend(0, "");
    let result = backend.stop("ghost-vm", 10).await;
    assert!(result.is_err());
}

// ── CID allocation ──────────────────────────────────────────────

#[test]
fn cid_allocation_starts_at_3_and_increments() {
    let backend = VmmBackend::new();
    assert_eq!(backend.allocate_cid(), 3);
    assert_eq!(backend.allocate_cid(), 4);
    assert_eq!(backend.allocate_cid(), 5);
}

// ── Exec with non-zero exit code ────────────────────────────────

#[tokio::test]
async fn test_exec_returns_nonzero_exit_code() {
    let backend = mock_backend(42, "error output");
    let vm = test_vm("vm-fail-exec", None, "alpine:latest", VmState::Running);
    backend.insert_vm_with_cid(vm, 3).await;

    let req = ExecRequest::new(vec!["false".to_owned()]);

    let result = backend.exec("vm-fail-exec", req).await.unwrap();
    assert_eq!(result.exit_code, 42);
    assert_eq!(result.stdout, "error output");
}

// ── Stop with timeout ────────────────────────────────────────────

#[tokio::test]
async fn stop_running_vm_completes_without_hanging() {
    let backend = mock_backend(0, "");
    let vm = test_vm("vm-stop-timeout", None, "alpine:latest", VmState::Running);
    backend.insert_vm(vm).await;

    // stop() should complete quickly even without a real vsock connection
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        backend.stop("vm-stop-timeout", 10),
    )
    .await;

    // Should not time out
    assert!(result.is_ok(), "stop() should complete within 5s");
    assert!(result.unwrap().is_ok());

    let vm = backend.get("vm-stop-timeout").await.unwrap();
    assert_eq!(vm.state, VmState::Stopped);
}

// ── Prefix matching ─────────────────────────────────────────────

#[tokio::test]
async fn resolve_vm_id_exact_match() {
    let backend = VmmBackend::new();
    backend
        .insert_vm(test_vm(
            "abc12345-full-uuid",
            None,
            "alpine:latest",
            VmState::Running,
        ))
        .await;
    let resolved = backend.resolve_id("abc12345-full-uuid").await.unwrap();
    assert_eq!(resolved, "abc12345-full-uuid");
}

#[tokio::test]
async fn resolve_vm_id_prefix_match() {
    let backend = VmmBackend::new();
    backend
        .insert_vm(test_vm(
            "abc12345-full-uuid",
            None,
            "alpine:latest",
            VmState::Running,
        ))
        .await;
    let resolved = backend.resolve_id("abc1").await.unwrap();
    assert_eq!(resolved, "abc12345-full-uuid");
}

#[tokio::test]
async fn resolve_vm_id_ambiguous_prefix_errors() {
    let backend = VmmBackend::new();
    backend
        .insert_vm(test_vm(
            "abc12345-first",
            None,
            "alpine:latest",
            VmState::Running,
        ))
        .await;
    backend
        .insert_vm(test_vm(
            "abc12345-second",
            None,
            "alpine:latest",
            VmState::Running,
        ))
        .await;
    let result = backend.resolve_id("abc1").await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("ambiguous"),
        "should mention ambiguous: {err_msg}"
    );
}

#[tokio::test]
async fn resolve_vm_id_no_match_errors() {
    let backend = VmmBackend::new();
    backend
        .insert_vm(test_vm(
            "abc12345-full-uuid",
            None,
            "alpine:latest",
            VmState::Running,
        ))
        .await;
    let result = backend.resolve_id("zzz").await;
    assert!(result.is_err());
    let err_msg = format!("{}", result.unwrap_err());
    assert!(
        err_msg.contains("not found"),
        "should mention not found: {err_msg}"
    );
}

#[tokio::test]
async fn resolve_vm_id_by_name() {
    let backend = VmmBackend::new();
    backend
        .insert_vm(test_vm(
            "abc12345-full-uuid",
            Some("my_vm"),
            "alpine:latest",
            VmState::Running,
        ))
        .await;
    let resolved = backend.resolve_id("my_vm").await.unwrap();
    assert_eq!(resolved, "abc12345-full-uuid");
}

#[tokio::test]
async fn resolve_vm_id_by_name_errors_when_duplicate_names_exist() {
    let backend = VmmBackend::new();

    let mut stopped = test_vm(
        "vm-old",
        Some("shared-name"),
        "alpine:latest",
        VmState::Stopped,
    );
    stopped.created_at = "2026-03-08T09:00:00Z".to_owned();
    let mut running = test_vm(
        "vm-new",
        Some("shared-name"),
        "alpine:latest",
        VmState::Running,
    );
    running.created_at = "2026-03-08T09:00:01Z".to_owned();

    backend.insert_vm(stopped).await;
    backend.insert_vm(running).await;

    let err = backend.resolve_id("shared-name").await.unwrap_err();
    assert!(
        err.to_string().contains("ambiguous VM name"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn choose_unique_vm_name_retries_generated_collisions() {
    let mut existing = HashMap::new();
    existing.insert(
        "vm-1".to_owned(),
        test_vm(
            "vm-1",
            Some("bright_batman"),
            "alpine:latest",
            VmState::Running,
        ),
    );

    let mut generated = vec!["bright_batman".to_owned(), "calm_superman".to_owned()].into_iter();
    let name = VmmBackend::choose_unique_vm_name_with_generator(&existing, None, || {
        generated
            .next()
            .expect("generator should have another value")
    })
    .unwrap();

    assert_eq!(name, "calm_superman");
}

#[tokio::test]
async fn resolve_vm_id_by_name_errors_when_all_stopped_duplicates_exist() {
    let backend = VmmBackend::new();

    let mut older = test_vm(
        "vm-old",
        Some("shared-name"),
        "alpine:latest",
        VmState::Stopped,
    );
    older.created_at = "2026-03-08T09:00:00Z".to_owned();
    let mut newer = test_vm(
        "vm-new",
        Some("shared-name"),
        "alpine:latest",
        VmState::Stopped,
    );
    newer.created_at = "2026-03-08T09:00:01Z".to_owned();

    backend.insert_vm(older).await;
    backend.insert_vm(newer).await;

    let err = backend.resolve_id("shared-name").await.unwrap_err();
    assert!(
        err.to_string().contains("ambiguous VM name"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn create_rejects_duplicate_requested_name_before_boot() {
    let backend = mock_backend(0, "");
    backend
        .insert_vm(test_vm(
            "existing-vm",
            Some("shared-name"),
            "alpine:latest",
            VmState::Stopped,
        ))
        .await;

    let mut config = VmConfig::new("alpine:latest");
    config.name = Some("shared-name".to_owned());
    config.detach = true;

    let err = backend.create(config).await.unwrap_err();
    assert!(
        err.to_string().contains("already exists"),
        "unexpected error: {err:#}"
    );
}

#[tokio::test]
async fn create_from_snapshot_rejects_duplicate_requested_name_before_restore() {
    let backend = mock_backend(0, "");
    backend
        .insert_vm(test_vm(
            "existing-vm",
            Some("shared-name"),
            "alpine:latest",
            VmState::Stopped,
        ))
        .await;

    let mut config = VmConfig::new("alpine:latest");
    config.name = Some("shared-name".to_owned());
    config.detach = true;

    let err = backend
        .create_from_snapshot(config, Path::new("/tmp/visor-missing-snapshot"))
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("already exists"),
        "unexpected error: {err:#}"
    );
}

// ── Kill (immediate, no graceful shutdown) ──────────────────────

#[tokio::test]
async fn kill_running_vm_transitions_to_stopped() {
    let backend = mock_backend(0, "");
    let vm = test_vm("vm-kill-1", None, "alpine:latest", VmState::Running);
    backend.insert_vm(vm).await;

    backend.kill("vm-kill-1").await.unwrap();

    let vm = backend.get("vm-kill-1").await.unwrap();
    assert_eq!(vm.state, VmState::Stopped);
}

#[tokio::test]
async fn kill_already_stopped_is_noop() {
    let backend = mock_backend(0, "");
    let vm = test_vm("vm-kill-2", None, "alpine:latest", VmState::Stopped);
    backend.insert_vm(vm).await;

    // Should not error
    backend.kill("vm-kill-2").await.unwrap();
    let vm = backend.get("vm-kill-2").await.unwrap();
    assert_eq!(vm.state, VmState::Stopped);
}

#[tokio::test]
async fn kill_nonexistent_returns_error() {
    let backend = mock_backend(0, "");
    let result = backend.kill("no-such-vm").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn kill_completes_faster_than_stop() {
    let backend = mock_backend(0, "");
    let vm = test_vm("vm-kill-fast", None, "alpine:latest", VmState::Running);
    backend.insert_vm(vm).await;

    // kill() should complete in well under 1 second (no vsock timeout)
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        backend.kill("vm-kill-fast"),
    )
    .await;

    assert!(result.is_ok(), "kill() should complete within 1s");
    assert!(result.unwrap().is_ok());
}

// ── Stop with grace period ──────────────────────────────────────

#[tokio::test]
async fn stop_accepts_custom_timeout() {
    let backend = mock_backend(0, "");
    let vm = test_vm("vm-stop-grace", None, "alpine:latest", VmState::Running);
    backend.insert_vm(vm).await;

    // stop() with explicit 5s timeout should work the same as default
    backend.stop("vm-stop-grace", 5).await.unwrap();

    let vm = backend.get("vm-stop-grace").await.unwrap();
    assert_eq!(vm.state, VmState::Stopped);
}

#[tokio::test]
async fn stop_with_zero_timeout_still_sets_kill_flag() {
    let backend = mock_backend(0, "");
    let vm = test_vm("vm-stop-zero", None, "alpine:latest", VmState::Running);
    backend.insert_vm(vm).await;

    // timeout=0 should skip vsock entirely and go straight to kill_flag
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        backend.stop("vm-stop-zero", 0),
    )
    .await;

    assert!(result.is_ok(), "stop(0) should complete quickly");
    assert!(result.unwrap().is_ok());

    let vm = backend.get("vm-stop-zero").await.unwrap();
    assert_eq!(vm.state, VmState::Stopped);
}

#[tokio::test]
async fn restore_vm_stores_fallback_config_for_restart() {
    let backend = mock_backend(0, "");
    let mut vm = test_vm("vm-config", Some("cfg"), "alpine:latest", VmState::Stopped);
    vm.memory_mib = 768;
    vm.vcpus = 2;
    vm.ports = vec![PortMapping::new(8080, 80)];

    backend.restore_vm(vm.clone()).await;

    let config = backend
        .get_vm_config("vm-config")
        .await
        .expect("expected stored config");
    assert_eq!(config.image, "alpine:latest");
    assert_eq!(config.memory_mib, 768);
    assert_eq!(config.vcpus, 2);
    assert_eq!(config.name.as_deref(), Some("cfg"));
    assert_eq!(config.ports.len(), 1);
    assert!(config.detach);
}

#[tokio::test]
async fn destroy_removes_stored_vm_config() {
    let backend = mock_backend(0, "");
    let vm = test_vm(
        "vm-destroy-config",
        Some("gone"),
        "alpine:latest",
        VmState::Stopped,
    );
    backend.restore_vm(vm).await;
    assert!(backend.get_vm_config("vm-destroy-config").await.is_some());

    backend.destroy("vm-destroy-config").await.unwrap();

    assert!(backend.get_vm_config("vm-destroy-config").await.is_none());
}

// ── Port forwarding ─────────────────────────────────────────────

#[test]
fn setup_port_forwards_returns_none_when_no_ports() {
    let config = VmConfig::new("alpine:latest");
    let result = super::setup_port_forwards(&config, &[]).unwrap();
    assert!(result.is_none(), "empty ports should yield None");
}

#[test]
fn setup_port_forwards_builds_spec_string_correctly() {
    // Verify that runtime PortMapping fields map to the expected
    // VMM spec format: "host_port:guest_port/protocol"
    let pm = PortMapping::new(8080, 80);
    let spec = format!("{}:{}/{}", pm.host_port, pm.guest_port, pm.protocol);
    assert_eq!(spec, "8080:80/tcp");

    let pm_udp = PortMapping::with_protocol(53, 53, "udp");
    let spec_udp = format!(
        "{}:{}/{}",
        pm_udp.host_port, pm_udp.guest_port, pm_udp.protocol
    );
    assert_eq!(spec_udp, "53:53/udp");
}

#[test]
fn guest_network_config_is_none_when_networking_is_disabled() {
    let mut config = VmConfig::new("alpine:latest");
    config.network_enabled = false;

    assert!(super::guest_network_configs_for_vm(&config, 3).is_empty());
}

#[test]
fn guest_network_config_uses_default_visor_network_when_enabled_without_ports() {
    let mut config = VmConfig::new("alpine:latest");
    config.network_enabled = true;

    let network = super::guest_network_configs_for_vm(&config, 3)
        .into_iter()
        .next()
        .expect("network-enabled VMs should get guest networking");

    assert_eq!(network.address, "172.20.0.2");
    assert_eq!(network.netmask, "255.255.255.252");
    assert_eq!(network.gateway, "172.20.0.1");
    assert_eq!(
        network.dns_servers.first().map(String::as_str),
        Some("172.20.0.1")
    );
}

#[test]
fn guest_network_config_uses_unique_point_to_point_link_per_cid() {
    let mut config = VmConfig::new("alpine:latest");
    config.ports = vec![PortMapping::new(8080, 80)];

    let first = super::guest_network_configs_for_vm(&config, 3)
        .into_iter()
        .next()
        .expect("port-mapped VMs should get guest networking");
    let second = super::guest_network_configs_for_vm(&config, 4)
        .into_iter()
        .next()
        .expect("port-mapped VMs should get guest networking");

    assert_eq!(first.address, "172.20.0.2");
    assert_eq!(first.netmask, "255.255.255.252");
    assert_eq!(first.gateway, "172.20.0.1");
    assert_eq!(second.address, "172.20.0.6");
    assert_eq!(second.netmask, "255.255.255.252");
    assert_eq!(second.gateway, "172.20.0.5");
    assert_eq!(
        first.dns_servers.first().map(String::as_str),
        Some("172.20.0.1")
    );
    assert_eq!(
        second.dns_servers.first().map(String::as_str),
        Some("172.20.0.5")
    );
}

#[test]
fn guest_network_config_prepends_host_access_link_for_named_networks_with_published_ports() {
    let mut config = VmConfig::new("nginx:alpine");
    config.ports = vec![PortMapping::new(8080, 80)];
    config.networks = vec!["alpha_default".to_owned()];

    let networks = super::guest_network_configs_for_vm(&config, 3);

    assert_eq!(networks.len(), 2, "published ports should keep host access");
    assert_eq!(networks[0].name, None);
    assert_eq!(networks[0].interface.as_deref(), Some("eth0"));
    assert_eq!(networks[0].address, "172.20.0.2");
    assert!(networks[0].default_route);
    assert_eq!(networks[1].name.as_deref(), Some("alpha_default"));
    assert_eq!(networks[1].interface.as_deref(), Some("eth1"));
    assert_eq!(
        networks[1].address,
        visor_types::GuestNetworkLink::for_named_network("alpha_default", 3)
            .guest_ip
            .to_string()
    );
    assert!(
        !networks[1].default_route,
        "named compose networks should keep their subnet routes without stealing the default route"
    );
}

#[test]
fn guest_network_config_uses_named_network_only_without_host_port_access() {
    let mut config = VmConfig::new("nginx:alpine");
    config.networks = vec!["alpha_default".to_owned()];

    let networks = super::guest_network_configs_for_vm(&config, 3);

    assert_eq!(networks.len(), 1);
    assert_eq!(networks[0].name.as_deref(), Some("alpha_default"));
    assert_eq!(networks[0].interface.as_deref(), Some("eth0"));
    assert!(networks[0].default_route);
}

#[test]
fn build_port_forward_mappings_includes_internal_service_routes() {
    let mut config = VmConfig::new("alpine:latest");
    config.ports = vec![PortMapping::new(18080, 8080)];
    config.service_ports = vec![visor_types::ServicePort::new(8080, "tcp")];
    let guest_network = super::guest_network_configs_for_vm(&config, 3)
        .into_iter()
        .next()
        .expect("network config should exist for service routes");
    let guest_ip = guest_network.address.parse().unwrap();
    let gateway_ip = guest_network.gateway.parse().unwrap();

    let mappings = super::build_port_forward_mappings(&config, guest_ip, gateway_ip, true).unwrap();

    assert_eq!(mappings.len(), 2);
    assert_eq!(mappings[0].host_port(), 18080);
    assert_eq!(mappings[0].guest_port(), 8080);
    assert_eq!(mappings[0].host_ip(), None);
    assert_eq!(mappings[1].host_port(), 8080);
    assert_eq!(mappings[1].guest_port(), 8080);
    assert_eq!(mappings[1].host_ip(), Some(gateway_ip));
}

#[test]
fn guest_extra_hosts_maps_vm_host_entries_to_run_config_entries() {
    let mut config = VmConfig::new("alpine:latest");
    config.extra_hosts = vec![
        visor_types::HostEntry::new("api", "172.20.0.1"),
        visor_types::HostEntry::new("db", "172.20.0.5"),
    ];

    let entries = super::guest_extra_hosts(&config);

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].hostname, "api");
    assert_eq!(entries[0].address, "172.20.0.1");
    assert_eq!(entries[1].hostname, "db");
    assert_eq!(entries[1].address, "172.20.0.5");
}

#[test]
fn visor_temp_root_from_env_prefers_override_directory() {
    let temp_override = std::path::Path::new("/var/tmp/visor-work");
    let home_dir = std::path::Path::new("/home/tester");

    let root = super::visor_temp_root_from_env(Some(home_dir), Some(temp_override));

    assert_eq!(root, temp_override);
}

#[test]
fn visor_temp_root_from_env_falls_back_to_home_visor_tmp() {
    let home_dir = std::path::Path::new("/home/tester");

    let root = super::visor_temp_root_from_env(Some(home_dir), None);

    assert_eq!(root, home_dir.join(".visor").join("tmp"));
}

#[test]
fn parse_guest_dns_servers_skips_loopback_and_ipv6_entries() {
    let servers = super::parse_guest_dns_servers(
        "nameserver 127.0.0.53\nnameserver ::1\nnameserver 9.9.9.9\nnameserver 1.1.1.1\n",
    );

    assert_eq!(servers, vec!["9.9.9.9".to_owned(), "1.1.1.1".to_owned()]);
}

#[test]
fn parse_guest_dns_servers_deduplicates_repeated_ipv4_entries() {
    let servers = super::parse_guest_dns_servers("nameserver 8.8.8.8\nnameserver 8.8.8.8\n");

    assert_eq!(servers, vec!["8.8.8.8".to_owned()]);
}

#[test]
fn fallback_guest_dns_servers_returns_public_resolvers() {
    assert_eq!(
        super::fallback_guest_dns_servers(),
        vec!["1.1.1.1".to_owned(), "8.8.8.8".to_owned()]
    );
}

#[test]
fn guest_dns_servers_from_paths_prefers_non_loopback_host_resolvers() {
    let tmp = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let stub = tmp.path().join("stub-resolv.conf");
    let uplink = tmp.path().join("uplink-resolv.conf");
    std::fs::write(&stub, "nameserver 127.0.0.53\n").unwrap();
    std::fs::write(&uplink, "nameserver 185.12.64.2\nnameserver 185.12.64.1\n").unwrap();

    let servers = super::guest_dns_servers_from_paths(&[stub.as_path(), uplink.as_path()]);

    assert_eq!(
        servers,
        vec!["185.12.64.2".to_owned(), "185.12.64.1".to_owned()]
    );
}

#[test]
fn encode_guest_archive_produces_base64_gzip_stream() {
    let archive = b"test-archive";
    let encoded = super::encode_guest_archive(archive).unwrap();
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .unwrap();
    let mut decoder = flate2::read::GzDecoder::new(&compressed[..]);
    let mut decoded = Vec::new();
    std::io::Read::read_to_end(&mut decoder, &mut decoded).unwrap();

    assert_eq!(decoded, archive);
}

#[test]
fn resolve_run_command_preserves_image_entrypoint_when_cmd_is_overridden() {
    let mut config = VmConfig::new("moby/buildkit:buildx-stable-1");
    config.cmd = vec!["--debug".to_owned()];

    let command = super::resolve_run_command(
        &config,
        &image_config_with_commands(Some(vec!["buildkitd"]), None),
    );

    assert_eq!(command, vec!["buildkitd".to_owned(), "--debug".to_owned()]);
}

#[test]
fn resolve_run_command_uses_explicit_entrypoint_override() {
    let mut config = VmConfig::new("alpine:latest");
    config.entrypoint = vec!["/custom-entrypoint".to_owned()];
    config.cmd = vec!["--flag".to_owned()];

    let command = super::resolve_run_command(
        &config,
        &image_config_with_commands(Some(vec!["/image-entrypoint"]), Some(vec!["/image-cmd"])),
    );

    assert_eq!(
        command,
        vec!["/custom-entrypoint".to_owned(), "--flag".to_owned()]
    );
}

#[test]
fn load_local_image_into_cache_reads_tagged_oci_layout() {
    let store_dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let cache_dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let stored = store_local_test_image(store_dir.path(), "local:test");
    let cache = crate::oci::cache::LayerCache::new(cache_dir.path()).unwrap();
    let image_ref = crate::oci::reference::ImageReference::parse("local:test").unwrap();

    let resolved = super::load_local_image_into_cache(
        "local:test",
        store_dir.path(),
        &cache,
        image_ref.registry().as_ref(),
        image_ref.repository().as_ref(),
        image_ref.tag().map_or("latest", |tag| tag.as_ref()),
    )
    .unwrap()
    .expect("local image should resolve");

    assert_eq!(resolved.manifest.config.digest, stored.config_digest);
    assert_eq!(resolved.manifest.layers.len(), 1);
    assert_eq!(
        resolved.image_config.cmd.as_ref().unwrap(),
        &vec!["cat".to_owned(), "/hello.txt".to_owned()]
    );
    assert!(cache.get(&stored.config_digest).unwrap().is_some());
    assert!(
        cache
            .get(&resolved.manifest.layers[0].digest)
            .unwrap()
            .is_some()
    );
    assert!(
        cache
            .get_manifest(
                image_ref.registry().as_ref(),
                image_ref.repository().as_ref(),
                image_ref.tag().map_or("latest", |tag| tag.as_ref()),
            )
            .unwrap()
            .is_some()
    );
}

#[test]
fn load_local_image_into_cache_returns_none_for_unknown_tag() {
    let store_dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let cache_dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let cache = crate::oci::cache::LayerCache::new(cache_dir.path()).unwrap();

    let resolved = super::load_local_image_into_cache(
        "missing:test",
        store_dir.path(),
        &cache,
        "docker.io",
        "library/missing",
        "test",
    )
    .unwrap();

    assert!(resolved.is_none());
}

#[test]
fn load_local_image_into_cache_falls_back_to_cached_manifest_when_store_layout_is_missing() {
    let store_dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let cache_dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let stored = store_local_test_image(store_dir.path(), "local:test");
    let cache = crate::oci::cache::LayerCache::new(cache_dir.path()).unwrap();
    let image_ref = crate::oci::reference::ImageReference::parse("local:test").unwrap();

    let initial = super::load_local_image_into_cache(
        "local:test",
        store_dir.path(),
        &cache,
        image_ref.registry().as_ref(),
        image_ref.repository().as_ref(),
        image_ref.tag().map_or("latest", |tag| tag.as_ref()),
    )
    .unwrap()
    .expect("local image should resolve from the store layout");
    assert_eq!(initial.manifest.config.digest, stored.config_digest);

    let digest_hex = stored
        .manifest_digest
        .strip_prefix("sha256:")
        .unwrap_or(&stored.manifest_digest);
    std::fs::remove_dir_all(store_dir.path().join(digest_hex)).unwrap();

    let fallback = super::load_local_image_into_cache(
        "local:test",
        store_dir.path(),
        &cache,
        image_ref.registry().as_ref(),
        image_ref.repository().as_ref(),
        image_ref.tag().map_or("latest", |tag| tag.as_ref()),
    )
    .unwrap()
    .expect("cached manifest should resolve when the store layout is gone");

    assert_eq!(fallback.manifest.config.digest, stored.config_digest);
    assert_eq!(
        fallback.image_config.cmd.as_ref().unwrap(),
        &vec!["cat".to_owned(), "/hello.txt".to_owned()]
    );
}

#[test]
fn load_local_docker_archive_image_unpacks_cached_layer() {
    let store_dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let cache_dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let merged_dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let store = ImageStore::new(store_dir.path().to_path_buf());
    store
        .load_docker_archive(&make_docker_image_archive())
        .unwrap();

    let cache = crate::oci::cache::LayerCache::new(cache_dir.path()).unwrap();
    let image_ref = crate::oci::reference::ImageReference::parse("loaded:test").unwrap();
    let resolved = super::load_local_image_into_cache(
        "loaded:test",
        store_dir.path(),
        &cache,
        image_ref.registry().as_ref(),
        image_ref.repository().as_ref(),
        image_ref.tag().map_or("latest", |tag| tag.as_ref()),
    )
    .unwrap()
    .expect("docker archive image should resolve locally");

    let layer_path = cache
        .get(&resolved.manifest.layers[0].digest)
        .unwrap()
        .expect("cached layer should exist");
    let merger = crate::oci::layers::LayerMerger::new(merged_dir.path()).unwrap();
    merger.unpack_layer(&layer_path).unwrap();

    assert_eq!(
        std::fs::read_to_string(merged_dir.path().join("hello.txt")).unwrap(),
        "hello from loaded image\n"
    );
}

#[test]
fn build_port_forward_mappings_uses_explicit_guest_ip() {
    let mut config = VmConfig::new("alpine:latest");
    config.ports = vec![PortMapping::with_protocol(53, 53, "udp")];

    let mappings = super::build_port_forward_mappings(
        &config,
        std::net::Ipv4Addr::new(172, 20, 0, 9),
        std::net::Ipv4Addr::new(172, 20, 0, 10),
        true,
    )
    .expect("port mappings should be converted");

    assert_eq!(mappings.len(), 1);
    assert_eq!(mappings[0].host_port(), 53);
    assert_eq!(mappings[0].guest_port(), 53);
    assert_eq!(
        mappings[0].guest_ip(),
        std::net::Ipv4Addr::new(172, 20, 0, 9)
    );
    assert_eq!(mappings[0].protocol(), "udp");
}

#[test]
fn vm_live_state_stores_port_forward_handle() {
    // Verify VmLiveState can be constructed with Some and None handles.
    let state_none = VmLiveState {
        cid: 3,
        thread: None,
        kill_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        completion_rx: None,
        serial_output: crate::vm::SerialOutput::new(),
        tmp_dir: std::path::PathBuf::new(),
        port_forward_handle: None,
    };
    assert!(state_none.port_forward_handle.is_none());

    let state_some = VmLiveState {
        cid: 4,
        thread: None,
        kill_flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        completion_rx: None,
        serial_output: crate::vm::SerialOutput::new(),
        tmp_dir: std::path::PathBuf::new(),
        port_forward_handle: Some(Box::new(MockPortForwardHandle { count: 2 })),
    };
    assert!(state_some.port_forward_handle.is_some());
}

#[tokio::test]
async fn copy_to_guest_for_running_vm_uses_vsock_connector() {
    let connector = std::sync::Arc::new(MockVsockConnector::new(0, ""));
    let backend = VmmBackend::with_connector(connector.clone());
    let vm = test_vm("vm-copy", None, "alpine:latest", VmState::Running);
    backend.insert_vm_with_cid(vm, 7).await;

    backend
        .copy_to_guest("vm-copy", b"archive-data".to_vec(), "/etc")
        .await
        .unwrap();

    let calls = connector.copy_calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0].0, 7);
    assert_eq!(calls[0].1, "/etc");
    assert_eq!(calls[0].2, b"archive-data".to_vec());
}

#[tokio::test]
async fn shutdown_all_running_vms_marks_live_vms_stopped() {
    let backend = mock_backend(0, "");

    let first = test_vm("vm-shutdown-1", None, "alpine:latest", VmState::Running);
    let second = test_vm("vm-shutdown-2", None, "alpine:latest", VmState::Running);
    backend.insert_vm_with_cid(first, 7).await;
    backend.insert_vm_with_cid(second, 8).await;

    backend.shutdown_all_running_vms().await;

    let stopped = backend.list().await.unwrap();
    assert!(
        stopped.iter().all(|vm| vm.state == VmState::Stopped),
        "all live VMs should be marked stopped after daemon shutdown"
    );

    let live = backend.live_vms.read().await;
    assert!(
        live.is_empty(),
        "daemon shutdown should remove live VM state so TAP/NAT resources can drop"
    );
}

/// Mock port-forward handle for testing VmLiveState construction.
struct MockPortForwardHandle {
    count: usize,
}

impl visor_vmm::net::PortForwardHandle for MockPortForwardHandle {
    fn mapping_count(&self) -> usize {
        self.count
    }

    fn teardown(&mut self) -> Result<(), visor_vmm::net::NetError> {
        Ok(())
    }
}

// ── create_from_snapshot fast path ────────────────────────────

#[tokio::test]
async fn create_from_snapshot_fails_with_missing_snapshot_dir() {
    let backend = mock_backend(0, "");
    let config = VmConfig::new("alpine:latest");
    let result = backend
        .create_from_snapshot(config, std::path::Path::new("/nonexistent/snapshot"))
        .await;
    assert!(
        result.is_err(),
        "create_from_snapshot should fail with missing snapshot dir"
    );
}

#[tokio::test]
async fn create_from_snapshot_fails_with_empty_snapshot_dir() {
    let backend = mock_backend(0, "");
    let config = VmConfig::new("alpine:latest");
    let dir = crate::testutil::tempdir("visor-runtime-backend-").unwrap();
    let result = backend.create_from_snapshot(config, dir.path()).await;
    assert!(
        result.is_err(),
        "create_from_snapshot should fail with empty snapshot dir (no memory.bin)"
    );
}
