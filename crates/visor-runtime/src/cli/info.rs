//! `visor info` — show daemon and host information.
//!
//! Queries the daemon for system information and prints it in a human-readable
//! format.

use anyhow::Context;

use crate::api::routes::info::SystemInfo;
use crate::pool::manager::PoolStatus;

fn format_duration_secs(total_secs: u64) -> String {
    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let seconds = total_secs % 60;

    if hours > 0 {
        format!("{hours}h {minutes}m {seconds}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds}s")
    } else {
        format!("{seconds}s")
    }
}

fn format_size_bytes(size: u64) -> String {
    const KIB: u64 = 1024;
    const MIB: u64 = 1024 * 1024;

    if size >= MIB {
        format!("{:.1} MiB", size as f64 / MIB as f64)
    } else if size >= KIB {
        format!("{:.1} KiB", size as f64 / KIB as f64)
    } else {
        format!("{size} B")
    }
}

fn format_toggle(enabled: bool) -> &'static str {
    if enabled { "enabled" } else { "disabled" }
}

fn format_pool_images(pool: &PoolStatus) -> String {
    let mut images: Vec<_> = pool.images.iter().collect();
    images.sort_by(|(left, _), (right, _)| left.cmp(right));
    images
        .into_iter()
        .map(|(image, status)| format!("{image} {}/{}", status.available, status.target))
        .collect::<Vec<_>>()
        .join(", ")
}

fn format_system_info(info: &SystemInfo, pool: Option<&PoolStatus>) -> String {
    let mut lines = vec![
        format!("Version: {}", info.version),
        format!("Mode: {}", info.mode),
        format!("Uptime: {}", format_duration_secs(info.uptime_secs)),
        format!("Known VMs: {}", info.vm_count),
        format!("Kernel: {}", info.kernel_version),
        format!(
            "Kernel Image: {}",
            format_size_bytes(info.kernel_size_bytes)
        ),
        format!("Kernel SHA256: {}", info.kernel_sha256),
        String::new(),
        "Capabilities:".to_owned(),
        format!(
            "  Networking: {}",
            format_toggle(info.capabilities.guest.networking)
        ),
        format!(
            "  Volume mounts: {}",
            format_toggle(info.capabilities.guest.volume_mounts)
        ),
        format!(
            "  Snapshot restore: {}",
            format_toggle(info.capabilities.guest.snapshot_restore)
        ),
        format!(
            "  Warm pool: {}",
            format_toggle(info.capabilities.lifecycle.warm_pool)
        ),
        format!(
            "  Health monitoring: {}",
            format_toggle(info.capabilities.lifecycle.health_monitoring)
        ),
        format!(
            "  Metrics: {}",
            format_toggle(info.capabilities.observability.metrics)
        ),
        format!(
            "  Per-VM runtime metrics: {}",
            format_toggle(info.capabilities.observability.vm_runtime_metrics)
        ),
        format!(
            "  Seccomp sandbox: {}",
            format_toggle(info.capabilities.observability.seccomp_sandbox)
        ),
        String::new(),
        "Warm Pool State:".to_owned(),
    ];

    if let Some(pool) = pool {
        lines.push(format!("  Available: {}", pool.total));
        let target_total = pool
            .images
            .values()
            .map(|status| status.target)
            .sum::<usize>();
        lines.push(format!("  Target: {}", target_total));
        if pool.images.is_empty() {
            lines.push("  Images: none".to_owned());
        } else {
            lines.push(format!("  Images: {}", format_pool_images(pool)));
        }
    } else {
        lines.push("  Not configured".to_owned());
    }

    lines.join("\n")
}

async fn fetch_pool_status(
    client: &reqwest::Client,
    addr: &str,
) -> anyhow::Result<Option<PoolStatus>> {
    let url = format!("{addr}/v1/pool");
    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to query warm pool status")?;

    if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
        return Ok(None);
    }
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp
            .text()
            .await
            .unwrap_or_else(|_| "unknown error".to_owned());
        anyhow::bail!("daemon error ({status}) while reading warm pool status: {body}");
    }

    let pool = resp
        .json()
        .await
        .context("failed to parse warm pool status response")?;
    Ok(Some(pool))
}

/// Executes the `visor info` subcommand.
///
/// GETs system info from the daemon's `/v1/info` endpoint and prints it in a
/// human-readable key-value format.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the daemon returns a
/// non-success status.
pub async fn execute(addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/info");

    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    let status = resp.status();
    if !status.is_success() {
        let body: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse daemon error response")?;
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("daemon error ({status}): {msg}");
    }

    let info: SystemInfo = resp.json().await.context("failed to parse info response")?;
    let pool = match fetch_pool_status(&client, addr).await {
        Ok(pool) => pool,
        Err(error) => {
            eprintln!("warning: failed to read warm pool status: {error:#}");
            None
        }
    };

    println!("{}", format_system_info(&info, pool.as_ref()));

    Ok(())
}

#[cfg(test)]
#[path = "info_test.rs"]
mod tests;
