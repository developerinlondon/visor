//! `visor start` — launch the daemon.
//!
//! By default the daemon forks to the background and the parent exits
//! immediately after confirming the child is listening. Pass `--foreground`
//! to keep the process in the terminal.

use std::os::unix::process::CommandExt as _;

use anyhow::Context;

use super::StartArgs;
use crate::daemon::DaemonConfig;

/// Returns the daemon log path under the persistent Visor home.
fn log_path() -> anyhow::Result<std::path::PathBuf> {
    let path = crate::paths::daemon_log_path().context("determine daemon log path")?;
    let dir = path
        .parent()
        .context("daemon log path must have a parent directory")?;
    std::fs::create_dir_all(dir)
        .with_context(|| format!("create daemon log directory {}", dir.display()))?;
    Ok(path)
}

/// Checks whether a visor daemon is already listening on the given address.
///
/// Sends a GET request to the health endpoint with a 2-second timeout.
/// Returns `true` if any response is received.
pub(crate) async fn is_daemon_running(addr: &str) -> bool {
    let url = format!("http://{addr}/v1/health");
    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
    else {
        return false;
    };
    client.get(&url).send().await.is_ok()
}

/// Executes the `visor start` subcommand.
///
/// Without `--foreground`, re-execs the current binary with `--foreground`
/// as a detached child process in its own process group. The parent waits
/// briefly to confirm the daemon is listening, prints the PID, and exits.
///
/// With `--foreground`, runs the daemon in the current process (blocks).
///
/// # Errors
///
/// Returns an error if the daemon fails to start or the background spawn fails.
pub async fn execute(args: StartArgs) -> anyhow::Result<()> {
    if is_daemon_running(&args.listen).await {
        println!("visor daemon is already running on {}", args.listen);
        println!();
        println!("To restart:  visor restart");
        println!("To stop:     visor stop");
        return Ok(());
    }

    if args.foreground {
        let config = DaemonConfig {
            listen_addr: args.listen,
        };
        return crate::daemon::run_daemon(config).await;
    }

    // Background mode: re-exec ourselves with --foreground as a detached child.
    let exe = std::env::current_exe().context("failed to resolve current executable")?;

    let log_path = log_path()?;
    let log_file = std::fs::File::create(&log_path).context(format!(
        "failed to create log file at {}",
        log_path.display()
    ))?;
    let stderr_file = log_file
        .try_clone()
        .context("failed to clone log file handle")?;

    let child = std::process::Command::new(&exe)
        .args(["start", "--foreground", "--listen", &args.listen])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::from(log_file))
        .stderr(std::process::Stdio::from(stderr_file))
        .process_group(0) // Detach into its own process group
        .spawn()
        .context("failed to spawn background daemon process")?;

    let pid = child.id();

    // Wait briefly for the daemon to bind its port before reporting success.
    let addr = args.listen.clone();
    let ready =
        tokio::time::timeout(std::time::Duration::from_secs(5), wait_for_listen(&addr)).await;

    if let Ok(Ok(())) = ready {
        let display_addr = addr.replace("0.0.0.0", "127.0.0.1");
        println!("visor daemon started (pid {pid}) on {addr}");
        println!("Swagger UI: http://{display_addr}/docs");
        println!("Logs: {}", log_path.display());
    } else {
        eprintln!("visor daemon spawned (pid {pid}) but did not become ready within 5s");
        eprintln!("Check logs: {}", log_path.display());
    }

    Ok(())
}

/// Polls the daemon's health endpoint until it responds.
async fn wait_for_listen(addr: &str) -> anyhow::Result<()> {
    let url = format!("http://{addr}/v1/health");
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .build()
        .context("failed to build HTTP client")?;

    for _ in 0..25 {
        if client.get(&url).send().await.is_ok() {
            return Ok(());
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }

    anyhow::bail!("daemon did not become ready within timeout")
}

#[cfg(test)]
#[path = "start_test.rs"]
mod tests;
