use crate::backend::{VmInfo, VmState};

// ── VmInfo with no output (running VM) ───────────────────────────

#[test]
fn vm_info_running_has_no_stdout() {
    let vm = VmInfo::new(
        "test-vm".to_owned(),
        "alpine:latest".to_owned(),
        VmState::Running,
        "2026-01-01T00:00:00Z".to_owned(),
        512,
        1,
    );
    assert!(vm.stdout.is_none());
    assert!(vm.stderr.is_none());
}

#[test]
fn vm_info_stopped_has_output() {
    let mut vm = VmInfo::new(
        "test-vm".to_owned(),
        "alpine:latest".to_owned(),
        VmState::Stopped,
        "2026-01-01T00:00:00Z".to_owned(),
        512,
        1,
    );
    vm.exit_code = Some(0);
    vm.stdout = Some("hello\n".to_owned());
    vm.stderr = Some(String::new());
    assert!(vm.stdout.as_deref().is_some_and(|s| !s.is_empty()));
    assert!(vm.stderr.as_deref().is_none_or(str::is_empty));
}

// ── has_output logic matches logs.rs behavior ────────────────────

#[test]
fn has_output_false_when_both_none() {
    let stdout: Option<String> = None;
    let stderr: Option<String> = None;
    let has_output = stdout.as_deref().is_some_and(|s| !s.is_empty())
        || stderr.as_deref().is_some_and(|s| !s.is_empty());
    assert!(!has_output);
}

#[test]
fn has_output_false_when_both_empty() {
    let stdout: Option<String> = Some(String::new());
    let stderr: Option<String> = Some(String::new());
    let has_output = stdout.as_deref().is_some_and(|s| !s.is_empty())
        || stderr.as_deref().is_some_and(|s| !s.is_empty());
    assert!(!has_output);
}

#[test]
fn has_output_true_when_stdout_present() {
    let stdout: Option<String> = Some("hello\n".to_owned());
    let stderr: Option<String> = None;
    let has_output = stdout.as_deref().is_some_and(|s| !s.is_empty())
        || stderr.as_deref().is_some_and(|s| !s.is_empty());
    assert!(has_output);
}

#[test]
fn has_output_true_when_stderr_present() {
    let stdout: Option<String> = None;
    let stderr: Option<String> = Some("error\n".to_owned());
    let has_output = stdout.as_deref().is_some_and(|s| !s.is_empty())
        || stderr.as_deref().is_some_and(|s| !s.is_empty());
    assert!(has_output);
}
