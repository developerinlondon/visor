//! `visor ps` — list running VMs.
//!
//! Queries the daemon for all known VMs and prints a formatted table.

use std::collections::HashMap;

use anyhow::Context;

use crate::backend::VmInfo;
use crate::pool::health::{HealthStatus, VmHealthReport};

fn format_vm_state(vm: &VmInfo) -> String {
    serde_json::to_value(vm.state)
        .ok()
        .and_then(|value| value.as_str().map(ToOwned::to_owned))
        .unwrap_or_else(|| format!("{:?}", vm.state))
}

fn format_vm_health(report: Option<&VmHealthReport>) -> String {
    match report {
        Some(VmHealthReport {
            status: HealthStatus::Healthy,
            ..
        }) => "healthy".to_owned(),
        Some(VmHealthReport {
            status: HealthStatus::Unknown,
            ..
        }) => "unknown".to_owned(),
        Some(VmHealthReport {
            status: HealthStatus::Unhealthy(_),
            consecutive_failures,
            ..
        }) if *consecutive_failures > 0 => format!("unhealthy({consecutive_failures})"),
        Some(VmHealthReport {
            status: HealthStatus::Unhealthy(_),
            ..
        }) => "unhealthy".to_owned(),
        None => "-".to_owned(),
    }
}

fn format_vm_ports(vm: &VmInfo) -> String {
    if vm.ports.is_empty() {
        return "-".to_owned();
    }

    vm.ports
        .iter()
        .map(|port| format!("{}->{}/{}", port.host_port, port.guest_port, port.protocol))
        .collect::<Vec<_>>()
        .join(",")
}

fn render_vm_table(vms: &[VmInfo], health_reports: &HashMap<String, VmHealthReport>) -> String {
    let mut lines = vec![format!(
        "{:<36}  {:<16}  {:<20}  {:<10}  {:<14}  {:<5}  {:<22}  CREATED",
        "ID", "NAME", "IMAGE", "STATE", "HEALTH", "CID", "PORTS"
    )];

    for vm in vms {
        let name = vm.name.as_deref().unwrap_or("-");
        let state = format_vm_state(vm);
        let health = format_vm_health(health_reports.get(&vm.id));
        let cid = vm.cid.map_or_else(|| "-".to_owned(), |cid| cid.to_string());
        let ports = format_vm_ports(vm);
        lines.push(format!(
            "{:<36}  {:<16}  {:<20}  {:<10}  {:<14}  {:<5}  {:<22}  {}",
            vm.id, name, vm.image, state, health, cid, ports, vm.created_at,
        ));
    }

    lines.join("\n")
}

async fn fetch_health_reports(
    client: &reqwest::Client,
    addr: &str,
    vms: &[VmInfo],
) -> HashMap<String, VmHealthReport> {
    let mut reports = HashMap::new();

    for vm in vms {
        let url = format!("{addr}/v1/vms/{}/health", vm.id);
        let response = match client.get(&url).send().await {
            Ok(response) => response,
            Err(_) => continue,
        };

        if !response.status().is_success() {
            continue;
        }

        if let Ok(report) = response.json::<VmHealthReport>().await {
            reports.insert(vm.id.clone(), report);
        }
    }

    reports
}

/// Executes the `visor ps` subcommand.
///
/// GETs the list of VMs from the daemon's `/v1/vms` endpoint and prints a
/// table with columns: ID, NAME, IMAGE, STATE, CREATED.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the daemon returns a
/// non-success status.
pub async fn execute(addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/vms");

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

    let vms: Vec<VmInfo> = resp.json().await.context("failed to parse VM list")?;
    let health_reports = fetch_health_reports(&client, addr, &vms).await;

    println!("{}", render_vm_table(&vms, &health_reports));

    Ok(())
}

#[cfg(test)]
#[path = "ps_test.rs"]
mod tests;
