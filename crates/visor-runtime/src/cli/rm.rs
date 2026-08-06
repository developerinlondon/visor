//! `visor rm` — remove stopped or failed VMs.
//!
//! Sends a DELETE request to the daemon to remove a VM from the list.
//! The VM must be stopped or failed — running VMs must be stopped first.

use anyhow::Context;

use super::RmArgs;

/// Executes the `visor rm` subcommand.
///
/// DELETEs `/v1/vms/{id}` to remove the VM from the daemon's list.
/// Supports removing multiple VMs at once.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the daemon returns a
/// non-success status.
pub async fn execute(addr: &str, args: RmArgs) -> anyhow::Result<()> {
    let client = super::http_client()?;

    for vm_id in &args.vm_ids {
        let url = format!("{addr}/v1/vms/{vm_id}");
        let resp = client
            .delete(&url)
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
            eprintln!("Error removing {vm_id}: {msg}");
            continue;
        }

        println!("{vm_id}");
    }

    Ok(())
}
