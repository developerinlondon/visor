//! Prometheus metrics exposition endpoint.
//!
//! `GET /v1/metrics` returns daemon and fleet-level metrics in Prometheus text
//! exposition format (`text/plain; version=0.0.4`).
//!
//! # Metrics Exposed
//!
//! | Metric | Type | Description |
//! |--------|------|-------------|
//! | `visor_vms_total` | gauge | Total number of VMs |
//! | `visor_vms_running` | gauge | Number of running VMs |
//! | `visor_pool_available_total` | gauge | Total warm VMs currently available |
//! | `visor_pool_target_total` | gauge | Total configured warm-pool target |
//! | `visor_vm_health_healthy` | gauge | Number of healthy VMs |
//! | `visor_vm_health_unhealthy` | gauge | Number of unhealthy VMs |
//! | `visor_vm_health_unknown` | gauge | Number of VMs without health status |
//! | `visor_vm_runtime_metrics_available` | gauge | Whether real per-VM runtime metrics are exported |

use std::fmt::Write as _;

use anyhow::Context as _;
use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};

use crate::api::router::AppState;
use crate::api::routes::vms::ApiError;
use crate::pool::health::HealthStatus;

/// Prometheus text format content type.
const PROMETHEUS_CONTENT_TYPE: &str = "text/plain; version=0.0.4; charset=utf-8";

const RUNTIME_VM_METRICS_AVAILABLE: u8 = 0;

/// Returns VM metrics in Prometheus text exposition format.
///
/// Exposes only metrics that reflect current daemon state truthfully.
///
/// # Errors
///
/// Returns an error if the backend cannot list VMs.
pub async fn get_metrics(State(state): State<AppState>) -> Result<Response, ApiError> {
    let vms = state
        .backend
        .list()
        .await
        .context("failed to list VMs for metrics")?;

    let mut buf = String::with_capacity(4096);

    // visor_vms_total gauge
    write_metric_header(&mut buf, "visor_vms_total", "Total number of VMs.", "gauge");
    let _ = writeln!(buf, "visor_vms_total {}", vms.len());

    // visor_vms_running gauge
    write_metric_header(
        &mut buf,
        "visor_vms_running",
        "Number of running VMs.",
        "gauge",
    );
    let running = vms
        .iter()
        .filter(|v| v.state == crate::backend::VmState::Running)
        .count();
    let _ = writeln!(buf, "visor_vms_running {running}");

    write_metric_header(
        &mut buf,
        "visor_pool_available_total",
        "Total number of warm VMs currently available.",
        "gauge",
    );
    write_metric_header(
        &mut buf,
        "visor_pool_target_total",
        "Total target size across all configured or active warm pools.",
        "gauge",
    );
    let (pool_available_total, pool_target_total) = if let Some(pool) = &state.pool {
        let status = pool.status().await;
        let target_total = status
            .images
            .values()
            .map(|image| image.target)
            .sum::<usize>();
        (status.total, target_total)
    } else {
        (0usize, 0usize)
    };
    let _ = writeln!(buf, "visor_pool_available_total {pool_available_total}");
    let _ = writeln!(buf, "visor_pool_target_total {pool_target_total}");

    write_metric_header(
        &mut buf,
        "visor_vm_health_healthy",
        "Number of VMs currently marked healthy by the health loop.",
        "gauge",
    );
    write_metric_header(
        &mut buf,
        "visor_vm_health_unhealthy",
        "Number of VMs currently marked unhealthy by the health loop.",
        "gauge",
    );
    write_metric_header(
        &mut buf,
        "visor_vm_health_unknown",
        "Number of VMs without health status yet.",
        "gauge",
    );
    let (healthy_count, unhealthy_count, unknown_count) = if let Some(health) = &state.health {
        let statuses = health.statuses().await;
        let mut healthy = 0usize;
        let mut unhealthy = 0usize;
        let mut unknown = 0usize;
        for status in statuses.values() {
            match status {
                HealthStatus::Healthy => healthy += 1,
                HealthStatus::Unhealthy(_) => unhealthy += 1,
                HealthStatus::Unknown => unknown += 1,
            }
        }
        (healthy, unhealthy, unknown)
    } else {
        (0usize, 0usize, 0usize)
    };
    let _ = writeln!(buf, "visor_vm_health_healthy {healthy_count}");
    let _ = writeln!(buf, "visor_vm_health_unhealthy {unhealthy_count}");
    let _ = writeln!(buf, "visor_vm_health_unknown {unknown_count}");

    write_metric_header(
        &mut buf,
        "visor_vm_runtime_metrics_available",
        "Whether real per-VM CPU, memory, disk, and network runtime metrics are currently exported.",
        "gauge",
    );
    let _ = writeln!(
        buf,
        "visor_vm_runtime_metrics_available {RUNTIME_VM_METRICS_AVAILABLE}"
    );

    Ok(([(header::CONTENT_TYPE, PROMETHEUS_CONTENT_TYPE)], buf).into_response())
}

/// Writes `# HELP` and `# TYPE` header lines for a Prometheus metric family.
fn write_metric_header(buf: &mut String, name: &str, help: &str, metric_type: &str) {
    let _ = writeln!(buf, "# HELP {name} {help}");
    let _ = writeln!(buf, "# TYPE {name} {metric_type}");
}

/// Extracts the value for a named metric from a [`VmMetrics`] snapshot.
#[cfg(test)]
#[path = "metrics_test.rs"]
mod tests;
