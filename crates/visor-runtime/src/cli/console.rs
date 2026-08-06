//! `visor console` — attach to a VM serial console.
//!
//! Opens a WebSocket connection to the daemon's log-streaming endpoint
//! and prints serial console output to stdout in real time.

use std::io::IsTerminal;
use std::time::Duration;

use anyhow::Context;
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};
use futures_util::StreamExt;

use super::shell::to_ws_url;
use crate::api::ws::WsMessage;

/// Arguments for the `visor console` subcommand.
#[derive(Debug, clap::Args)]
#[non_exhaustive]
pub struct ConsoleArgs {
    /// VM ID to attach to.
    pub vm_id: String,
    /// Escape key sequence to detach from the console (for example `^]` or
    /// `^a`).
    #[arg(long, default_value = "^]")]
    pub escape_key: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EscapeKey {
    code: KeyCode,
    modifiers: KeyModifiers,
}

struct RawModeGuard;

impl RawModeGuard {
    fn new() -> anyhow::Result<Self> {
        enable_raw_mode().context("enable raw terminal mode for console detach")?;
        Ok(Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
    }
}

fn parse_escape_key(sequence: &str) -> anyhow::Result<EscapeKey> {
    if let Some(control) = sequence.strip_prefix('^') {
        let mut chars = control.chars();
        let Some(ch) = chars.next() else {
            anyhow::bail!("escape key cannot be empty");
        };
        if chars.next().is_some() {
            anyhow::bail!("escape key must be a single character or control sequence like ^]");
        }
        return Ok(EscapeKey {
            code: KeyCode::Char(ch.to_ascii_lowercase()),
            modifiers: KeyModifiers::CONTROL,
        });
    }

    let mut chars = sequence.chars();
    let Some(ch) = chars.next() else {
        anyhow::bail!("escape key cannot be empty");
    };
    if chars.next().is_some() {
        anyhow::bail!("escape key must be a single character or control sequence like ^]");
    }

    Ok(EscapeKey {
        code: KeyCode::Char(ch),
        modifiers: KeyModifiers::NONE,
    })
}

fn matches_escape_key(key: KeyEvent, escape_key: EscapeKey) -> bool {
    let normalized = match key.code {
        KeyCode::Char(ch) => KeyCode::Char(ch.to_ascii_lowercase()),
        code => code,
    };
    normalized == escape_key.code && key.modifiers.contains(escape_key.modifiers)
}

fn poll_for_detach(escape_key: EscapeKey) -> anyhow::Result<bool> {
    if !event::poll(Duration::from_millis(0)).context("poll console detach key")? {
        return Ok(false);
    }

    let Event::Key(key) = event::read().context("read console detach key")? else {
        return Ok(false);
    };
    if key.kind != KeyEventKind::Press {
        return Ok(false);
    }

    Ok(matches_escape_key(key, escape_key))
}

fn console_connect_message(args: &ConsoleArgs) -> String {
    format!(
        "Connected to serial console for {} (detach with {}, use `visor shell {}` for a guest shell or `visor exec {} -- <cmd>` for one-shot commands)",
        args.vm_id, args.escape_key, args.vm_id, args.vm_id
    )
}

/// Executes the `visor console` subcommand.
///
/// Connects to the daemon via WebSocket at `/v1/vms/{id}/logs` and
/// streams serial console output to stdout. Runs until the connection
/// closes, the user detaches with the configured escape key, or the
/// process is interrupted with Ctrl-C.
///
/// # Errors
///
/// Returns an error if the VM does not exist, the WebSocket connection
/// fails, or an I/O error occurs.
pub async fn execute(args: &ConsoleArgs, addr: &str) -> anyhow::Result<()> {
    let escape_key = parse_escape_key(&args.escape_key)?;

    let vm = super::fetch_vm_info(addr, &args.vm_id).await?;
    super::ensure_interactive_vm_running(&vm, &args.vm_id, "console")?;

    // Connect WebSocket to logs endpoint.
    let ws_base = to_ws_url(addr);
    let ws_url = format!("{ws_base}/v1/vms/{}/logs", args.vm_id);

    let (ws_stream, _response) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .with_context(|| format!("failed to connect WebSocket to {ws_url}"))?;

    let (_ws_sink, mut ws_source) = ws_stream.split();

    let raw_mode_guard = std::io::stdin()
        .is_terminal()
        .then(RawModeGuard::new)
        .transpose()?;

    eprintln!("{}", console_connect_message(args));

    loop {
        tokio::select! {
            frame = ws_source.next() => {
                let Some(frame) = frame else {
                    break;
                };
                let frame = match frame {
                    Ok(f) => f,
                    Err(e) => {
                        eprintln!("WebSocket error: {e}");
                        break;
                    }
                };

                let text = match frame {
                    tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
                    tokio_tungstenite::tungstenite::Message::Close(_) => break,
                    _ => continue,
                };

                match serde_json::from_str::<WsMessage>(&text) {
                    Ok(WsMessage::Stdout { data }) => print!("{data}"),
                    Ok(WsMessage::Stderr { data }) => eprint!("{data}"),
                    Ok(WsMessage::Info { data }) => eprintln!("{data}"),
                    Ok(WsMessage::Error { data }) => {
                        eprintln!("error: {data}");
                        break;
                    }
                    _ => {}
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(50)), if raw_mode_guard.is_some() => {
                if poll_for_detach(escape_key)? {
                    eprintln!("Detached from console");
                    break;
                }
            }
        }
    }

    eprintln!("Disconnected from console");
    Ok(())
}

#[cfg(test)]
#[path = "console_test.rs"]
mod tests;
