//! `visor restart` — stop and restart the daemon.
//!
//! Attempts to gracefully stop an existing daemon, waits for it to exit,
//! then starts a fresh daemon with the same listen address.

use anyhow::Context;

use super::{RestartArgs, StartArgs};

/// Executes the `visor restart` subcommand.
///
/// Sends a shutdown request to any running daemon, waits up to 3 seconds
/// for it to stop, then delegates to `start::execute` to launch a new one.
///
/// # Errors
///
/// Returns an error if the new daemon fails to start.
pub async fn execute(addr: &str, args: RestartArgs) -> anyhow::Result<()> {
    // Try to stop existing daemon (ignore errors — it may not be running).
    if super::start::is_daemon_running(&args.listen).await {
        println!("Stopping existing daemon...");
        let _ = try_stop_daemon(addr).await;
    }

    // Wait for the daemon to actually stop (up to 3s timeout).
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    while tokio::time::Instant::now() < deadline {
        if !super::start::is_daemon_running(&args.listen).await {
            println!("Daemon stopped.");
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }

    let start_args = StartArgs {
        listen: args.listen,
        foreground: false,
    };
    super::start::execute(start_args).await
}

/// Sends a shutdown request to the daemon. Errors are intentionally ignored
/// by the caller since the daemon may not be running.
async fn try_stop_daemon(addr: &str) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .context("failed to build HTTP client")?;

    let url = format!("{addr}/v1/shutdown");
    client
        .post(&url)
        .send()
        .await
        .context("failed to send shutdown request")?;

    Ok(())
}

#[cfg(test)]
#[path = "restart_test.rs"]
mod tests;
