//! Daemon lifecycle: HTTP server on TCP, graceful shutdown.
//!
//! `visor start` launches the daemon, which serves the REST API on a
//! configurable TCP address (default `127.0.0.1:7800`). The daemon manages
//! the [`AppState`](crate::api::router::AppState) containing the execution
//! backend and event broadcaster.

use std::sync::Arc;

use anyhow::Context;
use async_trait::async_trait;
use tokio::net::TcpListener;
use tracing::info;

/// Configuration for the visor daemon.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DaemonConfig {
    /// Address to listen on (e.g., `"127.0.0.1:7800"`).
    pub listen_addr: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:7800".to_owned(),
        }
    }
}

impl DaemonConfig {
    /// Create a new `DaemonConfig` with the given listen address.
    #[must_use]
    pub fn new(listen_addr: String) -> Self {
        Self { listen_addr }
    }
}

struct DockerDnsServiceDiscovery {
    registry: Arc<tokio::sync::RwLock<crate::net::dns::DnsRegistry>>,
}

impl DockerDnsServiceDiscovery {
    fn new(registry: Arc<tokio::sync::RwLock<crate::net::dns::DnsRegistry>>) -> Self {
        Self { registry }
    }
}

#[async_trait]
impl visor_docker::ServiceDiscovery for DockerDnsServiceDiscovery {
    async fn register_name(&self, name: &str, ip: std::net::Ipv4Addr) {
        self.registry.write().await.register(name, ip);
    }

    async fn unregister_name(&self, name: &str) {
        self.registry.write().await.unregister(name);
    }

    async fn snapshot_names(&self) -> Vec<(String, std::net::Ipv4Addr)> {
        let mut entries = self
            .registry
            .read()
            .await
            .all_entries()
            .into_iter()
            .map(|(name, ip)| (name.to_owned(), ip))
            .collect::<Vec<_>>();
        entries.sort_by(|left, right| left.0.cmp(&right.0));
        entries
    }
}

/// Runs the visor daemon until shutdown signal (Ctrl+C or API request).
///
/// Binds a TCP listener on the configured address, builds the Axum router
/// with shared application state, and serves until a SIGINT/SIGTERM signal
/// is received or a `POST /v1/shutdown` request arrives.
///
/// # Errors
///
/// Returns an error if the server fails to bind or encounters a fatal error.
pub async fn run_daemon(config: DaemonConfig) -> anyhow::Result<()> {
    visor_types::configure_named_network_supernet_from_env()
        .context("configure named network supernet")?;

    // 1. Initialize tracing subscriber
    init_tracing();
    // 1a. On macOS, verify the binary has the HVF entitlement before doing
    //     anything that touches Hypervisor.framework. Fail early with a clear
    //     message instead of a cryptic HV_DENIED later.
    crate::codesign::verify_current_binary()?;


    // 2. Clean up orphan Linux TAP interfaces from previous daemon runs.
    if let Err(error) = cleanup_orphan_linux_interfaces() {
        tracing::warn!(error = %error, "failed to clean up orphan Linux TAP interfaces");
    }
    if let Err(error) = cleanup_orphan_linux_firewall_rules() {
        tracing::warn!(error = %error, "failed to clean up stale Visor iptables rules");
    }

    let image_store_path =
        crate::paths::persistent_subdir("images").context("determine OCI image store path")?;

    // 3. Create the execution backend
    let backend = Arc::new(crate::backend::VmmBackend::with_image_store_path(
        image_store_path.clone(),
    ));

    // 3a. Restore any previously persisted VMs
    restore_vms(&backend).await;

    // 3b. Clean up stale vsock muxer sockets from previous daemon runs.
    //      On macOS the vsock muxer uses Unix-domain sockets at
    //      /var/run/visor/vsock/{cid}.sock — these become stale if the
    //      previous daemon exited uncleanly.
    cleanup_stale_vsock_sockets();

    // 4. Create the event broadcaster
    let events = Arc::new(crate::api::sse::EventBroadcaster::new(1024));

    // 5. Shutdown signal (shared between API handler and signal listener)
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let (background_shutdown_tx, background_shutdown_rx) = tokio::sync::watch::channel(false);

    // 5. Create the warm pool manager
    let vmm_backend = Arc::clone(&backend);
    let backend_arc = Arc::clone(&backend) as Arc<dyn crate::backend::ExecutionBackend>;
    let snapshot_cache_dir = crate::pool::snapshot_cache::SnapshotCache::default_dir()
        .context("determine snapshot cache path")?;
    let pool = Arc::new(crate::pool::manager::PoolManager::new(
        crate::pool::manager::PoolConfig::default(),
        Arc::clone(&backend_arc),
        crate::pool::snapshot_cache::SnapshotCache::new(snapshot_cache_dir),
    ));
    tokio::spawn(spawn_pool_refill_loop(
        Arc::clone(&pool),
        background_shutdown_rx.clone(),
    ));

    // 6. Create the DNS registry (shared across daemon)
    let dns = Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new()));

    let health = Arc::new(crate::pool::health::HealthCheckLoop::new(
        crate::pool::health::HealthChecker::new(
            Arc::new(crate::pool::health::RealVsockPinger),
            crate::pool::health::HealthCheckConfig::default(),
        ),
        Arc::clone(&events),
        crate::pool::health::HealthCheckConfig::default(),
    ));
    let vm_provider = Arc::clone(&backend) as Arc<dyn crate::pool::health::RunningVmProvider>;
    tokio::spawn(Arc::clone(&health).run(background_shutdown_rx.clone(), vm_provider));

    // 6a. Start embedded DNS server.
    //      Linux guest TAP interfaces are created on demand, so the gateway
    //      IP may not exist yet when the daemon starts. Bind to 0.0.0.0 so
    //      guest queries to the gateway IP still reach the daemon once the
    //      TAP interface comes up.
    let dns_config = crate::net::dns::DnsResolverConfig::new(embedded_dns_listen_ip());
    let _dns_server =
        match crate::net::server::DnsServer::start(&dns_config, Arc::clone(&dns)).await {
            Ok(server) => {
                info!(addr = %server.addr(), "embedded DNS server started");
                Some(server)
            }
            Err(e) => {
                tracing::warn!(error = %e, "failed to start embedded DNS server (non-fatal)");
                None
            }
        };

    // 7. Build AppState
    // 7a. Clone backend for Docker compat layer before moving into AppState
    let docker_backend = Arc::clone(&backend_arc);

    let state = crate::api::router::AppState {
        backend: backend_arc,
        events,
        start_time: std::time::Instant::now(),
        shutdown: Arc::clone(&shutdown),
        health: Some(health),
        pool: Some(pool),
        networks: Arc::new(tokio::sync::RwLock::new(
            crate::api::routes::networks::NetworkManager::new(),
        )),
        dns: Arc::clone(&dns),
    };

    // 7b. Build router with Docker Engine API compat layer
    let image_store = Arc::new(visor_build::ImageStore::new(image_store_path.clone()));
    let image_manager = Arc::new(crate::image_manager::RuntimeImageManager::new(
        image_store_path.clone(),
    ));
    let build_service = Arc::new(crate::vsock::build_service::VmmBuildService::new(
        Arc::clone(&docker_backend),
        image_store_path,
    ));
    let docker_service_discovery = Arc::new(DockerDnsServiceDiscovery::new(Arc::clone(&dns)));
    let native_router = crate::api::router::build_router(state);
    let docker_router = visor_docker::docker_router_with_service_discovery(
        docker_backend,
        Some(build_service),
        Some(image_store),
        Some(image_manager),
        Some(docker_service_discovery),
    );
    let app = native_router.merge(docker_router);

    // 7. Bind TCP listener
    let listener = TcpListener::bind(&config.listen_addr)
        .await
        .context(format!("failed to bind to {}", config.listen_addr))?;

    info!(addr = %config.listen_addr, "visor daemon started");
    let display_addr = displayable_addr(&config.listen_addr);
    info!("Swagger UI available at http://{display_addr}/docs");

    // 8. Serve with graceful shutdown on Ctrl+C or API shutdown request
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await
        .context("server error")?;
    let _ = background_shutdown_tx.send(true);

    // 9. Persist running VM metadata before teardown so a clean shutdown still
    //    leaves restart-recovery state behind.
    snapshot_all_vms(&vmm_backend).await;

    // 10. Force-stop live VMs so host-side TAP/NAT resources are released
    //     before the daemon exits.
    vmm_backend.shutdown_all_running_vms().await;

    info!("visor daemon stopped cleanly");
    Ok(())
}

fn embedded_dns_listen_ip() -> std::net::Ipv4Addr {
    if cfg!(target_os = "linux") {
        std::net::Ipv4Addr::UNSPECIFIED
    } else {
        std::net::Ipv4Addr::LOCALHOST
    }
}

async fn spawn_pool_refill_loop(
    pool: Arc<crate::pool::manager::PoolManager>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) {
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(30));
    interval.tick().await;

    loop {
        tokio::select! {
            _ = interval.tick() => {
                if let Err(error) = pool.refill_once().await {
                    tracing::warn!(error = %error, "pool refill loop iteration failed");
                }
            }
            _ = shutdown.changed() => {
                tracing::info!("pool refill loop shutting down");
                break;
            }
        }
    }
}

/// Waits for a shutdown signal (Ctrl+C or API shutdown request).
async fn shutdown_signal(notify: Arc<tokio::sync::Notify>) {
    tokio::select! {
        () = notify.notified() => {
            info!("shutdown requested via API");
        }
        result = tokio::signal::ctrl_c() => {
            if let Err(e) = result {
                tracing::error!("failed to install signal handler: {e}");
            }
            info!("shutdown signal received");
        }
    }
}

/// Initializes the tracing subscriber with JSON formatting.
pub(crate) fn init_tracing() {
    use tracing_subscriber::{fmt, EnvFilter};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    fmt().with_env_filter(filter).with_target(true).init();
}

/// Replaces `0.0.0.0` with `127.0.0.1` for display purposes so users get a
/// clickable URL instead of an unroutable wildcard address.
fn displayable_addr(addr: &str) -> String {
    addr.replace("0.0.0.0", "127.0.0.1")
}

fn cleanup_orphan_linux_interfaces() -> anyhow::Result<()> {
    if !cfg!(target_os = "linux") {
        return Ok(());
    }

    let output = std::process::Command::new("ip")
        .args(["-o", "link", "show"])
        .output()
        .context("run `ip -o link show` for orphan TAP cleanup")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("`ip -o link show` failed: {stderr}");
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    for name in visor_linux_interface_names(&stdout) {
        let delete = std::process::Command::new("ip")
            .args(["link", "delete", &name])
            .output()
            .with_context(|| format!("delete orphan interface {name}"))?;

        if !delete.status.success() {
            let stderr = String::from_utf8_lossy(&delete.stderr);
            tracing::warn!(interface = %name, error = %stderr, "failed to delete orphan visor interface");
        }
    }

    Ok(())
}

fn cleanup_orphan_linux_firewall_rules() -> anyhow::Result<()> {
    #[cfg(target_os = "linux")]
    {
        let removed = visor_vmm::net::cleanup_visor_iptables_rules()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        if removed > 0 {
            info!(count = removed, "cleaned up stale Visor iptables rules");
        }
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    {
        Ok(())
    }
}

fn visor_linux_interface_names(ip_link_output: &str) -> Vec<String> {
    ip_link_output
        .lines()
        .filter_map(|line| {
            let mut fields = line.splitn(3, ':');
            let _index = fields.next()?;
            let raw_name = fields.next()?.trim();
            let name = raw_name.split('@').next().unwrap_or(raw_name);
            name.starts_with("vsr").then(|| name.to_owned())
        })
        .collect()
}

/// Restores VMs from persisted state on daemon startup.
///
/// Reads `~/.visor/state/`, cleans up incomplete directories, then
/// loads each VM's metadata and inserts it into the backend as stopped.
async fn restore_vms(backend: &crate::backend::VmmBackend) {
    use crate::state::persistence;

    let base = match persistence::state_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("failed to determine state directory: {e}");
            return;
        }
    };

    if !base.exists() {
        return;
    }

    // Clean up incomplete state dirs from previous crashes.
    match persistence::cleanup_incomplete(&base) {
        Ok(0) => {}
        Ok(n) => info!(count = n, "cleaned up incomplete VM state directories"),
        Err(e) => tracing::warn!("crash recovery cleanup failed: {e}"),
    }

    let vm_ids = match persistence::scan_state_dir(&base) {
        Ok(ids) => ids,
        Err(e) => {
            tracing::warn!("failed to scan state directory: {e}");
            return;
        }
    };

    let mut restored = 0u32;
    for vm_id in &vm_ids {
        let vm_dir = base.join(vm_id);
        match persistence::load_vm_meta(&vm_dir) {
            Ok(meta) => {
                let mut vm_info = crate::backend::VmInfo::new(
                    meta.id.clone(),
                    meta.image.clone(),
                    crate::backend::VmState::Stopped,
                    meta.created_at.clone(),
                    meta.memory_mib,
                    meta.vcpus,
                );
                vm_info.name.clone_from(&meta.name);
                vm_info.ports.clone_from(&meta.ports);
                backend.restore_vm_with_config(vm_info, meta.config).await;
                restored += 1;
            }
            Err(e) => {
                tracing::warn!(vm_id, "failed to restore VM state: {e}");
            }
        }
    }

    if restored > 0 {
        info!(count = restored, "restored VMs from persisted state");
    }
}

/// Removes all `.sock` files from the vsock muxer socket directory.
///
/// On macOS, each VM gets a `{cid}.sock` file in `/var/run/visor/vsock/`.
/// When the daemon exits uncleanly these sockets become stale and prevent
/// new VMs from binding.  We remove them unconditionally on startup since
/// no muxer tasks are running yet.
///
/// On Linux, vsock uses `AF_VSOCK` natively and there are no UDS files to
/// clean up, so this is a no-op.
#[cfg(target_os = "macos")]
fn cleanup_stale_vsock_sockets() {
    use visor_vmm::comms::macos::MacosCommsBackend;

    let dir = std::path::Path::new(MacosCommsBackend::DEFAULT_SOCKET_DIR);
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) => {
            if e.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(dir = %dir.display(), error = %e, "failed to read vsock socket directory");
            }
            return;
        }
    };

    let mut removed = 0u32;
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "sock") {
            if let Err(e) = std::fs::remove_file(&path) {
                tracing::warn!(path = %path.display(), error = %e, "failed to remove stale vsock socket");
            } else {
                removed += 1;
            }
        }
    }

    if removed > 0 {
        info!(count = removed, dir = %dir.display(), "cleaned up stale vsock muxer sockets");
    }
}

/// No-op on non-macOS platforms (vsock uses `AF_VSOCK` natively).
#[cfg(not(target_os = "macos"))]
fn cleanup_stale_vsock_sockets() {}

/// Snapshots all running VMs to disk on daemon shutdown.
///
/// Saves metadata for each running VM so it can be restored on next startup.
async fn snapshot_all_vms(backend: &crate::backend::VmmBackend) {
    use crate::state::persistence;

    let base = match persistence::state_dir() {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!("failed to determine state directory: {e}");
            return;
        }
    };

    let saved = persist_running_vm_state(backend, &base).await;
    if saved > 0 {
        info!(
            count = saved,
            "persisted running VM state for restart recovery"
        );
    }
}

async fn persist_running_vm_state(
    backend: &crate::backend::VmmBackend,
    base: &std::path::Path,
) -> u32 {
    use crate::state::persistence;

    let running_ids = backend.running_vm_ids().await;
    if running_ids.is_empty() {
        return 0;
    }

    let mut saved = 0u32;
    for vm_id in &running_ids {
        let Some(vm_info) = backend.get_vm_info(vm_id).await else {
            continue;
        };

        let config = backend.get_vm_config(vm_id).await.unwrap_or_else(|| {
            let mut cfg = crate::backend::VmConfig::new(vm_info.image.clone());
            cfg.memory_mib = vm_info.memory_mib;
            cfg.vcpus = vm_info.vcpus;
            cfg.name.clone_from(&vm_info.name);
            cfg.ports.clone_from(&vm_info.ports);
            cfg.detach = true;
            cfg
        });

        let meta = persistence::VmMeta {
            id: vm_info.id.clone(),
            name: vm_info.name.clone(),
            image: vm_info.image.clone(),
            config,
            created_at: vm_info.created_at.clone(),
            cid: 0, // CID is not persisted for metadata-only snapshots
            memory_mib: vm_info.memory_mib,
            vcpus: vm_info.vcpus,
            ports: vm_info.ports.clone(),
        };

        let vm_dir = base.join(vm_id);
        match persistence::save_vm_meta(&vm_dir, &meta) {
            Ok(()) => saved += 1,
            Err(e) => tracing::warn!(vm_id, "failed to snapshot VM state: {e}"),
        }
    }

    saved
}

#[cfg(test)]
#[path = "daemon_test.rs"]
mod tests;
