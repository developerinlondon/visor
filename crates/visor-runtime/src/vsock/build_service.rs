//! [`BuildService`] implementation that orchestrates real builds via VMs.
//!
//! `VmmBuildService` boots a build VM, connects over vsock, runs the
//! build engine, and cleans up the VM afterward.  This wires together
//! the Docker `/build` endpoint with visor's microVM infrastructure.

use anyhow::Context as _;
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tracing::{debug, info, warn};
use visor_types::{
    BuildOutput, BuildProgress, BuildRequest, BuildService, ExecutionBackend, ImageManager,
    VmConfig,
};

use super::client::{VSOCK_AGENT_PORT, VsockClient};
use super::executor::VsockBuildExecutor;

/// Helper image used to boot build VMs with basic userland tools available.
const BUILD_VM_IMAGE: &str = "alpine:latest";
const BUILD_VM_AGENT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const BUILD_VM_AGENT_RETRY_INTERVAL: Duration = Duration::from_millis(250);

/// Orchestrates OCI image builds inside ephemeral microVMs.
///
/// Creates a short-lived VM for each build, connects to the guest agent
/// over vsock, runs the Dockerfile through [`visor_build::BuildEngine`],
/// and tears down the VM when complete (or on error).
pub struct VmmBuildService {
    backend: Arc<dyn ExecutionBackend>,
    image_store_path: PathBuf,
}

impl VmmBuildService {
    /// Create a new build service backed by the given execution backend.
    ///
    /// `image_store_path` is the directory used for OCI image storage
    /// (e.g. `~/.visor/images/`).
    #[must_use]
    pub fn new(backend: Arc<dyn ExecutionBackend>, image_store_path: PathBuf) -> Self {
        Self {
            backend,
            image_store_path,
        }
    }
}

#[async_trait]
impl BuildService for VmmBuildService {
    /// Build an OCI image from a Dockerfile inside an ephemeral VM.
    ///
    /// # Workflow
    ///
    /// 1. Parse the Dockerfile
    /// 2. Boot a minimal build VM
    /// 3. Connect to the guest agent over vsock
    /// 4. Run [`BuildEngine::build()`] via [`VsockBuildExecutor`]
    /// 5. Convert the result into [`BuildOutput`]
    /// 6. Destroy the build VM (always, even on error)
    ///
    /// # Errors
    ///
    /// Returns an error if any step fails: Dockerfile parsing, VM boot,
    /// vsock connection, build execution, or image assembly.
    async fn build_image(&self, request: BuildRequest) -> anyhow::Result<BuildOutput> {
        // 1. Parse Dockerfile
        let parsed = visor_build::DockerfileParser::parse(&request.dockerfile_content)
            .context("failed to parse Dockerfile")?;

        // 2. Create build VM config
        let mut vm_config = VmConfig::new(BUILD_VM_IMAGE);
        vm_config.memory_mib = 256;
        vm_config.vcpus = 1;
        vm_config.detach = true;
        vm_config.mode = Some("agent".to_owned());

        debug!("booting build VM");

        // 3. Boot the VM
        let vm_info = self
            .backend
            .create(vm_config)
            .await
            .context("failed to create build VM")?;

        let vm_id = vm_info.id.clone();
        info!(vm_id = %vm_id, "build VM created");

        // Use a guard to ensure we always clean up the VM
        let result = self.run_build(&vm_info, &request, parsed).await;

        // 6. Always destroy the build VM
        debug!(vm_id = %vm_id, "destroying build VM");
        if let Err(e) = self.backend.destroy(&vm_id).await {
            warn!(vm_id = %vm_id, error = %e, "failed to destroy build VM");
        }

        result
    }
}

impl VmmBuildService {
    /// Inner build logic, separated so the caller can always run cleanup.
    ///
    /// # Errors
    ///
    /// Returns an error if vsock connection or build execution fails.
    async fn run_build(
        &self,
        vm_info: &visor_types::VmInfo,
        request: &BuildRequest,
        parsed: visor_build::ParsedDockerfile,
    ) -> anyhow::Result<BuildOutput> {
        // 4. Get CID and connect vsock
        let cid = vm_info.cid.context("build VM has no CID assigned")?;

        debug!(cid, "connecting to build VM agent");

        let comms = crate::backend::comms_backend();
        let start = Instant::now();
        let client = loop {
            match VsockClient::connect(&comms, cid, VSOCK_AGENT_PORT).await {
                Ok(client) => break client,
                Err(error) if start.elapsed() < BUILD_VM_AGENT_CONNECT_TIMEOUT => {
                    debug!(
                        cid,
                        error = %error,
                        "build VM agent not ready yet, retrying"
                    );
                    tokio::time::sleep(BUILD_VM_AGENT_RETRY_INTERVAL).await;
                }
                Err(error) => {
                    return Err(error).context("failed to connect to build VM agent");
                }
            }
        };

        // 5. Create executor and engine
        let executor = VsockBuildExecutor::new(client);

        let mut config = visor_build::BuildConfig::new(parsed, request.context_dir.clone());
        config.build_args.clone_from(&request.build_args);
        config.target.clone_from(&request.target);
        config.no_cache = request.no_cache;
        config.tag.clone_from(&request.tag);

        let engine = visor_build::BuildEngine::new(executor, config);

        info!("starting build");
        let build_result = engine.build().await.context("build execution failed")?;

        // Convert BuildResult → BuildOutput
        let steps: Vec<BuildProgress> = build_result
            .steps
            .iter()
            .map(|s| BuildProgress::new(s.number, s.total, s.instruction.clone()))
            .collect();

        let (layers, metadata) = self
            .resolve_assembly_inputs(&build_result)
            .await
            .context("resolve image assembly inputs")?;

        let image_id = if layers.is_empty() {
            "sha256:empty".to_owned()
        } else {
            // Assemble OCI image on disk
            let stored = self
                .assemble_image(&layers, &metadata, request.tag.as_deref())
                .context("assembling OCI image")?;
            stored.manifest_digest
        };

        info!(image_id = %image_id, steps = steps.len(), "build completed");

        Ok(BuildOutput::new(image_id, steps))
    }

    /// Assemble an OCI image on disk and tag it in the image store.
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation, assembly, or tagging fails.
    fn assemble_image(
        &self,
        layers: &[visor_build::ProcessedLayer],
        metadata: &visor_build::ImageMetadata,
        tag: Option<&str>,
    ) -> anyhow::Result<visor_build::StoredImage> {
        std::fs::create_dir_all(&self.image_store_path)
            .context("creating image store directory")?;

        // Stage into a temp directory under the store path.
        // After assembly we know the digest and can rename.
        let staging_name = format!("staging-{}", uuid::Uuid::new_v4());
        let staging_dir = self.image_store_path.join(&staging_name);

        let stored = visor_build::ImageAssembler::assemble(layers, metadata, &staging_dir)
            .context("OCI image assembly")?;

        // Rename staging dir to final {digest_hex}/ path
        let digest_hex = stored
            .manifest_digest
            .strip_prefix("sha256:")
            .unwrap_or(&stored.manifest_digest);
        let output_dir = self.image_store_path.join(digest_hex);

        if output_dir.exists() {
            std::fs::remove_dir_all(&output_dir).context("removing existing image directory")?;
        }
        std::fs::rename(&staging_dir, &output_dir).context("moving assembled image to store")?;

        // Tag in the image store
        let store = visor_build::ImageStore::new(self.image_store_path.clone());
        if let Some(t) = tag {
            store
                .tag(t, &stored.manifest_digest)
                .context("tagging image")?;
        }
        // Also auto-tag by digest
        store
            .tag(&stored.manifest_digest, &stored.manifest_digest)
            .context("auto-tagging image by digest")?;

        Ok(stored)
    }

    async fn resolve_assembly_inputs(
        &self,
        build_result: &visor_build::BuildResult,
    ) -> anyhow::Result<(Vec<visor_build::ProcessedLayer>, visor_build::ImageMetadata)> {
        let built_layers: Vec<visor_build::ProcessedLayer> = build_result
            .layers
            .iter()
            .filter(|layer| !layer.empty)
            .map(built_layer_to_processed)
            .collect::<anyhow::Result<Vec<_>>>()
            .context("converting built layers to processed layers")?;

        let Some(base_image) = build_result.base_image.as_deref() else {
            return Ok((built_layers, build_result.config.clone()));
        };

        self.ensure_base_image_available(base_image)
            .await
            .with_context(|| format!("ensure base image {base_image} is available locally"))?;
        let mut base = resolve_base_image(base_image, &self.image_store_path)
            .with_context(|| format!("resolve base image {base_image}"))?;
        base.layers.extend(built_layers);
        let metadata = merge_image_metadata(base.metadata, build_result.config.clone());
        Ok((base.layers, metadata))
    }

    async fn ensure_base_image_available(&self, reference: &str) -> anyhow::Result<()> {
        if reference == "scratch" {
            return Ok(());
        }

        let store = visor_build::ImageStore::new(self.image_store_path.clone());
        if image_store_manifest_digest(&store, reference)?.is_some() {
            return Ok(());
        }

        crate::image_manager::RuntimeImageManager::new(self.image_store_path.clone())
            .pull_image(reference)
            .await
            .with_context(|| format!("pull base image {reference} into local store"))?;
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
struct ResolvedBaseImage {
    layers: Vec<visor_build::ProcessedLayer>,
    metadata: visor_build::ImageMetadata,
}

#[derive(Debug, serde::Deserialize)]
struct StoredImageRootfs {
    rootfs: StoredImageRootfsData,
}

#[derive(Debug, serde::Deserialize)]
struct StoredImageRootfsData {
    diff_ids: Vec<String>,
}

fn load_stored_base_image(reference: &str, store_dir: &Path) -> anyhow::Result<ResolvedBaseImage> {
    if reference == "scratch" {
        return Ok(ResolvedBaseImage::default());
    }

    let store = visor_build::ImageStore::new(store_dir.to_path_buf());
    let manifest_digest = image_store_manifest_digest(&store, reference)?
        .with_context(|| format!("image {reference} not found in local store"))?;
    let image_dir = store_dir.join(
        manifest_digest
            .strip_prefix("sha256:")
            .unwrap_or(&manifest_digest),
    );

    let manifest_path = oci_blob_path(&image_dir, &manifest_digest);
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read manifest {}", manifest_path.display()))?;
    let manifest: crate::oci::registry::Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse local OCI manifest")?;

    let config_path = oci_blob_path(&image_dir, &manifest.config.digest);
    let config_bytes = std::fs::read(&config_path)
        .with_context(|| format!("read image config {}", config_path.display()))?;
    let image_config = crate::oci::config::ImageConfig::from_json(&config_bytes)
        .context("parse local image config")?;
    let rootfs: StoredImageRootfs =
        serde_json::from_slice(&config_bytes).context("parse local image rootfs")?;

    anyhow::ensure!(
        rootfs.rootfs.diff_ids.len() == manifest.layers.len(),
        "rootfs diff_ids count {} did not match manifest layer count {} for {reference}",
        rootfs.rootfs.diff_ids.len(),
        manifest.layers.len()
    );

    let layers = manifest
        .layers
        .iter()
        .zip(rootfs.rootfs.diff_ids.iter())
        .map(|(descriptor, diff_id)| {
            let layer_path = oci_blob_path(&image_dir, &descriptor.digest);
            let compressed_data = std::fs::read(&layer_path)
                .with_context(|| format!("read base layer {}", layer_path.display()))?;
            Ok(visor_build::ProcessedLayer::new(
                compressed_data,
                descriptor.digest.clone(),
                diff_id.clone(),
                descriptor.size,
                0,
                descriptor.media_type.clone(),
                false,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(ResolvedBaseImage {
        layers,
        metadata: image_metadata_from_config(&image_config),
    })
}

fn resolve_base_image(reference: &str, store_dir: &Path) -> anyhow::Result<ResolvedBaseImage> {
    load_stored_base_image(reference, store_dir).or_else(|_| load_cached_base_image(reference))
}

fn load_cached_base_image(reference: &str) -> anyhow::Result<ResolvedBaseImage> {
    if reference == "scratch" {
        return Ok(ResolvedBaseImage::default());
    }

    let image_ref =
        crate::oci::reference::ImageReference::parse(reference).context("parse image reference")?;
    let registry = image_ref.registry().as_ref();
    let repository = image_ref.repository().as_ref();
    let tag = image_ref.tag().map_or("latest", |value| value.as_ref());
    let cache = crate::oci::cache::LayerCache::new(crate::oci::cache::LayerCache::default_path()?)
        .context("open layer cache")?;
    let manifest_bytes = cache
        .get_manifest(registry, repository, tag)
        .with_context(|| format!("read cached manifest for {reference}"))?
        .with_context(|| format!("cached manifest missing for {reference}"))?;
    let manifest: crate::oci::registry::Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse cached OCI manifest")?;

    let config_path = cache
        .get(&manifest.config.digest)
        .with_context(|| format!("lookup cached config blob {}", manifest.config.digest))?
        .with_context(|| format!("cached config blob missing for {}", manifest.config.digest))?;
    let config_bytes = std::fs::read(&config_path)
        .with_context(|| format!("read cached image config {}", config_path.display()))?;
    let image_config = crate::oci::config::ImageConfig::from_json(&config_bytes)
        .context("parse cached image config")?;
    let rootfs: StoredImageRootfs =
        serde_json::from_slice(&config_bytes).context("parse cached image rootfs")?;

    anyhow::ensure!(
        rootfs.rootfs.diff_ids.len() == manifest.layers.len(),
        "cached rootfs diff_ids count {} did not match manifest layer count {} for {reference}",
        rootfs.rootfs.diff_ids.len(),
        manifest.layers.len()
    );

    let layers = manifest
        .layers
        .iter()
        .zip(rootfs.rootfs.diff_ids.iter())
        .map(|(descriptor, diff_id)| {
            let layer_path = cache
                .get(&descriptor.digest)
                .with_context(|| format!("lookup cached layer {}", descriptor.digest))?
                .with_context(|| format!("cached layer blob missing for {}", descriptor.digest))?;
            let compressed_data = std::fs::read(&layer_path)
                .with_context(|| format!("read cached layer {}", layer_path.display()))?;
            Ok(visor_build::ProcessedLayer::new(
                compressed_data,
                descriptor.digest.clone(),
                diff_id.clone(),
                descriptor.size,
                0,
                descriptor.media_type.clone(),
                false,
            ))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    Ok(ResolvedBaseImage {
        layers,
        metadata: image_metadata_from_config(&image_config),
    })
}

fn image_store_manifest_digest(
    store: &visor_build::ImageStore,
    reference: &str,
) -> anyhow::Result<Option<String>> {
    for candidate in image_store_candidates(reference) {
        if let Some(digest) = store
            .get_by_tag(&candidate)
            .with_context(|| format!("read local image tag {candidate}"))?
        {
            return Ok(Some(digest));
        }
    }
    Ok(None)
}

fn image_store_candidates(reference: &str) -> Vec<String> {
    let mut candidates = vec![reference.to_owned()];
    let tail = reference.rsplit('/').next().unwrap_or(reference);
    if !tail.contains(':') && !reference.contains('@') {
        candidates.push(format!("{reference}:latest"));
    }
    candidates.sort();
    candidates.dedup();
    candidates
}

fn oci_blob_path(image_dir: &Path, digest: &str) -> PathBuf {
    image_dir
        .join("blobs")
        .join("sha256")
        .join(digest.strip_prefix("sha256:").unwrap_or(digest))
}

fn image_metadata_from_config(
    config: &crate::oci::config::ImageConfig,
) -> visor_build::ImageMetadata {
    let mut metadata = visor_build::ImageMetadata::default();
    metadata.cmd = config.cmd.clone();
    metadata.entrypoint = config.entrypoint.clone();
    metadata.env = config
        .env
        .iter()
        .map(|entry| match entry.split_once('=') {
            Some((key, value)) => (key.to_owned(), value.to_owned()),
            None => (entry.clone(), String::new()),
        })
        .collect();
    metadata.working_dir = config.working_dir.clone();
    metadata.user = config.user.clone();
    metadata.exposed_ports = config
        .exposed_ports
        .iter()
        .map(|port| (*port, "tcp".to_owned()))
        .collect();
    metadata.labels = config
        .labels
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    metadata.stop_signal = config.stop_signal.clone();
    metadata
}

fn merge_image_metadata(
    base: visor_build::ImageMetadata,
    overlay: visor_build::ImageMetadata,
) -> visor_build::ImageMetadata {
    let mut metadata = visor_build::ImageMetadata::default();
    metadata.cmd = overlay.cmd.or(base.cmd);
    metadata.entrypoint = overlay.entrypoint.or(base.entrypoint);
    metadata.env = merge_key_value_pairs(base.env, overlay.env);
    metadata.working_dir = overlay.working_dir.or(base.working_dir);
    metadata.user = overlay.user.or(base.user);
    metadata.exposed_ports = merge_unique_items(base.exposed_ports, overlay.exposed_ports);
    metadata.labels = merge_key_value_pairs(base.labels, overlay.labels);
    metadata.shell = overlay.shell.or(base.shell);
    metadata.stop_signal = overlay.stop_signal.or(base.stop_signal);
    metadata.volumes = merge_unique_items(base.volumes, overlay.volumes);
    metadata
}

fn merge_key_value_pairs(
    base: Vec<(String, String)>,
    overlay: Vec<(String, String)>,
) -> Vec<(String, String)> {
    let overlay_keys = overlay
        .iter()
        .map(|(key, _)| key.clone())
        .collect::<BTreeSet<_>>();
    let mut merged = base
        .into_iter()
        .filter(|(key, _)| !overlay_keys.contains(key))
        .collect::<Vec<_>>();
    merged.extend(overlay);
    merged
}

fn merge_unique_items<T>(base: Vec<T>, overlay: Vec<T>) -> Vec<T>
where
    T: Ord,
{
    let mut merged = base.into_iter().collect::<BTreeSet<_>>();
    merged.extend(overlay);
    merged.into_iter().collect()
}

/// Convert a [`BuiltLayer`](visor_build::BuiltLayer) to a
/// [`ProcessedLayer`](visor_build::ProcessedLayer) by base64-decoding the data.
///
/// # Errors
///
/// Returns an error if base64 decoding fails.
pub(crate) fn built_layer_to_processed(
    layer: &visor_build::BuiltLayer,
) -> anyhow::Result<visor_build::ProcessedLayer> {
    let compressed_data = decode_layer_data(&layer.data)?;

    Ok(visor_build::ProcessedLayer::new(
        compressed_data,
        layer.compressed_digest.clone(),
        layer.uncompressed_digest.clone(),
        layer.compressed_size,
        0,
        "application/vnd.oci.image.layer.v1.tar+gzip".to_owned(),
        layer.empty,
    ))
}

fn decode_layer_data(data: &str) -> anyhow::Result<Vec<u8>> {
    use base64::Engine as _;

    base64::engine::general_purpose::STANDARD
        .decode(data)
        .or_else(|_| base64::engine::general_purpose::STANDARD_NO_PAD.decode(data))
        .context("base64-decoding layer data")
}

#[cfg(test)]
#[path = "build_service_test.rs"]
mod tests;
