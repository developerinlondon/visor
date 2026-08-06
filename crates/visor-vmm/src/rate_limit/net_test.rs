use std::time::Duration;

use super::*;

// ── NetRateLimitConfig ────────────────────────────────────────────────

#[test]
fn config_default_is_unlimited() {
    let config = NetRateLimitConfig::default();
    assert!(config.rx_bytes.is_none());
    assert!(config.tx_bytes.is_none());
}

#[test]
fn config_debug() {
    let config = NetRateLimitConfig {
        rx_bytes: Some(1_000_000),
        tx_bytes: Some(500_000),
    };
    let debug = format!("{config:?}");
    assert!(debug.contains("NetRateLimitConfig"));
    assert!(debug.contains("1000000"));
}

#[test]
fn config_clone() {
    let config = NetRateLimitConfig {
        rx_bytes: Some(1_000_000),
        tx_bytes: Some(500_000),
    };
    let cloned = config.clone();
    assert_eq!(cloned.rx_bytes, Some(1_000_000));
    assert_eq!(cloned.tx_bytes, Some(500_000));
}

// ── Unlimited (no rate limiting) ──────────────────────────────────────

#[test]
fn unlimited_config_always_allows() {
    let config = NetRateLimitConfig::default();
    let mut limiter = NetRateLimiter::new(&config);
    for _ in 0..10_000 {
        assert!(limiter.try_rx(1_000_000));
        assert!(limiter.try_tx(1_000_000));
    }
}

// ── RX limiting ───────────────────────────────────────────────────────

#[test]
fn rx_limiting_rejects_after_exhaustion() {
    let config = NetRateLimitConfig {
        rx_bytes: Some(100), // low rate so refill is negligible
        tx_bytes: None,
    };
    let mut limiter = NetRateLimiter::new(&config);
    assert!(limiter.try_rx(100));
    // At 100 bytes/sec, sub-microsecond gap refills < 0.001 tokens.
    assert!(!limiter.try_rx(1));
}

#[test]
fn rx_does_not_affect_tx() {
    let config = NetRateLimitConfig {
        rx_bytes: Some(100),
        tx_bytes: Some(100),
    };
    let mut limiter = NetRateLimiter::new(&config);
    // Exhaust RX.
    assert!(limiter.try_rx(100));
    assert!(!limiter.try_rx(1));
    // TX should still work.
    assert!(limiter.try_tx(100));
}

#[test]
fn rx_refill_over_time() {
    let config = NetRateLimitConfig {
        rx_bytes: Some(1_000_000),
        tx_bytes: None,
    };
    let mut limiter = NetRateLimiter::new(&config);
    assert!(limiter.try_rx(1_000_000));

    // Wait 100ms → ~100k bytes refilled.
    std::thread::sleep(Duration::from_millis(100));
    assert!(limiter.try_rx(50_000));
}

// ── TX limiting ───────────────────────────────────────────────────────

#[test]
fn tx_limiting_rejects_after_exhaustion() {
    let config = NetRateLimitConfig {
        rx_bytes: None,
        tx_bytes: Some(100), // low rate so refill is negligible
    };
    let mut limiter = NetRateLimiter::new(&config);
    assert!(limiter.try_tx(100));
    assert!(!limiter.try_tx(1));
}

#[test]
fn tx_does_not_affect_rx() {
    let config = NetRateLimitConfig {
        rx_bytes: Some(100),
        tx_bytes: Some(100),
    };
    let mut limiter = NetRateLimiter::new(&config);
    // Exhaust TX.
    assert!(limiter.try_tx(100));
    assert!(!limiter.try_tx(1));
    // RX should still work.
    assert!(limiter.try_rx(100));
}

#[test]
fn tx_refill_over_time() {
    let config = NetRateLimitConfig {
        rx_bytes: None,
        tx_bytes: Some(1_000_000),
    };
    let mut limiter = NetRateLimiter::new(&config);
    assert!(limiter.try_tx(1_000_000));

    // Wait 100ms → ~100k bytes refilled.
    std::thread::sleep(Duration::from_millis(100));
    assert!(limiter.try_tx(50_000));
}

// ── Zero rate blocks all ──────────────────────────────────────────────

#[test]
fn zero_rx_rate_blocks_all() {
    let config = NetRateLimitConfig {
        rx_bytes: Some(0),
        tx_bytes: None,
    };
    let mut limiter = NetRateLimiter::new(&config);
    assert!(!limiter.try_rx(1));
}

#[test]
fn zero_tx_rate_blocks_all() {
    let config = NetRateLimitConfig {
        rx_bytes: None,
        tx_bytes: Some(0),
    };
    let mut limiter = NetRateLimiter::new(&config);
    assert!(!limiter.try_tx(1));
}

// ── Async wait_for ────────────────────────────────────────────────────

#[tokio::test]
async fn wait_for_rx_unlimited_succeeds() {
    let config = NetRateLimitConfig::default();
    let mut limiter = NetRateLimiter::new(&config);
    limiter.wait_for_rx(1_000_000).await.unwrap();
}

#[tokio::test]
async fn wait_for_tx_unlimited_succeeds() {
    let config = NetRateLimitConfig::default();
    let mut limiter = NetRateLimiter::new(&config);
    limiter.wait_for_tx(1_000_000).await.unwrap();
}

#[tokio::test]
async fn wait_for_rx_with_available_tokens_succeeds() {
    let config = NetRateLimitConfig {
        rx_bytes: Some(1_000_000),
        tx_bytes: None,
    };
    let mut limiter = NetRateLimiter::new(&config);
    limiter.wait_for_rx(100).await.unwrap();
}

#[tokio::test]
async fn wait_for_tx_with_available_tokens_succeeds() {
    let config = NetRateLimitConfig {
        rx_bytes: None,
        tx_bytes: Some(1_000_000),
    };
    let mut limiter = NetRateLimiter::new(&config);
    limiter.wait_for_tx(100).await.unwrap();
}

#[tokio::test]
async fn wait_for_rx_exceeds_burst_returns_error() {
    let config = NetRateLimitConfig {
        rx_bytes: Some(100),
        tx_bytes: None,
    };
    let mut limiter = NetRateLimiter::new(&config);
    let result = limiter.wait_for_rx(200).await;
    assert!(result.is_err());
}

#[tokio::test]
async fn wait_for_tx_exceeds_burst_returns_error() {
    let config = NetRateLimitConfig {
        rx_bytes: None,
        tx_bytes: Some(100),
    };
    let mut limiter = NetRateLimiter::new(&config);
    let result = limiter.wait_for_tx(200).await;
    assert!(result.is_err());
}

// ── Debug ─────────────────────────────────────────────────────────────

#[test]
fn net_rate_limiter_debug() {
    let config = NetRateLimitConfig::default();
    let limiter = NetRateLimiter::new(&config);
    let debug = format!("{limiter:?}");
    assert!(debug.contains("NetRateLimiter"));
}
