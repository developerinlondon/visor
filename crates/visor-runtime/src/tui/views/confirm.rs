//! Confirmation dialog overlay.
//!
//! Renders a centered dialog box on top of the current view when a
//! destructive action (stop, kill, delete) is pending.
//!
//! ```text
//! ┌─── Confirm ──────────────────┐
//! │                              │
//! │  Stop VM abc123?             │
//! │                              │
//! │  [y] Yes    [n] No           │
//! │                              │
//! └──────────────────────────────┘
//! ```

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::app::{App, PendingAction};

/// Renders the confirmation overlay centered on the screen.
pub fn render(frame: &mut Frame, app: &App) {
    let Some(pending) = app.pending_action() else {
        return;
    };

    let (action_label, vm_id) = match pending {
        PendingAction::Stop { vm_id } => ("Stop", vm_id.as_str()),
        PendingAction::Kill { vm_id } => ("Kill", vm_id.as_str()),
        PendingAction::Delete { vm_id } => ("Delete", vm_id.as_str()),
    };

    let id_display = if vm_id.len() > 16 {
        &vm_id[..16]
    } else {
        vm_id
    };

    let lines = vec![
        Line::from(""),
        Line::from(format!("  {action_label} VM {id_display}?")),
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                "[y]",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" Yes    "),
            Span::styled(
                "[n]",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" No"),
        ]),
        Line::from(""),
    ];

    let dialog = Paragraph::new(lines).block(
        Block::default()
            .title(" Confirm ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Yellow)),
    );

    let area = centered_rect(36, 7, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(dialog, area);
}

/// Creates a centered rectangle within the given area.
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let [vertical] = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas(area);
    let [horizontal] = Layout::horizontal([Constraint::Length(width)])
        .flex(Flex::Center)
        .areas(vertical);
    horizontal
}

#[cfg(test)]
#[path = "confirm_test.rs"]
mod tests;
