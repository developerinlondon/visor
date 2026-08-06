//! Health checking for running VMs via vsock ping/pong.
//!
//! Provides [`HealthChecker`] for single-VM health probes and [`HealthCheckLoop`]
//! for periodic monitoring of all running VMs. Health state transitions emit
//! events via [`EventBroadcaster`](crate::api::sse::EventBroadcaster).
//!
//! # Architecture
//!
//! ```text
//! HealthCheckLoop
//!   ├── periodically iterates running VMs
//!   ├── HealthChecker::check_vm(cid) for each
//!   │     └── VsockHealthPinger::ping(cid, timeout)
//!   ├── tracks consecutive failures per VM
//!   └── emits vm.health.unhealthy / vm.health.recovered events
//! ```

use std::collections::HashMap;
use std::fmt;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use utoipa::ToSchema;

use crate::api::sse::{EventBroadcaster, VmEvent};
use crate::vsock::client::{VSOCK_AGENT_PORT, VsockClient};

// ── HealthStatus ─────────────────────────────────────────────────

/// Health status of a VM as determined by vsock ping probes.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum HealthStatus {
    /// VM responded to ping within the timeout.
    Healthy,
    /// VM failed to respond — contains the reason string.
    Unhealthy(String),
    /// VM has not been checked yet.
    Unknown,
}

impl fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Healthy => write!(f, "healthy"),
            Self::Unhealthy(reason) => write!(f, "unhealthy: {reason}"),
            Self::Unknown => write!(f, "unknown"),
        }
    }
}

impl HealthStatus {
    /// Returns `true` if this status is [`Healthy`](Self::Healthy).
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(self, Self::Healthy)
    }
}

// ── VmHealthReport ───────────────────────────────────────────────

/// Per-VM health report including status and failure count.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct VmHealthReport {
    /// VM identifier.
    pub vm_id: String,
    /// Current health status.
    pub status: HealthStatus,
    /// Number of consecutive health check failures.
    pub consecutive_failures: u32,
}

// ── HealthCheckConfig ────────────────────────────────────────────

/// Configuration for health check behavior.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HealthCheckConfig {
    /// Timeout for a single vsock ping (default: 2s).
    pub ping_timeout: Duration,
    /// Interval between health check sweeps (default: 30s).
    pub check_interval: Duration,
    /// Number of consecutive failures before marking unhealthy (default: 3).
    pub failure_threshold: u32,
}

impl Default for HealthCheckConfig {
    fn default() -> Self {
        Self {
            ping_timeout: Duration::from_secs(2),
            check_interval: Duration::from_secs(30),
            failure_threshold: 3,
        }
    }
}

// ── VsockHealthPinger trait ──────────────────────────────────────

/// Trait abstracting the vsock ping operation for testability.
///
/// The real implementation sends a JSON-RPC `ping` to the guest via vsock;
/// tests inject a mock.
#[async_trait]
pub trait VsockHealthPinger: Send + Sync {
    /// Send a ping to the guest at the given CID and wait for pong.
    ///
    /// # Errors
    ///
    /// Returns an error if the ping fails or times out.
    async fn ping(&self, cid: u32, timeout: Duration) -> anyhow::Result<()>;
}

/// Real vsock pinger that connects via `AF_VSOCK` and sends a JSON-RPC ping.
pub struct RealVsockPinger;

#[async_trait]
impl VsockHealthPinger for RealVsockPinger {
    async fn ping(&self, cid: u32, timeout: Duration) -> anyhow::Result<()> {
        let backend = crate::backend::comms_backend();
        let connect_result = tokio::time::timeout(
            timeout,
            VsockClient::connect(&backend, cid, VSOCK_AGENT_PORT),
        )
        .await;

        let mut client = match connect_result {
            Ok(Ok(c)) => c,
            Ok(Err(e)) => {
                return Err(e).context(format!("vsock health ping connect to CID {cid}"));
            }
            Err(_elapsed) => {
                anyhow::bail!("vsock health ping connect to CID {cid} timed out after {timeout:?}");
            }
        };

        client.set_request_timeout(timeout);
        let pong = client
            .ping()
            .await
            .context(format!("vsock health ping to CID {cid}"))?;
        if pong != "pong" {
            anyhow::bail!(
                "unexpected ping response from CID {cid}: expected \"pong\", got {pong:?}"
            );
        }
        Ok(())
    }
}

// ── HealthChecker ────────────────────────────────────────────────

/// Checks a single VM's health via vsock ping.
///
/// Wraps a [`VsockHealthPinger`] with a configured timeout. Use
/// [`check_vm`](Self::check_vm) to probe a VM by its CID.
pub struct HealthChecker {
    pinger: Arc<dyn VsockHealthPinger>,
    config: HealthCheckConfig,
}

impl HealthChecker {
    /// Creates a new health checker with the given pinger and config.
    #[must_use]
    pub fn new(pinger: Arc<dyn VsockHealthPinger>, config: HealthCheckConfig) -> Self {
        Self { pinger, config }
    }

    /// Check a single VM's health by sending a vsock ping.
    ///
    /// Returns [`HealthStatus::Healthy`] if the ping succeeds within the timeout,
    /// or [`HealthStatus::Unhealthy`] with the error reason if it fails.
    pub async fn check_vm(&self, cid: u32) -> HealthStatus {
        match self.pinger.ping(cid, self.config.ping_timeout).await {
            Ok(()) => HealthStatus::Healthy,
            Err(e) => HealthStatus::Unhealthy(format!("{e}")),
        }
    }
}

// ── Per-VM tracking state ────────────────────────────────────────

/// Internal tracking state for a single VM's health.
struct VmHealthState {
    status: HealthStatus,
    consecutive_failures: u32,
    /// Whether we already emitted an unhealthy event (to avoid duplicates).
    notified_unhealthy: bool,
}

impl VmHealthState {
    fn new() -> Self {
        Self {
            status: HealthStatus::Unknown,
            consecutive_failures: 0,
            notified_unhealthy: false,
        }
    }
}

// ── HealthCheckLoop ──────────────────────────────────────────────

/// Periodically checks all running VMs and tracks health state.
///
/// Emits `vm.health.unhealthy` events when a VM exceeds the failure threshold,
/// and `vm.health.recovered` events when a previously unhealthy VM recovers.
pub struct HealthCheckLoop {
    checker: RwLock<HealthChecker>,
    events: Arc<EventBroadcaster>,
    config: HealthCheckConfig,
    /// Per-VM health tracking state.
    states: RwLock<HashMap<String, VmHealthState>>,
}

impl HealthCheckLoop {
    /// Creates a new health check loop.
    #[must_use]
    pub fn new(
        checker: HealthChecker,
        events: Arc<EventBroadcaster>,
        config: HealthCheckConfig,
    ) -> Self {
        Self {
            checker: RwLock::new(checker),
            events,
            config,
            states: RwLock::new(HashMap::new()),
        }
    }

    /// Returns the current config.
    #[must_use]
    pub fn config(&self) -> &HealthCheckConfig {
        &self.config
    }

    /// Replace the health checker (used in tests to swap mock pingers).
    pub async fn replace_checker(&self, checker: HealthChecker) {
        *self.checker.write().await = checker;
    }

    /// Run a single health check sweep across all provided running VMs.
    ///
    /// `running_vms` is a list of `(vm_id, cid)` pairs for VMs that are
    /// currently in the Running state.
    pub async fn check_all(&self, running_vms: &[(String, u32)]) {
        let checker = self.checker.read().await;
        let mut vm_states = self.states.write().await;

        // Remove VMs that are no longer running.
        let active_ids: std::collections::HashSet<&str> =
            running_vms.iter().map(|(id, _)| id.as_str()).collect();
        vm_states.retain(|id, _| active_ids.contains(id.as_str()));

        for (vm_id, cid) in running_vms {
            let check_result = checker.check_vm(*cid).await;
            let entry = vm_states
                .entry(vm_id.clone())
                .or_insert_with(VmHealthState::new);

            match &check_result {
                HealthStatus::Healthy => {
                    let was_unhealthy = entry.notified_unhealthy;
                    entry.consecutive_failures = 0;
                    entry.status = HealthStatus::Healthy;

                    // Emit recovery event if transitioning from unhealthy.
                    if was_unhealthy {
                        entry.notified_unhealthy = false;
                        self.events.send(
                            VmEvent::new("vm.health.recovered", vm_id.as_str()).with_data(
                                serde_json::json!({
                                    "status": "healthy",
                                }),
                            ),
                        );
                    }
                }
                HealthStatus::Unhealthy(reason) => {
                    entry.consecutive_failures += 1;
                    entry.status = HealthStatus::Unhealthy(reason.clone());

                    // Emit unhealthy event only when crossing the threshold.
                    if entry.consecutive_failures >= self.config.failure_threshold
                        && !entry.notified_unhealthy
                    {
                        entry.notified_unhealthy = true;
                        self.events.send(
                            VmEvent::new("vm.health.unhealthy", vm_id.as_str()).with_data(
                                serde_json::json!({
                                    "reason": reason,
                                    "consecutive_failures": entry.consecutive_failures,
                                }),
                            ),
                        );
                    }
                }
                HealthStatus::Unknown => {
                    // Should not happen from check_vm, but handle gracefully.
                    entry.status = HealthStatus::Unknown;
                }
            }
        }
    }

    /// Returns a snapshot of current health statuses for all tracked VMs.
    pub async fn statuses(&self) -> HashMap<String, HealthStatus> {
        let states = self.states.read().await;
        states
            .iter()
            .map(|(id, state)| (id.clone(), state.status.clone()))
            .collect()
    }

    /// Returns a detailed health report for a specific VM.
    ///
    /// Returns `None` if the VM has no health data.
    pub async fn report(&self, vm_id: &str) -> Option<VmHealthReport> {
        let states = self.states.read().await;
        states.get(vm_id).map(|state| VmHealthReport {
            vm_id: vm_id.to_owned(),
            status: state.status.clone(),
            consecutive_failures: state.consecutive_failures,
        })
    }

    /// Returns the check interval from the config.
    #[must_use]
    pub fn check_interval(&self) -> Duration {
        self.config.check_interval
    }

    /// Run the health check loop until the cancellation token fires.
    ///
    /// # Errors
    ///
    /// Returns an error if the loop encounters an unrecoverable issue.
    pub async fn run(
        self: Arc<Self>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
        vm_provider: Arc<dyn RunningVmProvider>,
    ) {
        let mut interval = tokio::time::interval(self.config.check_interval);
        interval.tick().await; // skip first immediate tick

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    let running = vm_provider.running_vms().await;
                    self.check_all(&running).await;
                }
                _ = shutdown.changed() => {
                    tracing::info!("health check loop shutting down");
                    break;
                }
            }
        }
    }
}

// ── RunningVmProvider ────────────────────────────────────────────

/// Trait for providing the list of currently running VMs.
///
/// Abstracts backend access so the health loop can query running VMs
/// without owning the backend directly.
#[async_trait]
pub trait RunningVmProvider: Send + Sync {
    /// Returns `(vm_id, cid)` pairs for all currently running VMs.
    async fn running_vms(&self) -> Vec<(String, u32)>;
}

#[cfg(test)]
#[path = "health_test.rs"]
mod tests;
