//! `visor tui` — launch the terminal dashboard.
//!
//! Connects to a running visor daemon's HTTP API and displays a live-updating
//! terminal UI with VM list, metrics, and events.

/// Executes the `visor tui` subcommand.
///
/// Launches the ratatui-based terminal dashboard pointed at the given daemon
/// address. The TUI polls the daemon's REST API for VM data and renders
/// the dashboard until the user presses `q` to quit.
///
/// # Errors
///
/// Returns an error if terminal initialisation or the event loop fails.
pub fn execute(addr: &str) -> anyhow::Result<()> {
    crate::tui::run(addr.to_owned())
}
