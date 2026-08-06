//! `visor run` — create and run a VM from an OCI image.
//!
//! Sends a VM creation request to the daemon. The daemon pulls the OCI image,
//! boots a KVM microVM, waits for the command to finish, and returns stdout,
//! stderr, and the exit code in the response.

use anyhow::Context;
use visor_types::GuestVirtualizationMode;

use super::RunArgs;
use crate::backend::VmConfig;
use crate::cli::{parse_port_mapping, parse_volume_mount};

/// Executes the `visor run` subcommand.
///
/// Parses port mappings, builds a [`VmConfig`] from the CLI arguments, and
/// POSTs it to the daemon's `/v1/vms` endpoint. Prints stdout from the VM
/// and exits with the VM's exit code.
///
/// # Errors
///
/// Returns an error if port mappings are invalid, the HTTP request fails, or
/// the daemon returns a non-success status.
pub async fn execute(addr: &str, args: RunArgs) -> anyhow::Result<()> {
    let ports: Vec<_> = args
        .port
        .iter()
        .map(|p| parse_port_mapping(p))
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to parse port mappings")?;

    let volumes: Vec<_> = args
        .volume
        .iter()
        .map(|v| parse_volume_mount(v))
        .collect::<anyhow::Result<Vec<_>>>()
        .context("failed to parse volume mounts")?;

    let mut config = VmConfig::new(args.image);
    config.cmd = args.cmd;
    config.env = args.env;
    config.working_dir = args.workdir;
    config.memory_mib = args.memory;
    config.vcpus = args.cpus;
    config.name = args.name;
    config.network_enabled = !args.no_network;
    config.ports = ports;
    config.detach = args.detach;
    config.volumes = volumes;
    if args.nested_virt {
        config.guest_virtualization = GuestVirtualizationMode::Nested;
    }

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("failed to create HTTP client")?;

    let url = format!("{addr}/v1/vms");

    let resp = client
        .post(&url)
        .json(&config)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    let status = resp.status();
    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse daemon response")?;

    if !status.is_success() {
        let msg = body
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown error");
        anyhow::bail!("daemon error ({status}): {msg}");
    }

    // In detach mode, print the VM ID and return.
    if args.detach {
        let id = body.get("id").and_then(|v| v.as_str()).unwrap_or("unknown");
        let name = body
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");
        println!("{id} ({name})");
        return Ok(());
    }

    // Print stdout from the VM.
    if let Some(stdout) = body.get("stdout").and_then(|v| v.as_str()) {
        if !stdout.is_empty() {
            print!("{stdout}");
        }
    }

    // Print stderr from the VM.
    if let Some(stderr) = body.get("stderr").and_then(|v| v.as_str()) {
        if !stderr.is_empty() {
            eprint!("{stderr}");
        }
    }

    // Exit with the VM's exit code.
    if let Some(exit_code) = body.get("exit_code").and_then(serde_json::Value::as_i64) {
        let code = i32::try_from(exit_code).unwrap_or(1);
        if code != 0 {
            std::process::exit(code);
        }
    }

    Ok(())
}

#[cfg(test)]
#[path = "run_test.rs"]
mod tests;
