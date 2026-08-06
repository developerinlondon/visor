//! [`BuildExecutor`] implementation over vsock.
//!
//! Bridges the visor-build engine with a running guest VM by delegating
//! build operations to visor-init's agent over virtio-vsock.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use base64::Engine as _;
use tokio::sync::Mutex;
use visor_build::dockerfile::MountType;
use visor_build::engine::{BuildExecutor, LayerSnapshot, ResolvedMount};
use visor_vmm::comms::AsyncStream;

use super::client::VsockClient;

/// Executes Dockerfile build operations inside a guest VM via vsock.
///
/// Wraps a [`VsockClient`] and implements the [`BuildExecutor`] trait so the
/// visor-build engine can drive builds without knowing about the transport.
///
/// The client is protected by `Arc<Mutex<_>>` because [`BuildExecutor`] methods
/// take `&self` while [`VsockClient`] methods require `&mut self`.
pub struct VsockBuildExecutor {
    client: Arc<Mutex<VsockClient<Box<dyn AsyncStream>>>>,
}

impl VsockBuildExecutor {
    /// Create a new executor wrapping an existing vsock client connection.
    ///
    /// # Examples
    ///
    /// ```rust,ignore
    /// use visor_runtime::vsock::executor::VsockBuildExecutor;
    /// use visor_runtime::vsock::client::VsockClient;
    ///
    /// let client = VsockClient::connect(&backend, cid, 52).await?;
    /// let executor = VsockBuildExecutor::new(client);
    /// ```
    #[must_use]
    pub fn new(client: VsockClient<Box<dyn AsyncStream>>) -> Self {
        Self {
            client: Arc::new(Mutex::new(client)),
        }
    }
}

#[async_trait]
impl BuildExecutor for VsockBuildExecutor {
    async fn overlay_init(&self, lower_dir: Option<String>) -> anyhow::Result<()> {
        self.client
            .lock()
            .await
            .overlay_init(lower_dir)
            .await
            .context("vsock overlay_init failed")
    }

    async fn exec(
        &self,
        cmd: &[String],
        env: &[String],
        workdir: &str,
    ) -> anyhow::Result<(i32, String, String)> {
        let result = self
            .client
            .lock()
            .await
            .exec(cmd.to_vec(), env.to_vec(), workdir.to_owned())
            .await
            .context("vsock exec failed")?;
        Ok((result.exit_code, result.stdout, result.stderr))
    }

    async fn snapshot_layer(&self) -> anyhow::Result<LayerSnapshot> {
        let result = self
            .client
            .lock()
            .await
            .snapshot_layer()
            .await
            .context("vsock snapshot_layer failed")?;
        Ok(LayerSnapshot::new(
            result.data,
            result.compressed_digest,
            result.uncompressed_digest,
            result.compressed_size,
        ))
    }

    async fn flatten_overlay(&self) -> anyhow::Result<()> {
        self.client
            .lock()
            .await
            .flatten_overlay()
            .await
            .context("vsock flatten_overlay failed")
    }

    async fn copy_to_guest(&self, host_paths: &[PathBuf], dest: &str) -> anyhow::Result<()> {
        // 1. Create tar.gz archive of the files
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        {
            let mut tar = tar::Builder::new(&mut encoder);
            for path in host_paths {
                if path.is_file() {
                    let name = path
                        .file_name()
                        .context("path has no filename")?
                        .to_str()
                        .context("non-UTF-8 filename")?;
                    tar.append_path_with_name(path, name)
                        .context("failed to add file to tar")?;
                } else if path.is_dir() {
                    tar.append_dir_all(".", path)
                        .context("failed to add directory to tar")?;
                }
            }
            tar.finish().context("failed to finalize tar")?;
        }
        let compressed = encoder.finish().context("failed to finish gzip")?;

        // 2. Base64-encode
        let data_b64 = base64::engine::general_purpose::STANDARD.encode(&compressed);

        // 3. Send via vsock
        let result = self
            .client
            .lock()
            .await
            .copy_files(data_b64, dest.to_owned())
            .await
            .context("vsock copy_files failed")?;

        tracing::debug!(files = result.files_written, dest, "copied files to guest");
        Ok(())
    }

    async fn setup_mount(&self, mount: &ResolvedMount) -> anyhow::Result<()> {
        match mount.mount_type {
            MountType::Tmpfs => {
                let cmd = vec![
                    "mount".to_owned(),
                    "-t".to_owned(),
                    "tmpfs".to_owned(),
                    "tmpfs".to_owned(),
                    mount.target.clone(),
                ];
                self.run_guest_cmd(&cmd)
                    .await
                    .context("vsock setup_mount tmpfs failed")
            }
            MountType::Cache => {
                let cmd = vec!["mkdir".to_owned(), "-p".to_owned(), mount.target.clone()];
                self.run_guest_cmd(&cmd)
                    .await
                    .context("vsock setup_mount cache failed")
            }
            MountType::Bind => {
                tracing::warn!(target = %mount.target, "bind mount not yet implemented");
                Ok(())
            }
            MountType::Secret | MountType::Ssh | _ => Ok(()),
        }
    }

    async fn teardown_mount(&self, mount: &ResolvedMount) -> anyhow::Result<()> {
        match mount.mount_type {
            MountType::Tmpfs => {
                let cmd = vec!["umount".to_owned(), mount.target.clone()];
                self.run_guest_cmd(&cmd)
                    .await
                    .context("vsock teardown_mount tmpfs failed")
            }
            _ => Ok(()),
        }
    }
}

impl VsockBuildExecutor {
    /// Run a command in the guest with no env vars or special workdir.
    ///
    /// Used internally by [`setup_mount`](BuildExecutor::setup_mount) and
    /// [`teardown_mount`](BuildExecutor::teardown_mount).
    ///
    /// # Errors
    ///
    /// Returns an error if the vsock exec call fails or the command exits
    /// non-zero.
    async fn run_guest_cmd(&self, cmd: &[String]) -> anyhow::Result<()> {
        let result = self
            .client
            .lock()
            .await
            .exec(cmd.to_vec(), vec![], "/".to_owned())
            .await
            .context("vsock run_guest_cmd failed")?;

        if result.exit_code != 0 {
            anyhow::bail!(
                "guest command {:?} failed (exit code {}): {}",
                cmd,
                result.exit_code,
                result.stderr
            );
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "executor_test.rs"]
mod tests;
