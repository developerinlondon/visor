//! Vsock agent listener for host↔guest JSON-RPC communication.
//!
//! Listens on `AF_VSOCK` port 52 for incoming connections from the host.
//! Each connection receives newline-delimited JSON-RPC 2.0 requests,
//! dispatches them to the appropriate handler, and returns responses.
//!
//! # Wire Protocol
//!
//! Each message is a JSON object on a single line, terminated by `\n`.
//! The host sends a request + `\n`, and the guest sends a response + `\n`.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::fd::{AsFd as _, AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::process::ExitStatusExt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Context as _;
use nix::poll::{PollFd, PollFlags, PollTimeout, poll};
use nix::pty::openpty;
use nix::sys::socket::{
    AddressFamily, Backlog, SockFlag, SockType, VsockAddr, accept, bind, listen, socket,
};

use crate::agent::{
    self, AgentMethod, ExecResult, INTERNAL_ERROR, JsonRpcResponse, METHOD_NOT_FOUND, PARSE_ERROR,
};
use crate::config::RunConfig;
use crate::overlay::BuildOverlay;

/// The well-known vsock port that the agent listens on.
const VSOCK_PORT: u32 = 52;

/// Listen on any CID (standard vsock constant, value `0xFFFFFFFF`).
const VMADDR_CID_ANY: u32 = u32::MAX;

/// Shared state for the vsock agent.
///
/// Holds the run configuration, optional build overlay, and child process
/// tracking. Protected by a mutex in the listener for thread safety.
pub struct AgentState {
    /// The run configuration received from the host.
    pub config: RunConfig,
    /// The build overlay, initialized lazily via `overlay_init`.
    pub overlay: Option<BuildOverlay>,
    /// PID of the currently running child process (for kill support).
    pub child_pid: Option<nix::unistd::Pid>,
    /// Whether a shutdown has been requested.
    pub shutdown_requested: bool,
}

impl AgentState {
    /// Create a new agent state with the given configuration.
    #[must_use]
    pub fn new(config: RunConfig) -> Self {
        Self {
            config,
            overlay: None,
            child_pid: None,
            shutdown_requested: false,
        }
    }
}

/// Start the vsock agent listener on port 52.
///
/// Creates an `AF_VSOCK` socket, binds to [`VMADDR_CID_ANY`]:[`VSOCK_PORT`],
/// and accepts connections in a loop. Each connection is handled in a
/// separate thread.
///
/// # Errors
///
/// Returns an error if socket creation, binding, or listening fails.
/// Individual connection errors are logged to stdout (serial console)
/// but do not terminate the listener.
pub fn start_listener(config: RunConfig) -> anyhow::Result<()> {
    let sock_fd = socket(
        AddressFamily::Vsock,
        SockType::Stream,
        SockFlag::SOCK_CLOEXEC,
        None,
    )
    .context("failed to create vsock socket")?;

    let addr = VsockAddr::new(VMADDR_CID_ANY, VSOCK_PORT);
    bind(sock_fd.as_raw_fd(), &addr).context("failed to bind vsock socket")?;

    let backlog = Backlog::new(4).context("invalid backlog value")?;
    listen(&sock_fd, backlog).context("failed to listen on vsock socket")?;

    println!("visor-init: agent listening on vsock port {VSOCK_PORT}");

    let state = Arc::new(Mutex::new(AgentState::new(config)));

    loop {
        match accept(sock_fd.as_raw_fd()) {
            Ok(client_raw_fd) => {
                let client_fd = raw_fd_to_owned(client_raw_fd);
                let state = Arc::clone(&state);
                std::thread::spawn(move || {
                    if let Err(e) = handle_connection(client_fd, &state) {
                        println!("visor-init: connection error: {e:?}");
                    }
                });
            }
            Err(e) => {
                println!("visor-init: accept error: {e}");
            }
        }
    }
}

/// Convert a raw file descriptor from `nix::sys::socket::accept()` into an
/// [`OwnedFd`] that manages its lifetime.
///
/// `nix` 0.31's `accept()` returns a bare `RawFd` (i32). This is the only
/// place in visor-init that requires unsafe code.
#[allow(unsafe_code)]
fn raw_fd_to_owned(raw: std::os::fd::RawFd) -> OwnedFd {
    // SAFETY: `accept()` returns a new, valid file descriptor on success.
    // We take exclusive ownership — no other code holds or closes this fd.
    unsafe { OwnedFd::from_raw_fd(raw) }
}

/// Handle a single client connection.
///
/// Reads newline-delimited JSON-RPC requests, dispatches each one,
/// and sends back the response followed by a newline.
///
/// # Errors
///
/// Returns an error if reading from or writing to the connection fails.
fn handle_connection(fd: OwnedFd, state: &Arc<Mutex<AgentState>>) -> anyhow::Result<()> {
    let file = std::fs::File::from(fd);
    let mut reader = BufReader::new(
        file.try_clone()
            .context("failed to clone file descriptor")?,
    );
    let mut writer = file;

    loop {
        let mut line = String::new();
        let bytes_read = reader
            .read_line(&mut line)
            .context("failed to read line from vsock connection")?;
        if bytes_read == 0 {
            break;
        }
        let line = line.trim_end_matches(['\n', '\r']);
        if line.is_empty() {
            continue;
        }

        let request = match agent::parse_request(line) {
            Ok(request) => request,
            Err(e) => {
                let resp = JsonRpcResponse::error(
                    serde_json::Value::Null,
                    PARSE_ERROR,
                    format!("parse error: {e}"),
                );
                write_response_line(&mut writer, &resp)?;
                continue;
            }
        };

        let id = request.id.clone();
        let method = match agent::dispatch_method(&request.method, request.params.as_ref()) {
            Ok(method) => method,
            Err(e) => {
                let resp = JsonRpcResponse::error(id, METHOD_NOT_FOUND, format!("{e}"));
                write_response_line(&mut writer, &resp)?;
                continue;
            }
        };

        if let AgentMethod::ExecStream(params) = method {
            let socket_reader = reader.into_inner();
            handle_exec_stream_connection(socket_reader, writer, state, &params, id)?;
            return Ok(());
        }

        let mut state_guard = state
            .lock()
            .map_err(|e| anyhow::anyhow!("mutex poisoned: {e}"))?;
        let response = handle_method(method, &mut state_guard, id);
        let should_exit = state_guard.shutdown_requested;
        drop(state_guard);

        write_response_line(&mut writer, &response)?;

        if should_exit {
            std::process::exit(0);
        }
    }

    Ok(())
}

fn write_response_line(
    writer: &mut std::fs::File,
    response: &JsonRpcResponse,
) -> anyhow::Result<()> {
    let json = response
        .to_json()
        .context("failed to serialize JSON-RPC response")?;
    writer
        .write_all(json.as_bytes())
        .context("failed to write response")?;
    writer
        .write_all(b"\n")
        .context("failed to write newline delimiter")?;
    writer.flush().context("failed to flush response")
}

fn handle_exec_stream_connection(
    socket_reader: std::fs::File,
    mut socket_writer: std::fs::File,
    state: &Arc<Mutex<AgentState>>,
    params: &agent::ExecParams,
    id: serde_json::Value,
) -> anyhow::Result<()> {
    if params.tty {
        return handle_exec_stream_tty_connection(socket_reader, socket_writer, state, params, id);
    }

    let parsed_env: Vec<(&str, &str)> = params
        .env
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .collect();

    let process_limit_enabled = state
        .lock()
        .map_err(|e| anyhow::anyhow!("mutex poisoned: {e}"))?
        .config
        .process_limit
        .is_some();
    let mut command = crate::entrypoint::workload_command(&params.cmd, process_limit_enabled)?;
    command
        .envs(parsed_env)
        .current_dir(&params.workdir)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());

    let mut child = command
        .spawn()
        .context("failed to execute streaming command")?;

    let child_pid_raw =
        i32::try_from(child.id()).context("streaming child pid did not fit in i32")?;
    {
        let mut state_guard = state
            .lock()
            .map_err(|e| anyhow::anyhow!("mutex poisoned: {e}"))?;
        state_guard.child_pid = Some(nix::unistd::Pid::from_raw(child_pid_raw));
    }

    let response = JsonRpcResponse::success(id, serde_json::json!("ok"));
    write_response_line(&mut socket_writer, &response)?;

    let child_stdin = child
        .stdin
        .take()
        .context("streaming child stdin was not piped")?;
    let child_stdout = child
        .stdout
        .take()
        .context("streaming child stdout was not piped")?;
    let shared_writer = Arc::new(Mutex::new(socket_writer));
    let stop_stdin = Arc::new(AtomicBool::new(false));
    let stop_stdin_for_thread = Arc::clone(&stop_stdin);

    let stdin_thread = std::thread::spawn(move || {
        forward_stream_input(
            socket_reader,
            child_stdin,
            stop_stdin_for_thread.as_ref(),
            false,
        )
    });
    let stdout_thread = {
        let writer = Arc::clone(&shared_writer);
        std::thread::spawn(move || {
            write_stream_output(child_stdout, &writer, StreamOutputMode::DockerFramed(1))
        })
    };
    let child_stderr = child
        .stderr
        .take()
        .context("streaming child stderr was not piped")?;
    let stderr_thread = {
        let writer = Arc::clone(&shared_writer);
        std::thread::spawn(move || {
            write_stream_output(child_stderr, &writer, StreamOutputMode::DockerFramed(2))
        })
    };

    child
        .wait()
        .context("failed to wait for streaming command")?;

    stdout_thread
        .join()
        .map_err(|_| anyhow::anyhow!("streaming stdout thread panicked"))?
        .context("failed to forward streaming stdout")?;
    stderr_thread
        .join()
        .map_err(|_| anyhow::anyhow!("streaming stderr thread panicked"))?
        .context("failed to forward streaming stderr")?;

    stop_stdin.store(true, Ordering::Release);

    stdin_thread
        .join()
        .map_err(|_| anyhow::anyhow!("streaming stdin thread panicked"))?
        .context("failed to forward streaming stdin")?;

    let mut state_guard = state
        .lock()
        .map_err(|e| anyhow::anyhow!("mutex poisoned: {e}"))?;
    state_guard.child_pid = None;

    Ok(())
}

fn handle_exec_stream_tty_connection(
    socket_reader: std::fs::File,
    mut socket_writer: std::fs::File,
    state: &Arc<Mutex<AgentState>>,
    params: &agent::ExecParams,
    id: serde_json::Value,
) -> anyhow::Result<()> {
    let parsed_env: Vec<(&str, &str)> = params
        .env
        .iter()
        .filter_map(|entry| entry.split_once('='))
        .collect();

    let pty = openpty(None, None).context("failed to allocate PTY for streaming command")?;
    let master = std::fs::File::from(pty.master);
    let slave = std::fs::File::from(pty.slave);
    let stdin_slave = slave
        .try_clone()
        .context("failed to clone PTY slave for stdin")?;
    let stdout_slave = slave
        .try_clone()
        .context("failed to clone PTY slave for stdout")?;

    let process_limit_enabled = state
        .lock()
        .map_err(|e| anyhow::anyhow!("mutex poisoned: {e}"))?
        .config
        .process_limit
        .is_some();
    let mut command = crate::entrypoint::workload_command(&params.cmd, process_limit_enabled)?;
    command
        .envs(parsed_env)
        .current_dir(&params.workdir)
        .stdin(std::process::Stdio::from(stdin_slave))
        .stdout(std::process::Stdio::from(stdout_slave))
        .stderr(std::process::Stdio::from(slave));

    let mut child = command
        .spawn()
        .context("failed to execute streaming command with PTY")?;

    let child_pid_raw =
        i32::try_from(child.id()).context("streaming child pid did not fit in i32")?;
    {
        let mut state_guard = state
            .lock()
            .map_err(|e| anyhow::anyhow!("mutex poisoned: {e}"))?;
        state_guard.child_pid = Some(nix::unistd::Pid::from_raw(child_pid_raw));
    }

    let response = JsonRpcResponse::success(id, serde_json::json!("ok"));
    write_response_line(&mut socket_writer, &response)?;

    let master_reader = master
        .try_clone()
        .context("failed to clone PTY master for output")?;
    let shared_writer = Arc::new(Mutex::new(socket_writer));
    let stop_stdin = Arc::new(AtomicBool::new(false));
    let stop_stdin_for_thread = Arc::clone(&stop_stdin);

    let stdin_thread = std::thread::spawn(move || {
        forward_stream_input(socket_reader, master, stop_stdin_for_thread.as_ref(), false)
    });
    let output_thread = {
        let writer = Arc::clone(&shared_writer);
        std::thread::spawn(move || {
            write_stream_output(master_reader, &writer, StreamOutputMode::Raw)
        })
    };

    child
        .wait()
        .context("failed to wait for streaming PTY command")?;

    output_thread
        .join()
        .map_err(|_| anyhow::anyhow!("streaming PTY output thread panicked"))?
        .context("failed to forward streaming PTY output")?;

    stop_stdin.store(true, Ordering::Release);

    stdin_thread
        .join()
        .map_err(|_| anyhow::anyhow!("streaming PTY stdin thread panicked"))?
        .context("failed to forward streaming PTY stdin")?;

    let mut state_guard = state
        .lock()
        .map_err(|e| anyhow::anyhow!("mutex poisoned: {e}"))?;
    state_guard.child_pid = None;

    Ok(())
}

fn forward_stream_input<W: Write>(
    mut socket_reader: std::fs::File,
    mut child_stdin: W,
    stop: &AtomicBool,
    log_preview: bool,
) -> anyhow::Result<()> {
    let mut buffer = [0u8; 8192];
    let mut logged = false;
    loop {
        if stop.load(Ordering::Acquire) {
            break;
        }
        let ready = poll(
            &mut [PollFd::new(socket_reader.as_fd(), PollFlags::POLLIN)],
            PollTimeout::from(10u16),
        )
        .context("poll streaming socket for input")?;
        if ready == 0 {
            continue;
        }

        let bytes_read = socket_reader
            .read(&mut buffer)
            .context("failed to read from streaming socket")?;
        if bytes_read == 0 {
            break;
        }
        if log_preview && !logged {
            eprintln!(
                "visor-init: streaming stdin preview: {:02x?}",
                &buffer[..bytes_read.min(32)]
            );
            logged = true;
        }
        if let Err(error) = child_stdin.write_all(&buffer[..bytes_read]) {
            if error.kind() == std::io::ErrorKind::BrokenPipe
                || error.raw_os_error() == Some(libc::EIO)
            {
                break;
            }
            return Err(error).context("failed to write to streaming child stdin");
        }
        if let Err(error) = child_stdin.flush() {
            if error.kind() == std::io::ErrorKind::BrokenPipe
                || error.raw_os_error() == Some(libc::EIO)
            {
                break;
            }
            return Err(error).context("failed to flush streaming child stdin");
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum StreamOutputMode {
    DockerFramed(u8),
    Raw,
}

fn write_stream_output<R: Read>(
    mut source: R,
    socket_writer: &Arc<Mutex<std::fs::File>>,
    mode: StreamOutputMode,
) -> anyhow::Result<()> {
    let mut buffer = [0u8; 8192];
    loop {
        let bytes_read = match source.read(&mut buffer) {
            Ok(bytes_read) => bytes_read,
            Err(error) if error.raw_os_error() == Some(libc::EIO) => break,
            Err(error) => return Err(error).context("failed to read from streaming child output"),
        };
        if bytes_read == 0 {
            break;
        }
        let mut writer = socket_writer
            .lock()
            .map_err(|e| anyhow::anyhow!("mutex poisoned: {e}"))?;
        match mode {
            StreamOutputMode::DockerFramed(stream_type) => {
                write_docker_stream_frame(&mut writer, stream_type, &buffer[..bytes_read])?;
            }
            StreamOutputMode::Raw => {
                writer
                    .write_all(&buffer[..bytes_read])
                    .context("failed to write raw streaming output")?;
                writer
                    .flush()
                    .context("failed to flush raw streaming output")?;
            }
        }
    }
    Ok(())
}

fn write_docker_stream_frame(
    writer: &mut std::fs::File,
    stream_type: u8,
    payload: &[u8],
) -> anyhow::Result<()> {
    let size = u32::try_from(payload.len())
        .context("streaming payload exceeded Docker frame size limit")?
        .to_be_bytes();
    let header = [stream_type, 0, 0, 0, size[0], size[1], size[2], size[3]];
    writer
        .write_all(&header)
        .context("failed to write Docker stream header")?;
    writer
        .write_all(payload)
        .context("failed to write Docker stream payload")?;
    writer
        .flush()
        .context("failed to flush Docker stream payload")
}

/// Process a single JSON-RPC request line and return the serialized response.
///
/// This is the unit-testable core of the listener. Parses the JSON line,
/// dispatches to the appropriate method handler, and serializes the response.
///
/// Always returns a valid JSON string — errors are encoded as JSON-RPC
/// error responses, never panics.
pub fn handle_request_line(line: &str, state: &mut AgentState) -> String {
    // Parse the JSON-RPC request
    let request = match agent::parse_request(line) {
        Ok(req) => req,
        Err(e) => {
            let resp = JsonRpcResponse::error(
                serde_json::Value::Null,
                PARSE_ERROR,
                format!("parse error: {e}"),
            );
            return resp.to_json().unwrap_or_else(|_| {
                r#"{"jsonrpc":"2.0","error":{"code":-32700,"message":"parse error"},"id":null}"#
                    .to_owned()
            });
        }
    };

    let id = request.id.clone();

    // Dispatch the method
    let method = match agent::dispatch_method(&request.method, request.params.as_ref()) {
        Ok(m) => m,
        Err(e) => {
            let resp = JsonRpcResponse::error(id, METHOD_NOT_FOUND, format!("{e}"));
            return resp.to_json().unwrap_or_else(|_| {
                r#"{"jsonrpc":"2.0","error":{"code":-32601,"message":"unknown method"},"id":null}"#
                    .to_owned()
            });
        }
    };

    // Execute the handler
    let resp = handle_method(method, state, id);
    resp.to_json().unwrap_or_else(|_| {
        r#"{"jsonrpc":"2.0","error":{"code":-32603,"message":"serialization error"},"id":null}"#
            .to_owned()
    })
}

/// Execute a dispatched method and return the JSON-RPC response.
///
/// Delegates to the appropriate handler based on the [`AgentMethod`] variant,
/// converting any handler errors into JSON-RPC error responses.
pub fn handle_method(
    method: AgentMethod,
    state: &mut AgentState,
    id: serde_json::Value,
) -> JsonRpcResponse {
    match method {
        AgentMethod::Ping => agent::ping_response(id),
        AgentMethod::Exec(params) => {
            match execute_command(&params, state.config.process_limit.is_some()) {
                Ok(result) => agent::exec_response(id.clone(), &result)
                    .unwrap_or_else(|e| JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}"))),
                Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}")),
            }
        }
        AgentMethod::ExecStream(_) => JsonRpcResponse::error(
            id,
            INTERNAL_ERROR,
            "exec_stream requires a dedicated streaming connection",
        ),
        AgentMethod::Kill(params) => match kill_process(state, params.signal) {
            Ok(()) => agent::kill_response(id),
            Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}")),
        },
        AgentMethod::GetConfig => agent::get_config_response(id.clone(), &state.config)
            .unwrap_or_else(|e| JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}"))),
        AgentMethod::Shutdown => {
            state.shutdown_requested = true;
            agent::shutdown_response(id)
        }
        AgentMethod::OverlayInit(params) => {
            let lower = params.lower_dir.as_deref().unwrap_or("/");
            match BuildOverlay::init(lower) {
                Ok(overlay) => {
                    state.overlay = Some(overlay);
                    agent::overlay_init_response(id)
                }
                Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}")),
            }
        }
        AgentMethod::SnapshotLayer => match &mut state.overlay {
            Some(overlay) => match overlay.snapshot_layer() {
                Ok(result) => agent::snapshot_layer_response(id.clone(), &result)
                    .unwrap_or_else(|e| JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}"))),
                Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}")),
            },
            None => JsonRpcResponse::error(id, INTERNAL_ERROR, "overlay not initialized"),
        },
        AgentMethod::FlattenOverlay => match &mut state.overlay {
            Some(overlay) => match overlay.flatten() {
                Ok(()) => agent::flatten_overlay_response(id),
                Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}")),
            },
            None => JsonRpcResponse::error(id, INTERNAL_ERROR, "overlay not initialized"),
        },
        AgentMethod::CopyFiles(params) => match copy_files_to_guest(&params) {
            Ok(result) => agent::copy_files_response(id.clone(), &result)
                .unwrap_or_else(|e| JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}"))),
            Err(e) => JsonRpcResponse::error(id, INTERNAL_ERROR, format!("{e}")),
        },
    }
}

/// Execute a command and capture its output.
///
/// Spawns the command with stdout and stderr piped, waits for completion,
/// and returns the exit code and captured output.
///
/// # Errors
///
/// Returns an error if the command cannot be spawned.
fn execute_command(
    params: &agent::ExecParams,
    process_limit_enabled: bool,
) -> anyhow::Result<ExecResult> {
    let parsed_env: Vec<(&str, &str)> = params
        .env
        .iter()
        .filter_map(|e| e.split_once('='))
        .collect();

    let output = crate::entrypoint::workload_command(&params.cmd, process_limit_enabled)?
        .envs(parsed_env)
        .current_dir(&params.workdir)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .output()
        .context("failed to execute command")?;

    let exit_code = output
        .status
        .code()
        .unwrap_or_else(|| output.status.signal().map_or(-1, |sig| 128 + sig));

    Ok(ExecResult {
        exit_code,
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Send a signal to the tracked child process.
///
/// # Errors
///
/// Returns an error if no child process is tracked or the signal cannot be sent.
fn kill_process(state: &AgentState, signal: i32) -> anyhow::Result<()> {
    let pid = state.child_pid.context("no child process to kill")?;
    let sig = nix::sys::signal::Signal::try_from(signal).context("invalid signal number")?;
    nix::sys::signal::kill(pid, sig).context("failed to send signal to process")
}

/// Extract a base64-encoded tar.gz archive into a destination directory.
///
/// Decodes the base64 data, decompresses gzip, and extracts the tar archive
/// into `params.dest`, creating directories as needed.
///
/// # Errors
///
/// Returns an error if base64 decoding, gzip decompression, or tar extraction fails.
fn copy_files_to_guest(params: &agent::CopyFilesParams) -> anyhow::Result<agent::CopyFilesResult> {
    use base64::Engine as _;

    // 1. Decode base64
    let compressed = base64::engine::general_purpose::STANDARD
        .decode(&params.data)
        .context("failed to decode base64 data")?;

    // 2. Decompress gzip
    let decoder = flate2::read::GzDecoder::new(&compressed[..]);

    // 3. Extract tar to dest
    let dest = std::path::Path::new(&params.dest);
    std::fs::create_dir_all(dest).context("failed to create destination directory")?;

    let mut archive = tar::Archive::new(decoder);
    let mut files_written: u64 = 0;
    for entry_result in archive.entries().context("failed to read tar entries")? {
        let mut entry = entry_result.context("failed to read tar entry")?;
        let entry_path = entry
            .path()
            .context("failed to read tar entry path")?
            .into_owned();
        let entry_type = entry.header().entry_type();
        entry
            .unpack_in(dest)
            .context("failed to extract tar entry")?;
        rewrite_loopback_only_resolv_conf(dest, &entry_path)
            .context("failed to sanitize copied resolv.conf")?;
        // Count regular files (not directories)
        if entry_type.is_file() {
            files_written += 1;
        }
    }

    Ok(agent::CopyFilesResult { files_written })
}

fn rewrite_loopback_only_resolv_conf(
    dest: &std::path::Path,
    entry_path: &std::path::Path,
) -> anyhow::Result<()> {
    if entry_path.file_name().and_then(|name| name.to_str()) != Some("resolv.conf") {
        return Ok(());
    }

    let resolv_path = dest.join(entry_path);
    let needs_rewrite = match std::fs::read_to_string(&resolv_path) {
        Ok(contents) => resolv_conf_uses_only_loopback_nameservers(&contents),
        Err(_) => true,
    };
    if !needs_rewrite {
        return Ok(());
    }

    write_fallback_resolv_conf(&resolv_path)?;
    Ok(())
}

fn resolv_conf_uses_only_loopback_nameservers(contents: &str) -> bool {
    let mut saw_nameserver = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some(server) = trimmed
            .strip_prefix("nameserver")
            .and_then(|value| value.split_whitespace().next())
        else {
            continue;
        };

        let Ok(address) = server.parse::<std::net::IpAddr>() else {
            return false;
        };
        saw_nameserver = true;
        if !address.is_loopback() {
            return false;
        }
    }

    saw_nameserver
}

fn fallback_resolv_conf_contents() -> &'static str {
    "nameserver 1.1.1.1\nnameserver 8.8.8.8\n"
}

fn write_fallback_resolv_conf(resolv_path: &std::path::Path) -> anyhow::Result<()> {
    match std::fs::remove_file(resolv_path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(error) => {
            return Err(error).with_context(|| {
                format!(
                    "failed to remove copied resolv.conf at {}",
                    resolv_path.display()
                )
            });
        }
    }

    std::fs::write(resolv_path, fallback_resolv_conf_contents()).with_context(|| {
        format!(
            "failed to rewrite loopback-only resolv.conf at {}",
            resolv_path.display()
        )
    })?;
    Ok(())
}

#[cfg(test)]
#[path = "listener_test.rs"]
mod tests;
