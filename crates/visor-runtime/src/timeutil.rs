//! Shared time utilities for ISO 8601 timestamps.
//!
//! Uses the `time` crate instead of hand-rolled epoch arithmetic.

use time::OffsetDateTime;
use time::macros::format_description;

/// Returns the current UTC time as an ISO 8601 string (`YYYY-MM-DDTHH:MM:SSZ`).
///
/// # Panics
///
/// Never panics — falls back to Unix epoch if system clock is unavailable.
#[must_use]
pub fn utc_now_iso8601() -> String {
    let now = OffsetDateTime::now_utc();
    let format = format_description!("[year]-[month]-[day]T[hour]:[minute]:[second]Z");
    now.format(&format)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

#[cfg(test)]
#[path = "timeutil_test.rs"]
mod tests;
