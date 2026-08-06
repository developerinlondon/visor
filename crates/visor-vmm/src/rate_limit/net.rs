//! Per-NIC network bandwidth rate limiting.
//!
//! [`NetRateLimiter`] enforces separate rx and tx bandwidth caps on a single
//! network interface using two independent [`TokenBucket`](super::TokenBucket)
//! instances.
//!
//! # Example
//!
//! ```
//! use visor_vmm::rate_limit::net::{NetRateLimitConfig, NetRateLimiter};
//!
//! let mut config = NetRateLimitConfig::default();
//! config.rx_bytes = Some(100 * 1024 * 1024); // 100 MB/s receive
//! config.tx_bytes = Some(50 * 1024 * 1024);  // 50 MB/s transmit
//! let mut limiter = NetRateLimiter::new(&config);
//!
//! // Try to receive a 1500-byte packet.
//! if limiter.try_rx(1500) {
//!     // Proceed with receive.
//! }
//! ```

use super::{RateLimitError, TokenBucket};

/// Configuration for per-NIC network bandwidth rate limiting.
///
/// Each field controls one direction's bandwidth cap. `None` means unlimited
/// (no cap applied for that direction).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct NetRateLimitConfig {
    /// Maximum receive bandwidth in bytes per second. `None` = unlimited.
    pub rx_bytes: Option<u64>,

    /// Maximum transmit bandwidth in bytes per second. `None` = unlimited.
    pub tx_bytes: Option<u64>,
}

/// Per-NIC network bandwidth rate limiter.
///
/// Wraps two [`TokenBucket`] instances: one for receive (rx) and one for
/// transmit (tx) bandwidth, each measured in bytes per second. The two
/// directions are fully independent — exhausting rx does not affect tx.
///
/// When a bucket is `None` (configured with `None` in
/// [`NetRateLimitConfig`]), that direction is unlimited.
#[derive(Debug)]
#[non_exhaustive]
pub struct NetRateLimiter {
    /// Receive bandwidth token bucket (1 token per byte).
    rx_bucket: Option<TokenBucket>,
    /// Transmit bandwidth token bucket (1 token per byte).
    tx_bucket: Option<TokenBucket>,
}

impl NetRateLimiter {
    /// Creates a new network rate limiter from the given configuration.
    ///
    /// For each `Some(rate)` in the config, a [`TokenBucket`] is created
    /// with `burst = rate` (allowing a 1-second burst window). `None`
    /// fields result in no rate limiting for that direction.
    #[must_use]
    pub fn new(config: &NetRateLimitConfig) -> Self {
        Self {
            rx_bucket: config.rx_bytes.map(|rate| TokenBucket::new(rate, rate)),
            tx_bucket: config.tx_bytes.map(|rate| TokenBucket::new(rate, rate)),
        }
    }

    /// Attempts to consume `bytes` from the receive bandwidth bucket.
    ///
    /// Returns `true` if the receive is allowed, `false` if the rx bandwidth
    /// cap would be exceeded. Always returns `true` when rx is unlimited.
    #[must_use]
    pub fn try_rx(&mut self, bytes: u64) -> bool {
        self.rx_bucket.as_mut().is_none_or(|b| b.consume(bytes))
    }

    /// Attempts to consume `bytes` from the transmit bandwidth bucket.
    ///
    /// Returns `true` if the transmit is allowed, `false` if the tx bandwidth
    /// cap would be exceeded. Always returns `true` when tx is unlimited.
    #[must_use]
    pub fn try_tx(&mut self, bytes: u64) -> bool {
        self.tx_bucket.as_mut().is_none_or(|b| b.consume(bytes))
    }

    /// Waits until `bytes` of receive bandwidth is available, then consumes.
    ///
    /// For unlimited rx, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`RateLimitError::ExceedsBurst`] if `bytes` exceeds the
    /// rx bucket's burst capacity.
    pub async fn wait_for_rx(&mut self, bytes: u64) -> Result<(), RateLimitError> {
        if let Some(b) = &mut self.rx_bucket {
            b.wait_for(bytes).await?;
        }
        Ok(())
    }

    /// Waits until `bytes` of transmit bandwidth is available, then consumes.
    ///
    /// For unlimited tx, this is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`RateLimitError::ExceedsBurst`] if `bytes` exceeds the
    /// tx bucket's burst capacity.
    pub async fn wait_for_tx(&mut self, bytes: u64) -> Result<(), RateLimitError> {
        if let Some(b) = &mut self.tx_bucket {
            b.wait_for(bytes).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "net_test.rs"]
mod tests;
