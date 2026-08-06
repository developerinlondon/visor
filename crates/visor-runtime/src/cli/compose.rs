//! `visor compose` — manage multi-service compose deployments.
//!
//! Provides Docker Compose–style orchestration for groups of VMs
//! defined in a compose file.

use anyhow::Context;
use clap::Subcommand;

/// Compose management subcommands.
#[derive(Debug, Subcommand)]
#[non_exhaustive]
pub enum ComposeCommand {
    /// Start services defined in a compose file.
    Up(ComposeUpArgs),
    /// Stop and remove services.
    Down(ComposeDownArgs),
    /// List running compose services.
    Ps,
    /// View logs from services.
    Logs(ComposeLogsArgs),
}

/// Arguments for `visor compose up`.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct ComposeUpArgs {
    /// Path to the compose file.
    #[arg(short, long, default_value = "compose.yml")]
    pub file: String,
    /// Run in detached mode.
    #[arg(short, long)]
    pub detach: bool,
}

/// Arguments for `visor compose down`.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct ComposeDownArgs {
    /// Path to the compose file.
    #[arg(short, long, default_value = "compose.yml")]
    pub file: String,
}

/// Arguments for `visor compose logs`.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct ComposeLogsArgs {
    /// Path to the compose file.
    #[arg(short, long, default_value = "compose.yml")]
    pub file: String,
    /// Service name to filter logs (all services if omitted).
    pub service: Option<String>,
}

/// Executes a compose subcommand.
///
/// Dispatches to the appropriate compose operation based on the
/// subcommand variant.
///
/// # Errors
///
/// Returns an error if the compose file cannot be read, the daemon
/// is unreachable, or the requested operation fails.
pub async fn execute(cmd: ComposeCommand, addr: &str) -> anyhow::Result<()> {
    match cmd {
        ComposeCommand::Up(args) => execute_up(&args, addr).await,
        ComposeCommand::Down(args) => execute_down(&args, addr).await,
        ComposeCommand::Ps => execute_ps(addr).await,
        ComposeCommand::Logs(args) => execute_logs(&args, addr).await,
    }
}

async fn execute_up(args: &ComposeUpArgs, addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/compose/up");
    let body = serde_json::json!({
        "file": args.file,
        "detach": args.detach,
    });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    check_status(&resp.status(), resp).await?;
    println!("Services started from {}", args.file);
    Ok(())
}

async fn execute_down(args: &ComposeDownArgs, addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/compose/down");
    let body = serde_json::json!({
        "file": args.file,
    });
    let resp = client
        .post(&url)
        .json(&body)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    check_status(&resp.status(), resp).await?;
    println!("Services stopped from {}", args.file);
    Ok(())
}

async fn execute_ps(addr: &str) -> anyhow::Result<()> {
    let client = super::http_client()?;
    let url = format!("{addr}/v1/compose/ps");
    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    let status = resp.status();
    if !status.is_success() {
        return check_status(&status, resp).await;
    }

    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse compose ps output")?;

    let json =
        serde_json::to_string_pretty(&body).context("failed to serialize compose ps output")?;
    println!("{json}");
    Ok(())
}

async fn execute_logs(args: &ComposeLogsArgs, addr: &str) -> anyhow::Result<()> {
    use std::fmt::Write;

    let client = super::http_client()?;
    let mut url = format!("{addr}/v1/compose/logs?file={}", args.file);
    if let Some(ref svc) = args.service {
        write!(url, "&service={svc}").ok();
    }

    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    let status = resp.status();
    if !status.is_success() {
        return check_status(&status, resp).await;
    }

    let text = resp.text().await.context("failed to read compose logs")?;
    print!("{text}");
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
#[path = "compose_test.rs"]
mod tests;
