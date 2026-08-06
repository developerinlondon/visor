//! Runtime implementation of Docker-compatible image operations.
//!
//! Keeps image pull/list/inspect/remove logic behind the
//! [`visor_types::ImageManager`] trait so the Docker API layer stays
//! decoupled from `visor-runtime` internals.

use std::path::PathBuf;

use anyhow::Context;
use async_trait::async_trait;
use sha2::{Digest, Sha256};
use visor_types::{ImageInfo, ImageManager};

use crate::oci::cache::LayerCache;
use crate::oci::reference::ImageReference;
use crate::oci::registry::RegistryClient;

/// Runtime-backed image manager for Docker-compatible endpoints.
pub struct RuntimeImageManager {
    image_store_path: PathBuf,
}

impl RuntimeImageManager {
    /// Create a new image manager rooted at the given image store path.
    #[must_use]
    pub fn new(image_store_path: PathBuf) -> Self {
        Self { image_store_path }
    }

    fn image_store(&self) -> visor_build::ImageStore {
        visor_build::ImageStore::new(self.image_store_path.clone())
    }

    fn open_cache() -> anyhow::Result<LayerCache> {
        LayerCache::new(LayerCache::default_path().context("determine layer cache path")?)
            .context("open layer cache")
    }

    fn reference_aliases(reference: &str) -> anyhow::Result<Vec<String>> {
        if reference.starts_with("sha256:") {
            return Ok(vec![reference.to_owned()]);
        }

        let image_ref =
            ImageReference::parse(reference).context("parse image reference for aliasing")?;
        let canonical = image_ref.to_string();

        let friendly_repository = if image_ref.registry().as_ref() == "docker.io" {
            image_ref
                .repository()
                .as_ref()
                .strip_prefix("library/")
                .unwrap_or(image_ref.repository().as_ref())
                .to_owned()
        } else {
            format!(
                "{}/{}",
                image_ref.registry().as_ref(),
                image_ref.repository().as_ref()
            )
        };

        let mut friendly = friendly_repository;
        if let Some(tag) = image_ref.tag() {
            friendly.push(':');
            friendly.push_str(tag);
        }
        if let Some(digest) = image_ref.digest() {
            friendly.push('@');
            friendly.push_str(digest);
        }

        let mut aliases = vec![reference.to_owned(), canonical, friendly];
        aliases.sort();
        aliases.dedup();
        Ok(aliases)
    }
}

#[async_trait]
impl ImageManager for RuntimeImageManager {
    async fn list_images(&self) -> anyhow::Result<Vec<ImageInfo>> {
        let tags = self
            .image_store()
            .list_tags()
            .context("list stored image tags")?;
        let mut grouped = std::collections::BTreeMap::<String, Vec<String>>::new();
        for (tag, digest) in tags {
            grouped.entry(digest).or_default().push(tag);
        }

        Ok(grouped
            .into_iter()
            .map(|(digest, mut repo_tags)| {
                repo_tags.sort();
                ImageInfo::new(digest, repo_tags)
            })
            .collect())
    }

    async fn pull_image(&self, reference: &str) -> anyhow::Result<ImageInfo> {
        let image_ref = ImageReference::parse(reference).context("parse image reference")?;
        let registry = image_ref.registry().as_ref();
        let repository = image_ref.repository().as_ref();
        let tag = image_ref.tag().map_or("latest", |tag| tag.as_ref());

        let cache = Self::open_cache()?;
        let mut client = RegistryClient::new(registry).context("create registry client")?;
        client
            .authenticate(repository)
            .await
            .context("authenticate with registry")?;

        let manifest = client
            .pull_manifest(repository, tag)
            .await
            .with_context(|| format!("pull manifest for {reference}"))?;
        let manifest_bytes = serde_json::to_vec(&manifest).context("serialize manifest")?;
        let manifest_digest = format!("sha256:{:x}", Sha256::digest(&manifest_bytes));

        cache
            .put_manifest(registry, repository, tag, &manifest_bytes)
            .context("cache manifest")?;

        if cache
            .get(&manifest.config.digest)
            .context("check config cache")?
            .is_none()
        {
            let blob = client
                .pull_blob(repository, &manifest.config.digest)
                .await
                .context("pull config blob")?;
            cache
                .put(&manifest.config.digest, &blob)
                .context("cache config blob")?;
        }

        let mut total_size = 0u64;
        for layer in &manifest.layers {
            total_size = total_size.saturating_add(layer.size);
            if cache.has(&layer.digest) {
                continue;
            }

            let blob = client
                .pull_blob(repository, &layer.digest)
                .await
                .with_context(|| format!("pull layer {}", layer.digest))?;
            cache
                .put(&layer.digest, &blob)
                .with_context(|| format!("cache layer {}", layer.digest))?;
        }

        let store = self.image_store();
        for alias in Self::reference_aliases(reference)? {
            store
                .tag(&alias, &manifest_digest)
                .with_context(|| format!("tag pulled image as {alias}"))?;
        }
        store
            .tag(&manifest_digest, &manifest_digest)
            .context("tag pulled image by digest")?;

        let mut info = ImageInfo::new(manifest_digest, Self::reference_aliases(reference)?);
        info.size = total_size;
        Ok(info)
    }

    async fn inspect_image(&self, reference: &str) -> anyhow::Result<ImageInfo> {
        let store = self.image_store();
        for alias in Self::reference_aliases(reference)? {
            if let Some(digest) = store
                .get_by_tag(&alias)
                .with_context(|| format!("read image tag {alias} from store"))?
            {
                return Ok(ImageInfo::new(digest, vec![reference.to_owned()]));
            }
        }

        if reference.starts_with("sha256:") {
            return Ok(ImageInfo::new(
                reference.to_owned(),
                vec![reference.to_owned()],
            ));
        }

        anyhow::bail!("image not found: {reference}");
    }

    async fn remove_image(&self, reference: &str) -> anyhow::Result<()> {
        let store = self.image_store();
        let mut removed_any = false;
        for alias in Self::reference_aliases(reference)? {
            removed_any |= store
                .remove_tag(&alias)
                .with_context(|| format!("remove image tag {alias} from store"))?;
        }

        anyhow::ensure!(removed_any, "image not found: {reference}");
        Ok(())
    }
}

#[cfg(test)]
#[path = "image_manager_test.rs"]
mod tests;
