use std::collections::HashMap;

use ratatui::crossterm::event::KeyCode;

use super::*;
use crate::pool::manager::{ImagePoolStatus, PoolStatus};

#[test]
fn dashboard_keymap_includes_shell_and_console() {
    assert_eq!(
        map_key(KeyCode::Char('e'), View::Dashboard, None, false),
        Some(Action::OpenShell)
    );
    assert_eq!(
        map_key(KeyCode::Char('o'), View::Dashboard, None, false),
        Some(Action::OpenConsole)
    );
}

#[test]
fn vm_detail_keymap_includes_logs_shell_and_console() {
    assert_eq!(
        map_key(KeyCode::Char('l'), View::VmDetail, None, false),
        Some(Action::ToggleLogs)
    );
    assert_eq!(
        map_key(KeyCode::Char('e'), View::VmDetail, None, false),
        Some(Action::OpenShell)
    );
    assert_eq!(
        map_key(KeyCode::Char('o'), View::VmDetail, None, false),
        Some(Action::OpenConsole)
    );
}

#[test]
fn lifecycle_key_stops_running_vms_and_starts_stopped_vms() {
    assert_eq!(
        map_key(
            KeyCode::Char('s'),
            View::Dashboard,
            Some(crate::backend::VmState::Running),
            false,
        ),
        Some(Action::Stop)
    );
    assert_eq!(
        map_key(
            KeyCode::Char('s'),
            View::Dashboard,
            Some(crate::backend::VmState::Stopped),
            false,
        ),
        Some(Action::Start)
    );
    assert_eq!(
        map_key(
            KeyCode::Char('s'),
            View::Dashboard,
            Some(crate::backend::VmState::Failed),
            false,
        ),
        Some(Action::Start)
    );
}

#[test]
fn nested_cli_args_target_shell() {
    let args = nested_cli_args("http://127.0.0.1:7800", "vm-123", VmSurface::Shell);
    assert_eq!(
        args,
        vec!["--addr", "http://127.0.0.1:7800", "shell", "vm-123"]
    );
}

#[test]
fn nested_cli_args_target_console() {
    let args = nested_cli_args("http://127.0.0.1:7800", "vm-123", VmSurface::Console);
    assert_eq!(
        args,
        vec!["--addr", "http://127.0.0.1:7800", "console", "vm-123"]
    );
}

#[test]
fn take_sse_frames_extracts_complete_messages() {
    let mut buffer = String::from("data: one\n\ndata: two\n\npartial");
    let frames = take_sse_frames(&mut buffer);

    assert_eq!(frames, vec!["data: one", "data: two"]);
    assert_eq!(buffer, "partial");
}

#[test]
fn parse_sse_frame_decodes_vm_event_payload() {
    let frame = concat!(
        "event: vm.created\n",
        "data: {\"event_type\":\"vm.created\",\"vm_id\":\"vm-123\",\"timestamp\":\"2026-03-09T19:40:01Z\",\"data\":null}\n"
    );

    let event = parse_sse_frame(frame).expect("expected event");

    assert_eq!(event.event_type, "vm.created");
    assert_eq!(event.vm_id, "vm-123");
    assert_eq!(event.timestamp, "19:40:01");
}

#[test]
fn parse_sse_frame_ignores_keepalive_comments() {
    assert!(parse_sse_frame(": keepalive").is_none());
}

#[test]
fn display_timestamp_falls_back_for_non_iso_values() {
    assert_eq!(display_timestamp("custom"), "custom");
}

#[test]
fn pool_totals_sum_available_and_target_counts() {
    let status = PoolStatus {
        images: HashMap::from([
            (
                "alpine:latest".to_owned(),
                ImagePoolStatus {
                    available: 2,
                    target: 3,
                },
            ),
            (
                "nginx:latest".to_owned(),
                ImagePoolStatus {
                    available: 1,
                    target: 4,
                },
            ),
        ]),
        total: 3,
    };

    assert_eq!(pool_totals(&status), (3, 7));
}

#[test]
fn append_log_section_uses_placeholder_for_empty_content() {
    let mut lines = Vec::new();

    append_log_section(&mut lines, "STDOUT", "", ratatui::style::Color::White);

    assert_eq!(lines.len(), 2);
    assert_eq!(lines[0].to_string(), "STDOUT");
    assert_eq!(lines[1].to_string(), "  (no output captured)");
}

#[test]
fn append_log_section_splits_multiline_content() {
    let mut lines = Vec::new();

    append_log_section(
        &mut lines,
        "STDERR",
        "first line\nsecond line",
        ratatui::style::Color::LightRed,
    );

    assert_eq!(lines.len(), 3);
    assert_eq!(lines[0].to_string(), "STDERR");
    assert_eq!(lines[1].to_string(), "first line");
    assert_eq!(lines[2].to_string(), "second line");
}
