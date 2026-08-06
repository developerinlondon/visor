//! OCI image assembly from built layers and metadata.
//!
//! Takes [`ProcessedLayer`]s and [`ImageMetadata`] and produces a
//! complete [OCI image layout](https://github.com/opencontainers/image-spec/blob/v1.1.0/image-layout.md)
//! on disk, plus a simple tag store backed by JSON.

use std::collections::HashMap;
use std::fs;
use std::io::{Cursor, Read as _};
use std::path::{Path, PathBuf};

use anyhow::Context as _;
use flate2::Compression;
use flate2::write::GzEncoder;
use oci_spec::image::{
    Arch, ConfigBuilder, DescriptorBuilder, HistoryBuilder, ImageConfigurationBuilder,
    ImageIndexBuilder, ImageManifestBuilder, MediaType, Os, RootFsBuilder, SCHEMA_VERSION,
    Sha256Digest,
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::engine::ImageMetadata;
use crate::layer::ProcessedLayer;

// ── Constants ───────────────────────────────────────────────────────────

/// OCI image layout version identifier.
const OCI_LAYOUT_VERSION: &str = r#"{"imageLayoutVersion":"1.0.0"}"#;

/// Tags file name within the store directory.
const TAGS_FILE: &str = "tags.json";

// ── ImageAssembler ──────────────────────────────────────────────────────

/// Assembles built layers and metadata into a complete OCI image.
pub struct ImageAssembler;

impl ImageAssembler {
    /// Assemble an OCI image from build results and write to disk.
    ///
    /// Creates the OCI layout directory structure at `output_dir`:
    /// ```text
    /// output_dir/
    ///   oci-layout          → {"imageLayoutVersion": "1.0.0"}
    ///   index.json           → image index pointing to manifest
    ///   blobs/sha256/{hash}  → config, manifest, and layer blobs
    /// ```
    ///
    /// # Errors
    ///
    /// Returns an error if directory creation or file writing fails.
    pub fn assemble(
        layers: &[ProcessedLayer],
        metadata: &ImageMetadata,
        output_dir: &Path,
    ) -> anyhow::Result<StoredImage> {
        let blobs_dir = output_dir.join("blobs").join("sha256");
        fs::create_dir_all(&blobs_dir).context("creating blobs/sha256 directory")?;

        // Write layer blobs.
        for layer in layers {
            write_blob(&blobs_dir, &layer.digest, &layer.compressed_data)?;
        }

        // Build + write OCI image config.
        let arch = host_architecture();
        let config_json =
            build_config_json(layers, metadata, &arch).context("building OCI image config")?;
        let config_digest = write_json_blob(&blobs_dir, &config_json)?;

        // Build + write OCI manifest.
        let manifest_json = build_manifest_json(layers, &config_digest, config_json.len() as u64)
            .context("building OCI manifest")?;
        let manifest_digest = write_json_blob(&blobs_dir, &manifest_json)?;

        // Write OCI layout marker.
        write_oci_layout(output_dir)?;

        // Write index.json.
        write_index_json(output_dir, &manifest_digest, manifest_json.len() as u64)?;

        let total_size = layers.iter().map(|l| l.compressed_size).sum();

        Ok(StoredImage {
            manifest_digest,
            config_digest,
            total_size,
            architecture: arch,
            os: "linux".to_owned(),
        })
    }
}

// ── StoredImage ─────────────────────────────────────────────────────────

/// Metadata about a stored OCI image.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StoredImage {
    /// Manifest digest (`sha256:...`).
    pub manifest_digest: String,
    /// Config digest (`sha256:...`).
    pub config_digest: String,
    /// Total compressed size of all layers.
    pub total_size: u64,
    /// Architecture (e.g. `"amd64"`, `"arm64"`).
    pub architecture: String,
    /// OS (always `"linux"`).
    pub os: String,
}

// ── ImageStore ──────────────────────────────────────────────────────────

/// Simple image tag store backed by a JSON file.
///
/// Tags are persisted as a JSON object mapping tag strings to manifest
/// digest strings. The file lives at `{store_dir}/tags.json`.
pub struct ImageStore {
    store_dir: PathBuf,
}

impl ImageStore {
    /// Create or open an image store at the given directory.
    #[must_use]
    pub fn new(store_dir: PathBuf) -> Self {
        Self { store_dir }
    }

    /// Tag an image by digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the tags file cannot be read or written.
    pub fn tag(&self, tag: &str, manifest_digest: &str) -> anyhow::Result<()> {
        let mut tags = self.load_tags()?;
        tags.insert(tag.to_owned(), manifest_digest.to_owned());
        self.save_tags(&tags)
    }

    /// Get manifest digest for a tag.
    ///
    /// # Errors
    ///
    /// Returns an error if the tags file cannot be read.
    pub fn get_by_tag(&self, tag: &str) -> anyhow::Result<Option<String>> {
        let tags = self.load_tags()?;
        Ok(tags.get(tag).cloned())
    }

    /// List all tags as `(tag, manifest_digest)` pairs.
    ///
    /// # Errors
    ///
    /// Returns an error if the tags file cannot be read.
    pub fn list_tags(&self) -> anyhow::Result<Vec<(String, String)>> {
        let tags = self.load_tags()?;
        Ok(tags.into_iter().collect())
    }

    /// Remove a tag. Returns `true` if the tag existed.
    ///
    /// # Errors
    ///
    /// Returns an error if the tags file cannot be read or written.
    pub fn remove_tag(&self, tag: &str) -> anyhow::Result<bool> {
        let mut tags = self.load_tags()?;
        let existed = tags.remove(tag).is_some();
        if existed {
            self.save_tags(&tags)?;
        }
        Ok(existed)
    }

    /// Load a Docker image archive into the OCI-backed image store.
    ///
    /// Accepts the tar stream produced by Docker-compatible save/export flows,
    /// converts it into Visor's OCI on-disk layout, and tags the imported
    /// images in the local tag store.
    ///
    /// Returns the loaded repo tags, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the archive cannot be parsed, required files are
    /// missing, or the OCI image cannot be assembled into the store.
    pub fn load_docker_archive(&self, archive: &[u8]) -> anyhow::Result<Vec<String>> {
        let files = docker_archive_files(archive)?;
        let manifest_bytes = files
            .get("manifest.json")
            .context("docker archive missing manifest.json")?;
        let manifests: Vec<DockerArchiveManifestEntry> =
            serde_json::from_slice(manifest_bytes).context("parsing docker archive manifest")?;

        let mut loaded_tags = Vec::new();
        for manifest in manifests {
            let config_bytes = files
                .get(&manifest.config)
                .with_context(|| format!("docker archive missing config {}", manifest.config))?;
            let config: DockerArchiveConfig =
                serde_json::from_slice(config_bytes).context("parsing docker config blob")?;

            let layers = manifest
                .layers
                .iter()
                .map(|layer_path| {
                    let layer_bytes = files
                        .get(layer_path)
                        .with_context(|| format!("docker archive missing layer {layer_path}"))?;
                    processed_layer_from_docker_archive(layer_bytes)
                })
                .collect::<anyhow::Result<Vec<_>>>()?;

            let repo_tags = manifest.repo_tags.unwrap_or_default();
            let metadata = image_metadata_from_docker_config(&config);
            store_assembled_image(&self.store_dir, &layers, &metadata, &repo_tags)
                .context("storing imported docker image")?;
            loaded_tags.extend(repo_tags);
        }

        loaded_tags.sort();
        loaded_tags.dedup();
        Ok(loaded_tags)
    }

    // ── Private helpers ─────────────────────────────────────────────────

    /// Path to the tags JSON file.
    fn tags_path(&self) -> PathBuf {
        self.store_dir.join(TAGS_FILE)
    }

    /// Load the tags map from disk. Returns empty map if file doesn't exist.
    fn load_tags(&self) -> anyhow::Result<HashMap<String, String>> {
        let path = self.tags_path();
        if !path.exists() {
            return Ok(HashMap::new());
        }
        let data = fs::read_to_string(&path).context("reading tags file")?;
        let tags: HashMap<String, String> =
            serde_json::from_str(&data).context("parsing tags file")?;
        Ok(tags)
    }

    /// Write the tags map to disk atomically.
    fn save_tags(&self, tags: &HashMap<String, String>) -> anyhow::Result<()> {
        fs::create_dir_all(&self.store_dir).context("creating store directory")?;
        let data = serde_json::to_string_pretty(tags).context("serializing tags")?;
        fs::write(self.tags_path(), data).context("writing tags file")?;
        Ok(())
    }
}

#[derive(Debug, Deserialize)]
struct DockerArchiveManifestEntry {
    #[serde(rename = "Config")]
    config: String,
    #[serde(rename = "RepoTags")]
    repo_tags: Option<Vec<String>>,
    #[serde(rename = "Layers")]
    layers: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct DockerArchiveConfig {
    config: Option<DockerArchiveContainerConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct DockerArchiveContainerConfig {
    #[serde(rename = "Cmd")]
    cmd: Option<Vec<String>>,
    #[serde(rename = "Entrypoint")]
    entrypoint: Option<Vec<String>>,
    #[serde(rename = "Env")]
    env: Option<Vec<String>>,
    #[serde(rename = "WorkingDir")]
    working_dir: Option<String>,
    #[serde(rename = "User")]
    user: Option<String>,
    #[serde(rename = "ExposedPorts")]
    exposed_ports: Option<HashMap<String, serde_json::Value>>,
    #[serde(rename = "Labels")]
    labels: Option<HashMap<String, String>>,
    #[serde(rename = "StopSignal")]
    stop_signal: Option<String>,
    #[serde(rename = "Volumes")]
    volumes: Option<HashMap<String, serde_json::Value>>,
}

fn docker_archive_files(archive: &[u8]) -> anyhow::Result<HashMap<String, Vec<u8>>> {
    let mut files = HashMap::new();
    let mut archive = tar::Archive::new(Cursor::new(archive));

    for entry_result in archive
        .entries()
        .context("reading docker archive entries")?
    {
        let mut entry = entry_result.context("reading docker archive entry")?;
        if !entry.header().entry_type().is_file() {
            continue;
        }

        let raw_path = entry
            .path()
            .context("reading docker archive entry path")?
            .to_string_lossy()
            .into_owned();
        let normalized_path = raw_path.strip_prefix("./").unwrap_or(&raw_path).to_owned();

        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .with_context(|| format!("reading docker archive file {normalized_path}"))?;
        files.insert(normalized_path, bytes);
    }

    Ok(files)
}

fn processed_layer_from_docker_archive(layer_tar: &[u8]) -> anyhow::Result<ProcessedLayer> {
    let (compressed_data, uncompressed_data) = if gzip_magic_bytes(layer_tar) {
        let mut decoder = flate2::read::GzDecoder::new(layer_tar);
        let mut uncompressed_data = Vec::new();
        decoder
            .read_to_end(&mut uncompressed_data)
            .context("decompressing precompressed docker layer")?;
        (layer_tar.to_vec(), uncompressed_data)
    } else {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        std::io::Write::write_all(&mut encoder, layer_tar).context("compressing docker layer")?;
        let compressed_data = encoder.finish().context("finalizing compressed layer")?;
        (compressed_data, layer_tar.to_vec())
    };

    let diff_id = format!("sha256:{:x}", Sha256::digest(&uncompressed_data));
    let digest = format!("sha256:{:x}", Sha256::digest(&compressed_data));
    let compressed_size =
        u64::try_from(compressed_data.len()).context("compressed layer size exceeds u64")?;
    let uncompressed_size =
        u64::try_from(uncompressed_data.len()).context("uncompressed layer size exceeds u64")?;

    Ok(ProcessedLayer::new(
        compressed_data,
        digest,
        diff_id,
        compressed_size,
        uncompressed_size,
        MediaType::ImageLayerGzip.to_string(),
        false,
    ))
}

fn gzip_magic_bytes(data: &[u8]) -> bool {
    data.starts_with(&[0x1f, 0x8b])
}

fn image_metadata_from_docker_config(config: &DockerArchiveConfig) -> ImageMetadata {
    let container = config.config.clone().unwrap_or_default();

    ImageMetadata {
        cmd: container.cmd,
        entrypoint: container.entrypoint,
        env: container
            .env
            .unwrap_or_default()
            .into_iter()
            .filter_map(|entry| {
                let (key, value) = entry.split_once('=')?;
                Some((key.to_owned(), value.to_owned()))
            })
            .collect(),
        working_dir: container.working_dir.filter(|value| !value.is_empty()),
        user: container.user.filter(|value| !value.is_empty()),
        exposed_ports: container
            .exposed_ports
            .unwrap_or_default()
            .into_keys()
            .filter_map(|entry| {
                let (port, proto) = entry.split_once('/')?;
                Some((port.parse().ok()?, proto.to_owned()))
            })
            .collect(),
        labels: container.labels.unwrap_or_default().into_iter().collect(),
        shell: None,
        stop_signal: container.stop_signal.filter(|value| !value.is_empty()),
        volumes: container.volumes.unwrap_or_default().into_keys().collect(),
    }
}

fn store_assembled_image(
    store_dir: &Path,
    layers: &[ProcessedLayer],
    metadata: &ImageMetadata,
    tags: &[String],
) -> anyhow::Result<StoredImage> {
    fs::create_dir_all(store_dir).context("creating image store directory")?;

    let staging_dir = store_dir.join(format!("staging-{}", Uuid::new_v4()));
    let stored = ImageAssembler::assemble(layers, metadata, &staging_dir)
        .context("assembling imported OCI image")?;

    let digest_hex = stored
        .manifest_digest
        .strip_prefix("sha256:")
        .unwrap_or(&stored.manifest_digest);
    let output_dir = store_dir.join(digest_hex);

    if output_dir.exists() {
        fs::remove_dir_all(&output_dir).context("removing existing imported image directory")?;
    }
    fs::rename(&staging_dir, &output_dir).context("moving imported image into store")?;

    let store = ImageStore::new(store_dir.to_path_buf());
    for tag in tags {
        store
            .tag(tag, &stored.manifest_digest)
            .with_context(|| format!("tagging imported image as {tag}"))?;
    }
    store
        .tag(&stored.manifest_digest, &stored.manifest_digest)
        .context("tagging imported image by digest")?;

    Ok(stored)
}

// ── OCI Config Generation ───────────────────────────────────────────────

/// Build the OCI image configuration JSON bytes.
fn build_config_json(
    layers: &[ProcessedLayer],
    metadata: &ImageMetadata,
    arch: &str,
) -> anyhow::Result<Vec<u8>> {
    // Container config section.
    let mut config_builder = ConfigBuilder::default();

    if let Some(cmd) = &metadata.cmd {
        config_builder = config_builder.cmd(cmd.clone());
    }
    if let Some(ep) = &metadata.entrypoint {
        config_builder = config_builder.entrypoint(ep.clone());
    }
    if !metadata.env.is_empty() {
        let env_vec: Vec<String> = metadata
            .env
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect();
        config_builder = config_builder.env(env_vec);
    }
    if let Some(wd) = &metadata.working_dir {
        config_builder = config_builder.working_dir(wd.clone());
    }
    if let Some(user) = &metadata.user {
        config_builder = config_builder.user(user.clone());
    }
    if let Some(sig) = &metadata.stop_signal {
        config_builder = config_builder.stop_signal(sig.clone());
    }
    if !metadata.exposed_ports.is_empty() {
        let ports: Vec<String> = metadata
            .exposed_ports
            .iter()
            .map(|(port, proto)| format!("{port}/{proto}"))
            .collect();
        config_builder = config_builder.exposed_ports(ports);
    }
    if !metadata.volumes.is_empty() {
        config_builder = config_builder.volumes(metadata.volumes.clone());
    }
    if !metadata.labels.is_empty() {
        let labels: HashMap<String, String> = metadata.labels.iter().cloned().collect();
        config_builder = config_builder.labels(labels);
    }

    let container_config = config_builder
        .build()
        .context("building container config")?;

    // RootFs.
    let diff_ids: Vec<String> = layers.iter().map(|l| l.diff_id.clone()).collect();
    let rootfs = RootFsBuilder::default()
        .typ("layers")
        .diff_ids(diff_ids)
        .build()
        .context("building rootfs")?;

    // History.
    let history: Vec<_> = layers
        .iter()
        .enumerate()
        .map(|(i, _)| {
            HistoryBuilder::default()
                .created_by(format!("layer {i}"))
                .build()
                .expect("history entry is valid")
        })
        .collect();

    let oci_arch = parse_arch(arch);
    let image_config = ImageConfigurationBuilder::default()
        .architecture(oci_arch)
        .os(Os::Linux)
        .config(container_config)
        .rootfs(rootfs)
        .history(history)
        .build()
        .context("building image configuration")?;

    serde_json::to_vec(&image_config).context("serializing image config")
}

// ── OCI Manifest Generation ─────────────────────────────────────────────

/// Build the OCI image manifest JSON bytes.
fn build_manifest_json(
    layers: &[ProcessedLayer],
    config_digest: &str,
    config_size: u64,
) -> anyhow::Result<Vec<u8>> {
    let config_hash = config_digest
        .strip_prefix("sha256:")
        .context("config digest missing sha256: prefix")?;
    let config_descriptor = DescriptorBuilder::default()
        .media_type(MediaType::ImageConfig)
        .size(config_size)
        .digest(
            config_hash
                .parse::<Sha256Digest>()
                .map_err(|e| anyhow::anyhow!("invalid config digest: {e}"))?,
        )
        .build()
        .context("building config descriptor")?;

    let layer_descriptors: Vec<_> = layers
        .iter()
        .map(|layer| {
            let hash = layer
                .digest
                .strip_prefix("sha256:")
                .context("layer digest missing sha256: prefix")?;
            DescriptorBuilder::default()
                .media_type(MediaType::ImageLayerGzip)
                .size(layer.compressed_size)
                .digest(
                    hash.parse::<Sha256Digest>()
                        .map_err(|e| anyhow::anyhow!("invalid layer digest: {e}"))?,
                )
                .build()
                .context("building layer descriptor")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let manifest = ImageManifestBuilder::default()
        .schema_version(SCHEMA_VERSION)
        .media_type(MediaType::ImageManifest)
        .config(config_descriptor)
        .layers(layer_descriptors)
        .build()
        .context("building image manifest")?;

    serde_json::to_vec(&manifest).context("serializing manifest")
}

// ── OCI Layout Helpers ──────────────────────────────────────────────────

/// Write the `oci-layout` file.
fn write_oci_layout(output_dir: &Path) -> anyhow::Result<()> {
    fs::write(output_dir.join("oci-layout"), OCI_LAYOUT_VERSION).context("writing oci-layout file")
}

/// Write `index.json` pointing to the manifest.
fn write_index_json(
    output_dir: &Path,
    manifest_digest: &str,
    manifest_size: u64,
) -> anyhow::Result<()> {
    let manifest_hash = manifest_digest
        .strip_prefix("sha256:")
        .context("manifest digest missing sha256: prefix")?;
    let manifest_descriptor = DescriptorBuilder::default()
        .media_type(MediaType::ImageManifest)
        .size(manifest_size)
        .digest(
            manifest_hash
                .parse::<Sha256Digest>()
                .map_err(|e| anyhow::anyhow!("invalid manifest digest: {e}"))?,
        )
        .build()
        .context("building manifest descriptor for index")?;

    let index = ImageIndexBuilder::default()
        .schema_version(SCHEMA_VERSION)
        .manifests(vec![manifest_descriptor])
        .build()
        .context("building image index")?;

    let index_json = serde_json::to_string(&index).context("serializing index")?;
    fs::write(output_dir.join("index.json"), index_json).context("writing index.json")
}

// ── Blob Helpers ────────────────────────────────────────────────────────

/// Write raw data as a blob file at `blobs_dir/{hash}`.
fn write_blob(blobs_dir: &Path, digest: &str, data: &[u8]) -> anyhow::Result<()> {
    let hash = digest
        .strip_prefix("sha256:")
        .context("digest missing sha256: prefix")?;
    fs::write(blobs_dir.join(hash), data).context("writing blob file")
}

/// Write JSON bytes as a blob, computing the sha256 digest. Returns `sha256:{hex}`.
fn write_json_blob(blobs_dir: &Path, json_bytes: &[u8]) -> anyhow::Result<String> {
    let hash = Sha256::digest(json_bytes);
    let hex_hash = hex::encode(hash);
    let digest = format!("sha256:{hex_hash}");
    fs::write(blobs_dir.join(&hex_hash), json_bytes).context("writing JSON blob")?;
    Ok(digest)
}

// ── Architecture Helpers ────────────────────────────────────────────────

/// Detect host architecture and map to OCI architecture string.
fn host_architecture() -> String {
    match std::env::consts::ARCH {
        "x86_64" => "amd64",
        "aarch64" => "arm64",
        other => other,
    }
    .to_owned()
}

/// Parse an architecture string into the `oci_spec` `Arch` enum.
fn parse_arch(arch: &str) -> Arch {
    match arch {
        "amd64" => Arch::Amd64,
        "arm64" => Arch::ARM64,
        _ => Arch::Other(arch.to_owned()),
    }
}

#[cfg(test)]
#[path = "assembly_test.rs"]
mod tests;
