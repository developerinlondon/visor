//! Main dashboard view.
//!
//! Renders a master-detail layout:
//! - **VMs table** (left): list of all VMs with state, image, memory.
//! - **Detail panel** (right): full info for the selected VM + compact metrics.
//! - **Events panel** (bottom): rolling SSE event stream.
//!
//! ```text
//! ┌─ VMs ──────────────────┬─ VM Detail ─────────────────┐
//! │ ▶ vm1 alpine  Running  │ ID:     abc123...            │
//! │   vm2 ubuntu  Stopped  │ Name:   zen_ramanujan        │
//! │                        │ Image:  alpine               │
//! │                        │ State:  Running              │
//! │                        │ Memory: 512 MiB              │
//! │                        │ Ports:  8080→80              │
//! │                        ├─ Metrics ────────────────────┤
//! │                        │ Total: 5  Running: 3  512MiB │
//! ├────────────────────────┴─────────────────────────────┤
//! │ Events (SSE stream)                                   │
//! └───────────────────────────────────────────────────────┘
//! ```

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Cell, Paragraph, Row, Table, TableState};

use crate::backend::VmState;
use crate::tui::app::{App, Pane};

/// Renders the dashboard view into the given frame.
pub fn render(frame: &mut Frame, app: &App) {
    let [top, bottom, help_area] = Layout::vertical([
        Constraint::Min(10),
        Constraint::Length(8),
        Constraint::Length(3),
    ])
    .areas(frame.area());

    let [vm_area, detail_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(top);

    render_vm_table(frame, app, vm_area);
    render_detail_panel(frame, app, detail_area);
    render_events(frame, app, bottom);
    render_help(frame, app, help_area);
}

/// Renders the VM list as a table with selection highlight.
fn render_vm_table(frame: &mut Frame, app: &App, area: Rect) {
    let header = Row::new(vec!["ID", "NAME", "IMAGE", "STATE", "MEM"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

    let rows: Vec<Row> = app
        .vms()
        .iter()
        .map(|vm| {
            let state_color = state_to_color(vm.state);

            let id_display = truncate(&vm.id, 12);

            let image_display = truncate(&vm.image, 16);

            Row::new(vec![
                Cell::from(id_display.to_owned()),
                Cell::from(vm.name.as_deref().unwrap_or("—").to_owned()),
                Cell::from(image_display.to_owned()),
                Cell::from(Span::styled(
                    format!("{:?}", vm.state),
                    Style::default().fg(state_color),
                )),
                Cell::from(format!("{}", vm.memory_mib)),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(14),
        Constraint::Length(16),
        Constraint::Length(16),
        Constraint::Length(10),
        Constraint::Length(6),
    ];

    let border_color = if app.active_pane() == Pane::VmList {
        Color::Cyan
    } else {
        Color::White
    };

    let table = Table::new(rows, widths)
        .header(header)
        .block(
            Block::default()
                .title(" VMs ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(border_color)),
        )
        .column_spacing(1)
        .row_highlight_style(
            Style::default()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol("▶ ");

    let mut table_state = TableState::default();
    if !app.vms().is_empty() {
        table_state.select(Some(app.selected_vm_index()));
    }

    frame.render_stateful_widget(table, area, &mut table_state);
}

/// Renders the right-side detail panel: selected VM info on top, compact metrics below.
fn render_detail_panel(frame: &mut Frame, app: &App, area: Rect) {
    let [vm_detail_area, metrics_area] =
        Layout::vertical([Constraint::Min(8), Constraint::Length(5)]).areas(area);

    render_vm_detail(frame, app, vm_detail_area);
    render_metrics(frame, app, metrics_area);
}

/// Renders selected VM detail in the right panel.
fn render_vm_detail(frame: &mut Frame, app: &App, area: Rect) {
    let border_style = Style::default().fg(Color::Cyan);

    let Some(vm) = app.selected_vm() else {
        let msg = Paragraph::new("No VM selected").block(
            Block::default()
                .title(" Detail ")
                .borders(Borders::ALL)
                .border_style(border_style),
        );
        frame.render_widget(msg, area);
        return;
    };

    let state_color = state_to_color(vm.state);
    let name_display = vm.name.as_deref().unwrap_or("—");
    let exit_display = vm
        .exit_code
        .map_or_else(|| "—".to_owned(), |c| c.to_string());

    let ports_display = if vm.ports.is_empty() {
        "—".to_owned()
    } else {
        vm.ports
            .iter()
            .map(|p| format!("{}→{}", p.host_port, p.guest_port))
            .collect::<Vec<_>>()
            .join(", ")
    };

    let memory_str = format!("{} MiB", vm.memory_mib);
    let vcpus_str = format!("{}", vm.vcpus);

    let lines = vec![
        field_line("ID:        ", &vm.id),
        field_line("Name:      ", name_display),
        field_line("Image:     ", &vm.image),
        Line::from(vec![
            Span::styled("State:     ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{:?}", vm.state),
                Style::default()
                    .fg(state_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        field_line("Created:   ", &vm.created_at),
        field_line("Memory:    ", &memory_str),
        field_line("vCPUs:     ", &vcpus_str),
        field_line("Exit Code: ", &exit_display),
        field_line("Ports:     ", &ports_display),
    ];

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(format!(" {name_display} "))
            .borders(Borders::ALL)
            .border_style(border_style),
    );

    frame.render_widget(paragraph, area);
}

/// Renders a compact metrics summary below the VM detail.
fn render_metrics(frame: &mut Frame, app: &App, area: Rect) {
    let metrics = app.compute_metrics();

    let memory_display = if metrics.total_memory_mib >= 1024 {
        let gib = metrics.total_memory_mib / 1024;
        let frac = (metrics.total_memory_mib % 1024) * 10 / 1024;
        format!("{gib}.{frac} GiB")
    } else {
        format!("{} MiB", metrics.total_memory_mib)
    };

    let stopped = metrics.total_vms.saturating_sub(metrics.running_vms);

    let lines = vec![
        Line::from(vec![
            Span::styled("VMs: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}", metrics.total_vms),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  Running: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}", metrics.running_vms),
                Style::default().fg(Color::Green),
            ),
            Span::styled("  Stopped: ", Style::default().fg(Color::Gray)),
            Span::styled(format!("{stopped}"), Style::default().fg(Color::Red)),
        ]),
        Line::from(vec![
            Span::styled("Mem: ", Style::default().fg(Color::Gray)),
            Span::styled(memory_display, Style::default().fg(Color::Cyan)),
            Span::styled("  vCPUs: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}", metrics.total_vcpus),
                Style::default().fg(Color::Cyan),
            ),
            Span::styled("  Pool: ", Style::default().fg(Color::Gray)),
            Span::styled(
                format!("{}/{}", metrics.pool_warm_count, metrics.pool_target),
                Style::default().fg(if metrics.pool_warm_count > 0 {
                    Color::Green
                } else {
                    Color::Yellow
                }),
            ),
        ]),
    ];

    let border_color = if app.active_pane() == Pane::Metrics {
        Color::Cyan
    } else {
        Color::White
    };

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Metrics ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color)),
    );

    frame.render_widget(paragraph, area);
}

/// Renders the events panel showing recent SSE events.
fn render_events(frame: &mut Frame, app: &App, area: Rect) {
    let visible_height = area.height.saturating_sub(2) as usize; // account for borders
    let events = app.events();
    let start = events.len().saturating_sub(visible_height);
    let visible_events = &events[start..];

    let lines: Vec<Line> = visible_events
        .iter()
        .map(|e| {
            Line::from(vec![
                Span::styled(
                    format!("[{}] ", e.timestamp),
                    Style::default().fg(Color::DarkGray),
                ),
                Span::styled(
                    format!("{} ", e.event_type),
                    Style::default().fg(Color::Yellow),
                ),
                Span::styled(e.vm_id.clone(), Style::default().fg(Color::White)),
            ])
        })
        .collect();

    let border_color = if app.active_pane() == Pane::Events {
        Color::Cyan
    } else {
        Color::White
    };

    let paragraph = Paragraph::new(lines).block(
        Block::default()
            .title(" Events (SSE stream) ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color)),
    );

    frame.render_widget(paragraph, area);
}

/// Renders the help bar at the bottom of the dashboard.
fn render_help(frame: &mut Frame, app: &App, area: Rect) {
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let mut spans = vec![
        Span::styled("q", key_style),
        Span::raw(" quit  "),
        Span::styled("j/k", key_style),
        Span::raw(" move  "),
        Span::styled("Enter", key_style),
        Span::raw(" open  "),
        Span::styled("l", key_style),
        Span::raw(" vm logs  "),
        Span::styled("e", key_style),
        Span::raw(" shell  "),
        Span::styled("o", key_style),
        Span::raw(" cons  "),
        Span::styled("s", key_style),
        Span::raw(" start/stop  "),
        Span::styled("x", key_style),
        Span::raw(" kill  "),
        Span::styled("d", key_style),
        Span::raw(" rm  "),
        Span::styled("c", key_style),
        Span::raw(" create"),
    ];

    if let Some(status) = app.status_message() {
        spans.push(Span::raw("  │ "));
        spans.push(Span::styled(
            status.to_owned(),
            Style::default().fg(Color::Yellow),
        ));
    }

    let help = Paragraph::new(Line::from(spans)).block(Block::default().borders(Borders::ALL));

    frame.render_widget(help, area);
}

/// Maps a [`VmState`] to its display color.
fn state_to_color(state: VmState) -> Color {
    match state {
        VmState::Running => Color::Green,
        VmState::Stopped => Color::Red,
        VmState::Creating => Color::Yellow,
        VmState::Failed => Color::Magenta,
        _ => unreachable!(),
    }
}

/// Creates a label-value line for the detail view.
fn field_line<'a>(label: &'a str, value: &'a str) -> Line<'a> {
    Line::from(vec![
        Span::styled(label, Style::default().fg(Color::Gray)),
        Span::styled(value, Style::default().fg(Color::White)),
    ])
}

/// Truncates a string to `max_len` characters, returning a reference to the slice.
fn truncate(s: &str, max_len: usize) -> &str {
    if s.len() > max_len { &s[..max_len] } else { s }
}

#[cfg(test)]
#[path = "dashboard_test.rs"]
mod tests;
