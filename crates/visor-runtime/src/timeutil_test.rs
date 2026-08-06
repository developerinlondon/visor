use super::*;

#[test]
fn utc_now_iso8601_matches_iso_format() {
    let ts = utc_now_iso8601();
    // Must match YYYY-MM-DDTHH:MM:SSZ
    assert!(
        ts.len() == 20,
        "timestamp should be 20 chars (YYYY-MM-DDTHH:MM:SSZ), got {}: '{ts}'",
        ts.len()
    );
    assert!(ts.ends_with('Z'), "timestamp should end with Z: '{ts}'");
    assert_eq!(&ts[4..5], "-", "position 4 should be '-': '{ts}'");
    assert_eq!(&ts[7..8], "-", "position 7 should be '-': '{ts}'");
    assert_eq!(&ts[10..11], "T", "position 10 should be 'T': '{ts}'");
    assert_eq!(&ts[13..14], ":", "position 13 should be ':': '{ts}'");
    assert_eq!(&ts[16..17], ":", "position 16 should be ':': '{ts}'");
}

#[test]
fn utc_now_iso8601_year_is_reasonable() {
    let ts = utc_now_iso8601();
    let year: u32 = ts[0..4].parse().unwrap();
    assert!(year >= 2025, "year should be >= 2025, got {year}");
    assert!(year <= 2100, "year should be <= 2100, got {year}");
}

#[test]
fn utc_now_iso8601_month_is_valid() {
    let ts = utc_now_iso8601();
    let month: u32 = ts[5..7].parse().unwrap();
    assert!(
        (1..=12).contains(&month),
        "month should be 1-12, got {month}"
    );
}

#[test]
fn utc_now_iso8601_day_is_valid() {
    let ts = utc_now_iso8601();
    let day: u32 = ts[8..10].parse().unwrap();
    assert!((1..=31).contains(&day), "day should be 1-31, got {day}");
}

#[test]
fn utc_now_iso8601_called_twice_does_not_go_backwards() {
    let ts1 = utc_now_iso8601();
    let ts2 = utc_now_iso8601();
    assert!(
        ts2 >= ts1,
        "second call should be >= first: '{ts1}' vs '{ts2}'"
    );
}
