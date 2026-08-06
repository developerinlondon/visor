use ratatui::Terminal;
use ratatui::backend::TestBackend;

use crate::backend::{VmInfo, VmState};
use visor_types::PortMapping;

use super::*;

#[test]
fn render_vm_detail_shows_interactive_help() {
    let mut terminal = Terminal::new(TestBackend::new(120, 30)).unwrap();
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    let mut vm = dummy_vm("vm-help", "alpine:latest", VmState::Running);
    vm.ports = vec![PortMapping::new(8080, 80)];
    app.set_vms(vec![vm]);
    app.handle_action(crate::tui::app::Action::Enter);

    terminal.draw(|frame| render(frame, &app)).unwrap();

    let buf = terminal.backend().buffer().clone();
    let content: String = buf
        .content()
        .iter()
        .map(|cell| cell.symbol().to_owned())
        .collect();
    assert!(content.contains("logs"), "should show logs shortcut");
    assert!(content.contains("shell"), "should show shell shortcut");
    assert!(content.contains("cons"), "should show console shortcut");
    assert!(content.contains("delete"), "should show delete shortcut");
}

fn dummy_vm(id: &str, image: &str, state: VmState) -> VmInfo {
    VmInfo::new(
        id.to_owned(),
        image.to_owned(),
        state,
        "2026-03-09T12:00:00Z".to_owned(),
        256,
        1,
    )
}
