//! `visor shell` — open an interactive shell session in a VM.
//!
//! Opens a WebSocket connection to the daemon's attach endpoint and runs a
//! REPL loop: reads lines from stdin, executes each one through `/bin/sh -lc`
//! inside the guest, and displays the results.

use anyhow::Context;
use futures_util::{SinkExt, StreamExt};

use super::ShellArgs;
use crate::api::ws::WsMessage;

/// Converts an HTTP(S) daemon address to a WebSocket URL scheme.
///
/// - `http://` → `ws://`
/// - `https://` → `wss://`
/// - Other schemes are left unchanged.
#[must_use]
pub fn to_ws_url(addr: &str) -> String {
    if let Some(rest) = addr.strip_prefix("https://") {
        format!("wss://{rest}")
    } else if let Some(rest) = addr.strip_prefix("http://") {
        format!("ws://{rest}")
    } else {
        addr.to_owned()
    }
}

/// Executes the `visor shell` subcommand.
///
/// Verifies the VM exists, opens a WebSocket to `/v1/vms/{id}/attach`, and
/// runs a REPL loop: reads lines from stdin, executes each one through
/// `/bin/sh -lc` in the guest, and displays stdout/stderr results. Exits on
/// `Ctrl-D` (EOF) or typing `exit`.
///
/// # Errors
///
/// Returns an error if the VM does not exist, the WebSocket connection fails,
/// or an I/O error occurs during the session.
pub async fn execute(addr: &str, args: &ShellArgs) -> anyhow::Result<()> {
    let vm = super::fetch_vm_info(addr, &args.vm_id).await?;
    super::ensure_interactive_vm_running(&vm, &args.vm_id, "shell")?;

    // 2. Connect WebSocket to attach endpoint.
    let ws_base = to_ws_url(addr);
    let ws_url = format!("{ws_base}/v1/vms/{}/attach", args.vm_id);

    let (ws_stream, _response) = tokio_tungstenite::connect_async(&ws_url)
        .await
        .with_context(|| format!("failed to connect WebSocket to {ws_url}"))?;

    let (mut ws_sink, mut ws_source) = ws_stream.split();

    // 3. Read and display the initial info/greeting message.
    if let Some(Ok(tokio_tungstenite::tungstenite::Message::Text(text))) = ws_source.next().await {
        if let Ok(WsMessage::Info { data }) = serde_json::from_str(&text) {
            eprintln!("{data}");
        }
    }

    // 4. REPL loop: read stdin lines, send them for shell execution, display results.
    let stdin = tokio::io::stdin();
    let reader = tokio::io::BufReader::new(stdin);
    let mut lines = tokio::io::AsyncBufReadExt::lines(reader);

    loop {
        // Print prompt to stderr (so it doesn't mix with stdout output).
        eprint!("visor> ");

        // Read next line from stdin.
        let line = match lines.next_line().await {
            Ok(Some(line)) => line,
            Ok(None) => {
                // EOF (Ctrl-D).
                eprintln!();
                break;
            }
            Err(e) => {
                return Err(e).context("failed to read from stdin");
            }
        };

        let trimmed = line.trim();

        // Exit command.
        if trimmed == "exit" || trimmed == "quit" {
            break;
        }

        // Skip empty lines.
        if trimmed.is_empty() {
            continue;
        }

        // Send stdin message.
        let ws_msg = WsMessage::Stdin {
            data: format!("{trimmed}\n"),
        };
        let json = serde_json::to_string(&ws_msg).context("failed to serialize command")?;
        ws_sink
            .send(tokio_tungstenite::tungstenite::Message::Text(json.into()))
            .await
            .context("failed to send command to daemon")?;

        // Read responses until we get an Exit message.
        loop {
            let Some(frame) = ws_source.next().await else {
                eprintln!("connection closed by server");
                return Ok(());
            };
            let frame = frame.context("WebSocket read error")?;

            let text = match frame {
                tokio_tungstenite::tungstenite::Message::Text(t) => t.to_string(),
                tokio_tungstenite::tungstenite::Message::Close(_) => {
                    eprintln!("connection closed by server");
                    return Ok(());
                }
                _ => continue,
            };

            let response: WsMessage =
                serde_json::from_str(&text).context("invalid response from server")?;

            match response {
                WsMessage::Stdout { data } => print!("{data}"),
                WsMessage::Stderr { data } => eprint!("{data}"),
                WsMessage::Exit { .. } => break,
                WsMessage::Error { data } => {
                    eprintln!("error: {data}");
                    break;
                }
                WsMessage::Info { data } => eprintln!("{data}"),
                _ => {}
            }
        }
    }

    // 5. Close WebSocket.
    ws_sink.close().await.context("failed to close WebSocket")?;

    Ok(())
}

#[cfg(test)]
#[path = "shell_test.rs"]
mod tests;
