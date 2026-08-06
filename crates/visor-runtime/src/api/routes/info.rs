//! System information, health check, metrics, and shutdown routes.
//!
//! - `GET /v1/info` — host capabilities, mode, uptime, VM count
//! - `GET /v1/health` — health check (always 200)
//! - `POST /v1/shutdown` — graceful daemon shutdown

use axum::Json;
use axum::extract::State;
use axum::http::StatusCode;
use serde::Serialize;
use utoipa::ToSchema;

use crate::api::router::AppState;

/// System information returned by `GET /v1/info`.
#[derive(Debug, Serialize, serde::Deserialize, ToSchema)]
#[non_exhaustive]
pub struct SystemInfo {
    /// Visor version string.
    pub version: String,
    /// Hypervisor mode (e.g. `"kvm"`, `"hvf"`).
    pub mode: String,
    /// Daemon uptime in seconds.
    pub uptime_secs: u64,
    /// Number of known VMs (all states).
    pub vm_count: usize,
    /// Linux version string baked into the embedded kernel.
    pub kernel_version: String,
    /// Size of the embedded kernel binary in bytes.
    pub kernel_size_bytes: u64,
    /// SHA-256 hash of the embedded kernel binary.
    pub kernel_sha256: String,
    /// Runtime capability flags for the current daemon instance.
    pub capabilities: SystemCapabilities,
}

/// Runtime capability flags reported by `GET /v1/info`.
#[derive(Debug, Serialize, serde::Deserialize, ToSchema)]
#[non_exhaustive]
pub struct SystemCapabilities {
    /// Guest-facing runtime capabilities.
    #[serde(flatten)]
    pub guest: GuestCapabilities,
    /// Lifecycle and orchestration capabilities.
    #[serde(flatten)]
    pub lifecycle: LifecycleCapabilities,
    /// Observability and hardening capabilities.
    #[serde(flatten)]
    pub observability: ObservabilityCapabilities,
}

/// Guest-facing runtime capabilities reported by `GET /v1/info`.
#[derive(Debug, Serialize, serde::Deserialize, ToSchema)]
#[non_exhaustive]
pub struct GuestCapabilities {
    /// Whether Linux guest networking is available.
    pub networking: bool,
    /// Whether guest volume mounts are available.
    pub volume_mounts: bool,
    /// Whether snapshot save/restore is available.
    pub snapshot_restore: bool,
}

/// Lifecycle and orchestration capabilities reported by `GET /v1/info`.
#[derive(Debug, Serialize, serde::Deserialize, ToSchema)]
#[non_exhaustive]
pub struct LifecycleCapabilities {
    /// Whether the warm pool manager is configured.
    pub warm_pool: bool,
    /// Whether the daemon health loop is configured.
    pub health_monitoring: bool,
}

/// Observability and hardening capabilities reported by `GET /v1/info`.
#[derive(Debug, Serialize, serde::Deserialize, ToSchema)]
#[non_exhaustive]
pub struct ObservabilityCapabilities {
    /// Whether the metrics endpoint is available.
    pub metrics: bool,
    /// Whether per-VM runtime metrics are exported with real values.
    pub vm_runtime_metrics: bool,
    /// Whether seccomp sandboxing is active for the current daemon instance.
    pub seccomp_sandbox: bool,
}

/// Returns system information including version, mode, uptime, and VM count.
#[utoipa::path(
    get,
    path = "/v1/info",
    tag = "system",
    responses(
        (status = 200, description = "System information", body = SystemInfo)
    )
)]
pub async fn get_info(State(state): State<AppState>) -> Json<SystemInfo> {
    let vm_count = state.backend.list().await.map_or(0, |vms| vms.len());

    Json(SystemInfo {
        version: env!("CARGO_PKG_VERSION").to_owned(),
        mode: if cfg!(target_os = "macos") {
            "hvf"
        } else {
            "kvm"
        }
        .to_owned(),
        uptime_secs: state.start_time.elapsed().as_secs(),
        vm_count,
        kernel_version: visor_kernel::kernel_version().to_owned(),
        kernel_size_bytes: visor_kernel::kernel_size(),
        kernel_sha256: visor_kernel::kernel_sha256().to_owned(),
        capabilities: SystemCapabilities {
            guest: GuestCapabilities {
                networking: cfg!(target_os = "linux"),
                volume_mounts: cfg!(target_os = "linux"),
                snapshot_restore: cfg!(target_os = "linux"),
            },
            lifecycle: LifecycleCapabilities {
                warm_pool: state.pool.is_some(),
                health_monitoring: state.health.is_some(),
            },
            observability: ObservabilityCapabilities {
                metrics: true,
                vm_runtime_metrics: false,
                seccomp_sandbox: false,
            },
        },
    })
}

/// Simple health check — always returns `200 OK`.
#[utoipa::path(
    get,
    path = "/v1/health",
    tag = "system",
    responses(
        (status = 200, description = "Service is healthy")
    )
)]
pub async fn health_check() -> StatusCode {
    StatusCode::OK
}

/// Triggers a graceful daemon shutdown.
///
/// The server stops accepting new connections and finishes in-flight
/// requests before exiting.
#[utoipa::path(
    post,
    path = "/v1/shutdown",
    tag = "system",
    responses(
        (status = 200, description = "Shutdown initiated")
    )
)]
pub async fn shutdown_daemon(State(state): State<AppState>) -> Json<serde_json::Value> {
    tracing::info!("shutdown requested via API");
    state.shutdown.notify_one();
    Json(serde_json::json!({ "status": "shutting down" }))
}

#[cfg(test)]
#[path = "info_test.rs"]
mod tests;
