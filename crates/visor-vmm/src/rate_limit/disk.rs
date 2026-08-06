//! Per-drive disk I/O rate limiting.
//!
//! [`DiskRateLimiter`] enforces IOPS and bandwidth caps on a single block
//! device using two independent [`TokenBucket`](super::TokenBucket) instances.
//!
//! # Example
//!
//! ```
//! use visor_vmm::rate_limit::disk::{DiskRateLimitConfig, DiskRateLimiter};
//!
//! let mut config = DiskRateLimitConfig::default();
//! config.iops = Some(1000);                         // 1000 IOPS
//! config.bandwidth_bytes = Some(100 * 1024 * 1024); // 100 MB/s
//! let mut limiter = DiskRateLimiter::new(&config);
//!
//! // Try a 4 KiB I/O (consumes 1 IOPS token + 4096 bandwidth tokens).
//! if limiter.try_io(4096) {
//!     // Proceed with I/O.
//! }
//! ```

use super::{RateLimitError, TokenBucket};

/// Configuration for per-drive disk I/O rate limiting.
///
/// Each field controls one rate-limiting dimension. `None` means unlimited
/// (no cap applied for that dimension).
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct DiskRateLimitConfig {
    /// Maximum I/O operations per second. `None` = unlimited.
    pub iops: Option<u64>,

    /// Maximum bandwidth in bytes per second. `None` = unlimited.
    pub bandwidth_bytes: Option<u64>,
}

/// Per-drive disk I/O rate limiter.
///
/// Wraps two [`TokenBucket`] instances: one for IOPS (operations per second)
/// and one for bandwidth (bytes per second). Each I/O operation consumes
/// 1 token from the IOPS bucket and `bytes` tokens from the bandwidth bucket.
///
/// When either bucket is `None` (configured with `None` in
/// [`DiskRateLimitConfig`]), that dimension is unlimited.
#[derive(Debug)]
#[non_exhaustive]
pub struct DiskRateLimiter {
    /// IOPS token bucket (1 token per I/O operation).
    iops_bucket: Option<TokenBucket>,
    /// Bandwidth token bucket (1 token per byte).
    bandwidth_bucket: Option<TokenBucket>,
}

impl DiskRateLimiter {
    /// Creates a new disk rate limiter from the given configuration.
    ///
    /// For each `Some(rate)` in the config, a [`TokenBucket`] is created
    /// with `burst = rate` (allowing a 1-second burst window). `None`
    /// fields result in no rate limiting for that dimension.
    #[must_use]
    pub fn new(config: &DiskRateLimitConfig) -> Self {
        Self {
            iops_bucket: config.iops.map(|rate| TokenBucket::new(rate, rate)),
            bandwidth_bucket: config
                .bandwidth_bytes
                .map(|rate| TokenBucket::new(rate, rate)),
        }
    }

    /// Attempts a disk I/O of `bytes` bytes without blocking.
    ///
    /// Checks both the IOPS bucket (1 token) and bandwidth bucket (`bytes`
    /// tokens) atomically. Returns `true` only if both dimensions allow the
    /// operation. Neither bucket is consumed if either check fails.
    #[must_use]
    pub fn try_io(&mut self, bytes: u64) -> bool {
        let iops_ok = self.iops_bucket.as_mut().is_none_or(|b| b.has(1));
        let bw_ok = self.bandwidth_bucket.as_mut().is_none_or(|b| b.has(bytes));

        if iops_ok && bw_ok {
            if let Some(b) = &mut self.iops_bucket {
                b.force_consume(1);
            }
            if let Some(b) = &mut self.bandwidth_bucket {
                b.force_consume(bytes);
            }
            true
        } else {
            false
        }
    }

    /// Waits until a disk I/O of `bytes` bytes is allowed, then consumes tokens.
    ///
    /// Waits for the IOPS bucket first (1 token), then the bandwidth bucket
    /// (`bytes` tokens). This sequential approach is correct because time
    /// passes while waiting for the first bucket, allowing the second to refill.
    ///
    /// For unlimited configurations, the corresponding wait is a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`RateLimitError::ExceedsBurst`] if `bytes` exceeds the
    /// bandwidth bucket's burst capacity (the I/O can never be satisfied).
    pub async fn wait_for_io(&mut self, bytes: u64) -> Result<(), RateLimitError> {
        if let Some(b) = &mut self.iops_bucket {
            b.wait_for(1).await?;
        }
        if let Some(b) = &mut self.bandwidth_bucket {
            b.wait_for(bytes).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "disk_test.rs"]
mod tests;
