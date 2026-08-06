//! Compose orchestrator: manages multi-service deployments.
//!
//! Handles the lifecycle of multi-VM compose projects, including
//! creating networks, booting services in dependency order,
//! and tearing down deployments.

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use tokio::sync::RwLock;
use tracing::info;

use super::types::{
    ComposeDependsOn, ComposeEnvironment, ComposePort, ComposeProject, ComposeService,
};
use crate::api::routes::networks::{NetworkConfig, NetworkManager};
use crate::backend::{ExecutionBackend, PortMapping, VmConfig, VmInfo, VmState};
use crate::net::dns::DnsRegistry;
use crate::pool::health::{HealthCheckConfig, HealthChecker, VsockHealthPinger};

/// A running compose deployment.
#[derive(Debug)]
#[non_exhaustive]
pub struct ComposeInstance {
    /// Project name.
    pub name: String,
    /// Service name → VM info.
    pub services: HashMap<String, VmInfo>,
    /// Network name → network ID.
    pub networks: HashMap<String, String>,
}

/// Status of a compose service.
#[derive(Debug, Clone, serde::Serialize)]
#[non_exhaustive]
pub struct ServiceStatus {
    /// Service name.
    pub name: String,
    /// OCI image reference.
    pub image: String,
    /// Current state (e.g. `"running"`, `"stopped"`).
    pub state: String,
    /// VM identifier, if assigned.
    pub vm_id: Option<String>,
}

/// Compose orchestrator: manages multi-service deployments.
#[non_exhaustive]
pub struct Orchestrator {
    backend: Arc<dyn ExecutionBackend>,
    networks: Arc<RwLock<NetworkManager>>,
    dns: Arc<RwLock<DnsRegistry>>,
    ip_alloc: crate::net::ip_alloc::SubnetAllocator,
    health_pinger: Arc<dyn VsockHealthPinger>,
}

impl Orchestrator {
    /// Create a new orchestrator.
    ///
    /// # Errors
    ///
    /// Returns an error if the default subnet allocator cannot be initialised.
    pub fn new(
        backend: Arc<dyn ExecutionBackend>,
        networks: Arc<RwLock<NetworkManager>>,
        dns: Arc<RwLock<DnsRegistry>>,
        health_pinger: Arc<dyn VsockHealthPinger>,
    ) -> anyhow::Result<Self> {
        let ip_alloc = crate::net::ip_alloc::SubnetAllocator::default_network()
            .context("initialise subnet allocator for compose")?;
        Ok(Self {
            backend,
            networks,
            dns,
            ip_alloc,
            health_pinger,
        })
    }

    /// Bring up all services in the compose project.
    ///
    /// Creates networks first, then boots services in dependency order.
    ///
    /// # Errors
    ///
    /// Returns error if any service fails to start or dependency cycle is detected.
    pub async fn up(&self, project: &ComposeProject) -> anyhow::Result<ComposeInstance> {
        let project_name = project.name.clone().unwrap_or_else(|| "default".to_owned());

        // 1. Create all networks from project.networks.
        let mut network_ids = HashMap::new();
        for (name, net_config) in &project.networks {
            let (subnet, gateway) = net_config
                .ipam
                .as_ref()
                .and_then(|ipam| ipam.config.first())
                .map_or((None, None), |c| (c.subnet.clone(), c.gateway.clone()));

            let config = NetworkConfig {
                name: format!("{project_name}_{name}"),
                subnet,
                gateway,
            };
            let info = self
                .networks
                .write()
                .await
                .create(config)
                .with_context(|| format!("create network '{name}'"))?;
            network_ids.insert(name.clone(), info.id);
        }

        // 2. Sort services by dependency order.
        let order =
            dependency_sort(&project.services).context("sort services by dependency order")?;

        // 3. Boot services in order.
        let mut services = HashMap::new();
        for svc_name in &order {
            let svc = project
                .services
                .get(svc_name)
                .context(format!("service '{svc_name}' not found in project"))?;

            let vm_config = build_vm_config(svc_name, svc);
            let vm_info = self
                .backend
                .create(vm_config)
                .await
                .with_context(|| format!("create VM for service '{svc_name}'"))?;

            // 4. Connect VM to its declared networks.
            for net_name in &svc.networks {
                if let Some(net_id) = network_ids.get(net_name) {
                    self.networks
                        .write()
                        .await
                        .connect_vm(net_id, &vm_info.id)
                        .with_context(|| {
                            format!("connect service '{svc_name}' to network '{net_name}'")
                        })?;
                }
            }

            // 5. Allocate an IP and register service name in DNS.
            if let Ok(ip) = self.ip_alloc.allocate() {
                self.dns.write().await.register(svc_name, ip);
                info!(service = %svc_name, ip = %ip, "registered compose service in DNS");
            }

            // 6. If another service depends on this one with service_healthy,
            //    wait for health check to pass before proceeding.
            if needs_health_wait(svc_name, &project.services) {
                let cid = vm_info
                    .cid
                    .context(format!("service '{svc_name}' has no CID for health check"))?;
                self.wait_for_healthy(svc_name, cid, Duration::from_secs(60))
                    .await
                    .with_context(|| format!("wait for service '{svc_name}' to become healthy"))?;
            }

            services.insert(svc_name.clone(), vm_info);
        }

        Ok(ComposeInstance {
            name: project_name,
            services,
            networks: network_ids,
        })
    }

    /// Bring down all services and remove networks.
    ///
    /// Stops all services, destroys their VMs, then deletes all networks.
    ///
    /// # Errors
    ///
    /// Returns error if any service fails to stop or any network fails to delete.
    pub async fn down(&self, instance: &ComposeInstance) -> anyhow::Result<()> {
        // Unregister service names from DNS.
        {
            let mut dns = self.dns.write().await;
            for svc_name in instance.services.keys() {
                dns.unregister(svc_name);
                info!(service = %svc_name, "unregistered compose service from DNS");
            }
        }

        // Stop all services.
        for (svc_name, vm_info) in &instance.services {
            self.backend
                .stop(&vm_info.id, 10)
                .await
                .with_context(|| format!("stop service '{svc_name}'"))?;
        }

        // Destroy all services.
        for (svc_name, vm_info) in &instance.services {
            self.backend
                .destroy(&vm_info.id)
                .await
                .with_context(|| format!("destroy service '{svc_name}'"))?;
        }

        // Delete all networks.
        for (net_name, net_id) in &instance.networks {
            self.networks
                .write()
                .await
                .delete(net_id)
                .with_context(|| format!("delete network '{net_name}'"))?;
        }

        Ok(())
    }

    /// List status of all services in a compose instance.
    #[must_use]
    pub fn ps(&self, instance: &ComposeInstance) -> Vec<ServiceStatus> {
        instance
            .services
            .iter()
            .map(|(name, vm)| ServiceStatus {
                name: name.clone(),
                image: vm.image.clone(),
                state: vm_state_str(vm.state),
                vm_id: Some(vm.id.clone()),
            })
            .collect()
    }

    /// Wait for a service's VM to become healthy via vsock ping.
    ///
    /// Polls the health checker at 1-second intervals until the VM responds
    /// to a ping or the timeout expires.
    async fn wait_for_healthy(
        &self,
        svc_name: &str,
        cid: u32,
        timeout: Duration,
    ) -> anyhow::Result<()> {
        info!(service = %svc_name, cid, "waiting for service to become healthy");
        let config = HealthCheckConfig {
            ping_timeout: Duration::from_secs(2),
            ..HealthCheckConfig::default()
        };
        let checker = HealthChecker::new(Arc::clone(&self.health_pinger), config);
        let deadline = tokio::time::Instant::now() + timeout;
        let mut interval = tokio::time::interval(Duration::from_secs(1));
        loop {
            interval.tick().await;
            let status = checker.check_vm(cid).await;
            if status.is_healthy() {
                info!(service = %svc_name, cid, "service is healthy");
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(anyhow::anyhow!(
                    "service '{svc_name}' (CID {cid}) did not become healthy within {timeout:?}"
                ));
            }
        }
    }
}

/// Check whether any service depends on `service_name` with `condition: service_healthy`.
///
/// Used by [`Orchestrator::up`] to decide whether to wait for a health check
/// after creating a service's VM.
pub fn needs_health_wait<S: std::hash::BuildHasher>(
    service_name: &str,
    services: &HashMap<String, ComposeService, S>,
) -> bool {
    services.values().any(|svc| {
        if let ComposeDependsOn::Extended(map) = &svc.depends_on {
            map.get(service_name)
                .and_then(|cond| cond.condition.as_deref())
                .is_some_and(|c| c == "service_healthy")
        } else {
            false
        }
    })
}

/// Sort services in topological order based on `depends_on`.
///
/// Uses Kahn's algorithm (BFS topological sort):
/// 1. Build in-degree map from `depends_on`
/// 2. Start with services having in-degree 0
/// 3. Process queue, decrementing dependents' in-degrees
/// 4. If not all services processed → cycle detected → error
///
/// # Errors
///
/// Returns error if a dependency cycle is detected or a dependency
/// references a non-existent service.
pub fn dependency_sort<S: std::hash::BuildHasher>(
    services: &HashMap<String, ComposeService, S>,
) -> anyhow::Result<Vec<String>> {
    if services.is_empty() {
        return Ok(Vec::new());
    }

    let mut in_degree: HashMap<String, usize> = HashMap::new();
    let mut dependents: HashMap<String, Vec<String>> = HashMap::new();

    for name in services.keys() {
        in_degree.entry(name.clone()).or_insert(0);
    }

    for (name, svc) in services {
        let deps = extract_dependency_names(&svc.depends_on);
        for dep in deps {
            anyhow::ensure!(
                services.contains_key(&dep),
                "service '{name}' depends on '{dep}', which is not defined"
            );
            *in_degree.entry(name.clone()).or_insert(0) += 1;
            dependents.entry(dep).or_default().push(name.clone());
        }
    }

    // Seed the queue with all services at in-degree 0 (sorted for determinism).
    let mut seeds: Vec<String> = in_degree
        .iter()
        .filter_map(
            |(name, deg)| {
                if *deg == 0 { Some(name.clone()) } else { None }
            },
        )
        .collect();
    seeds.sort();

    let mut queue: VecDeque<String> = VecDeque::from(seeds);
    let mut result = Vec::with_capacity(services.len());

    while let Some(name) = queue.pop_front() {
        if let Some(deps) = dependents.get(&name) {
            let mut next_ready: Vec<String> = Vec::new();
            for dep in deps {
                if let Some(deg) = in_degree.get_mut(dep) {
                    *deg -= 1;
                    if *deg == 0 {
                        next_ready.push(dep.clone());
                    }
                }
            }
            next_ready.sort();
            queue.extend(next_ready);
        }
        result.push(name);
    }

    anyhow::ensure!(
        result.len() == services.len(),
        "dependency cycle detected among services"
    );

    Ok(result)
}

/// Extract dependency names from a [`ComposeDependsOn`] value.
fn extract_dependency_names(depends_on: &ComposeDependsOn) -> Vec<String> {
    match depends_on {
        ComposeDependsOn::Empty => Vec::new(),
        ComposeDependsOn::Simple(names) => names.clone(),
        ComposeDependsOn::Extended(map) => map.keys().cloned().collect(),
    }
}

/// Build a [`VmConfig`] from a compose service definition.
fn build_vm_config(name: &str, svc: &ComposeService) -> VmConfig {
    let env = match &svc.environment {
        ComposeEnvironment::Empty => Vec::new(),
        ComposeEnvironment::List(items) => items.clone(),
        ComposeEnvironment::Map(map) => map.iter().map(|(k, v)| format!("{k}={v}")).collect(),
    };

    let memory_mib = svc.mem_limit.as_deref().map_or(512, parse_mem_limit);

    let mut config = VmConfig::new(svc.image.clone());
    config.cmd = svc.command.clone().unwrap_or_default();
    config.env = env;
    config.working_dir.clone_from(&svc.working_dir);
    config.memory_mib = memory_mib;
    config.name = Some(name.to_owned());
    config.detach = true;
    config.networks = svc.networks.clone();
    config.ports = svc
        .ports
        .iter()
        .filter_map(|p| compose_port_to_port_mapping(p).ok())
        .collect();
    config
}

/// Parse a memory limit string (e.g. `"512m"`, `"1g"`) into MiB.
///
/// Supports `g` (gibibytes) and `m` (mebibytes) suffixes.
/// Falls back to 512 MiB if parsing fails.
fn parse_mem_limit(limit: &str) -> u32 {
    let trimmed = limit.trim().to_lowercase();
    if let Some(val) = trimmed.strip_suffix('g') {
        if let Ok(n) = val.parse::<u32>() {
            return n.saturating_mul(1024);
        }
    } else if let Some(val) = trimmed.strip_suffix('m') {
        if let Ok(n) = val.parse::<u32>() {
            return n;
        }
    } else if let Ok(bytes) = trimmed.parse::<u64>() {
        return u32::try_from(bytes / (1024 * 1024)).unwrap_or(512);
    }
    512
}

/// Convert a [`ComposePort`] to a [`PortMapping`].
///
/// # Errors
///
/// Returns an error if a short-syntax port string cannot be parsed.
fn compose_port_to_port_mapping(port: &ComposePort) -> anyhow::Result<PortMapping> {
    match port {
        ComposePort::Short(s) => {
            // Strip optional protocol suffix (e.g. "8080:80/tcp" -> "8080:80")
            let port_spec = s.split('/').next().unwrap_or(s);
            crate::cli::parse_port_mapping(port_spec)
        }
        ComposePort::Long {
            target,
            published,
            protocol: _,
        } => {
            let host_port = published
                .ok_or_else(|| anyhow::anyhow!("long port syntax requires 'published' field"))?;
            Ok(PortMapping::new(host_port, *target))
        }
    }
}

/// Convert a [`VmState`] to a lowercase display string.
fn vm_state_str(state: VmState) -> String {
    match state {
        VmState::Creating => "creating",
        VmState::Running => "running",
        VmState::Stopped => "stopped",
        VmState::Failed => "failed",
        _ => unreachable!(),
    }
    .to_owned()
}

#[cfg(test)]
#[path = "orchestrator_test.rs"]
mod tests;
