//! Create-VM form overlay.
//!
//! Renders a centered dialog with selectable presets, text inputs,
//! and OK/Cancel buttons for creating a new VM.
//!
//! ```text
//! ┌─── Create VM ──────────────────────────────────────┐
//! │                                                     │
//! │  ▶ Image:    ◀ alpine:latest ▶                     │
//! │    Name:     [                              ]      │
//! │    Memory:   ◀ 128 MiB ▶                           │
//! │    vCPUs:    [1                             ]      │
//! │    Command:  [                              ]      │
//! │                                                     │
//! │    Error message here                               │
//! │                                                     │
//! │                  [ Create ]   [ Cancel ]            │
//! │                                                     │
//! │  ↑↓: rows  ←→: options/cursor  Esc: close          │
//! └────────────────────────────────────────────────────┘
//! ```

use ratatui::Frame;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};

use crate::tui::app::{App, IMAGE_PRESETS, MEMORY_PRESETS};

/// Field labels for each row (rows 0–4).
const FIELD_LABELS: [&str; 5] = [
    "Image:   ",
    "Name:    ",
    "Memory:  ",
    "vCPUs:   ",
    "Command: ",
];

/// Width of the overlay dialog.
const DIALOG_WIDTH: u16 = 56;

/// Height of the overlay dialog.
const DIALOG_HEIGHT: u16 = 16;

/// Renders the create-VM form overlay centered on the screen.
pub fn render(frame: &mut Frame, app: &App) {
    let Some(form) = app.create_form() else {
        return;
    };

    let lines = build_form_lines(form);

    let dialog = Paragraph::new(lines).block(
        Block::default()
            .title(" Create VM ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Green)),
    );

    let area = centered_rect(DIALOG_WIDTH, DIALOG_HEIGHT, frame.area());
    frame.render_widget(Clear, area);
    frame.render_widget(dialog, area);

    // Position cursor in the active text field.
    if form.is_text_input_active() {
        let label_len = u16::try_from(FIELD_LABELS[form.selected_row].len()).unwrap_or(0);
        let label_width = 2 + label_len; // indicator + label
        let cur_pos = u16::try_from(form.cursor_pos).unwrap_or(0);
        let cursor_x = area.x + 1 + label_width + cur_pos;
        // +2: border row + blank line, then row index
        let row_idx = u16::try_from(form.selected_row).unwrap_or(0);
        let cursor_y = area.y + 2 + row_idx;
        if cursor_x < area.x + area.width.saturating_sub(1) {
            frame.set_cursor_position((cursor_x, cursor_y));
        }
    }
}

/// Builds the `Line` content for the create-VM form dialog.
fn build_form_lines(form: &super::super::app::CreateVmForm) -> Vec<Line<'_>> {
    let sel = form.selected_row;
    let indicator_style = Style::default().fg(Color::Cyan);
    let label_style = Style::default().fg(Color::Gray);
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let mut lines: Vec<Line> = Vec::with_capacity(16);
    lines.push(Line::from("")); // blank after border

    // ── Rows 0–4: form fields ──────────────────────────────
    append_field_rows(form, &mut lines, indicator_style, label_style);

    lines.push(Line::from("")); // blank

    // ── Error line ──────────────────────────────────────────
    if let Some(err) = &form.error {
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                err.clone(),
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
        ]));
    } else {
        lines.push(Line::from(""));
    }

    lines.push(Line::from("")); // blank

    // ── Row 5: Buttons ──────────────────────────────────────
    let create_style = button_style(sel == 5 && form.button_index == 0);
    let cancel_style = button_style(sel == 5 && form.button_index == 1);
    lines.push(Line::from(vec![
        Span::raw("              "),
        Span::styled(" Create ", create_style),
        Span::raw("   "),
        Span::styled(" Cancel ", cancel_style),
    ]));

    lines.push(Line::from("")); // blank

    // ── Help line ───────────────────────────────────────────
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("↑↓", key_style),
        Span::raw(": rows  "),
        Span::styled("←→", key_style),
        Span::raw(": options  "),
        Span::styled("Esc", key_style),
        Span::raw(": close"),
    ]));

    lines
}

/// Appends field lines (rows 0–4) for the form fields.
fn append_field_rows<'a>(
    form: &'a super::super::app::CreateVmForm,
    lines: &mut Vec<Line<'a>>,
    indicator_style: Style,
    label_style: Style,
) {
    let sel = form.selected_row;
    // (row_index, label, text_value, is_select_mode, select_value)
    let fields: [(usize, &str, &str, bool, &str); 5] = [
        (
            0,
            FIELD_LABELS[0],
            &form.image_custom,
            !form.image_is_custom,
            IMAGE_PRESETS[form.image_preset],
        ),
        (1, FIELD_LABELS[1], &form.name, false, ""),
        (
            2,
            FIELD_LABELS[2],
            &form.memory_custom,
            !form.memory_is_custom,
            MEMORY_PRESETS[form.memory_preset].0,
        ),
        (3, FIELD_LABELS[3], &form.vcpus, false, ""),
        (4, FIELD_LABELS[4], &form.cmd, false, ""),
    ];
    for (row, label, text_val, is_select, select_val) in fields {
        let indicator = if sel == row { "▶ " } else { "  " };
        let is_active = sel == row;
        if is_select {
            lines.push(select_field_line(
                indicator,
                label,
                select_val,
                is_active,
                indicator_style,
                label_style,
            ));
        } else {
            lines.push(text_field_line(
                indicator,
                label,
                text_val,
                is_active,
                indicator_style,
                label_style,
            ));
        }
    }
}

/// Builds a select-style field line: `▶ Label:   ◀ value ▶`
fn select_field_line<'a>(
    indicator: &'a str,
    label: &'a str,
    value: &'a str,
    is_selected: bool,
    indicator_style: Style,
    label_style: Style,
) -> Line<'a> {
    let arrow_style = if is_selected {
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    let value_style = if is_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    Line::from(vec![
        Span::styled(indicator, indicator_style),
        Span::styled(label, label_style),
        Span::styled("◀ ", arrow_style),
        Span::styled(value, value_style),
        Span::styled(" ▶", arrow_style),
    ])
}

/// Builds a text-input field line: `▶ Label:   value`
fn text_field_line<'a>(
    indicator: &'a str,
    label: &'a str,
    value: &'a str,
    is_selected: bool,
    indicator_style: Style,
    label_style: Style,
) -> Line<'a> {
    let value_style = if is_selected {
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let display = if value.is_empty() && !is_selected {
        "—"
    } else {
        value
    };

    Line::from(vec![
        Span::styled(indicator, indicator_style),
        Span::styled(label, label_style),
        Span::styled(display, value_style),
    ])
}

/// Returns the style for a button (highlighted when focused).
fn button_style(is_focused: bool) -> Style {
    if is_focused {
        Style::default()
            .fg(Color::Black)
            .bg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(Color::White).bg(Color::DarkGray)
    }
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
#[path = "create_vm_test.rs"]
mod tests;
