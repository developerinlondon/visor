//! Container execution backend using Linux namespaces.
//!
//! Provides a lightweight alternative to KVM-based microVMs. Uses the re-exec
//! pattern: spawns a child process via `/proc/self/exe` with a `__container-run`
//! subcommand that applies namespace isolation.
//!
//! # Design
//!
//! The container backend stores container state in memory, similar to how
//! [`VmmBackend`](crate::backend::VmmBackend) manages VMs. The `create` method
//! generates a UUID, stores a [`ContainerInfo`] with state
//! [`Running`](crate::backend::VmState::Running), and (in production) would
//! spawn a namespaced child process.
//!
//! # Safety
//!
//! This module uses NO unsafe code. Namespace setup is delegated to a child
//! process via `std::process::Command`.

use std::collections::HashMap;
use std::fmt;

use anyhow::Context;
use async_trait::async_trait;
use tokio::sync::RwLock;
use uuid::Uuid;

use crate::backend::{ExecRequest, ExecResult, ExecutionBackend, VmConfig, VmInfo, VmState};

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

/// Container execution backend using Linux namespaces.
///
/// Provides a lightweight alternative to KVM-based microVMs. Uses `clone(2)`
/// with user/mount/PID/net/IPC/UTS namespaces for process isolation.
///
/// Containers are stored in memory and identified by UUID. The re-exec pattern
/// (`/proc/self/exe __container-run`) is used for namespace setup, delegating
/// isolation to a child process without any unsafe code.
#[non_exhaustive]
pub struct ContainerBackend {
    /// Running containers keyed by ID.
    containers: RwLock<HashMap<String, ContainerInfo>>,
}

/// Internal container metadata.
#[derive(Debug, Clone)]
struct ContainerInfo {
    /// VM-compatible information about this container.
    vm_info: VmInfo,
    /// PID of the container's init process, if spawned.
    pid: Option<u32>,
}

impl fmt::Debug for ContainerBackend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ContainerBackend")
            .field("container_count", &"<locked>")
            .finish_non_exhaustive()
    }
}

impl ContainerBackend {
    /// Create a new, empty `ContainerBackend`.
    ///
    /// # Examples
    ///
    /// ```
    /// use visor_runtime::container::ContainerBackend;
    /// let backend = ContainerBackend::new();
    /// ```
    #[must_use]
    pub fn new() -> Self {
        Self {
            containers: RwLock::new(HashMap::new()),
        }
    }
}

impl Default for ContainerBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionBackend for ContainerBackend {
    /// Create a new container from the given configuration.
    ///
    /// Generates a UUID, stores the container with state `Running`, and returns
    /// the container's [`VmInfo`]. In production, this would spawn a namespaced
    /// child process via `/proc/self/exe __container-run`; for now it only
    /// stores state.
    ///
    /// # Errors
    ///
    /// Currently infallible, but returns `Result` to match the trait contract.
    async fn create(&self, config: VmConfig) -> anyhow::Result<VmInfo> {
        let id = Uuid::new_v4().to_string();
        let mut info = VmInfo::new(
            id.clone(),
            config.image.clone(),
            VmState::Running,
            crate::timeutil::utc_now_iso8601(),
            config.memory_mib,
            config.vcpus,
        );
        info.name.clone_from(&config.name);
        info.ports.clone_from(&config.ports);
        let container = ContainerInfo {
            vm_info: info.clone(),
            pid: None,
        };

        self.containers.write().await.insert(id, container);
        Ok(info)
    }

    /// List all containers.
    ///
    /// # Errors
    ///
    /// Currently infallible.
    async fn list(&self) -> anyhow::Result<Vec<VmInfo>> {
        let containers = self.containers.read().await;
        Ok(containers.values().map(|c| c.vm_info.clone()).collect())
    }

    /// Get a container by its ID.
    ///
    /// # Errors
    ///
    /// Returns an error if no container with the given ID exists.
    async fn get(&self, id: &str) -> anyhow::Result<VmInfo> {
        let containers = self.containers.read().await;
        containers
            .get(id)
            .map(|c| c.vm_info.clone())
            .context(format!("container not found: {id}"))
    }

    /// Execute a command inside a running container.
    ///
    /// Currently unimplemented — will be wired to the re-exec child process
    /// in a future layer.
    ///
    /// # Errors
    ///
    /// Returns an error if the container is not found, not running, or exec
    /// is not yet supported.
    async fn exec(&self, id: &str, req: ExecRequest) -> anyhow::Result<ExecResult> {
        let containers = self.containers.read().await;
        let container = containers
            .get(id)
            .context(format!("container not found: {id}"))?;
        anyhow::ensure!(
            container.vm_info.state == VmState::Running,
            "container {id} is not running (state: {:?})",
            container.vm_info.state
        );
        anyhow::bail!(
            "exec not yet implemented for container backend (cmd: {:?})",
            req.cmd
        )
    }

    /// Stop a running container.
    ///
    /// Transitions the container to [`VmState::Stopped`]. If the container is
    /// already stopped or failed, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns an error if the container is not found.
    async fn stop(&self, id: &str, _timeout_secs: u64) -> anyhow::Result<()> {
        let mut containers = self.containers.write().await;
        let container = containers
            .get_mut(id)
            .context(format!("container not found: {id}"))?;

        if container.vm_info.state == VmState::Stopped || container.vm_info.state == VmState::Failed
        {
            return Ok(());
        }

        if let Some(pid) = container.pid {
            tracing::debug!(container_id = id, pid, "stopping container process");
        }
        container.vm_info.state = VmState::Stopped;
        container.pid = None;
        Ok(())
    }

    /// Force-kill a container immediately.
    ///
    /// # Errors
    ///
    /// Returns an error if the container is not found.
    async fn kill(&self, id: &str) -> anyhow::Result<()> {
        self.stop(id, 0).await
    }

    /// Destroy a container, removing it from the backend.
    ///
    /// # Errors
    ///
    /// Returns an error if the container is not found.
    async fn destroy(&self, id: &str) -> anyhow::Result<()> {
        let mut containers = self.containers.write().await;
        containers
            .remove(id)
            .context(format!("container not found: {id}"))?;
        Ok(())
    }

    async fn console_output(&self, id: &str) -> anyhow::Result<Vec<u8>> {
        let containers = self.containers.read().await;
        anyhow::ensure!(containers.contains_key(id), "container not found: {id}");
        // Container backend does not capture serial output.
        Ok(Vec::new())
    }
}
