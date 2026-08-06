use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::backend::{VmInfo, VmState};
use crate::tui::app::{Action, App};

use super::*;

#[test]
fn render_confirm_stop_does_not_panic() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc123")]);
    app.handle_action(Action::Stop);

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_owned())
        .collect();
    assert!(content.contains("Stop"), "should contain Stop label");
    assert!(content.contains("vm-abc123"), "should contain VM ID");
    assert!(content.contains("[y]"), "should show yes option");
    assert!(content.contains("[n]"), "should show no option");
}

#[test]
fn render_confirm_kill_shows_kill_label() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-def456")]);
    app.handle_action(Action::Kill);

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_owned())
        .collect();
    assert!(content.contains("Kill"), "should contain Kill label");
}

#[test]
fn render_confirm_delete_shows_delete_label() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-xyz")]);
    app.handle_action(Action::Delete);

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|c| c.symbol().to_owned())
        .collect();
    assert!(content.contains("Delete"), "should contain Delete label");
}

#[test]
fn render_confirm_without_pending_does_not_panic() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let app = App::new("http://127.0.0.1:7800".to_owned());

    // No pending action — should be a no-op.
    terminal.draw(|frame| render(frame, &app)).unwrap();
}

#[test]
fn render_confirm_narrow_terminal() {
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1")]);
    app.handle_action(Action::Stop);

    // Should not panic even on narrow terminal.
    terminal.draw(|frame| render(frame, &app)).unwrap();
}

fn dummy_vm(id: &str) -> VmInfo {
    VmInfo::new(
        id.to_owned(),
        "alpine:latest".to_owned(),
        VmState::Running,
        "2025-01-01T00:00:00Z".to_owned(),
        256,
        1,
    )
}
