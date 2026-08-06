//! `visor network` — manage virtual networks.
//!
//! Provides create, list, remove, connect, disconnect, and inspect
//! operations for virtual networks that link VMs together.

use anyhow::Context;
use clap::Subcommand;

/// Network management subcommands.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum NetworkCommand {
    /// Create a new virtual network.
    Create(NetworkCreateArgs),
    /// List all virtual networks.
    Ls,
    /// Remove a virtual network.
    Rm(NetworkRmArgs),
    /// Connect a VM to a network.
    Connect(NetworkConnectArgs),
    /// Disconnect a VM from a network.
    Disconnect(NetworkDisconnectArgs),
    /// Show detailed information about a network.
    Inspect(NetworkInspectArgs),
}

/// Arguments for `visor network create`.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct NetworkCreateArgs {
    /// Network name.
    pub name: String,
}

/// Arguments for `visor network rm`.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct NetworkRmArgs {
    /// Network name or ID.
    pub name: String,
}

/// Arguments for `visor network connect`.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct NetworkConnectArgs {
    /// Network name or ID.
    pub network: String,
    /// VM ID to connect.
    pub vm_id: String,
}

/// Arguments for `visor network disconnect`.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct NetworkDisconnectArgs {
    /// Network name or ID.
    pub network: String,
    /// VM ID to disconnect.
    pub vm_id: String,
}

/// Arguments for `visor network inspect`.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct NetworkInspectArgs {
    /// Network name or ID.
    pub name: String,
}

/// Executes a network subcommand.
///
/// Dispatches to the appropriate network operation. All operations
/// communicate with the daemon via HTTP.
///
/// # Errors
///
/// Returns an error if the daemon is unreachable or the requested
/// network operation fails.
pub async fn execute(cmd: NetworkCommand, addr: &str) -> anyhow::Result<()> {
    match cmd {
        NetworkCommand::Create(args) => execute_create(&args, addr).await,
        NetworkCommand::Ls => execute_ls(addr).await,
        NetworkCommand::Rm(args) => execute_rm(&args, addr).await,
        NetworkCommand::Connect(args) => execute_connect(&args, addr).await,
        NetworkCommand::Disconnect(args) => execute_disconnect(&args, addr).await,
        NetworkCommand::Inspect(args) => execute_inspect(&args, addr).await,
    }
}

async fn execute_create(args: &NetworkCreateArgs, addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/networks");
    let body = serde_json::json!({ "name": args.name });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    check_status(&resp.status(), resp).await?;
    println!("Created network '{}'", args.name);
    Ok(())
}

async fn execute_ls(addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/networks");
    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    let status = resp.status();
    if !status.is_success() {
        return check_status(&status, resp).await;
    }

    let body: serde_json::Value = resp.json().await.context("failed to parse network list")?;

    let json = serde_json::to_string_pretty(&body).context("failed to serialize network list")?;
    println!("{json}");
    Ok(())
}

async fn execute_rm(args: &NetworkRmArgs, addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/networks/{}", args.name);
    let resp = client
        .delete(&url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    check_status(&resp.status(), resp).await?;
    println!("Removed network '{}'", args.name);
    Ok(())
}

async fn execute_connect(args: &NetworkConnectArgs, addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/networks/{}/connect", args.network);
    let body = serde_json::json!({ "vm_id": args.vm_id });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    check_status(&resp.status(), resp).await?;
    println!(
        "Connected VM '{}' to network '{}'",
        args.vm_id, args.network
    );
    Ok(())
}

async fn execute_disconnect(args: &NetworkDisconnectArgs, addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/networks/{}/disconnect", args.network);
    let body = serde_json::json!({ "vm_id": args.vm_id });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    check_status(&resp.status(), resp).await?;
    println!(
        "Disconnected VM '{}' from network '{}'",
        args.vm_id, args.network,
    );
    Ok(())
}

async fn execute_inspect(args: &NetworkInspectArgs, addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/networks/{}", args.name);
    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    let status = resp.status();
    if !status.is_success() {
        return check_status(&status, resp).await;
    }

    let body: serde_json::Value = resp.json().await.context("failed to parse network info")?;

    let json = serde_json::to_string_pretty(&body).context("failed to serialize network info")?;
    println!("{json}");
    Ok(())
}

/// Checks the HTTP status and returns a descriptive error for non-success responses.
async fn check_status(status: &reqwest::StatusCode, resp: reqwest::Response) -> anyhow::Result<()> {
    if status.is_success() {
        return Ok(());
    }
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

#[cfg(test)]
#[path = "network_test.rs"]
mod tests;
