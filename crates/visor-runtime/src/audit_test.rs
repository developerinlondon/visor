use super::*;
use std::sync::{Arc, Mutex};
use tracing_subscriber::layer::SubscriberExt;

// ── Helper: capture tracing output ───────────────────────────────────

/// A simple layer that captures formatted log lines for assertion.
struct CapturingLayer {
    lines: Arc<Mutex<Vec<String>>>,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CapturingLayer {
    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        let mut visitor = StringVisitor::default();
        event.record(&mut visitor);
        if let Ok(mut lines) = self.lines.lock() {
            lines.push(visitor.output);
        }
    }
}

#[derive(Default)]
struct StringVisitor {
    output: String,
}

impl tracing::field::Visit for StringVisitor {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        use std::fmt::Write;
        let _ = write!(self.output, "{}={:?} ", field.name(), value);
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        use std::fmt::Write;
        let _ = write!(self.output, "{}={} ", field.name(), value);
    }
}

fn capturing_subscriber() -> (impl tracing::Subscriber, Arc<Mutex<Vec<String>>>) {
    let lines = Arc::new(Mutex::new(Vec::new()));
    let layer = CapturingLayer {
        lines: Arc::clone(&lines),
    };
    let subscriber = tracing_subscriber::registry().with(layer);
    (subscriber, lines)
}

// ── AuditEvent::new ──────────────────────────────────────────────────

#[test]
fn new_event_sets_action_and_outcome() {
    let event = AuditEvent::new(AuditAction::VmCreate, AuditOutcome::Success);
    assert_eq!(event.action, AuditAction::VmCreate);
    assert_eq!(event.outcome, AuditOutcome::Success);
}

#[test]
fn new_event_has_valid_iso8601_timestamp() {
    let event = AuditEvent::new(AuditAction::DaemonStart, AuditOutcome::Success);
    assert!(
        event.timestamp.contains('T'),
        "timestamp must contain 'T': {}",
        event.timestamp
    );
    assert!(
        event.timestamp.ends_with('Z'),
        "timestamp must end with 'Z': {}",
        event.timestamp
    );
}

#[test]
fn new_event_target_is_none() {
    let event = AuditEvent::new(AuditAction::VmCreate, AuditOutcome::Success);
    assert!(event.target.is_none());
}

#[test]
fn new_event_detail_is_none() {
    let event = AuditEvent::new(AuditAction::VmCreate, AuditOutcome::Success);
    assert!(event.detail.is_none());
}

// ── Builder pattern ──────────────────────────────────────────────────

#[test]
fn with_target_sets_target() {
    let event =
        AuditEvent::new(AuditAction::VmDestroy, AuditOutcome::Success).with_target("vm-abc-123");
    assert_eq!(event.target.as_deref(), Some("vm-abc-123"));
}

#[test]
fn with_detail_sets_detail() {
    let event = AuditEvent::new(AuditAction::VmExec, AuditOutcome::Failure)
        .with_detail("command timed out");
    assert_eq!(event.detail.as_deref(), Some("command timed out"));
}

#[test]
fn builder_chains_target_and_detail() {
    let event = AuditEvent::new(AuditAction::VmStop, AuditOutcome::Success)
        .with_target("vm-456")
        .with_detail("graceful shutdown");
    assert_eq!(event.action, AuditAction::VmStop);
    assert_eq!(event.outcome, AuditOutcome::Success);
    assert_eq!(event.target.as_deref(), Some("vm-456"));
    assert_eq!(event.detail.as_deref(), Some("graceful shutdown"));
}

// ── AuditAction serialization ────────────────────────────────────────

#[test]
fn audit_action_vm_create_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditAction::VmCreate).unwrap(),
        r#""vm_create""#,
    );
}

#[test]
fn audit_action_vm_destroy_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditAction::VmDestroy).unwrap(),
        r#""vm_destroy""#,
    );
}

#[test]
fn audit_action_vm_stop_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditAction::VmStop).unwrap(),
        r#""vm_stop""#,
    );
}

#[test]
fn audit_action_vm_exec_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditAction::VmExec).unwrap(),
        r#""vm_exec""#,
    );
}

#[test]
fn audit_action_daemon_start_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditAction::DaemonStart).unwrap(),
        r#""daemon_start""#,
    );
}

#[test]
fn audit_action_daemon_stop_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditAction::DaemonStop).unwrap(),
        r#""daemon_stop""#,
    );
}

// ── AuditOutcome serialization ───────────────────────────────────────

#[test]
fn audit_outcome_success_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditOutcome::Success).unwrap(),
        r#""success""#,
    );
}

#[test]
fn audit_outcome_failure_serializes_to_snake_case() {
    assert_eq!(
        serde_json::to_string(&AuditOutcome::Failure).unwrap(),
        r#""failure""#,
    );
}

// ── AuditAction deserialization ──────────────────────────────────────

#[test]
fn audit_action_deserializes_all_variants() {
    assert_eq!(
        serde_json::from_str::<AuditAction>(r#""vm_create""#).unwrap(),
        AuditAction::VmCreate,
    );
    assert_eq!(
        serde_json::from_str::<AuditAction>(r#""vm_destroy""#).unwrap(),
        AuditAction::VmDestroy,
    );
    assert_eq!(
        serde_json::from_str::<AuditAction>(r#""vm_stop""#).unwrap(),
        AuditAction::VmStop,
    );
    assert_eq!(
        serde_json::from_str::<AuditAction>(r#""vm_exec""#).unwrap(),
        AuditAction::VmExec,
    );
    assert_eq!(
        serde_json::from_str::<AuditAction>(r#""daemon_start""#).unwrap(),
        AuditAction::DaemonStart,
    );
    assert_eq!(
        serde_json::from_str::<AuditAction>(r#""daemon_stop""#).unwrap(),
        AuditAction::DaemonStop,
    );
}

// ── AuditOutcome deserialization ─────────────────────────────────────

#[test]
fn audit_outcome_deserializes_all_variants() {
    assert_eq!(
        serde_json::from_str::<AuditOutcome>(r#""success""#).unwrap(),
        AuditOutcome::Success,
    );
    assert_eq!(
        serde_json::from_str::<AuditOutcome>(r#""failure""#).unwrap(),
        AuditOutcome::Failure,
    );
}

// ── AuditEvent JSON round-trip ───────────────────────────────────────

#[test]
fn audit_event_json_round_trip() {
    let event = AuditEvent::new(AuditAction::VmCreate, AuditOutcome::Success)
        .with_target("vm-789")
        .with_detail("created from alpine:latest");

    let json = serde_json::to_string(&event).unwrap();
    let deserialized: AuditEvent = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.action, AuditAction::VmCreate);
    assert_eq!(deserialized.outcome, AuditOutcome::Success);
    assert_eq!(deserialized.target.as_deref(), Some("vm-789"));
    assert_eq!(
        deserialized.detail.as_deref(),
        Some("created from alpine:latest"),
    );
    assert_eq!(deserialized.timestamp, event.timestamp);
}

#[test]
fn audit_event_json_round_trip_without_optionals() {
    let event = AuditEvent::new(AuditAction::DaemonStart, AuditOutcome::Success);

    let json = serde_json::to_string(&event).unwrap();
    let deserialized: AuditEvent = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.action, AuditAction::DaemonStart);
    assert_eq!(deserialized.outcome, AuditOutcome::Success);
    assert!(deserialized.target.is_none());
    assert!(deserialized.detail.is_none());
}

// ── Clone trait ──────────────────────────────────────────────────────

#[test]
fn audit_event_clone_produces_equal_copy() {
    let event = AuditEvent::new(AuditAction::VmExec, AuditOutcome::Failure)
        .with_target("vm-clone-test")
        .with_detail("testing clone");
    let cloned = event.clone();

    assert_eq!(cloned.timestamp, event.timestamp);
    assert_eq!(cloned.action, event.action);
    assert_eq!(cloned.outcome, event.outcome);
    assert_eq!(cloned.target, event.target);
    assert_eq!(cloned.detail, event.detail);
}

// ── emit / log_success / log_failure ─────────────────────────────────

#[test]
fn emit_writes_to_tracing() {
    let (subscriber, lines) = capturing_subscriber();
    tracing::subscriber::with_default(subscriber, || {
        let event = AuditEvent::new(AuditAction::VmCreate, AuditOutcome::Success)
            .with_target("vm-emit-test");
        emit(&event);
    });

    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 1, "expected exactly one tracing event");
    let line = &captured[0];
    assert!(line.contains("vm_create"), "missing action in: {line}");
    assert!(line.contains("success"), "missing outcome in: {line}");
    assert!(line.contains("vm-emit-test"), "missing target in: {line}");
}

#[test]
fn log_success_emits_success_event() {
    let (subscriber, lines) = capturing_subscriber();
    tracing::subscriber::with_default(subscriber, || {
        log_success(
            AuditAction::DaemonStart,
            None,
            Some("listening on /run/visor.sock"),
        );
    });

    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let line = &captured[0];
    assert!(line.contains("daemon_start"), "missing action in: {line}");
    assert!(line.contains("success"), "missing outcome in: {line}");
    assert!(
        line.contains("listening on /run/visor.sock"),
        "missing detail in: {line}",
    );
}

#[test]
fn log_failure_emits_failure_event() {
    let (subscriber, lines) = capturing_subscriber();
    tracing::subscriber::with_default(subscriber, || {
        log_failure(
            AuditAction::VmDestroy,
            Some("vm-fail-test"),
            Some("vm not found"),
        );
    });

    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let line = &captured[0];
    assert!(line.contains("vm_destroy"), "missing action in: {line}");
    assert!(line.contains("failure"), "missing outcome in: {line}");
    assert!(line.contains("vm-fail-test"), "missing target in: {line}");
    assert!(line.contains("vm not found"), "missing detail in: {line}");
}

#[test]
fn log_success_with_target_and_detail() {
    let (subscriber, lines) = capturing_subscriber();
    tracing::subscriber::with_default(subscriber, || {
        log_success(
            AuditAction::VmExec,
            Some("vm-exec-123"),
            Some("ran /bin/ls"),
        );
    });

    let captured = lines.lock().unwrap();
    assert_eq!(captured.len(), 1);
    let line = &captured[0];
    assert!(line.contains("vm_exec"), "missing action in: {line}");
    assert!(line.contains("success"), "missing outcome in: {line}");
    assert!(line.contains("vm-exec-123"), "missing target in: {line}");
    assert!(line.contains("ran /bin/ls"), "missing detail in: {line}");
}

// ── Timestamp format validation ──────────────────────────────────────

#[test]
fn timestamp_format_is_valid_iso8601() {
    let event = AuditEvent::new(AuditAction::VmCreate, AuditOutcome::Success);
    let ts = &event.timestamp;

    // Must be "YYYY-MM-DDTHH:MM:SSZ" format
    assert_eq!(
        ts.len(),
        20,
        "expected 20-char ISO 8601 timestamp, got {}: {ts}",
        ts.len()
    );
    assert_eq!(&ts[4..5], "-", "expected '-' at position 4: {ts}");
    assert_eq!(&ts[7..8], "-", "expected '-' at position 7: {ts}");
    assert_eq!(&ts[10..11], "T", "expected 'T' at position 10: {ts}");
    assert_eq!(&ts[13..14], ":", "expected ':' at position 13: {ts}");
    assert_eq!(&ts[16..17], ":", "expected ':' at position 16: {ts}");
    assert_eq!(&ts[19..20], "Z", "expected 'Z' at position 19: {ts}");

    // Year, month, day should be numeric
    assert!(
        ts[0..4].chars().all(|c| c.is_ascii_digit()),
        "year not numeric: {ts}"
    );
    assert!(
        ts[5..7].chars().all(|c| c.is_ascii_digit()),
        "month not numeric: {ts}"
    );
    assert!(
        ts[8..10].chars().all(|c| c.is_ascii_digit()),
        "day not numeric: {ts}"
    );
}

// ── Display impl for AuditAction ─────────────────────────────────────

#[test]
fn audit_action_display_matches_serialization() {
    let action = AuditAction::VmCreate;
    let display = format!("{action}");
    assert_eq!(display, "vm_create");
}

#[test]
fn audit_outcome_display_matches_serialization() {
    let outcome = AuditOutcome::Success;
    let display = format!("{outcome}");
    assert_eq!(display, "success");
}
