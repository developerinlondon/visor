//! CLI definitions and subcommand implementations.
//!
//! Uses `clap` derive API for argument parsing. Each subcommand communicates
//! with the visor daemon via HTTP to the configured address.

use anyhow::Context;
use clap::{Parser, Subcommand};

pub mod build;
pub mod compose;
pub mod console;
pub mod exec;
pub mod images;
pub mod info;
pub mod inspect;
pub mod kill;
pub mod logs;
pub mod network;
pub mod ps;
pub mod pull;
pub mod push;
pub mod restart;
pub mod rm;
pub mod rmi;
pub mod run;
pub mod service;
pub mod shell;
pub mod start;
pub mod stop;
pub mod top;
pub mod tui;
pub mod volume;

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

/// Top-level CLI for the visor container runtime.
///
/// Parses command-line arguments and dispatches to the appropriate subcommand.
/// All subcommands except `start` communicate with a running visor daemon via
/// HTTP at the configured address.
#[derive(Parser, Debug)]
#[command(name = "visor", version, about = "Run OCI containers as microVMs")]
#[non_exhaustive]
pub struct Cli {
    /// Daemon address.
    #[arg(long, default_value = "http://127.0.0.1:7800", global = true)]
    pub addr: String,

    /// Subcommand to execute.
    #[command(subcommand)]
    pub command: Command,
}

/// Available CLI subcommands.
#[derive(Subcommand, Debug)]
#[non_exhaustive]
pub enum Command {
    /// Start the visor daemon.
    Start(StartArgs),
    /// Run a command in a new VM.
    Run(RunArgs),
    /// Execute a command in a running VM.
    Exec(ExecArgs),
    /// List running VMs.
    Ps,
    /// Stop a running VM, or stop the daemon if no VM ID is given.
    Stop(StopArgs),
    /// Open a shell in a VM.
    Shell(ShellArgs),
    /// Show daemon and host information.
    Info,
    /// Launch the terminal dashboard.
    Tui,
    /// Manage persistent volumes.
    #[command(subcommand)]
    Volume(volume::VolumeCommand),
    /// Manage system service installation.
    #[command(subcommand)]
    Service(service::ServiceCommand),
    /// List cached OCI images.
    Images(images::ImagesArgs),
    /// Show guest processes in a VM.
    Top(top::TopArgs),
    /// Attach to a VM serial console.
    Console(console::ConsoleArgs),
    /// Manage multi-service compose deployments.
    #[command(subcommand)]
    Compose(compose::ComposeCommand),
    /// Manage virtual networks.
    #[command(subcommand)]
    Network(network::NetworkCommand),
    /// Remove one or more VMs.
    Rm(RmArgs),
    /// View stdout/stderr from a VM.
    Logs(LogsArgs),
    /// Show detailed VM information as JSON.
    Inspect(InspectArgs),
    /// Force-kill a running VM.
    Kill(KillArgs),
    /// Download an image from a registry.
    Pull(PullArgs),
    /// Remove one or more cached images.
    Rmi(RmiArgs),
    /// Stop and restart the daemon.
    Restart(RestartArgs),
    /// Build an image from a Dockerfile.
    Build(BuildArgs),
    /// Push an image to a registry.
    Push(PushArgs),
    /// Internal: run as a VM worker process (hidden from help).
    #[command(hide = true)]
    VmWorker(VmWorkerArgs),
}

/// Arguments for the `visor start` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct StartArgs {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:7800")]
    pub listen: String,
    /// Run in foreground (don't daemonize).
    #[arg(long)]
    pub foreground: bool,
}

/// Arguments for the `visor run` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct RunArgs {
    /// OCI image reference (e.g., `alpine:3.20`).
    pub image: String,
    /// Command to run inside the VM.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cmd: Vec<String>,
    /// Environment variables (`KEY=VALUE`).
    #[arg(short, long)]
    pub env: Vec<String>,
    /// Memory in MiB.
    #[arg(short, long, default_value = "512")]
    pub memory: u32,
    /// Number of vCPUs.
    #[arg(long, default_value = "1")]
    pub cpus: u32,
    /// VM name.
    #[arg(long)]
    pub name: Option<String>,
    /// Port mapping (`host:guest`).
    #[arg(short, long)]
    pub port: Vec<String>,
    /// Enable guest networking and DNS inside the VM.
    ///
    /// Networking is enabled by default; this flag is accepted for explicitness.
    #[arg(long, conflicts_with = "no_network")]
    pub network: bool,
    /// Disable guest networking and DNS inside the VM.
    #[arg(long, conflicts_with = "network")]
    pub no_network: bool,
    /// Run in detached mode (return VM ID immediately).
    #[arg(short, long)]
    pub detach: bool,
    /// Expose nested virtualization to the guest for builder workloads.
    #[arg(long)]
    pub nested_virt: bool,
    /// Working directory inside VM.
    #[arg(short, long)]
    pub workdir: Option<String>,
    /// Volume mounts (`host:guest[:ro]`).
    #[arg(short = 'v', long = "volume")]
    pub volume: Vec<String>,
}

/// Arguments for the `visor exec` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct ExecArgs {
    /// VM ID.
    pub vm_id: String,
    /// Command to execute.
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub cmd: Vec<String>,
    /// Environment variables (`KEY=VALUE`).
    #[arg(short, long)]
    pub env: Vec<String>,
    /// Working directory inside the VM.
    #[arg(short, long)]
    pub workdir: Option<String>,
}

/// Arguments for the `visor stop` subcommand.
///
/// If no VM ID is given, stops the daemon instead.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct StopArgs {
    /// VM ID to stop. Omit to stop the daemon.
    pub vm_id: Option<String>,

    /// Seconds to wait for graceful shutdown before force-killing (default: 10).
    #[arg(short = 't', long = "time", default_value_t = 10)]
    pub time: u64,
}

/// Arguments for the `visor shell` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct ShellArgs {
    /// VM ID.
    pub vm_id: String,
}

/// Arguments for the `visor rm` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct RmArgs {
    /// One or more VM IDs to remove.
    #[arg(required = true)]
    pub vm_ids: Vec<String>,
}

/// Arguments for the `visor logs` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct LogsArgs {
    /// VM ID.
    pub vm_id: String,
}

/// Arguments for the `visor inspect` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct InspectArgs {
    /// VM ID.
    pub vm_id: String,
}

/// Arguments for the `visor kill` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct KillArgs {
    /// VM ID.
    pub vm_id: String,
}

/// Arguments for the `visor pull` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct PullArgs {
    /// Image reference to pull (e.g., `alpine:latest`).
    pub image: String,
}

/// Arguments for the `visor rmi` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct RmiArgs {
    /// One or more image references to remove.
    #[arg(required = true)]
    pub images: Vec<String>,
}

/// Arguments for the `visor restart` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct RestartArgs {
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0:7800")]
    pub listen: String,
}

/// Arguments for the `visor build` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct BuildArgs {
    /// Build context directory (default: current directory).
    #[arg(default_value = ".")]
    pub context: String,

    /// Tag for the built image (e.g. `myapp:latest`).
    #[arg(short, long)]
    pub tag: Option<String>,

    /// Path to Dockerfile within context.
    #[arg(short, long, default_value = "Dockerfile")]
    pub file: String,

    /// Build arguments (`KEY=VALUE`).
    #[arg(long = "build-arg")]
    pub build_arg: Vec<String>,

    /// Target build stage.
    #[arg(long)]
    pub target: Option<String>,

    /// Disable build cache.
    #[arg(long)]
    pub no_cache: bool,

    /// Suppress build output.
    #[arg(short, long)]
    pub quiet: bool,
}

/// Arguments for the `visor push` subcommand.
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct PushArgs {
    /// Image tag to push (e.g. `myapp:latest`).
    pub tag: String,
}

/// Parses a port mapping string in `host:guest` format.
///
/// # Errors
///
/// Returns an error if the string is not in `host:guest` format or if either
/// port number is invalid.
pub fn parse_port_mapping(s: &str) -> anyhow::Result<crate::backend::PortMapping> {
    let parts: Vec<&str> = s.split(':').collect();
    anyhow::ensure!(
        parts.len() == 2,
        "port mapping must be host:guest, got '{s}'"
    );
    let host_port: u16 = parts[0].parse().context("invalid host port")?;
    let guest_port: u16 = parts[1].parse().context("invalid guest port")?;
    Ok(crate::backend::PortMapping::new(host_port, guest_port))
}

/// Parses a volume mount string in `host:guest` or `host:guest:ro` format.
///
/// # Errors
///
/// Returns an error if the string is not in a valid volume mount format,
/// if either path is empty, or if the guest path is not absolute.
pub fn parse_volume_mount(s: &str) -> anyhow::Result<crate::backend::VolumeMount> {
    anyhow::ensure!(!s.is_empty(), "volume mount must not be empty");
    let parts: Vec<&str> = s.splitn(3, ':').collect();
    anyhow::ensure!(
        parts.len() >= 2,
        "volume mount must be host:guest[:ro], got '{s}'"
    );
    let host_path = parts[0];
    let guest_path = parts[1];
    anyhow::ensure!(
        !host_path.is_empty(),
        "host path must not be empty in '{s}'"
    );
    anyhow::ensure!(
        !guest_path.is_empty(),
        "guest path must not be empty in '{s}'"
    );
    anyhow::ensure!(
        guest_path.starts_with('/'),
        "guest path must be absolute (start with '/'), got '{guest_path}'"
    );
    let read_only = parts.get(2).is_some_and(|opt| *opt == "ro");
    if read_only {
        Ok(crate::backend::VolumeMount::read_only(
            host_path, guest_path,
        ))
    } else {
        Ok(crate::backend::VolumeMount::new(host_path, guest_path))
    }
}

/// Creates a `reqwest` client configured for the visor daemon.
///
/// # Errors
///
/// Returns an error if the HTTP client cannot be constructed.
pub fn http_client() -> anyhow::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()
        .context("failed to create HTTP client")
}

/// Fetches VM info from the daemon by ID or name.
///
/// # Errors
///
/// Returns an error if the daemon cannot be reached, the VM is not found, or
/// the response body cannot be parsed as [`crate::backend::VmInfo`].
pub(crate) async fn fetch_vm_info(
    addr: &str,
    vm_ref: &str,
) -> anyhow::Result<crate::backend::VmInfo> {
    let client = http_client()?;
    let url = format!("{addr}/v1/vms/{vm_ref}");
    let resp = client
        .get(&url)
        .send()
        .await
        .context("failed to connect to visor daemon")?;

    if !resp.status().is_success() {
        let msg = daemon_error_message(resp).await?;
        anyhow::bail!("vm not found: {msg}");
    }

    resp.json().await.context("failed to parse VM info")
}

/// Ensures an interactive surface is only opened for a running VM.
///
/// # Errors
///
/// Returns an error if the VM is not in the running state.
pub(crate) fn ensure_interactive_vm_running(
    vm: &crate::backend::VmInfo,
    vm_ref: &str,
    surface: &str,
) -> anyhow::Result<()> {
    match vm.state {
        crate::backend::VmState::Running => Ok(()),
        crate::backend::VmState::Creating => {
            anyhow::bail!(
                "cannot open {surface}: VM '{vm_ref}' is still creating; wait for it to start and try again"
            )
        }
        state @ (crate::backend::VmState::Stopped | crate::backend::VmState::Failed) => {
            anyhow::bail!(
                "cannot open {surface}: VM '{vm_ref}' is {}; run `visor start {vm_ref}` first",
                format!("{state:?}").to_ascii_lowercase()
            )
        }
        state => {
            anyhow::bail!(
                "cannot open {surface}: VM '{vm_ref}' is {}; wait for it to become running and try again",
                format!("{state:?}").to_ascii_lowercase()
            )
        }
    }
}

async fn daemon_error_message(resp: reqwest::Response) -> anyhow::Result<String> {
    let body: serde_json::Value = resp
        .json()
        .await
        .context("failed to parse daemon error response")?;
    Ok(body
        .get("error")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown error")
        .to_owned())
}

/// Arguments for the `visor vm-worker` subcommand (internal).
#[derive(clap::Args, Debug)]
#[non_exhaustive]
pub struct VmWorkerArgs {
    /// Run in pool mode (wait for VM assignment instead of reading config from stdin).
    #[arg(long)]
    pub pool: bool,
    /// Socket path for pool mode (path to connect to for control socket).
    #[arg(long)]
    pub socket_path: Option<std::path::PathBuf>,
}

/// Execute the VM worker entry point (internal, hidden from help).
///
/// # Errors
///
/// Returns an error if the worker fails to start or encounters a fatal error.
pub async fn execute_vm_worker(args: VmWorkerArgs) -> anyhow::Result<()> {
    crate::daemon::init_tracing();
    if args.pool {
        let socket_path = args
            .socket_path
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("--pool requires --socket-path"))?;
        crate::lifecycle::vm_worker::run_pool_worker(socket_path).await
    } else {
        crate::lifecycle::vm_worker::run_worker().await
    }
}
