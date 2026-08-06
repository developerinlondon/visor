//! `visor stop` — stop a running VM or the daemon.
//!
//! With a VM ID, stops the VM and removes it from the daemon. Without a VM ID,
//! sends a shutdown request to the daemon.

use anyhow::Context;

use super::StopArgs;

/// Executes the `visor stop` subcommand.
///
/// If a VM ID is given, POSTs to `/v1/vms/{id}/stop` and then DELETEs
/// `/v1/vms/{id}` to remove it. Without a VM ID, POSTs to `/v1/shutdown`
/// to stop the daemon.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the daemon returns a
/// non-success status.
pub async fn execute(addr: &str, args: StopArgs) -> anyhow::Result<()> {
    match args.vm_id {
        Some(vm_id) => stop_vm(addr, &vm_id, args.time).await,
        None => stop_daemon(addr).await,
    }
}

/// Stops a VM and removes it from the daemon.
async fn stop_vm(addr: &str, vm_id: &str, timeout_secs: u64) -> anyhow::Result<()> {
    let client = super::http_client()?;

    // Stop the VM (pass grace period as ?t=N, matching Docker API convention)
    let stop_url = format!("{addr}/v1/vms/{vm_id}/stop?t={timeout_secs}");
    let resp = client
        .post(&stop_url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    if !resp.status().is_success() {
        let body: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse daemon error response")?;
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("daemon error: {msg}");
    }

    // Destroy (remove) the VM from the list
    let destroy_url = format!("{addr}/v1/vms/{vm_id}");
    let resp = client
        .delete(&destroy_url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    if !resp.status().is_success() {
        let body: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse daemon error response")?;
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("failed to remove VM: {msg}");
    }

    println!("Removed VM {vm_id}");
    Ok(())
}

/// Sends a shutdown request to the daemon.
async fn stop_daemon(addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/shutdown");

    let resp = client
        .post(&url)
        .send()
        .await
        .context("failed to connect to visor daemon — is it running?")?;

    if !resp.status().is_success() {
        let body: serde_json::Value = resp
            .json()
            .await
            .context("failed to parse daemon error response")?;
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("daemon error: {msg}");
    }

    println!("Daemon shutting down");
    Ok(())
}
