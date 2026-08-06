//! Process execution, signal forwarding, and zombie reaping.
//!
//! Spawns the user's command, forwards signals, and reaps child processes
//! as PID 1 responsibilities require.

use std::thread;
use std::time::Duration;

use anyhow::Context;
use nix::sys::signal::{self, SigSet, SigmaskHow, Signal};
use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};
use nix::unistd::Pid;

use crate::config::RunConfig;

/// Default PATH used for guest command execution when none is provided.
const DEFAULT_PATH: &str = "/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin";

/// Parameters for spawning the user's command.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExecParams {
    /// Command and arguments to execute.
    pub cmd: Vec<String>,
    /// Environment variables as `KEY=VALUE` pairs.
    pub env: Vec<String>,
    /// Working directory for the command.
    pub workdir: String,
}

/// Result of a child process execution.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExecResult {
    /// Exit code of the child process.
    pub exit_code: i32,
}

impl ExecParams {
    /// Extracts execution parameters from a [`RunConfig`].
    #[must_use]
    pub fn from_config(config: &RunConfig) -> Self {
        Self {
            cmd: config.cmd.clone(),
            env: config.env.clone(),
            workdir: config.workdir.clone(),
        }
    }
}

/// Spawns the user's command as a child process.
///
/// Uses `std::process::Command` to create the child with the specified
/// command, environment variables, and working directory.
///
/// # Errors
///
/// Returns an error if the command vector is empty, the binary cannot be
/// found, or the process fails to start.
pub fn spawn_child(params: &ExecParams) -> anyhow::Result<Pid> {
    anyhow::ensure!(!params.cmd.is_empty(), "command must not be empty");

    let mut command = std::process::Command::new(&params.cmd[0]);
    command.args(&params.cmd[1..]);

    command.env_clear();
    command.envs(build_command_env(&params.env));
    // Create workdir if it doesn't exist (matches Docker behavior).
    let workdir = std::path::Path::new(&params.workdir);
    if !workdir.exists() {
        std::fs::create_dir_all(workdir).context("failed to create working directory")?;
    }
    command.current_dir(workdir);

    let child = command.spawn().context("failed to spawn child process")?;
    let raw_pid = child.id();

    // We manage the child lifecycle via waitpid(2) directly (PID 1 duty),
    // not through Rust's Child API. Forgetting prevents implicit wait on drop.
    std::mem::forget(child);

    let pid = Pid::from_raw(i32::try_from(raw_pid).context("child PID exceeds i32 range")?);
    Ok(pid)
}

/// Builds the environment for a guest command.
///
/// Preserves explicitly provided variables and injects a container-style
/// default `PATH` when one is absent so bare commands like `sh` or `echo`
/// resolve correctly inside minimal images.
fn build_command_env(env: &[String]) -> Vec<(String, String)> {
    let mut parsed_env: Vec<(String, String)> = env
        .iter()
        .filter_map(|entry| {
            let (key, value) = entry.split_once('=')?;
            Some((key.to_owned(), value.to_owned()))
        })
        .collect();

    if !parsed_env.iter().any(|(key, _)| key == "PATH") {
        parsed_env.push(("PATH".to_owned(), DEFAULT_PATH.to_owned()));
    }

    parsed_env
}

/// Waits for a specific child process to exit.
///
/// Loops with non-blocking `waitpid` until the target child exits.
/// The caller should also call [`reap_zombies`] periodically in the
/// main init loop to fulfill PID 1 reaping duties.
///
/// # Errors
///
/// Returns an error if `waitpid` fails with an unexpected error.
pub fn wait_for_child(pid: Pid) -> anyhow::Result<ExecResult> {
    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::Exited(_, code)) => {
                return Ok(ExecResult { exit_code: code });
            }
            Ok(WaitStatus::Signaled(_, sig, _)) => {
                return Ok(ExecResult {
                    exit_code: 128 + sig as i32,
                });
            }
            Ok(WaitStatus::StillAlive | _) => {
                // Child still running or stopped/continued — keep waiting
            }
            Err(nix::errno::Errno::ECHILD) => {
                return Ok(ExecResult { exit_code: -1 });
            }
            Err(e) => {
                return Err(e).context("waitpid failed for child process");
            }
        }

        thread::sleep(Duration::from_millis(10));
    }
}

/// Reaps zombie child processes in a non-blocking loop.
///
/// Calls `waitpid(-1, WNOHANG)` repeatedly until no more zombies remain.
/// This is a PID 1 responsibility — without reaping, orphaned child processes
/// accumulate as zombies in the process table.
///
/// # Returns
///
/// The number of zombie processes reaped.
#[must_use]
pub fn reap_zombies() -> usize {
    let mut count = 0;
    loop {
        match waitpid(Pid::from_raw(-1), Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) | Err(_) => break,
            Ok(_) => count += 1,
        }
    }
    count
}

/// Forwards a signal to a child process.
///
/// # Errors
///
/// Returns an error if the signal cannot be delivered (e.g., the process
/// does not exist).
pub fn forward_signal(pid: Pid, signal: Signal) -> anyhow::Result<()> {
    signal::kill(pid, signal).context("failed to forward signal to child process")
}

/// Sets up signal handling for PID 1 duties.
///
/// Masks `SIGCHLD` for explicit handling via `waitpid`, and blocks
/// `SIGTERM`, `SIGINT`, `SIGUSR1`, and `SIGUSR2` for explicit forwarding
/// to the child process in the main loop.
///
/// # Errors
///
/// Returns an error if the signal mask cannot be applied.
pub fn setup_signal_handlers() -> anyhow::Result<()> {
    let mut mask = SigSet::empty();
    mask.add(Signal::SIGCHLD);
    mask.add(Signal::SIGTERM);
    mask.add(Signal::SIGINT);
    mask.add(Signal::SIGUSR1);
    mask.add(Signal::SIGUSR2);

    signal::sigprocmask(SigmaskHow::SIG_BLOCK, Some(&mask), None)
        .context("failed to set up signal mask")?;

    Ok(())
}

#[cfg(test)]
#[path = "entrypoint_test.rs"]
mod tests;
