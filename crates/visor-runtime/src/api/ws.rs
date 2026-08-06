//! WebSocket endpoints for interactive VM sessions.
//!
//! - `GET /v1/vms/{id}/attach` — bidirectional shell access
//! - `GET /v1/vms/{id}/logs` — live log streaming
//!
//! These use raw axum WebSocket support (built into axum 0.8, no extra deps).
//! The attach handler bridges WebSocket I/O to the guest VM's vsock agent
//! for interactive command execution via a shell-backed REPL protocol.

use axum::extract::ws::{Message, WebSocket};
use axum::extract::{Path, State, WebSocketUpgrade};
use axum::response::IntoResponse;

use crate::api::router::AppState;
use crate::backend::ExecRequest;

/// WebSocket message types for interactive VM sessions.
///
/// Uses internally-tagged JSON (`"type"` field) to distinguish message
/// kinds. The protocol is asymmetric:
///
/// - **Client → Server**: [`Stdin`](WsMessage::Stdin)
/// - **Server → Client**: [`Stdout`](WsMessage::Stdout), [`Stderr`](WsMessage::Stderr),
///   [`Exit`](WsMessage::Exit), [`Error`](WsMessage::Error), [`Info`](WsMessage::Info)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type")]
#[non_exhaustive]
pub enum WsMessage {
    /// Client→Server: command input.
    #[serde(rename = "stdin")]
    Stdin {
        /// The command text (including trailing newline).
        data: String,
    },
    /// Server→Client: standard output from command.
    #[serde(rename = "stdout")]
    Stdout {
        /// Captured stdout text.
        data: String,
    },
    /// Server→Client: standard error from command.
    #[serde(rename = "stderr")]
    Stderr {
        /// Captured stderr text.
        data: String,
    },
    /// Server→Client: command exited with code.
    #[serde(rename = "exit")]
    Exit {
        /// Process exit code.
        code: i32,
    },
    /// Server→Client: error message.
    #[serde(rename = "error")]
    Error {
        /// Error description.
        data: String,
    },
    /// Server→Client: informational message.
    #[serde(rename = "info")]
    Info {
        /// Informational text.
        data: String,
    },
}

/// Upgrades to a WebSocket for interactive shell access to a VM.
///
/// The WebSocket carries stdin/stdout/stderr between the client and the guest
/// VM via a shell-backed REPL protocol. Each stdin message is executed through
/// `/bin/sh -lc`, and the results are sent back as stdout/stderr/exit messages.
///
/// # Protocol
///
/// ```text
/// Client → Server: { "type": "stdin", "data": "ls | wc -l\n" }
/// Server → Client: { "type": "stdout", "data": "bin  etc  home..." }
/// Server → Client: { "type": "exit", "code": 0 }
/// ```
pub async fn ws_attach(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_attach(socket, id, state))
}

/// Upgrades to a WebSocket for live log streaming from a VM.
///
/// Streams console output from the VM's serial device in real time.
/// Polls the backend's serial buffer at a fixed interval and sends
/// new bytes as `Stdout` messages.
pub async fn ws_logs(
    State(state): State<AppState>,
    Path(id): Path<String>,
    ws: WebSocketUpgrade,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| handle_logs(socket, id, state))
}

/// Serialize a [`WsMessage`] and send it over the WebSocket.
///
/// Returns `true` on success, `false` if the send failed (client disconnected).
async fn send_ws_msg(socket: &mut WebSocket, msg: &WsMessage) -> bool {
    match serde_json::to_string(msg) {
        Ok(text) => socket.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

/// Handles an interactive shell WebSocket session.
///
/// For each incoming `Stdin` message, executes the raw line through
/// `/bin/sh -lc` via the backend's `exec()` method, then sends back
/// `Stdout`, `Stderr`, and `Exit` messages.
async fn handle_attach(mut socket: WebSocket, id: String, state: AppState) {
    // Verify VM exists and is accessible.
    if let Err(e) = state.backend.get(&id).await {
        let _ = send_ws_msg(
            &mut socket,
            &WsMessage::Error {
                data: format!("vm not found: {e}"),
            },
        )
        .await;
        return;
    }

    // Send info greeting.
    if !send_ws_msg(
        &mut socket,
        &WsMessage::Info {
            data: format!("attached to vm {id} (/bin/sh -lc per line)"),
        },
    )
    .await
    {
        return;
    }

    // REPL loop: read stdin messages, execute, send results.
    while let Some(Ok(msg)) = socket.recv().await {
        let text = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };

        // Parse the WebSocket message.
        let ws_msg: WsMessage = match serde_json::from_str(&text) {
            Ok(m) => m,
            Err(e) => {
                if !send_ws_msg(
                    &mut socket,
                    &WsMessage::Error {
                        data: format!("invalid message: {e}"),
                    },
                )
                .await
                {
                    break;
                }
                continue;
            }
        };

        // Only handle Stdin messages from the client.
        let WsMessage::Stdin { data: input } = ws_msg else {
            continue;
        };

        // Trim input and run it through the guest shell so quoting, pipes,
        // redirects, and conditionals behave like an actual shell command.
        let trimmed = input.trim();
        if trimmed.is_empty() {
            // Empty input: send exit code 0 and continue.
            if !send_ws_msg(&mut socket, &WsMessage::Exit { code: 0 }).await {
                break;
            }
            continue;
        }

        let request = shell_exec_request(trimmed);

        // Execute via the backend (which handles vsock internally).
        tracing::debug!(vm_id = %id, cmd = trimmed, "ws attach: executing command");
        match state.backend.exec(&id, request).await {
            Ok(result) => {
                if !result.stdout.is_empty()
                    && !send_ws_msg(
                        &mut socket,
                        &WsMessage::Stdout {
                            data: result.stdout,
                        },
                    )
                    .await
                {
                    break;
                }
                if !result.stderr.is_empty()
                    && !send_ws_msg(
                        &mut socket,
                        &WsMessage::Stderr {
                            data: result.stderr,
                        },
                    )
                    .await
                {
                    break;
                }
                if !send_ws_msg(
                    &mut socket,
                    &WsMessage::Exit {
                        code: result.exit_code,
                    },
                )
                .await
                {
                    break;
                }
            }
            Err(e) => {
                if !send_ws_msg(
                    &mut socket,
                    &WsMessage::Error {
                        data: format!("exec failed: {e:#}"),
                    },
                )
                .await
                {
                    break;
                }
            }
        }
    }
}

fn shell_exec_request(command: &str) -> ExecRequest {
    ExecRequest::new(vec![
        "/bin/sh".to_owned(),
        "-lc".to_owned(),
        command.to_owned(),
    ])
}

/// Streams serial console output from a running VM.
///
/// Polls `console_output()` every 200ms and sends new bytes as `Stdout`
/// messages. Closes when the client disconnects or an error occurs.
async fn handle_logs(mut socket: WebSocket, id: String, state: AppState) {
    // Verify VM exists.
    if let Err(e) = state.backend.get(&id).await {
        let _ = send_ws_msg(
            &mut socket,
            &WsMessage::Error {
                data: format!("vm not found: {e}"),
            },
        )
        .await;
        return;
    }

    // Send greeting.
    if !send_ws_msg(
        &mut socket,
        &WsMessage::Info {
            data: format!("streaming logs for vm {id}"),
        },
    )
    .await
    {
        return;
    }

    let mut cursor: usize = 0;
    let poll_interval = std::time::Duration::from_millis(200);

    loop {
        // Check for incoming close/ping messages (non-blocking).
        if let Ok(Some(Ok(Message::Close(_)) | Err(_)) | None) =
            tokio::time::timeout(poll_interval, socket.recv()).await
        {
            break;
        }

        // Poll console output.
        let Ok(bytes) = state.backend.console_output(&id).await else {
            // VM stopped or was destroyed — send final message and close.
            let _ = send_ws_msg(
                &mut socket,
                &WsMessage::Info {
                    data: format!("vm {id} is no longer running"),
                },
            )
            .await;
            break;
        };

        // Send new bytes since last cursor position.
        if bytes.len() > cursor {
            let new_data = String::from_utf8_lossy(&bytes[cursor..]).into_owned();
            cursor = bytes.len();
            if !send_ws_msg(&mut socket, &WsMessage::Stdout { data: new_data }).await {
                break;
            }
        }
    }
}

#[cfg(test)]
#[path = "ws_test.rs"]
mod tests;
