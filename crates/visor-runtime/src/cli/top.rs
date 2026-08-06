//! `visor top` — show guest processes in a VM.
//!
//! Queries the daemon for the process list of a specific VM and prints
//! a table similar to the Unix `top` command.

use anyhow::Context;

/// Arguments for the `visor top` subcommand.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct TopArgs {
    /// VM ID to inspect.
    pub vm_id: String,
    /// Sort column for process listing.
    #[arg(long, default_value = "pid")]
    pub sort: String,
}

/// Executes the `visor top` subcommand.
///
/// GETs the process list from the daemon's `/v1/vms/{id}/top` endpoint
/// and prints a formatted table of guest processes.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the daemon returns a
/// non-success status.
pub async fn execute(args: &TopArgs, addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/vms/{}/top?sort={}", args.vm_id, args.sort);

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

    let body: serde_json::Value = resp.json().await.context("failed to parse top output")?;

    let json = serde_json::to_string_pretty(&body).context("failed to serialize top output")?;
    println!("{json}");

    Ok(())
}

#[cfg(test)]
#[path = "top_test.rs"]
mod tests;
