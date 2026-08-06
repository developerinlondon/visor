//! `visor inspect` — show detailed VM information.
//!
//! Fetches full VM info from the daemon and prints it as formatted JSON,
//! similar to `docker inspect`.

use anyhow::Context;

use super::InspectArgs;

/// Executes the `visor inspect` subcommand.
///
/// GETs `/v1/vms/{id}` and prints the full VM info as pretty-printed JSON.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the VM is not found.
pub async fn execute(addr: &str, args: InspectArgs) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/vms/{}", args.vm_id);

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

    let vm: serde_json::Value = resp.json().await.context("failed to parse VM info")?;

    let output = serde_json::to_string_pretty(&vm).context("failed to format VM info")?;
    println!("{output}");

    Ok(())
}
