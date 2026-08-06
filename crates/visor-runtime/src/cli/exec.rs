//! `visor exec` — execute a command in a running VM.
//!
//! Sends an exec request to the daemon and prints stdout. Exits with the
//! command's exit code.

use anyhow::Context;

use super::ExecArgs;
use crate::backend::{ExecRequest, ExecResult};

/// Executes the `visor exec` subcommand.
///
/// POSTs an [`ExecRequest`] to the daemon's `/v1/vms/{id}/exec` endpoint.
/// Prints stdout and stderr from the result, then exits with the remote
/// command's exit code.
///
/// # Errors
///
/// Returns an error if the HTTP request fails or the daemon returns a
/// non-success status.
pub async fn execute(addr: &str, args: ExecArgs) -> anyhow::Result<()> {
    let mut request = ExecRequest::new(args.cmd);
    request.env = args.env;
    request.working_dir = args.workdir;

    let client = super::http_client()?;
    let url = format!("{addr}/v1/vms/{}/exec", args.vm_id);

    let resp = client
        .post(&url)
        .json(&request)
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

    let result: ExecResult = resp.json().await.context("failed to parse exec result")?;

    if !result.stdout.is_empty() {
        print!("{}", result.stdout);
    }

    if !result.stderr.is_empty() {
        eprint!("{}", result.stderr);
    }

    if result.exit_code != 0 {
        std::process::exit(result.exit_code);
    }

    Ok(())
}
