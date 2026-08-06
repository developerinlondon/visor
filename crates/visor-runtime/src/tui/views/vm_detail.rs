//! VM detail panel view.
//!
//! Shows full information for a single VM when the user presses Enter
//! on the dashboard VM list. Includes all fields from [`VmInfo`].

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Row, Table};

use crate::backend::{VmInfo, VmState};
use crate::tui::app::App;

/// Renders the VM detail view for the currently selected VM.
///
/// If no VM is selected, renders a placeholder message.
pub fn render(frame: &mut Frame, app: &App) {
    let area = frame.area();

    let Some(vm) = app.selected_vm() else {
        let msg = Paragraph::new("No VM selected. Press Esc to go back.")
            .block(Block::default().title(" VM Detail ").borders(Borders::ALL));
        frame.render_widget(msg, area);
        return;
    };

    let [info_area, ports_area, help_area] = Layout::vertical([
        Constraint::Min(12),
        Constraint::Length(6),
        Constraint::Length(3),
    ])
    .areas(area);

    render_info(frame, vm, info_area);
    render_ports(frame, vm, ports_area);
    render_help(frame, help_area);
}

/// Renders the VM info fields panel.
fn render_info(frame: &mut Frame, vm: &VmInfo, area: Rect) {
    let state_color = state_to_color(vm.state);
    let name_display = vm.name.as_deref().unwrap_or("<unnamed>");
    let exit_display = vm
        .exit_code
        .map_or_else(|| "—".to_owned(), |c| c.to_string());
    let memory_display = format!("{} MiB", vm.memory_mib);
    let vcpus_display = format!("{}", vm.vcpus);

    let info_lines = vec![
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
        field_line("Memory:    ", &memory_display),
        field_line("vCPUs:     ", &vcpus_display),
        field_line("Exit Code: ", &exit_display),
    ];

    let paragraph = Paragraph::new(info_lines).block(
        Block::default()
            .title(format!(" VM: {} ", vm.id))
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    );

    frame.render_widget(paragraph, area);
}

/// Renders the port mappings table or a placeholder.
fn render_ports(frame: &mut Frame, vm: &VmInfo, area: Rect) {
    if vm.ports.is_empty() {
        let no_ports = Paragraph::new("No port mappings.")
            .block(Block::default().title(" Ports ").borders(Borders::ALL));
        frame.render_widget(no_ports, area);
        return;
    }

    let header = Row::new(vec!["Host", "Guest", "Protocol"])
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

    let rows: Vec<Row> = vm
        .ports
        .iter()
        .map(|p| {
            Row::new(vec![
                format!("{}", p.host_port),
                format!("{}", p.guest_port),
                p.protocol.clone(),
            ])
        })
        .collect();

    let widths = [
        Constraint::Length(8),
        Constraint::Length(8),
        Constraint::Length(10),
    ];

    let table = Table::new(rows, widths)
        .header(header)
        .block(Block::default().title(" Ports ").borders(Borders::ALL));

    frame.render_widget(table, area);
}

/// Renders the help bar at the bottom.
fn render_help(frame: &mut Frame, area: Rect) {
    let key_style = Style::default()
        .fg(Color::Cyan)
        .add_modifier(Modifier::BOLD);

    let help = Paragraph::new(Line::from(vec![
        Span::styled("Esc", key_style),
        Span::raw(": back  "),
        Span::styled("q", key_style),
        Span::raw(": quit  "),
        Span::styled("l", key_style),
        Span::raw(": vm logs  "),
        Span::styled("e", key_style),
        Span::raw(": shell  "),
        Span::styled("o", key_style),
        Span::raw(": cons  "),
        Span::styled("s", key_style),
        Span::raw(": start/stop  "),
        Span::styled("x", key_style),
        Span::raw(": kill  "),
        Span::styled("d", key_style),
        Span::raw(": delete"),
    ]))
    .block(Block::default().borders(Borders::ALL));

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

#[cfg(test)]
#[path = "vm_detail_test.rs"]
mod tests;
