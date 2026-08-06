use std::time::{Duration, Instant};

use super::*;

// ── TokenBucket construction ──────────────────────────────────────────

#[test]
fn new_bucket_starts_full() {
    let mut bucket = TokenBucket::new(100, 200);
    assert_eq!(bucket.rate(), 100);
    assert_eq!(bucket.burst(), 200);
    // Bucket starts at burst capacity.
    assert_eq!(bucket.available(), 200);
}

#[test]
fn new_bucket_zero_rate_zero_burst() {
    let mut bucket = TokenBucket::new(0, 0);
    assert_eq!(bucket.rate(), 0);
    assert_eq!(bucket.burst(), 0);
    assert_eq!(bucket.available(), 0);
}

// ── consume ───────────────────────────────────────────────────────────

#[test]
fn consume_zero_always_succeeds() {
    let mut bucket = TokenBucket::new(0, 0);
    assert!(bucket.consume(0));

    let mut bucket2 = TokenBucket::new(100, 100);
    assert!(bucket2.consume(0));
}

#[test]
fn consume_within_capacity_succeeds() {
    let mut bucket = TokenBucket::new(100, 100);
    assert!(bucket.consume(50));
    assert!(bucket.consume(30));
}

#[test]
fn consume_exactly_capacity_succeeds() {
    let mut bucket = TokenBucket::new(100, 100);
    assert!(bucket.consume(100));
}

#[test]
fn consume_beyond_available_fails() {
    let mut bucket = TokenBucket::new(100, 100);
    assert!(bucket.consume(80));
    // Only ~20 left.
    assert!(!bucket.consume(30));
}

#[test]
fn consume_all_then_fails() {
    let mut bucket = TokenBucket::new(100, 100);
    assert!(bucket.consume(100));
    assert!(!bucket.consume(1));
}

#[test]
fn consume_exceeding_burst_fails() {
    let mut bucket = TokenBucket::new(100, 100);
    assert!(!bucket.consume(101));
}

#[test]
fn zero_burst_rejects_nonzero_consume() {
    let mut bucket = TokenBucket::new(1000, 0);
    assert!(!bucket.consume(1));
}

// ── Token refill over time ────────────────────────────────────────────

#[test]
fn tokens_refill_after_elapsed_time() {
    let mut bucket = TokenBucket::new(10_000, 10_000);
    assert!(bucket.consume(10_000));
    assert!(!bucket.consume(1));

    // 100ms at 10k tokens/sec = ~1000 tokens.
    std::thread::sleep(Duration::from_millis(100));

    // Conservative threshold to avoid flakiness.
    assert!(bucket.consume(500));
}

#[test]
fn tokens_do_not_exceed_burst_after_long_wait() {
    let mut bucket = TokenBucket::new(100, 100);
    std::thread::sleep(Duration::from_millis(200));
    // Even after waiting, available <= burst.
    assert!(bucket.available() <= 100);
}

// ── wait_for ──────────────────────────────────────────────────────────

#[tokio::test]
async fn wait_for_within_capacity_succeeds_immediately() {
    let mut bucket = TokenBucket::new(10_000, 10_000);
    bucket.wait_for(100).await.unwrap();
}

#[tokio::test]
async fn wait_for_zero_tokens_succeeds() {
    let mut bucket = TokenBucket::new(100, 100);
    bucket.wait_for(0).await.unwrap();
}

#[tokio::test]
async fn wait_for_exceeding_burst_returns_error() {
    let mut bucket = TokenBucket::new(100, 100);
    let result = bucket.wait_for(101).await;
    assert!(result.is_err());
    let err = result.unwrap_err();
    let msg = err.to_string();
    assert!(msg.contains("101"));
    assert!(msg.contains("100"));
}

#[tokio::test]
async fn wait_for_waits_until_tokens_available() {
    let mut bucket = TokenBucket::new(100_000, 100_000);
    assert!(bucket.consume(100_000));

    let start = Instant::now();
    // Need 10k tokens, refill at 100k/sec → ~100ms wait.
    bucket.wait_for(10_000).await.unwrap();
    let elapsed = start.elapsed();

    assert!(
        elapsed >= Duration::from_millis(50),
        "expected >= 50ms wait, got {elapsed:?}"
    );
}

// ── RateLimitError ────────────────────────────────────────────────────

#[test]
fn error_exceeds_burst_display() {
    let err = RateLimitError::ExceedsBurst {
        requested: 200,
        capacity: 100,
    };
    let msg = err.to_string();
    assert!(msg.contains("200"));
    assert!(msg.contains("100"));
}

#[test]
fn error_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<RateLimitError>();
}

// ── TokenBucket is Debug ──────────────────────────────────────────────

#[test]
fn token_bucket_debug() {
    let bucket = TokenBucket::new(100, 200);
    let debug = format!("{bucket:?}");
    assert!(debug.contains("TokenBucket"));
}
