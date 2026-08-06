use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::backend::{VmInfo, VmState};
use crate::tui::app::{App, TuiEvent};
use visor_types::PortMapping;

use super::*;

// ── Layout ──────────────────────────────────────────────────────────

#[test]
fn render_empty_dashboard_does_not_panic() {
    let mut terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    let app = App::new("http://127.0.0.1:7800".to_owned());

    terminal.draw(|frame| render(frame, &app)).unwrap();
}

#[test]
fn render_dashboard_with_vms() {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![
        dummy_vm("vm-abc123", "alpine:latest", VmState::Running, 256),
        dummy_vm("vm-def456", "ubuntu:22.04", VmState::Stopped, 512),
    ]);
    app.push_event(TuiEvent {
        timestamp: "12:00:01".to_owned(),
        event_type: "vm.created".to_owned(),
        vm_id: "vm-abc123".to_owned(),
    });

    terminal.draw(|frame| render(frame, &app)).unwrap();

    // Verify buffer contains expected text fragments.
    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect();
    assert!(content.contains("VMs"), "should contain VMs header");
    assert!(content.contains("Metrics"), "should contain Metrics header");
    assert!(content.contains("shell"), "should show shell shortcut help");
    assert!(
        content.contains("cons"),
        "should show console shortcut help"
    );
    assert!(content.contains("rm"), "should show delete shortcut help");
}

#[test]
fn render_dashboard_narrow_terminal() {
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm(
        "vm1",
        "alpine:latest",
        VmState::Running,
        256,
    )]);

    // Should not panic even with very narrow terminal.
    terminal.draw(|frame| render(frame, &app)).unwrap();
}

#[test]
fn render_dashboard_shows_ports_in_detail() {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    let mut vm = dummy_vm("vm-ports", "alpine:latest", VmState::Running, 256);
    vm.ports = vec![PortMapping::new(8080, 80), PortMapping::new(9090, 90)];
    app.set_vms(vec![vm]);

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect();
    // Ports should appear in the detail panel (right side)
    assert!(
        content.contains("8080"),
        "should show port 8080 in detail panel"
    );
}

#[test]
fn render_dashboard_shows_pool_status() {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_pool_status(3, 5);

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect();
    assert!(content.contains("Pool:"), "should contain Pool: label");
    assert!(content.contains("3/5"), "should show warm/target counts");
}

#[test]
fn render_dashboard_shows_selected_vm_detail() {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    let mut vm = dummy_vm("vm-detail-test", "alpine:3.19", VmState::Running, 512);
    vm.name = Some("test-container".to_owned());
    app.set_vms(vec![vm]);

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect();
    assert!(
        content.contains("vm-detail-test"),
        "detail panel should show VM ID"
    );
    assert!(
        content.contains("alpine:3.19"),
        "detail panel should show image"
    );
    assert!(
        content.contains("test-container"),
        "detail panel should show VM name"
    );
}

#[test]
fn render_dashboard_empty_shows_no_vm_selected() {
    let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
    let app = App::new("http://127.0.0.1:7800".to_owned());

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect();
    assert!(
        content.contains("No VM selected"),
        "should show placeholder when no VMs exist"
    );
}

// ── Helpers ─────────────────────────────────────────────────────────

fn dummy_vm(id: &str, image: &str, state: VmState, memory: u32) -> VmInfo {
    VmInfo::new(
        id.to_owned(),
        image.to_owned(),
        state,
        "2025-01-01T00:00:00Z".to_owned(),
        memory,
        1,
    )
}
