use std::time::Duration;

use super::*;

// ── DiskRateLimitConfig ───────────────────────────────────────────────

#[test]
fn config_default_is_unlimited() {
    let config = DiskRateLimitConfig::default();
    assert!(config.iops.is_none());
    assert!(config.bandwidth_bytes.is_none());
}

#[test]
fn config_debug() {
    let config = DiskRateLimitConfig {
        iops: Some(1000),
        bandwidth_bytes: Some(1_000_000),
    };
    let debug = format!("{config:?}");
    assert!(debug.contains("DiskRateLimitConfig"));
    assert!(debug.contains("1000"));
}

#[test]
fn config_clone() {
    let config = DiskRateLimitConfig {
        iops: Some(500),
        bandwidth_bytes: Some(4096),
    };
    let cloned = config.clone();
    assert_eq!(cloned.iops, Some(500));
    assert_eq!(cloned.bandwidth_bytes, Some(4096));
}

// ── Unlimited (no rate limiting) ──────────────────────────────────────

#[test]
fn unlimited_config_always_allows() {
    let config = DiskRateLimitConfig::default();
    let mut limiter = DiskRateLimiter::new(&config);
    for _ in 0..10_000 {
        assert!(limiter.try_io(1_000_000));
    }
}

// ── IOPS limiting ─────────────────────────────────────────────────────

#[test]
fn iops_limiting_rejects_after_exhaustion() {
    let config = DiskRateLimitConfig {
        iops: Some(100),
        bandwidth_bytes: None,
    };
    let mut limiter = DiskRateLimiter::new(&config);
    for _ in 0..100 {
        assert!(limiter.try_io(0));
    }
    assert!(!limiter.try_io(0));
}

#[test]
fn iops_refill_over_time() {
    let config = DiskRateLimitConfig {
        iops: Some(100),
        bandwidth_bytes: None,
    };
    let mut limiter = DiskRateLimiter::new(&config);
    // Exhaust all IOPS (100 iterations is fast enough to avoid refill).
    for _ in 0..100 {
        let _ = limiter.try_io(0);
    }
    assert!(!limiter.try_io(0));

    // Wait 200ms → ~20 tokens refilled at 100/sec.
    std::thread::sleep(Duration::from_millis(200));
    assert!(limiter.try_io(0));
}

// ── Bandwidth limiting ────────────────────────────────────────────────

#[test]
fn bandwidth_limiting_rejects_after_exhaustion() {
    let config = DiskRateLimitConfig {
        iops: None,
        bandwidth_bytes: Some(100), // low rate so refill is negligible
    };
    let mut limiter = DiskRateLimiter::new(&config);
    assert!(limiter.try_io(100));
    // At 100 bytes/sec, sub-microsecond gap refills < 0.001 tokens.
    assert!(!limiter.try_io(1));
}

#[test]
fn bandwidth_refill_over_time() {
    let config = DiskRateLimitConfig {
        iops: None,
        bandwidth_bytes: Some(1_000_000),
    };
    let mut limiter = DiskRateLimiter::new(&config);
    assert!(limiter.try_io(1_000_000));

    // Wait 100ms → ~100k bytes refilled.
    std::thread::sleep(Duration::from_millis(100));
    assert!(limiter.try_io(50_000));
}

// ── Both limits enforced ──────────────────────────────────────────────

#[test]
fn both_limits_bandwidth_exhausted_first() {
    let config = DiskRateLimitConfig {
        iops: Some(10),
        bandwidth_bytes: Some(4096),
    };
    let mut limiter = DiskRateLimiter::new(&config);
    // One large I/O exhausts bandwidth but leaves IOPS.
    assert!(limiter.try_io(4096));
    assert!(!limiter.try_io(1));
}

#[test]
fn both_limits_iops_exhausted_first() {
    let config = DiskRateLimitConfig {
        iops: Some(5),
        bandwidth_bytes: Some(1_000_000),
    };
    let mut limiter = DiskRateLimiter::new(&config);
    for _ in 0..5 {
        assert!(limiter.try_io(100));
    }
    // IOPS exhausted, bandwidth still available.
    assert!(!limiter.try_io(100));
}

// ── Zero rate blocks all I/O ──────────────────────────────────────────

#[test]
fn zero_iops_rate_blocks_all() {
    let config = DiskRateLimitConfig {
        iops: Some(0),
        bandwidth_bytes: None,
    };
    let mut limiter = DiskRateLimiter::new(&config);
    assert!(!limiter.try_io(0));
}

#[test]
fn zero_bandwidth_rate_blocks_all() {
    let config = DiskRateLimitConfig {
        iops: None,
        bandwidth_bytes: Some(0),
    };
    let mut limiter = DiskRateLimiter::new(&config);
    assert!(!limiter.try_io(1));
}

// ── Atomic two-phase: no partial consumption ──────────────────────────

#[test]
fn try_io_does_not_consume_iops_when_bandwidth_insufficient() {
    let config = DiskRateLimitConfig {
        iops: Some(5),
        bandwidth_bytes: Some(100),
    };
    let mut limiter = DiskRateLimiter::new(&config);
    // Exhaust bandwidth only.
    assert!(limiter.try_io(100));
    // Next call should fail (bandwidth exhausted). IOPS had 5, used 1 = 4 left.
    assert!(!limiter.try_io(100));
    // If IOPS were *not* leaked, we should still have 4 IOPS.
    // Verify by making the bandwidth available again via time.
    std::thread::sleep(Duration::from_millis(110));
    // After refill: ~11 bw tokens, 4+ IOPS tokens.
    assert!(limiter.try_io(10));
}

// ── Async wait_for_io ─────────────────────────────────────────────────

#[tokio::test]
async fn wait_for_io_unlimited_succeeds() {
    let config = DiskRateLimitConfig::default();
    let mut limiter = DiskRateLimiter::new(&config);
    limiter.wait_for_io(1_000_000).await.unwrap();
}

#[tokio::test]
async fn wait_for_io_with_available_tokens_succeeds() {
    let config = DiskRateLimitConfig {
        iops: Some(1000),
        bandwidth_bytes: Some(1_000_000),
    };
    let mut limiter = DiskRateLimiter::new(&config);
    limiter.wait_for_io(100).await.unwrap();
}

#[tokio::test]
async fn wait_for_io_bandwidth_exceeds_burst_returns_error() {
    let config = DiskRateLimitConfig {
        iops: None,
        bandwidth_bytes: Some(100),
    };
    let mut limiter = DiskRateLimiter::new(&config);
    let result = limiter.wait_for_io(200).await;
    assert!(result.is_err());
}

// ── Debug ─────────────────────────────────────────────────────────────

#[test]
fn disk_rate_limiter_debug() {
    let config = DiskRateLimitConfig::default();
    let limiter = DiskRateLimiter::new(&config);
    let debug = format!("{limiter:?}");
    assert!(debug.contains("DiskRateLimiter"));
}
