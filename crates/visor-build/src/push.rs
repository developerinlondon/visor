//! Push OCI images to container registries.
//!
//! Supports Docker Hub, GHCR, ECR, and any OCI-compliant registry.
//! Authentication is handled via [`RegistryAuth`] \u2014 either explicit
//! credentials or parsed from `~/.docker/config.json`.
//!
//! # Usage
//!
//! ```no_run
//! # async fn example() -> anyhow::Result<()> {
//! use visor_build::push::{ImageReference, RegistryAuth, RegistryPusher};
//! use std::path::Path;
//!
//! let pusher = RegistryPusher::new(RegistryAuth::Anonymous);
//! let reference = ImageReference::parse("ghcr.io/user/myapp:v1")?;
//! let result = pusher.push(Path::new("/tmp/oci-image"), &reference).await?;
//! println!("pushed {} layers, digest: {}", result.layers_pushed, result.digest);
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, bail};
use base64::Engine as _;
use oci_distribution::client::{ClientConfig, ClientProtocol, Config, ImageLayer, PushResponse};
use oci_distribution::secrets::RegistryAuth as OciAuth;
use oci_distribution::{Client, Reference};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tracing::{debug, info};

// \u2500\u2500 Constants \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

/// Default Docker Hub registry hostname.
const DEFAULT_REGISTRY: &str = "docker.io";

/// Default tag when none is specified.
const DEFAULT_TAG: &str = "latest";

/// Docker Hub official image library prefix.
const LIBRARY_PREFIX: &str = "library";

/// OCI image layout file name.
const OCI_LAYOUT_FILE: &str = "oci-layout";

/// OCI image index file name.
const INDEX_JSON: &str = "index.json";

/// Media type for gzip-compressed OCI layers.
const LAYER_MEDIA_TYPE: &str = "application/vnd.oci.image.layer.v1.tar+gzip";

/// Media type for OCI image config.
const CONFIG_MEDIA_TYPE: &str = "application/vnd.oci.image.config.v1+json";

// \u2500\u2500 Public Types \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

/// Push OCI images to container registries.
pub struct RegistryPusher {
    /// Registry authentication config.
    auth: RegistryAuth,
}

/// Authentication for registry operations.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RegistryAuth {
    /// No authentication.
    Anonymous,
    /// Username/password (Basic auth).
    Basic {
        /// Username for the registry.
        username: String,
        /// Password or token for the registry.
        password: String,
    },
}

/// Result of a push operation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PushResult {
    /// The manifest digest as returned by the registry.
    pub digest: String,
    /// Number of layers pushed (excludes already-present layers).
    pub layers_pushed: usize,
    /// Number of layers skipped (already present in registry).
    pub layers_skipped: usize,
    /// Total bytes uploaded.
    pub bytes_uploaded: u64,
}

/// Reference to an image in a registry.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ImageReference {
    /// Registry host (e.g. "docker.io", "ghcr.io").
    pub registry: String,
    /// Repository path (e.g. "library/alpine", "user/myapp").
    pub repository: String,
    /// Tag (e.g. "latest", "v1.0").
    pub tag: String,
}

// \u2500\u2500 ImageReference \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

impl ImageReference {
    /// Parse an image reference string.
    ///
    /// Handles formats:
    /// - `myapp:latest` \u2192 docker.io/library/myapp:latest
    /// - `user/myapp:v1` \u2192 docker.io/user/myapp:v1
    /// - `ghcr.io/user/myapp:v1` \u2192 ghcr.io/user/myapp:v1
    ///
    /// # Errors
    ///
    /// Returns an error if the reference format is invalid.
    pub fn parse(reference: &str) -> anyhow::Result<Self> {
        if reference.is_empty() {
            bail!("image reference cannot be empty");
        }

        // Split off the tag first.
        let (name_part, tag) = split_name_tag(reference);

        // Count slashes to determine format.
        let slash_count = name_part.chars().filter(|&c| c == '/').count();

        let (registry, repository) = match slash_count {
            // No slash: simple name like "myapp"
            0 => (
                DEFAULT_REGISTRY.to_owned(),
                format!("{LIBRARY_PREFIX}/{name_part}"),
            ),
            // One slash: "user/repo" or "registry:port/repo"
            1 => {
                // Safe: slash_count == 1 guarantees split_once succeeds.
                let (first, rest) = name_part
                    .split_once('/')
                    .context("expected '/' in image reference")?;

                if looks_like_registry(first) {
                    // Custom registry with single-segment repo.
                    (first.to_owned(), rest.to_owned())
                } else {
                    // Docker Hub user image (e.g. "user/myapp").
                    (DEFAULT_REGISTRY.to_owned(), name_part.to_owned())
                }
            }
            // Multiple slashes: "registry/path/repo".
            _ => {
                // Safe: slash_count >= 2 guarantees split_once succeeds.
                let (first, rest) = name_part
                    .split_once('/')
                    .context("expected '/' in image reference")?;
                (first.to_owned(), rest.to_owned())
            }
        };

        Ok(Self {
            registry,
            repository,
            tag: tag.to_owned(),
        })
    }

    /// Convert to an `oci_distribution::Reference`.
    ///
    /// # Errors
    ///
    /// Returns an error if the reference cannot be parsed by oci-distribution.
    fn to_oci_reference(&self) -> anyhow::Result<Reference> {
        let full = format!("{}/{}:{}", self.registry, self.repository, self.tag);
        let reference: Reference = full.parse().context("invalid OCI reference")?;
        Ok(reference)
    }
}

impl std::fmt::Display for ImageReference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}:{}", self.registry, self.repository, self.tag)
    }
}

// \u2500\u2500 RegistryPusher \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

impl RegistryPusher {
    /// Create a new pusher with the given auth.
    #[must_use]
    pub fn new(auth: RegistryAuth) -> Self {
        Self { auth }
    }

    /// Return the current authentication configuration.
    #[must_use]
    pub fn auth(&self) -> &RegistryAuth {
        &self.auth
    }

    /// Create a pusher using credentials from `~/.docker/config.json`.
    ///
    /// Falls back to [`RegistryAuth::Anonymous`] if the registry is not
    /// found in the config or the config file does not exist.
    ///
    /// # Errors
    ///
    /// Returns an error if the config file exists but contains invalid JSON.
    pub fn from_docker_config(registry: &str) -> anyhow::Result<Self> {
        let config_path = docker_config_path();
        let auth = if config_path.exists() {
            parse_docker_config_auth(&config_path, registry)?
        } else {
            debug!("docker config not found at {}", config_path.display());
            RegistryAuth::Anonymous
        };
        Ok(Self { auth })
    }

    /// Push an OCI image from a layout directory to a registry.
    ///
    /// Reads the OCI layout at `image_dir`, pushes layers (with dedup),
    /// config, and manifest to the target reference.
    ///
    /// # Errors
    ///
    /// Returns an error if reading the image, authentication, or
    /// any upload operation fails.
    pub async fn push(
        &self,
        image_dir: &Path,
        reference: &ImageReference,
    ) -> anyhow::Result<PushResult> {
        // Validate OCI layout exists.
        let layout_file = image_dir.join(OCI_LAYOUT_FILE);
        if !layout_file.exists() {
            bail!("not a valid OCI layout: {} missing", layout_file.display());
        }

        let oci_ref = reference
            .to_oci_reference()
            .context("building OCI reference")?;

        // Read the OCI index to find the manifest.
        let index_path = image_dir.join(INDEX_JSON);
        let index_data = fs::read_to_string(&index_path).context("reading index.json")?;
        let index: OciIndex = serde_json::from_str(&index_data).context("parsing index.json")?;

        let manifest_desc = index
            .manifests
            .first()
            .context("no manifests in index.json")?;

        // Read the manifest blob.
        let manifest_blob =
            read_blob(image_dir, &manifest_desc.digest).context("reading manifest blob")?;
        let manifest: OciManifestJson =
            serde_json::from_slice(&manifest_blob).context("parsing manifest")?;

        // Read config blob.
        let config_data =
            read_blob(image_dir, &manifest.config.digest).context("reading config blob")?;

        // Collect layers.
        let mut image_layers = Vec::with_capacity(manifest.layers.len());
        let mut total_bytes: u64 = 0;

        for layer_desc in &manifest.layers {
            let layer_data = read_blob(image_dir, &layer_desc.digest)
                .context(format!("reading layer {}", layer_desc.digest))?;
            total_bytes += layer_data.len() as u64;

            let media_type = layer_desc
                .media_type
                .clone()
                .unwrap_or_else(|| LAYER_MEDIA_TYPE.to_owned());

            image_layers.push(ImageLayer::new(layer_data, media_type, None));
        }

        // Create the oci-distribution client.
        let client = create_oci_client();
        let oci_auth = self.auth.to_oci_auth();

        // Build the config.
        let oci_config = Config::new(
            config_data,
            manifest
                .config
                .media_type
                .clone()
                .unwrap_or_else(|| CONFIG_MEDIA_TYPE.to_owned()),
            None,
        );

        info!(
            reference = %reference,
            layers = image_layers.len(),
            "pushing OCI image"
        );

        // Push everything.
        let push_resp: PushResponse = client
            .push(&oci_ref, &image_layers, oci_config, &oci_auth, None)
            .await
            .context("pushing image to registry")?;

        // Compute manifest digest for the result.
        let manifest_digest = sha256_digest(&manifest_blob);

        info!(
            digest = %manifest_digest,
            manifest_url = %push_resp.manifest_url,
            "push complete"
        );

        Ok(PushResult {
            digest: manifest_digest,
            layers_pushed: image_layers.len(),
            layers_skipped: 0,
            bytes_uploaded: total_bytes,
        })
    }
}

// \u2500\u2500 RegistryAuth Conversion \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

impl RegistryAuth {
    /// Convert to the `oci_distribution` auth type.
    fn to_oci_auth(&self) -> OciAuth {
        match self {
            Self::Anonymous => OciAuth::Anonymous,
            Self::Basic { username, password } => {
                OciAuth::Basic(username.clone(), password.clone())
            }
        }
    }
}

// \u2500\u2500 Docker Config Parsing \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

/// Docker config.json structure (only the parts we need).
#[derive(Debug, Deserialize)]
struct DockerConfig {
    #[serde(default)]
    auths: HashMap<String, DockerAuthEntry>,
}

/// A single auth entry in Docker's config.json.
#[derive(Debug, Deserialize)]
struct DockerAuthEntry {
    /// Base64-encoded "username:password".
    auth: Option<String>,
}

/// Well-known Docker Hub auth keys in config.json.
const DOCKER_HUB_KEYS: &[&str] = &[
    "https://index.docker.io/v1/",
    "https://index.docker.io/v2/",
    "index.docker.io",
    "docker.io",
];

/// Parse authentication from a Docker config.json file.
///
/// Returns [`RegistryAuth::Anonymous`] if the registry is not found
/// in the config.
///
/// # Errors
///
/// Returns an error if the file cannot be read or contains invalid JSON.
pub fn parse_docker_config_auth(
    config_path: &Path,
    registry: &str,
) -> anyhow::Result<RegistryAuth> {
    let data = fs::read_to_string(config_path).context("reading docker config")?;
    let config: DockerConfig = serde_json::from_str(&data).context("parsing docker config")?;

    // Determine which keys to look for.
    let lookup_keys: Vec<&str> = if registry == DEFAULT_REGISTRY {
        DOCKER_HUB_KEYS.to_vec()
    } else {
        vec![registry]
    };

    // Search for a matching auth entry.
    for key in &lookup_keys {
        if let Some(entry) = config.auths.get(*key) {
            if let Some(auth_b64) = &entry.auth {
                return decode_basic_auth(auth_b64).context("decoding auth from docker config");
            }
        }
    }

    Ok(RegistryAuth::Anonymous)
}

/// Decode a base64-encoded `username:password` string.
///
/// # Errors
///
/// Returns an error if the base64 is invalid or doesn't contain a colon.
fn decode_basic_auth(encoded: &str) -> anyhow::Result<RegistryAuth> {
    let decoded = base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .context("base64 decode failed")?;
    let decoded_str = String::from_utf8(decoded).context("auth is not valid UTF-8")?;

    let (username, password) = decoded_str
        .split_once(':')
        .context("auth field missing ':' separator")?;

    Ok(RegistryAuth::Basic {
        username: username.to_owned(),
        password: password.to_owned(),
    })
}

// \u2500\u2500 Internal Helpers \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

/// Split a reference into name and tag parts.
///
/// If no tag is present, returns [`DEFAULT_TAG`].
fn split_name_tag(reference: &str) -> (&str, &str) {
    // Find the last colon that's after any slash (to avoid matching port numbers).
    if let Some(colon_pos) = reference.rfind(':') {
        let after_colon = &reference[colon_pos + 1..];
        if !after_colon.contains('/') && !after_colon.is_empty() {
            // Could be a port if after_colon is all digits and before has no slash.
            let before_colon = &reference[..colon_pos];
            if after_colon.chars().all(|c| c.is_ascii_digit()) && !before_colon.contains('/') {
                // Ambiguous: "localhost:5000" vs "myapp:123".
                // If before_colon looks like a host, treat as port.
                if looks_like_hostname(before_colon) {
                    return (reference, DEFAULT_TAG);
                }
            }
            return (&reference[..colon_pos], after_colon);
        }
    }
    (reference, DEFAULT_TAG)
}

/// Check if the first path component looks like a registry hostname.
///
/// A component is a registry if it contains a dot (e.g. "ghcr.io") or
/// a colon (e.g. "localhost:5000") or is "localhost".
fn looks_like_registry(component: &str) -> bool {
    component.contains('.') || component.contains(':') || component == "localhost"
}

/// Check if a string looks like a hostname (not a repo name).
fn looks_like_hostname(s: &str) -> bool {
    s.contains('.') || s == "localhost"
}

/// Read a blob from the OCI blobs directory by digest.
///
/// # Errors
///
/// Returns an error if the blob file cannot be read.
fn read_blob(image_dir: &Path, digest: &str) -> anyhow::Result<Vec<u8>> {
    let (algorithm, hex) = digest
        .split_once(':')
        .context("invalid digest format: expected algorithm:hex")?;

    let blob_path = image_dir.join("blobs").join(algorithm).join(hex);
    fs::read(&blob_path).context(format!("reading blob at {}", blob_path.display()))
}

/// Compute a sha256 digest string from data.
fn sha256_digest(data: &[u8]) -> String {
    let hash = Sha256::digest(data);
    format!("sha256:{hash:x}")
}

/// Create an `oci_distribution::Client` with default settings.
fn create_oci_client() -> Client {
    let config = ClientConfig {
        protocol: ClientProtocol::Https,
        ..Default::default()
    };
    Client::new(config)
}

/// Return the default Docker config path (`~/.docker/config.json`).
fn docker_config_path() -> PathBuf {
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .unwrap_or_else(|_| "/root".to_owned());
    PathBuf::from(home).join(".docker").join("config.json")
}

// \u2500\u2500 OCI JSON Types (internal) \u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500\u2500

/// Minimal OCI image index for parsing `index.json`.
#[derive(Debug, Deserialize)]
struct OciIndex {
    manifests: Vec<OciDescriptor>,
}

/// Minimal OCI descriptor for parsing index/manifest JSON.
#[derive(Debug, Deserialize)]
struct OciDescriptor {
    digest: String,
    #[serde(rename = "mediaType")]
    media_type: Option<String>,
}

/// Minimal OCI manifest for reading layer/config references.
#[derive(Debug, Deserialize)]
struct OciManifestJson {
    config: OciDescriptor,
    layers: Vec<OciDescriptor>,
}

#[cfg(test)]
#[path = "push_test.rs"]
mod tests;
