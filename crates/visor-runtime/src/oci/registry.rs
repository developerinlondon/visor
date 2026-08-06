//! OCI registry client.
//!
//! Pulls manifests and layer blobs from OCI-compatible registries
//! (Docker Hub, GHCR, etc.) with anonymous and token-based auth.
//!
//! # Usage
//!
//! ```rust,no_run
//! # async fn example() -> anyhow::Result<()> {
//! use visor_runtime::oci::registry::RegistryClient;
//!
//! let mut client = RegistryClient::new("docker.io")?;
//! client.authenticate("library/alpine").await?;
//! let manifest = client.pull_manifest("library/alpine", "latest").await?;
//! println!("{manifest}");
//! # Ok(())
//! # }
//! ```

use std::collections::HashMap;
use std::fmt;
use std::time::Duration;

use anyhow::Context as _;
use serde::Deserialize;
use tokio::io::AsyncWriteExt as _;

/// OCI content descriptor — a reference to a blob by digest.
///
/// Used for config blobs and layer blobs within a manifest.
/// Fields follow the OCI image spec naming (`mediaType`, `digest`, `size`).
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Descriptor {
    /// MIME type of the referenced content (e.g. `application/vnd.oci.image.layer.v1.tar+gzip`).
    pub media_type: String,
    /// Content-addressable digest, always prefixed with the algorithm (e.g. `sha256:abcdef...`).
    pub digest: String,
    /// Size of the referenced content in bytes.
    pub size: u64,
}

impl fmt::Display for Descriptor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let short_digest = if self.digest.len() > 19 {
            &self.digest[..19]
        } else {
            &self.digest
        };
        write!(
            f,
            "{} {}... ({} bytes)",
            self.media_type, short_digest, self.size
        )
    }
}

/// OCI image manifest (schema version 2).
///
/// Describes the config blob and ordered list of layer blobs that compose
/// an image. Deserialized from the JSON returned by
/// `GET /v2/{name}/manifests/{reference}`.
#[derive(Debug, Clone, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Manifest {
    /// Manifest schema version (always `2` for current OCI/Docker manifests).
    pub schema_version: u32,
    /// Manifest MIME type. May be absent in some registry responses.
    pub media_type: Option<String>,
    /// Descriptor pointing to the image configuration blob.
    pub config: Descriptor,
    /// Ordered list of layer descriptors (base layer first).
    pub layers: Vec<Descriptor>,
}

impl fmt::Display for Manifest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let n = self.layers.len();
        write!(
            f,
            "Manifest v{} ({n} layer{})",
            self.schema_version,
            if n == 1 { "" } else { "s" }
        )
    }
}

/// OCI Image Index / Docker Manifest List (schema version 2).
///
/// Returned by registries for multi-arch images. Contains a list of
/// platform-specific manifest descriptors instead of `config` + `layers`.
///
/// See: <https://github.com/opencontainers/image-spec/blob/main/image-index.md>
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct ManifestIndex {
    /// Manifest schema version (always `2` for current OCI/Docker manifests).
    pub schema_version: u32,
    /// Index MIME type. May be absent in some registry responses.
    pub media_type: Option<String>,
    /// List of platform-specific manifest descriptors.
    pub manifests: Vec<PlatformDescriptor>,
}

/// Descriptor referencing a platform-specific manifest within a manifest index.
///
/// Each entry points to a full image manifest for a specific OS/architecture
/// combination, identified by digest.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct PlatformDescriptor {
    /// MIME type of the referenced manifest.
    pub media_type: String,
    /// Content-addressable digest of the platform-specific manifest.
    pub digest: String,
    /// Size of the referenced manifest in bytes.
    pub size: u64,
    /// Target platform (OS + architecture). May be absent for attestation manifests.
    pub platform: Option<Platform>,
}

/// Target platform for a manifest within a manifest index.
///
/// Identifies the OS and CPU architecture that a manifest is built for.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
#[non_exhaustive]
pub struct Platform {
    /// CPU architecture (e.g. `amd64`, `arm64`, `arm`).
    pub architecture: String,
    /// Operating system (e.g. `linux`, `windows`).
    pub os: String,
    /// Architecture variant (e.g. `v7` for armv7). Absent for most platforms.
    pub variant: Option<String>,
}

/// Token response from a Docker/OCI auth endpoint.
#[derive(Deserialize)]
struct TokenResponse {
    token: String,
}

/// HTTP timeout for the initial connect phase.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// HTTP timeout for manifest and auth requests.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// HTTP timeout for blob downloads (layers can be hundreds of MB).
const BLOB_TIMEOUT: Duration = Duration::from_secs(600);

/// Combined Accept header value for manifest requests.
///
/// Covers OCI image manifests, Docker v2 manifests, and Docker manifest lists.
const MANIFEST_ACCEPT: &str = "\
    application/vnd.oci.image.manifest.v1+json, \
    application/vnd.docker.distribution.manifest.v2+json, \
    application/vnd.docker.distribution.manifest.list.v2+json";

/// Async OCI Distribution client for pulling manifests and layer blobs.
///
/// Supports anonymous pulls from any OCI-compatible registry and
/// Docker Hub token-based authentication for public images.
///
/// The inner [`reqwest::Client`] is created once and reused for all
/// requests (connection pooling, TLS session caching).
#[derive(Debug)]
#[non_exhaustive]
pub struct RegistryClient {
    /// Shared HTTP client — created once, reused for every request.
    pub(crate) client: reqwest::Client,
    /// Base URL of the registry (e.g. `https://registry-1.docker.io`).
    pub(crate) base_url: String,
    /// Bearer token obtained via [`authenticate`](Self::authenticate).
    pub(crate) token: Option<String>,
}

impl RegistryClient {
    /// Create a new registry client for the given registry hostname.
    ///
    /// Docker Hub aliases (`docker.io`, `index.docker.io`) are automatically
    /// mapped to the canonical API endpoint `registry-1.docker.io`.
    ///
    /// # Errors
    ///
    /// Returns an error if `registry` is empty or the HTTP client cannot be
    /// built.
    pub fn new(registry: &str) -> anyhow::Result<Self> {
        let host = registry
            .strip_prefix("https://")
            .or_else(|| registry.strip_prefix("http://"))
            .unwrap_or(registry)
            .trim_end_matches('/');

        anyhow::ensure!(!host.is_empty(), "registry hostname cannot be empty");

        let base_url = match host {
            "docker.io" | "index.docker.io" => "https://registry-1.docker.io".to_owned(),
            other => format!("https://{other}"),
        };

        let client = reqwest::Client::builder()
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .context("failed to build HTTP client")?;

        Ok(Self {
            client,
            base_url,
            token: None,
        })
    }

    /// Authenticate with the registry for the given repository.
    ///
    /// Performs the OCI token-auth handshake:
    ///
    /// 1. `GET /v2/` — if the registry returns 401, the `Www-Authenticate`
    ///    header is parsed to extract realm, service, and scope.
    /// 2. `GET {realm}?service={service}&scope=repository:{repo}:pull` — the
    ///    auth server returns a bearer token.
    /// 3. The token is stored for all subsequent requests.
    ///
    /// If the registry returns 200 (no auth required), this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the registry returns an unexpected status, the
    /// `Www-Authenticate` header is missing or unparseable, or the token
    /// request fails.
    pub async fn authenticate(&mut self, repository: &str) -> anyhow::Result<()> {
        let url = format!("{}/v2/", self.base_url);
        let response = self
            .client
            .get(&url)
            .send()
            .await
            .context("failed to probe registry for auth requirements")?;

        if response.status() == reqwest::StatusCode::OK {
            return Ok(());
        }

        anyhow::ensure!(
            response.status() == reqwest::StatusCode::UNAUTHORIZED,
            "unexpected status {} from registry /v2/ endpoint",
            response.status()
        );

        let www_auth = response
            .headers()
            .get("www-authenticate")
            .context("registry returned 401 without Www-Authenticate header")?
            .to_str()
            .context("Www-Authenticate header contains non-ASCII bytes")?;

        let params =
            parse_www_authenticate(www_auth).context("failed to parse Www-Authenticate header")?;

        let auth_url = build_auth_url(&params, repository)?;

        let token_response = self
            .client
            .get(&auth_url)
            .send()
            .await
            .context("failed to request auth token")?;

        anyhow::ensure!(
            token_response.status().is_success(),
            "token request failed with status {}",
            token_response.status()
        );

        let token_data: TokenResponse = token_response
            .json()
            .await
            .context("failed to parse token response JSON")?;

        self.token = Some(token_data.token);
        Ok(())
    }

    /// Pull a manifest for the given repository and reference (tag or digest).
    ///
    /// Sends `GET /v2/{repository}/manifests/{reference}` with Accept headers
    /// for OCI and Docker v2 manifest formats.
    ///
    /// If the registry returns a manifest list (OCI Image Index), the
    /// native platform entry (`linux/arm64` on aarch64, `linux/amd64` on
    /// `x86_64`) is automatically selected and its platform-specific manifest
    /// is fetched by digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails, the registry returns a
    /// non-success status, the response body is not a valid manifest JSON,
    /// or no matching platform entry exists in a manifest list.
    pub async fn pull_manifest(
        &self,
        repository: &str,
        reference: &str,
    ) -> anyhow::Result<Manifest> {
        let body = self
            .pull_manifest_raw(repository, reference)
            .await
            .context("failed to pull raw manifest")?;

        let value: serde_json::Value =
            serde_json::from_slice(&body).context("failed to parse manifest response as JSON")?;

        // Manifest lists have a `manifests` array; single manifests have `config`.
        if value.get("manifests").is_some() {
            let index: ManifestIndex =
                serde_json::from_value(value).context("failed to parse manifest index JSON")?;
            let descriptor = find_native_platform(&index)
                .context("failed to select platform from manifest index")?;
            let platform_body = self
                .pull_manifest_raw(repository, &descriptor.digest)
                .await
                .context("failed to pull platform-specific manifest")?;
            let manifest: Manifest = serde_json::from_slice(&platform_body)
                .context("failed to parse platform-specific manifest JSON")?;
            Ok(manifest)
        } else {
            let manifest: Manifest =
                serde_json::from_value(value).context("failed to parse single manifest JSON")?;
            Ok(manifest)
        }
    }

    /// Fetch the raw bytes of a manifest (or manifest list) from the registry.
    ///
    /// This is the low-level HTTP call used by [`pull_manifest`](Self::pull_manifest).
    /// The `reference` can be a tag or a `sha256:...` digest.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or the registry returns a
    /// non-success status.
    async fn pull_manifest_raw(
        &self,
        repository: &str,
        reference: &str,
    ) -> anyhow::Result<Vec<u8>> {
        let url = format!(
            "{}/v2/{}/manifests/{}",
            self.base_url, repository, reference
        );

        let mut request = self.client.get(&url).header("Accept", MANIFEST_ACCEPT);

        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }

        let response = request
            .send()
            .await
            .context("failed to send manifest request")?;

        let status = response.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            anyhow::bail!(
                "image not found: {repository}:{reference} — check the image name and tag"
            );
        } else if status == reqwest::StatusCode::UNAUTHORIZED {
            anyhow::bail!(
                "authentication failed for {repository}:{reference} — image may not exist or may be private"
            );
        } else if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            anyhow::bail!("rate limited by registry — try again shortly");
        }
        anyhow::ensure!(
            status.is_success(),
            "registry returned {status} for {repository}:{reference}"
        );

        let bytes = response
            .bytes()
            .await
            .context("failed to read manifest response body")?;

        Ok(bytes.to_vec())
    }

    /// Pull a blob (layer or config) by digest into memory.
    ///
    /// Sends `GET /v2/{repository}/blobs/{digest}` and returns the full
    /// response body. For large layers, prefer [`pull_blob_to_writer`](Self::pull_blob_to_writer)
    /// which streams to disk.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails or the registry returns a
    /// non-success status.
    pub async fn pull_blob(&self, repository: &str, digest: &str) -> anyhow::Result<Vec<u8>> {
        let url = format!("{}/v2/{}/blobs/{}", self.base_url, repository, digest);

        let mut request = self.client.get(&url).timeout(BLOB_TIMEOUT);

        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }

        let response = request
            .send()
            .await
            .context("failed to send blob request")?;

        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "blob request for {digest} failed with status {status}"
        );

        let body = response
            .bytes()
            .await
            .context("failed to read blob response body")?;

        Ok(body.to_vec())
    }

    /// Pull a blob by digest, streaming it into the provided async writer.
    ///
    /// Returns the total number of bytes written. This is the preferred method
    /// for large layer blobs as it avoids buffering the entire blob in memory.
    ///
    /// # Errors
    ///
    /// Returns an error if the HTTP request fails, the registry returns a
    /// non-success status, or writing to `writer` fails.
    pub async fn pull_blob_to_writer(
        &self,
        repository: &str,
        digest: &str,
        writer: &mut (dyn tokio::io::AsyncWrite + Unpin + Send),
    ) -> anyhow::Result<u64> {
        let url = format!("{}/v2/{}/blobs/{}", self.base_url, repository, digest);

        let mut request = self.client.get(&url).timeout(BLOB_TIMEOUT);

        if let Some(token) = &self.token {
            request = request.bearer_auth(token);
        }

        let mut response = request
            .send()
            .await
            .context("failed to send streaming blob request")?;

        let status = response.status();
        anyhow::ensure!(
            status.is_success(),
            "streaming blob request for {digest} failed with status {status}"
        );

        let mut written: u64 = 0;
        while let Some(chunk) = response
            .chunk()
            .await
            .context("failed to read blob chunk")?
        {
            writer
                .write_all(&chunk)
                .await
                .context("failed to write blob chunk to writer")?;
            written += chunk.len() as u64;
        }

        Ok(written)
    }
}

/// Parse a `Www-Authenticate: Bearer realm="...",service="...",scope="..."`
/// header into key-value pairs.
///
/// Returns `None` if the header is not a Bearer challenge or cannot be parsed.
fn parse_www_authenticate(header: &str) -> Option<HashMap<String, String>> {
    let bearer = header.strip_prefix("Bearer ")?;
    let mut params = HashMap::new();

    for part in bearer.split(',') {
        let (key, value) = part.split_once('=')?;
        let key = key.trim().to_owned();
        let value = value.trim().trim_matches('"').to_owned();
        params.insert(key, value);
    }

    Some(params)
}

/// Build an auth token URL from parsed `Www-Authenticate` parameters.
///
/// Constructs `{realm}?service={service}&scope=repository:{repository}:pull`.
///
/// # Errors
///
/// Returns an error if `realm` or `service` keys are missing from `params`.
fn build_auth_url(params: &HashMap<String, String>, repository: &str) -> anyhow::Result<String> {
    let realm = params
        .get("realm")
        .context("missing 'realm' in Www-Authenticate parameters")?;
    let service = params
        .get("service")
        .context("missing 'service' in Www-Authenticate parameters")?;

    Ok(format!(
        "{realm}?service={service}&scope=repository:{repository}:pull"
    ))
}

/// Returns the OCI architecture string for the current host.
///
/// Maps Rust `target_arch` to the OCI image spec architecture names.
#[cfg(target_arch = "aarch64")]
const NATIVE_OCI_ARCH: &str = "arm64";
#[cfg(target_arch = "x86_64")]
const NATIVE_OCI_ARCH: &str = "amd64";
#[cfg(not(any(target_arch = "aarch64", target_arch = "x86_64")))]
const NATIVE_OCI_ARCH: &str = "amd64";

/// Find the manifest descriptor matching the native host architecture.
///
/// Scans the `manifests` array for an entry whose platform has
/// `os == "linux"` and `architecture` matching the host.
///
/// # Errors
///
/// Returns an error if no matching platform entry is found.
fn find_native_platform(index: &ManifestIndex) -> anyhow::Result<&PlatformDescriptor> {
    index
        .manifests
        .iter()
        .find(|d| {
            d.platform
                .as_ref()
                .is_some_and(|p| p.architecture == NATIVE_OCI_ARCH && p.os == "linux")
        })
        .with_context(|| format!("no linux/{NATIVE_OCI_ARCH} manifest found in manifest index"))
}

#[cfg(test)]
#[path = "registry_test.rs"]
mod tests;
