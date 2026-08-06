use super::*;

// ── View navigation ─────────────────────────────────────────────────

#[test]
fn default_app_starts_on_dashboard() {
    let app = App::new("http://127.0.0.1:7800".to_owned());
    assert_eq!(app.current_view(), View::Dashboard);
    assert!(app.vms().is_empty());
    assert!(app.events().is_empty());
    assert!(!app.should_quit());
}

#[test]
fn pressing_q_sets_quit() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.handle_action(Action::Quit);
    assert!(app.should_quit());
}

#[test]
fn navigate_down_increments_selected() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1"), dummy_vm("vm2"), dummy_vm("vm3")]);
    assert_eq!(app.selected_vm_index(), 0);

    app.handle_action(Action::Down);
    assert_eq!(app.selected_vm_index(), 1);

    app.handle_action(Action::Down);
    assert_eq!(app.selected_vm_index(), 2);
}

#[test]
fn navigate_down_does_not_exceed_list_bounds() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1"), dummy_vm("vm2")]);

    app.handle_action(Action::Down);
    app.handle_action(Action::Down);
    app.handle_action(Action::Down);
    assert_eq!(app.selected_vm_index(), 1);
}

#[test]
fn navigate_up_decrements_selected() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1"), dummy_vm("vm2"), dummy_vm("vm3")]);
    app.handle_action(Action::Down);
    app.handle_action(Action::Down);
    assert_eq!(app.selected_vm_index(), 2);

    app.handle_action(Action::Up);
    assert_eq!(app.selected_vm_index(), 1);
}

#[test]
fn navigate_up_does_not_go_below_zero() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1")]);

    app.handle_action(Action::Up);
    assert_eq!(app.selected_vm_index(), 0);
}

#[test]
fn enter_navigates_to_vm_detail() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1")]);

    app.handle_action(Action::Enter);
    assert_eq!(app.current_view(), View::VmDetail);
}

#[test]
fn enter_on_empty_list_stays_on_dashboard() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.handle_action(Action::Enter);
    assert_eq!(app.current_view(), View::Dashboard);
}

#[test]
fn escape_returns_to_dashboard() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1")]);
    app.handle_action(Action::Enter);
    assert_eq!(app.current_view(), View::VmDetail);

    app.handle_action(Action::Back);
    assert_eq!(app.current_view(), View::Dashboard);
}

#[test]
fn tab_switches_active_pane() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    assert_eq!(app.active_pane(), Pane::VmList);

    app.handle_action(Action::SwitchPane);
    assert_eq!(app.active_pane(), Pane::Metrics);

    app.handle_action(Action::SwitchPane);
    assert_eq!(app.active_pane(), Pane::Events);

    app.handle_action(Action::SwitchPane);
    assert_eq!(app.active_pane(), Pane::VmList);
}

#[test]
fn navigate_to_logs_view() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1")]);
    app.handle_action(Action::ToggleLogs);
    assert_eq!(app.current_view(), View::Logs);
    assert_eq!(app.logs_vm_id(), Some("vm1"));

    app.handle_action(Action::Back);
    assert_eq!(app.current_view(), View::Dashboard);
    assert!(app.logs().is_none());
}

#[test]
fn toggle_logs_without_selected_vm_stays_on_dashboard() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());

    app.handle_action(Action::ToggleLogs);

    assert_eq!(app.current_view(), View::Dashboard);
    assert!(app.logs().is_none());
}

// ── Metrics ─────────────────────────────────────────────────────────

#[test]
fn metrics_summary_computes_from_vms() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    let mut vm1 = dummy_vm("vm1");
    vm1.state = crate::backend::VmState::Running;
    vm1.memory_mib = 256;
    vm1.vcpus = 2;
    let mut vm2 = dummy_vm("vm2");
    vm2.state = crate::backend::VmState::Stopped;
    vm2.memory_mib = 512;
    vm2.vcpus = 1;
    let mut vm3 = dummy_vm("vm3");
    vm3.state = crate::backend::VmState::Running;
    vm3.memory_mib = 256;
    vm3.vcpus = 4;

    app.set_vms(vec![vm1, vm2, vm3]);
    let metrics = app.compute_metrics();

    assert_eq!(metrics.total_vms, 3);
    assert_eq!(metrics.running_vms, 2);
    assert_eq!(metrics.total_memory_mib, 1024);
    assert_eq!(metrics.total_vcpus, 7);
    assert_eq!(metrics.pool_warm_count, 0);
    assert_eq!(metrics.pool_target, 0);
}

#[test]
fn set_pool_status_stores_values() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    assert_eq!(app.pool_warm_count(), 0);
    assert_eq!(app.pool_target(), 0);

    app.set_pool_status(3, 5);
    assert_eq!(app.pool_warm_count(), 3);
    assert_eq!(app.pool_target(), 5);
}

#[test]
fn pool_status_reflected_in_metrics() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_pool_status(2, 4);
    let metrics = app.compute_metrics();
    assert_eq!(metrics.pool_warm_count, 2);
    assert_eq!(metrics.pool_target, 4);
}

#[test]
fn compute_metrics_total_vcpus_sums_all_vms() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    let mut vm1 = dummy_vm("vm1");
    vm1.vcpus = 4;
    let mut vm2 = dummy_vm("vm2");
    vm2.vcpus = 2;
    app.set_vms(vec![vm1, vm2]);
    let metrics = app.compute_metrics();
    assert_eq!(metrics.total_vcpus, 6);
}

// ── Events ──────────────────────────────────────────────────────────

#[test]
fn push_event_appends_to_list() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.push_event(TuiEvent {
        timestamp: "12:00:01".to_owned(),
        event_type: "vm.created".to_owned(),
        vm_id: "vm1".to_owned(),
    });

    assert_eq!(app.events().len(), 1);
    assert_eq!(app.events()[0].event_type, "vm.created");
}

#[test]
fn events_capped_at_max() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    for i in 0..MAX_EVENTS + 10 {
        app.push_event(TuiEvent {
            timestamp: format!("12:00:{i:02}"),
            event_type: "vm.created".to_owned(),
            vm_id: format!("vm{i}"),
        });
    }

    assert_eq!(app.events().len(), MAX_EVENTS);
}

// ── Selected VM ─────────────────────────────────────────────────────

#[test]
fn selected_vm_returns_correct_vm() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1"), dummy_vm("vm2")]);
    app.handle_action(Action::Down);

    let vm = app.selected_vm();
    assert!(vm.is_some());
    assert_eq!(vm.unwrap().id, "vm2");
}

#[test]
fn set_logs_from_vm_captures_stdout_and_stderr() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    let mut vm = dummy_vm("vm-logs");
    vm.stdout = Some("hello\n".to_owned());
    vm.stderr = Some("warn\n".to_owned());

    app.set_logs_from_vm(&vm);

    let logs = app.logs().expect("logs should be present");
    assert_eq!(logs.vm_id, "vm-logs");
    assert_eq!(logs.stdout, "hello\n");
    assert_eq!(logs.stderr, "warn\n");
}

#[test]
fn set_vms_refreshes_open_logs_snapshot() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    let mut vm = dummy_vm("vm-logs");
    vm.stdout = Some("old\n".to_owned());
    app.set_vms(vec![vm.clone()]);
    app.handle_action(Action::ToggleLogs);

    let mut refreshed_vm = vm;
    refreshed_vm.stdout = Some("new\n".to_owned());
    app.set_vms(vec![refreshed_vm]);

    let logs = app.logs().expect("logs should still be present");
    assert_eq!(logs.stdout, "new\n");
}

#[test]
fn selected_vm_none_when_empty() {
    let app = App::new("http://127.0.0.1:7800".to_owned());
    assert!(app.selected_vm().is_none());
}

#[test]
fn set_vms_clamps_selected_index() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm1"), dummy_vm("vm2"), dummy_vm("vm3")]);
    app.handle_action(Action::Down);
    app.handle_action(Action::Down);
    assert_eq!(app.selected_vm_index(), 2);

    // Shrink the list — index should clamp.
    app.set_vms(vec![dummy_vm("vm1")]);
    assert_eq!(app.selected_vm_index(), 0);
}

// ── Stop / Kill / Delete actions ────────────────────────────────────

#[test]
fn stop_action_sets_pending_stop() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc"), dummy_vm("vm-def")]);
    app.handle_action(Action::Down); // select vm-def

    app.handle_action(Action::Stop);
    assert!(app.has_pending_action());
    assert!(
        matches!(app.pending_action(), Some(PendingAction::Stop { vm_id }) if vm_id == "vm-def")
    );
}

#[test]
fn kill_action_sets_pending_kill() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc")]);

    app.handle_action(Action::Kill);
    assert!(app.has_pending_action());
    assert!(
        matches!(app.pending_action(), Some(PendingAction::Kill { vm_id }) if vm_id == "vm-abc")
    );
}

#[test]
fn delete_action_sets_pending_delete() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc")]);

    app.handle_action(Action::Delete);
    assert!(app.has_pending_action());
    assert!(
        matches!(app.pending_action(), Some(PendingAction::Delete { vm_id }) if vm_id == "vm-abc")
    );
}

#[test]
fn stop_on_empty_list_does_nothing() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.handle_action(Action::Stop);
    assert!(!app.has_pending_action());
}

#[test]
fn kill_on_empty_list_does_nothing() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.handle_action(Action::Kill);
    assert!(!app.has_pending_action());
}

#[test]
fn delete_on_empty_list_does_nothing() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.handle_action(Action::Delete);
    assert!(!app.has_pending_action());
}

// ── Confirmation flow ───────────────────────────────────────────────

#[test]
fn confirm_moves_pending_to_confirmed() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc")]);
    app.handle_action(Action::Stop);
    assert!(app.has_pending_action());

    app.handle_action(Action::Confirm);
    assert!(!app.has_pending_action());

    let confirmed = app.take_confirmed_action();
    assert!(matches!(confirmed, Some(PendingAction::Stop { vm_id }) if vm_id == "vm-abc"));
}

#[test]
fn cancel_clears_pending_action() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc")]);
    app.handle_action(Action::Kill);
    assert!(app.has_pending_action());

    app.handle_action(Action::Cancel);
    assert!(!app.has_pending_action());
    assert!(app.take_confirmed_action().is_none());
}

#[test]
fn confirm_without_pending_does_nothing() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.handle_action(Action::Confirm);
    assert!(app.take_confirmed_action().is_none());
}

#[test]
fn cancel_without_pending_does_nothing() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.handle_action(Action::Cancel);
    assert!(!app.has_pending_action());
}

#[test]
fn take_confirmed_action_clears_after_take() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc")]);
    app.handle_action(Action::Delete);
    app.handle_action(Action::Confirm);

    // First take returns the action.
    assert!(app.take_confirmed_action().is_some());
    // Second take returns None.
    assert!(app.take_confirmed_action().is_none());
}

// ── Status message ──────────────────────────────────────────────────

#[test]
fn set_status_and_read_back() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    assert!(app.status_message().is_none());

    app.set_status("VM stopped successfully".to_owned());
    assert_eq!(app.status_message(), Some("VM stopped successfully"));
}

#[test]
fn open_shell_action_does_not_change_view_state() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc")]);

    app.handle_action(Action::OpenShell);
    assert_eq!(app.current_view(), View::Dashboard);
    assert!(!app.has_pending_action());
}

#[test]
fn open_console_action_does_not_change_view_state() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc")]);

    app.handle_action(Action::OpenConsole);
    assert_eq!(app.current_view(), View::Dashboard);
    assert!(!app.has_pending_action());
}

#[test]
fn start_action_does_not_change_view_state() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.set_vms(vec![dummy_vm("vm-abc")]);

    app.handle_action(Action::Start);
    assert_eq!(app.current_view(), View::Dashboard);
    assert!(!app.has_pending_action());
}

#[test]
fn new_app_has_no_pending_action() {
    let app = App::new("http://127.0.0.1:7800".to_owned());
    assert!(!app.has_pending_action());
    assert!(app.pending_action().is_none());
    assert!(app.status_message().is_none());
}

// ── Create VM form ─────────────────────────────────────────────────

#[test]
fn create_new_action_opens_form() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    assert!(!app.has_create_form());

    app.handle_action(Action::CreateNew);
    assert!(app.has_create_form());
}

#[test]
fn close_create_form_clears_state() {
    let mut app = App::new("http://127.0.0.1:7800".to_owned());
    app.open_create_form();
    assert!(app.has_create_form());

    app.close_create_form();
    assert!(!app.has_create_form());
}

#[test]
fn create_form_defaults() {
    let form = CreateVmForm::new();
    assert_eq!(form.image_preset, 0);
    assert!(!form.image_is_custom);
    assert!(form.name.is_empty());
    assert_eq!(form.memory_preset, 1); // 128 MiB
    assert!(!form.memory_is_custom);
    assert_eq!(form.vcpus, "1");
    assert!(form.cmd.is_empty());
    assert_eq!(form.selected_row, 0);
    assert_eq!(form.button_index, 0);
    assert!(form.error.is_none());
}

#[test]
fn create_form_image_preset_cycling() {
    let mut form = CreateVmForm::new();
    assert_eq!(form.image_value(), "alpine:latest");

    form.cycle_right();
    assert_eq!(form.image_value(), "ubuntu:22.04");

    form.cycle_left();
    assert_eq!(form.image_value(), "alpine:latest");

    form.cycle_left(); // wraps to last
    assert_eq!(form.image_value(), "redis:latest");
}

#[test]
fn create_form_typing_switches_to_custom() {
    let mut form = CreateVmForm::new();
    assert!(!form.image_is_custom);

    form.insert_char('m');
    assert!(form.image_is_custom);
    assert_eq!(form.image_custom, "m");
    assert_eq!(form.image_value(), "m");
}

#[test]
fn create_form_backspace_reverts_to_preset() {
    let mut form = CreateVmForm::new();
    form.insert_char('x');
    assert!(form.image_is_custom);

    form.delete_char(); // deletes 'x', now empty
    form.delete_char(); // empty custom → reverts to preset
    assert!(!form.image_is_custom);
}

#[test]
fn create_form_row_navigation() {
    let mut form = CreateVmForm::new();
    assert_eq!(form.selected_row, 0);

    form.move_down();
    assert_eq!(form.selected_row, 1); // Name

    form.move_down();
    assert_eq!(form.selected_row, 2); // Memory

    form.move_up();
    assert_eq!(form.selected_row, 1); // back to Name
}

#[test]
fn create_form_row_navigation_clamps() {
    let mut form = CreateVmForm::new();
    form.move_up(); // already at 0
    assert_eq!(form.selected_row, 0);

    for _ in 0..10 {
        form.move_down();
    }
    assert_eq!(form.selected_row, CreateVmForm::row_count() - 1);
}

#[test]
fn create_form_button_cycling() {
    let mut form = CreateVmForm::new();
    form.selected_row = 5;
    assert_eq!(form.button_index, 0);

    form.cycle_right();
    assert_eq!(form.button_index, 1);

    form.cycle_right();
    assert_eq!(form.button_index, 0);
}

#[test]
fn create_form_text_input_on_name_field() {
    let mut form = CreateVmForm::new();
    form.move_down(); // row 1: Name
    form.insert_char('m');
    form.insert_char('y');
    assert_eq!(form.name, "my");
    assert_eq!(form.cursor_pos, 2);
}

#[test]
fn create_form_memory_preset_value() {
    let form = CreateVmForm::new();
    assert_eq!(form.memory_mib(), Ok(128));
}

#[test]
fn create_form_memory_custom_valid() {
    let mut form = CreateVmForm::new();
    form.memory_is_custom = true;
    form.memory_custom = "256".to_owned();
    assert_eq!(form.memory_mib(), Ok(256));
}

#[test]
fn create_form_memory_custom_too_small() {
    let mut form = CreateVmForm::new();
    form.memory_is_custom = true;
    form.memory_custom = "32".to_owned();
    assert!(form.memory_mib().is_err());
}

#[test]
fn create_form_is_text_input_active() {
    let mut form = CreateVmForm::new();
    assert!(!form.is_text_input_active());

    form.image_is_custom = true;
    assert!(form.is_text_input_active());

    form.selected_row = 1;
    assert!(form.is_text_input_active());

    form.selected_row = 5;
    assert!(!form.is_text_input_active());
}

// ── Helpers ─────────────────────────────────────────────────────────

fn dummy_vm(id: &str) -> crate::backend::VmInfo {
    crate::backend::VmInfo::new(
        id.to_owned(),
        "alpine:latest".to_owned(),
        crate::backend::VmState::Running,
        "2025-01-01T00:00:00Z".to_owned(),
        256,
        1,
    )
}
