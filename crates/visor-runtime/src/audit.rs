//! Structured JSON audit logging for API operations.
//!
//! Every API call that modifies state (create, exec, destroy) produces an
//! audit event with timestamp, action, target, and result. Events are emitted
//! via the `tracing` framework with structured JSON fields.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::time::{SystemTime, UNIX_EPOCH};
use utoipa::ToSchema;

/// Actions that generate audit events.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditAction {
    /// A VM was created.
    VmCreate,
    /// A VM was destroyed.
    VmDestroy,
    /// A VM was stopped.
    VmStop,
    /// A command was executed inside a VM.
    VmExec,
    /// The daemon started.
    DaemonStart,
    /// The daemon stopped.
    DaemonStop,
}

impl fmt::Display for AuditAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = serde_json::to_string(self).unwrap_or_default();
        // Strip surrounding quotes from JSON string
        let trimmed = s.trim_matches('"');
        f.write_str(trimmed)
    }
}

/// Outcome of an audited operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum AuditOutcome {
    /// The operation completed successfully.
    Success,
    /// The operation failed.
    Failure,
}

impl fmt::Display for AuditOutcome {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = serde_json::to_string(self).unwrap_or_default();
        let trimmed = s.trim_matches('"');
        f.write_str(trimmed)
    }
}

/// A structured audit event.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct AuditEvent {
    /// ISO 8601 timestamp.
    pub timestamp: String,
    /// The action that was performed.
    pub action: AuditAction,
    /// The outcome of the action.
    pub outcome: AuditOutcome,
    /// Target resource (e.g., VM ID).
    pub target: Option<String>,
    /// Human-readable detail message.
    pub detail: Option<String>,
}

/// Formats the current system time as an ISO 8601 UTC timestamp.
///
/// Output format: `YYYY-MM-DDTHH:MM:SSZ`
fn now_iso8601() -> String {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = duration.as_secs();

    // Break epoch seconds into date/time components.
    let days = total_secs / 86400;
    let time_secs = total_secs % 86400;
    let hours = time_secs / 3600;
    let minutes = (time_secs % 3600) / 60;
    let seconds = time_secs % 60;

    // Convert days since epoch to year/month/day.
    let (year, month, day) = days_to_ymd(days);

    format!("{year:04}-{month:02}-{day:02}T{hours:02}:{minutes:02}:{seconds:02}Z")
}

/// Converts days since Unix epoch (1970-01-01) to (year, month, day).
fn days_to_ymd(days: u64) -> (u64, u64, u64) {
    // Algorithm based on civil_from_days (Howard Hinnant)
    let era_days = days + 719_468;
    let era = era_days / 146_097;
    let doe = era_days - era * 146_097; // day of era [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m, d)
}

impl AuditEvent {
    /// Creates a new audit event with the current timestamp.
    #[must_use]
    pub fn new(action: AuditAction, outcome: AuditOutcome) -> Self {
        Self {
            timestamp: now_iso8601(),
            action,
            outcome,
            target: None,
            detail: None,
        }
    }

    /// Sets the target resource.
    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Sets a detail message.
    #[must_use]
    pub fn with_detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }
}

/// Emits an audit event via tracing.
///
/// Logs at INFO level with structured JSON fields under the `audit` target.
pub fn emit(event: &AuditEvent) {
    tracing::info!(
        target: "audit",
        action = %serde_json::to_string(&event.action).unwrap_or_default(),
        outcome = %serde_json::to_string(&event.outcome).unwrap_or_default(),
        target_resource = event.target.as_deref().unwrap_or(""),
        detail = event.detail.as_deref().unwrap_or(""),
        "audit event"
    );
}

/// Convenience: log a successful action.
pub fn log_success(action: AuditAction, target: Option<&str>, detail: Option<&str>) {
    let mut event = AuditEvent::new(action, AuditOutcome::Success);
    if let Some(t) = target {
        event = event.with_target(t);
    }
    if let Some(d) = detail {
        event = event.with_detail(d);
    }
    emit(&event);
}

/// Convenience: log a failed action.
pub fn log_failure(action: AuditAction, target: Option<&str>, detail: Option<&str>) {
    let mut event = AuditEvent::new(action, AuditOutcome::Failure);
    if let Some(t) = target {
        event = event.with_target(t);
    }
    if let Some(d) = detail {
        event = event.with_detail(d);
    }
    emit(&event);
}

#[cfg(test)]
#[path = "audit_test.rs"]
mod tests;
