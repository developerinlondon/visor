//! `visor logs` — view stdout/stderr from a VM.
//!
//! Fetches the VM info from the daemon and prints any captured stdout and
//! stderr output.

use anyhow::Context;

use super::LogsArgs;
use crate::backend::VmInfo;

/// Executes the `visor logs` subcommand.
///
/// GETs `/v1/vms/{id}` and prints the VM's captured stdout and stderr.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the VM is not found.
pub async fn execute(addr: &str, args: LogsArgs) -> anyhow::Result<()> {
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

    let vm: VmInfo = resp.json().await.context("failed to parse VM info")?;

    if let Some(stdout) = &vm.stdout {
        if !stdout.is_empty() {
            print!("{stdout}");
        }
    }
    if let Some(stderr) = &vm.stderr {
        if !stderr.is_empty() {
            eprint!("{stderr}");
        }
    }

    let has_output = vm.stdout.as_deref().is_some_and(|s| !s.is_empty())
        || vm.stderr.as_deref().is_some_and(|s| !s.is_empty());

    if !has_output {
        match vm.state {
            crate::backend::VmState::Running | crate::backend::VmState::Creating => {
                eprintln!("No logs yet \u{2014} VM is still running.");
            }
            _ => {
                eprintln!("No output captured.");
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[path = "logs_test.rs"]
mod tests;
