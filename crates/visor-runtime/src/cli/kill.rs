//! `visor kill` — force-kill a running VM.
//!
//! Immediately kills a VM without graceful shutdown. Unlike `visor stop`
//! which tries vsock shutdown and waits, `kill` sets the `kill_flag` instantly.

use anyhow::Context;

use super::KillArgs;

/// Executes the `visor kill` subcommand.
///
/// POSTs to `/v1/vms/{id}/kill` to force-kill the VM, then DELETEs
/// `/v1/vms/{id}` to remove it.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the VM is not found.
pub async fn execute(addr: &str, args: KillArgs) -> anyhow::Result<()> {
    let client = super::http_client()?;

    // Kill the VM (immediate, no grace period)
    let kill_url = format!("{addr}/v1/vms/{}/kill", args.vm_id);
    let resp = client
        .post(&kill_url)
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

    // Remove the VM from the list
    let destroy_url = format!("{addr}/v1/vms/{}", args.vm_id);
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

    println!("Killed {}", args.vm_id);
    Ok(())
}
