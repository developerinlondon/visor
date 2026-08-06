//! Terminal dashboard for the visor daemon.
//!
//! Provides a live-updating TUI accessible via `visor tui` that shows VMs,
//! warm-pool metrics, selected-VM logs, and lifecycle events from the running
//! daemon's HTTP API.
//! Built with `ratatui` and `crossterm`.
//!
//! # Architecture
//!
//! ```text
//! ┌─────────────┐      ┌──────────┐      ┌────────────┐
//! │ HTTP API    │─poll─▶│ App state│─draw─▶│ ratatui    │
//! │ /v1/vms     │      │ machine  │      │ terminal   │
//! │ /v1/pool    │      └──────────┘      └────────────┘
//! │ /v1/events  │─SSE──────────────────────────────────▶
//! └─────────────┘           ▲
//!                           │ actions
//!                      ┌────┴─────┐
//!                      │ keyboard │
//!                      │ input    │
//!                      └──────────┘
//! ```
//!
//! # Modules
//!
//! - [`app`] — State machine, actions, event buffer, selected-VM logs.
//! - [`views`] — Rendering functions for each view.

pub mod app;
pub mod views;

use std::time::Duration;

use anyhow::Context;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};

use crate::api::sse::VmEvent;
use crate::backend::VmInfo;
use crate::pool::manager::PoolStatus;

use self::app::{Action, App, PendingAction, View};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum VmSurface {
    Shell,
    Console,
}

/// Default polling interval for API data refresh.
const POLL_INTERVAL: Duration = Duration::from_secs(2);
const EVENT_RECONNECT_DELAY: Duration = Duration::from_secs(1);

struct EventListener {
    stop_tx: tokio::sync::watch::Sender<bool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl EventListener {
    fn stop(mut self) {
        let _ = self.stop_tx.send(true);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Runs the TUI event loop.
///
/// Initialises the terminal, polls the daemon API for VM and event data,
/// and renders the dashboard until the user quits.
///
/// # Errors
///
/// Returns an error if terminal initialisation fails, API polling fails
/// permanently, or rendering encounters an I/O error.
pub fn run(addr: String) -> anyhow::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, addr);
    ratatui::restore();
    result
}

/// Core event loop: polls input and refreshes data on a timer.
fn event_loop(terminal: &mut ratatui::DefaultTerminal, addr: String) -> anyhow::Result<()> {
    let mut app = App::new(addr);
    let client = crate::cli::http_client().context("create HTTP client for TUI")?;
    let (event_rx, event_listener) = spawn_event_listener(app.addr().to_owned());
    let mut last_poll = std::time::Instant::now()
        .checked_sub(POLL_INTERVAL)
        .unwrap_or_else(std::time::Instant::now);
    let result = loop_until_quit(terminal, &client, &event_rx, &mut app, &mut last_poll);
    event_listener.stop();
    result
}

fn loop_until_quit(
    terminal: &mut ratatui::DefaultTerminal,
    client: &reqwest::Client,
    event_rx: &std::sync::mpsc::Receiver<app::TuiEvent>,
    app: &mut App,
    last_poll: &mut std::time::Instant,
) -> anyhow::Result<()> {
    loop {
        drain_event_receiver(app, event_rx);

        // Poll API data on interval.
        if last_poll.elapsed() >= POLL_INTERVAL {
            if let Ok(vms) = poll_vms(client, app.addr()) {
                app.set_vms(vms);
            }
            if let Ok((warm, target)) = poll_pool_status(client, app.addr()) {
                app.set_pool_status(warm, target);
            }
            if app.current_view() == View::Logs {
                let _ignore = refresh_logs_view(client, app);
            }
            *last_poll = std::time::Instant::now();
        }

        // Draw.
        terminal
            .draw(|frame| {
                match app.current_view() {
                    View::Dashboard => views::dashboard::render(frame, app),
                    View::VmDetail => views::vm_detail::render(frame, app),
                    View::Logs => render_logs(frame, app),
                }
                // Render confirmation overlay on top if a pending action exists.
                // Render overlays on top of the current view.
                if app.has_pending_action() {
                    views::confirm::render(frame, app);
                } else if app.has_create_form() {
                    views::create_vm::render(frame, app);
                }
            })
            .context("draw TUI frame")?;

        // Handle input (non-blocking with short timeout).
        if event::poll(Duration::from_millis(100)).context("poll terminal events")? {
            if let Event::Key(key) = event::read().context("read terminal event")? {
                if key.kind == KeyEventKind::Press {
                    // Confirm dialog takes priority (matches rendering order).
                    let selected_state = app.selected_vm().map(|vm| vm.state);
                    if app.has_pending_action() {
                        if let Some(action) =
                            map_key(key.code, app.current_view(), selected_state, true)
                        {
                            app.handle_action(action);
                        }
                    } else if app.has_create_form() {
                        handle_create_form_key(client, app, key.code, key.modifiers, last_poll);
                    } else if let Some(action) =
                        map_key(key.code, app.current_view(), selected_state, false)
                    {
                        match action {
                            Action::OpenShell => {
                                run_selected_vm_surface(
                                    terminal,
                                    client,
                                    app,
                                    VmSurface::Shell,
                                    last_poll,
                                )?;
                            }
                            Action::OpenConsole => {
                                run_selected_vm_surface(
                                    terminal,
                                    client,
                                    app,
                                    VmSurface::Console,
                                    last_poll,
                                )?;
                            }
                            Action::Start => {
                                start_selected_vm(client, app, last_poll)?;
                            }
                            Action::ToggleLogs => {
                                toggle_selected_vm_logs(client, app, last_poll)?;
                            }
                            _ => app.handle_action(action),
                        }
                    }
                }
            }
        }

        // Execute confirmed VM actions via HTTP.
        if let Some(confirmed) = app.take_confirmed_action() {
            let result = execute_vm_action(client, app.addr(), &confirmed);
            match result {
                Ok(msg) => app.set_status(msg),
                Err(e) => app.set_status(format!("Error: {e}")),
            }
            // Refresh VM list immediately after action.
            if let Ok(vms) = poll_vms(client, app.addr()) {
                app.set_vms(vms);
            }
            *last_poll = std::time::Instant::now();
        }

        if app.should_quit() {
            break;
        }
    }

    Ok(())
}

/// Maps a key press to an [`Action`] based on the current view and pending state.
fn map_key(
    code: KeyCode,
    view: View,
    selected_state: Option<crate::backend::VmState>,
    has_pending: bool,
) -> Option<Action> {
    // When a confirmation dialog is showing, only y/n/Esc are accepted.
    if has_pending {
        return match code {
            KeyCode::Char('y') => Some(Action::Confirm),
            KeyCode::Char('n') | KeyCode::Esc => Some(Action::Cancel),
            _ => None,
        };
    }

    match view {
        View::Dashboard => match code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Char('j') | KeyCode::Down => Some(Action::Down),
            KeyCode::Char('k') | KeyCode::Up => Some(Action::Up),
            KeyCode::Enter => Some(Action::Enter),
            KeyCode::Tab => Some(Action::SwitchPane),
            KeyCode::Char('l') => Some(Action::ToggleLogs),
            KeyCode::Char('e') => Some(Action::OpenShell),
            KeyCode::Char('o') => Some(Action::OpenConsole),
            KeyCode::Char('s') => Some(primary_lifecycle_action(selected_state)),
            KeyCode::Char('x') => Some(Action::Kill),
            KeyCode::Char('d') => Some(Action::Delete),
            KeyCode::Char('c') => Some(Action::CreateNew),
            _ => None,
        },
        View::VmDetail => match code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Char('l') => Some(Action::ToggleLogs),
            KeyCode::Char('e') => Some(Action::OpenShell),
            KeyCode::Char('o') => Some(Action::OpenConsole),
            KeyCode::Char('s') => Some(primary_lifecycle_action(selected_state)),
            KeyCode::Char('x') => Some(Action::Kill),
            KeyCode::Char('d') => Some(Action::Delete),
            _ => None,
        },
        View::Logs => match code {
            KeyCode::Char('q') => Some(Action::Quit),
            KeyCode::Esc => Some(Action::Back),
            KeyCode::Char('l') => Some(Action::ToggleLogs),
            _ => None,
        },
    }
}

fn primary_lifecycle_action(selected_state: Option<crate::backend::VmState>) -> Action {
    match selected_state {
        Some(crate::backend::VmState::Stopped | crate::backend::VmState::Failed) => Action::Start,
        _ => Action::Stop,
    }
}

fn run_selected_vm_surface(
    terminal: &mut ratatui::DefaultTerminal,
    client: &reqwest::Client,
    app: &mut App,
    surface: VmSurface,
    last_poll: &mut std::time::Instant,
) -> anyhow::Result<()> {
    let Some(vm) = app.selected_vm() else {
        app.set_status("No VM selected".to_owned());
        return Ok(());
    };

    let vm_id = vm.id.clone();
    let status = run_vm_surface_command(terminal, app.addr(), &vm_id, surface)?;
    let label = match surface {
        VmSurface::Shell => "Shell",
        VmSurface::Console => "Console",
    };

    if status.success() {
        app.set_status(format!("{label} closed for {vm_id}"));
    } else if let Some(code) = status.code() {
        app.set_status(format!("{label} exited with status {code}"));
    } else {
        app.set_status(format!("{label} ended by signal"));
    }

    if let Ok(vms) = poll_vms(client, app.addr()) {
        app.set_vms(vms);
    }
    *last_poll = std::time::Instant::now();
    Ok(())
}

fn start_selected_vm(
    client: &reqwest::Client,
    app: &mut App,
    last_poll: &mut std::time::Instant,
) -> anyhow::Result<()> {
    let Some(vm) = app.selected_vm() else {
        app.set_status("No VM selected".to_owned());
        return Ok(());
    };
    let vm_id = vm.id.clone();
    let url = format!("{}/v1/vms/{vm_id}/start", app.addr());
    let result = execute_simple_vm_request(client, reqwest::Method::POST, &url);
    match result {
        Ok(()) => app.set_status(format!("Started VM {vm_id}")),
        Err(error) => app.set_status(format!("Error: {error}")),
    }
    if let Ok(vms) = poll_vms(client, app.addr()) {
        app.set_vms(vms);
    }
    *last_poll = std::time::Instant::now();
    Ok(())
}

fn toggle_selected_vm_logs(
    client: &reqwest::Client,
    app: &mut App,
    last_poll: &mut std::time::Instant,
) -> anyhow::Result<()> {
    if app.current_view() == View::Logs {
        app.handle_action(Action::ToggleLogs);
        return Ok(());
    }

    let Some(vm_id) = app.selected_vm().map(|vm| vm.id.clone()) else {
        app.set_status("No VM selected".to_owned());
        return Ok(());
    };

    app.handle_action(Action::ToggleLogs);
    if let Err(error) = refresh_logs_view(client, app) {
        app.set_status(format!("Error: failed to load logs for {vm_id}: {error}"));
    }
    *last_poll = std::time::Instant::now();
    Ok(())
}

fn run_vm_surface_command(
    terminal: &mut ratatui::DefaultTerminal,
    addr: &str,
    vm_id: &str,
    surface: VmSurface,
) -> anyhow::Result<std::process::ExitStatus> {
    let exe = std::env::current_exe().context("resolve current executable for TUI handoff")?;
    let args = nested_cli_args(addr, vm_id, surface);

    ratatui::restore();
    let result = std::process::Command::new(&exe)
        .args(&args)
        .status()
        .with_context(|| format!("launch {:?} from TUI", surface))?;
    *terminal = ratatui::init();
    Ok(result)
}

fn nested_cli_args(addr: &str, vm_id: &str, surface: VmSurface) -> Vec<String> {
    let subcommand = match surface {
        VmSurface::Shell => "shell",
        VmSurface::Console => "console",
    };

    vec![
        "--addr".to_owned(),
        addr.to_owned(),
        subcommand.to_owned(),
        vm_id.to_owned(),
    ]
}

fn spawn_event_listener(addr: String) -> (std::sync::mpsc::Receiver<app::TuiEvent>, EventListener) {
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    let thread = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build();
        let Ok(runtime) = runtime else {
            return;
        };
        runtime.block_on(run_event_listener(addr, event_tx, stop_rx));
    });

    (
        event_rx,
        EventListener {
            stop_tx,
            thread: Some(thread),
        },
    )
}

async fn run_event_listener(
    addr: String,
    event_tx: std::sync::mpsc::Sender<app::TuiEvent>,
    mut stop_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        if *stop_rx.borrow() {
            return;
        }

        if let Err(error) = stream_events_once(&addr, &event_tx, &mut stop_rx).await {
            tracing::debug!(error = %error, "tui event stream disconnected");
        }

        if *stop_rx.borrow() {
            return;
        }

        tokio::select! {
            _ = stop_rx.changed() => return,
            _ = tokio::time::sleep(EVENT_RECONNECT_DELAY) => {}
        }
    }
}

async fn stream_events_once(
    addr: &str,
    event_tx: &std::sync::mpsc::Sender<app::TuiEvent>,
    stop_rx: &mut tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let client = reqwest::Client::builder()
        .build()
        .context("create SSE client for TUI")?;
    let url = format!("{addr}/v1/events");
    let mut response = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("connect to TUI event stream at {url}"))?
        .error_for_status()
        .context("TUI event stream returned error status")?;
    let mut buffer = String::new();

    loop {
        tokio::select! {
            _ = stop_rx.changed() => return Ok(()),
            next = response.chunk() => {
                let Some(chunk) = next.context("read SSE chunk for TUI")? else {
                    return Ok(());
                };
                buffer.push_str(&String::from_utf8_lossy(&chunk));
                buffer.retain(|ch| ch != '\r');

                for frame in take_sse_frames(&mut buffer) {
                    if let Some(event) = parse_sse_frame(&frame) {
                        let _ = event_tx.send(event);
                    }
                }
            }
        }
    }
}

fn take_sse_frames(buffer: &mut String) -> Vec<String> {
    let mut frames = Vec::new();

    while let Some(index) = buffer.find("\n\n") {
        frames.push(buffer[..index].to_owned());
        buffer.drain(..index + 2);
    }

    frames
}

fn parse_sse_frame(frame: &str) -> Option<app::TuiEvent> {
    let payload = frame
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
        .collect::<Vec<_>>()
        .join("\n");
    if payload.is_empty() {
        return None;
    }

    let event: VmEvent = serde_json::from_str(&payload).ok()?;
    Some(app::TuiEvent {
        timestamp: display_timestamp(&event.timestamp),
        event_type: event.event_type,
        vm_id: event.vm_id,
    })
}

fn display_timestamp(timestamp: &str) -> String {
    if let Some(time) = timestamp.split('T').nth(1) {
        let clean = time.trim_end_matches('Z');
        return clean.split('.').next().unwrap_or(clean).to_owned();
    }

    timestamp.to_owned()
}

fn drain_event_receiver(app: &mut App, event_rx: &std::sync::mpsc::Receiver<app::TuiEvent>) {
    while let Ok(event) = event_rx.try_recv() {
        app.push_event(event);
    }
}

/// Polls the daemon for the current VM list.
fn poll_vms(client: &reqwest::Client, addr: &str) -> anyhow::Result<Vec<VmInfo>> {
    let url = format!("{addr}/v1/vms");
    let rt = tokio::runtime::Handle::try_current();

    if let Ok(handle) = rt {
        // We're inside a tokio runtime — use block_in_place.
        tokio::task::block_in_place(|| {
            handle.block_on(async {
                let resp = client.get(&url).send().await.context("GET /v1/vms")?;
                let vms: Vec<VmInfo> = resp.json().await.context("parse VM list response")?;
                Ok(vms)
            })
        })
    } else {
        // No runtime — create a temporary one.
        let rt = tokio::runtime::Runtime::new().context("create tokio runtime for TUI polling")?;
        rt.block_on(async {
            let resp = client.get(&url).send().await.context("GET /v1/vms")?;
            let vms: Vec<VmInfo> = resp.json().await.context("parse VM list response")?;
            Ok(vms)
        })
    }
}

fn poll_vm(client: &reqwest::Client, addr: &str, vm_id: &str) -> anyhow::Result<VmInfo> {
    let url = format!("{addr}/v1/vms/{vm_id}");
    let rt = tokio::runtime::Handle::try_current();

    if let Ok(handle) = rt {
        tokio::task::block_in_place(|| {
            handle.block_on(async {
                let vm: VmInfo = client
                    .get(&url)
                    .send()
                    .await
                    .with_context(|| format!("GET /v1/vms/{vm_id}"))?
                    .error_for_status()
                    .with_context(|| format!("GET /v1/vms/{vm_id} failed"))?
                    .json()
                    .await
                    .context("parse VM info response for logs view")?;
                Ok(vm)
            })
        })
    } else {
        let rt =
            tokio::runtime::Runtime::new().context("create tokio runtime for VM logs polling")?;
        rt.block_on(async {
            let vm: VmInfo = client
                .get(&url)
                .send()
                .await
                .with_context(|| format!("GET /v1/vms/{vm_id}"))?
                .error_for_status()
                .with_context(|| format!("GET /v1/vms/{vm_id} failed"))?
                .json()
                .await
                .context("parse VM info response for logs view")?;
            Ok(vm)
        })
    }
}

fn refresh_logs_view(client: &reqwest::Client, app: &mut App) -> anyhow::Result<()> {
    let Some(vm_id) = app.logs_vm_id().map(str::to_owned) else {
        return Ok(());
    };
    let vm = poll_vm(client, app.addr(), &vm_id)?;
    app.set_logs_from_vm(&vm);
    Ok(())
}

fn poll_pool_status(client: &reqwest::Client, addr: &str) -> anyhow::Result<(usize, usize)> {
    let url = format!("{addr}/v1/pool");
    let rt = tokio::runtime::Handle::try_current();

    if let Ok(handle) = rt {
        tokio::task::block_in_place(|| {
            handle.block_on(async {
                let resp = client.get(&url).send().await.context("GET /v1/pool")?;
                if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                    return Ok((0, 0));
                }
                let status: PoolStatus = resp
                    .error_for_status()
                    .context("GET /v1/pool failed")?
                    .json()
                    .await
                    .context("parse pool status response")?;
                Ok(pool_totals(&status))
            })
        })
    } else {
        let rt = tokio::runtime::Runtime::new().context("create tokio runtime for pool polling")?;
        rt.block_on(async {
            let resp = client.get(&url).send().await.context("GET /v1/pool")?;
            if resp.status() == reqwest::StatusCode::SERVICE_UNAVAILABLE {
                return Ok((0, 0));
            }
            let status: PoolStatus = resp
                .error_for_status()
                .context("GET /v1/pool failed")?
                .json()
                .await
                .context("parse pool status response")?;
            Ok(pool_totals(&status))
        })
    }
}

fn pool_totals(status: &PoolStatus) -> (usize, usize) {
    status
        .images
        .values()
        .fold((0, 0), |(available, target), image| {
            (available + image.available, target + image.target)
        })
}

/// Executes a confirmed VM lifecycle action via the daemon HTTP API.
///
/// # Errors
///
/// Returns an error if the HTTP request fails.
fn execute_vm_action(
    client: &reqwest::Client,
    addr: &str,
    action: &PendingAction,
) -> anyhow::Result<String> {
    let (method, url, label) = match action {
        PendingAction::Stop { vm_id } => (
            reqwest::Method::POST,
            format!("{addr}/v1/vms/{vm_id}/stop?t=10"),
            format!("Stopped VM {vm_id}"),
        ),
        PendingAction::Kill { vm_id } => (
            reqwest::Method::POST,
            format!("{addr}/v1/vms/{vm_id}/kill"),
            format!("Killed VM {vm_id}"),
        ),
        PendingAction::Delete { vm_id } => (
            reqwest::Method::DELETE,
            format!("{addr}/v1/vms/{vm_id}"),
            format!("Deleted VM {vm_id}"),
        ),
    };

    let rt = tokio::runtime::Handle::try_current();

    if let Ok(handle) = rt {
        tokio::task::block_in_place(|| {
            handle.block_on(async {
                send_vm_request(client, method.clone(), &url).await?;
                Ok(label)
            })
        })
    } else {
        let rt = tokio::runtime::Runtime::new().context("create tokio runtime for VM action")?;
        rt.block_on(async {
            send_vm_request(client, method, &url).await?;
            Ok(label)
        })
    }
}

fn execute_simple_vm_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
) -> anyhow::Result<()> {
    let rt = tokio::runtime::Handle::try_current();

    if let Ok(handle) = rt {
        tokio::task::block_in_place(|| {
            handle.block_on(send_vm_request(client, method.clone(), url))
        })
    } else {
        let rt = tokio::runtime::Runtime::new().context("create tokio runtime for VM request")?;
        rt.block_on(send_vm_request(client, method, url))
    }
}

async fn send_vm_request(
    client: &reqwest::Client,
    method: reqwest::Method,
    url: &str,
) -> anyhow::Result<()> {
    client
        .request(method, url)
        .send()
        .await
        .context("send VM action request")?
        .error_for_status()
        .context("VM action failed")?;
    Ok(())
}

/// Renders the full-screen logs view for the selected VM.
fn render_logs(frame: &mut ratatui::Frame, app: &App) {
    use ratatui::style::{Color, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Paragraph, Wrap};

    let area = frame.area();
    let Some(logs) = app.logs() else {
        let paragraph = Paragraph::new("No VM selected.")
            .block(
                Block::default()
                    .title(" VM Logs — Esc: back | q: quit ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: false });
        frame.render_widget(paragraph, area);
        return;
    };

    let vm_label = logs.vm_name.as_deref().unwrap_or(&logs.vm_id).to_owned();
    let state = format!("{:?}", logs.vm_state);
    let mut lines: Vec<ratatui::text::Line<'static>> = vec![
        Line::from(vec![
            Span::styled("VM: ", Style::default().fg(Color::DarkGray)),
            Span::styled(vm_label, Style::default().fg(Color::White)),
            Span::styled("  State: ", Style::default().fg(Color::DarkGray)),
            Span::styled(state, Style::default().fg(Color::Yellow)),
        ]),
        Line::default(),
    ];

    append_log_section(&mut lines, "STDOUT", &logs.stdout, Color::White);
    lines.push(Line::default());
    append_log_section(&mut lines, "STDERR", &logs.stderr, Color::LightRed);

    let paragraph = Paragraph::new(lines)
        .block(
            Block::default()
                .title(" VM Logs — Esc: back | q: quit ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .wrap(Wrap { trim: false });

    frame.render_widget(paragraph, area);
}

fn append_log_section(
    lines: &mut Vec<ratatui::text::Line<'static>>,
    title: &str,
    content: &str,
    content_color: ratatui::style::Color,
) {
    lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
        title.to_owned(),
        ratatui::style::Style::default().fg(ratatui::style::Color::Cyan),
    )));

    if content.is_empty() {
        lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
            "  (no output captured)".to_owned(),
            ratatui::style::Style::default().fg(ratatui::style::Color::DarkGray),
        )));
        return;
    }

    for line in content.lines() {
        lines.push(ratatui::text::Line::from(ratatui::text::Span::styled(
            line.to_owned(),
            ratatui::style::Style::default().fg(content_color),
        )));
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;

/// Handles key input when the create-VM form overlay is open.
fn handle_create_form_key(
    client: &reqwest::Client,
    app: &mut App,
    code: KeyCode,
    modifiers: KeyModifiers,
    last_poll: &mut std::time::Instant,
) {
    match code {
        KeyCode::Esc => {
            app.close_create_form();
        }
        KeyCode::Up | KeyCode::BackTab => {
            if let Some(form) = app.create_form_mut() {
                form.move_up();
            }
        }
        KeyCode::Down | KeyCode::Tab => {
            if let Some(form) = app.create_form_mut() {
                form.move_down();
            }
        }
        KeyCode::Left => {
            if let Some(form) = app.create_form_mut() {
                if form.is_button_row() || form.is_preset_mode() {
                    form.cycle_left();
                } else {
                    form.move_cursor_left();
                }
            }
        }
        KeyCode::Right => {
            if let Some(form) = app.create_form_mut() {
                if form.is_button_row() || form.is_preset_mode() {
                    form.cycle_right();
                } else {
                    form.move_cursor_right();
                }
            }
        }
        KeyCode::Enter => {
            let is_button_row = app
                .create_form()
                .is_some_and(app::CreateVmForm::is_button_row);
            if is_button_row {
                let is_cancel = app.create_form().is_some_and(|f| f.button_index == 1);
                if is_cancel {
                    app.close_create_form();
                } else {
                    submit_create_form(client, app, last_poll);
                }
            } else if let Some(form) = app.create_form_mut() {
                form.move_down();
            }
        }
        KeyCode::Backspace => {
            if let Some(form) = app.create_form_mut() {
                form.delete_char();
            }
        }
        KeyCode::Char(c) => {
            // Ignore ctrl/alt modified chars.
            if modifiers.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                return;
            }
            // No typing on the button row.
            let is_button = app
                .create_form()
                .is_some_and(app::CreateVmForm::is_button_row);
            if !is_button {
                if let Some(form) = app.create_form_mut() {
                    form.insert_char(c);
                }
            }
        }
        _ => {}
    }
}

/// Validates and submits the create-VM form.
///
/// On validation failure, keeps the form open with an error.
/// On HTTP failure, keeps the form open with the server error.
fn submit_create_form(client: &reqwest::Client, app: &mut App, last_poll: &mut std::time::Instant) {
    let Some(form) = app.create_form() else {
        return;
    };

    // Validate image.
    let image = form.image_value().trim().to_owned();
    if image.is_empty() {
        if let Some(f) = app.create_form_mut() {
            f.error = Some("Image is required".to_owned());
            f.selected_row = 0;
        }
        return;
    }

    // Validate memory.
    let memory_mib = match form.memory_mib() {
        Ok(v) => v,
        Err(msg) => {
            if let Some(f) = app.create_form_mut() {
                f.error = Some(msg.to_owned());
                f.selected_row = 2;
            }
            return;
        }
    };

    // Validate vCPUs.
    let vcpus: u32 = match form.vcpus.trim().parse() {
        Ok(v) if v >= 1 => v,
        _ => {
            if let Some(f) = app.create_form_mut() {
                f.error = Some("vCPUs must be at least 1".to_owned());
                f.selected_row = 3;
            }
            return;
        }
    };

    // Build command.
    let cmd: Vec<String> = if form.cmd.trim().is_empty() {
        Vec::new()
    } else {
        form.cmd.split_whitespace().map(String::from).collect()
    };

    // Build name.
    let name = if form.name.trim().is_empty() {
        None
    } else {
        Some(form.name.trim().to_owned())
    };

    let mut config = visor_types::VmConfig::new(image);
    config.cmd = cmd;
    config.memory_mib = memory_mib;
    config.vcpus = vcpus;
    config.name = name;
    config.detach = true;

    // Make HTTP call — form stays in memory, TUI blocks briefly.
    match post_create_vm(client, app.addr(), &config) {
        Ok(info) => {
            // Success: close form, show status.
            app.close_create_form();
            let display = info.name.as_deref().unwrap_or(&info.id);
            app.set_status(format!("Created VM {display}"));
        }
        Err(e) => {
            // Failure: keep form open, show error in form.
            if let Some(f) = app.create_form_mut() {
                f.error = Some(format!("{e}"));
            }
        }
    }

    // Refresh VM list.
    if let Ok(vms) = poll_vms(client, app.addr()) {
        app.set_vms(vms);
    }
    *last_poll = std::time::Instant::now();
}

/// POSTs a new VM creation request to the daemon.
fn post_create_vm(
    client: &reqwest::Client,
    addr: &str,
    config: &visor_types::VmConfig,
) -> anyhow::Result<VmInfo> {
    let url = format!("{addr}/v1/vms");
    let rt = tokio::runtime::Handle::try_current();

    if let Ok(handle) = rt {
        tokio::task::block_in_place(|| {
            handle.block_on(async {
                let resp = client
                    .post(&url)
                    .json(config)
                    .send()
                    .await
                    .context("POST /v1/vms")?
                    .error_for_status()
                    .context("create VM failed")?;
                let info: VmInfo = resp.json().await.context("parse create VM response")?;
                Ok(info)
            })
        })
    } else {
        let rt = tokio::runtime::Runtime::new().context("create tokio runtime for VM creation")?;
        rt.block_on(async {
            let resp = client
                .post(&url)
                .json(config)
                .send()
                .await
                .context("POST /v1/vms")?
                .error_for_status()
                .context("create VM failed")?;
            let info: VmInfo = resp.json().await.context("parse create VM response")?;
            Ok(info)
        })
    }
}
