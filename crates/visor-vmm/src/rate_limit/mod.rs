//! I/O rate limiting using the token bucket algorithm.
//!
//! Provides per-drive IOPS/bandwidth caps and per-NIC bandwidth caps
//! for controlling VM I/O throughput.
//!
//! # Architecture
//!
//! ```text
//! TokenBucket (core algorithm)
//! ├── DiskRateLimiter
//! │   ├── IOPS bucket (ops/sec)
//! │   └── Bandwidth bucket (bytes/sec)
//! └── NetRateLimiter
//!     ├── RX bucket (bytes/sec)
//!     └── TX bucket (bytes/sec)
//! ```
//!
//! Each rate limiter wraps one or two [`TokenBucket`] instances. When a
//! config field is `None`, that dimension is unlimited (no rate limiting).

pub mod disk;
pub mod net;

use std::time::{Duration, Instant};

/// Nanoseconds per second — shared scale factor for integer token math.
const NANOS_PER_SEC: u128 = 1_000_000_000;

/// Errors from rate limiting operations.
///
/// # Errors
///
/// Returned when a rate limiting operation cannot be satisfied due to
/// configuration constraints (e.g., requesting more tokens than burst capacity).
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RateLimitError {
    /// Requested token count exceeds the bucket's burst capacity.
    ///
    /// This means the request can never be satisfied, even after waiting
    /// indefinitely. The caller should reject the I/O or split it into
    /// smaller chunks.
    #[error("requested {requested} tokens exceeds burst capacity {capacity}")]
    ExceedsBurst {
        /// Number of tokens requested.
        requested: u64,
        /// Maximum burst capacity of the bucket.
        capacity: u64,
    },
}

/// Token bucket rate limiter.
///
/// Implements the [token bucket algorithm](https://en.wikipedia.org/wiki/Token_bucket)
/// for rate limiting I/O operations. Tokens are added at a constant `rate`
/// (tokens per second) up to a maximum `burst` capacity.
///
/// Internally uses `u128` scaled arithmetic (nanosecond precision) to avoid
/// floating-point imprecision.
///
/// # Example
///
/// ```
/// use visor_vmm::rate_limit::TokenBucket;
///
/// let mut bucket = TokenBucket::new(100, 100); // 100 tokens/sec, burst of 100
/// assert!(bucket.consume(50)); // consume 50 tokens
/// assert!(bucket.consume(50)); // consume remaining 50
/// assert!(!bucket.consume(1)); // bucket is empty
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct TokenBucket {
    /// Refill rate in tokens per second.
    rate: u64,
    /// Maximum token capacity.
    burst: u64,
    /// Current available tokens scaled by [`NANOS_PER_SEC`] for sub-token
    /// precision without floating point.
    scaled_tokens: u128,
    /// Timestamp of the last token refill.
    last_refill: Instant,
}

impl TokenBucket {
    /// Creates a new token bucket with the given rate and burst capacity.
    ///
    /// The bucket starts full (at burst capacity). `rate` is the refill rate
    /// in tokens per second. `burst` is the maximum number of tokens the
    /// bucket can hold.
    #[must_use]
    pub fn new(rate: u64, burst: u64) -> Self {
        Self {
            rate,
            burst,
            scaled_tokens: u128::from(burst) * NANOS_PER_SEC,
            last_refill: Instant::now(),
        }
    }

    /// Attempts to consume `tokens` from the bucket without blocking.
    ///
    /// Refills the bucket based on elapsed time, then checks if enough
    /// tokens are available. Returns `true` if the tokens were consumed,
    /// `false` if insufficient tokens are available.
    ///
    /// Consuming zero tokens always succeeds.
    #[must_use]
    pub fn consume(&mut self, tokens: u64) -> bool {
        if tokens == 0 {
            return true;
        }
        self.refill();
        let needed = u128::from(tokens) * NANOS_PER_SEC;
        if self.scaled_tokens >= needed {
            self.scaled_tokens -= needed;
            true
        } else {
            false
        }
    }

    /// Waits until `tokens` are available, then consumes them.
    ///
    /// If the requested tokens exceed the burst capacity, returns
    /// [`RateLimitError::ExceedsBurst`] immediately. Otherwise, sleeps
    /// until enough tokens have accumulated.
    ///
    /// Waiting for zero tokens always succeeds immediately.
    ///
    /// # Errors
    ///
    /// Returns [`RateLimitError::ExceedsBurst`] if `tokens` exceeds the
    /// bucket's burst capacity.
    pub async fn wait_for(&mut self, tokens: u64) -> Result<(), RateLimitError> {
        if tokens == 0 {
            return Ok(());
        }
        if tokens > self.burst {
            return Err(RateLimitError::ExceedsBurst {
                requested: tokens,
                capacity: self.burst,
            });
        }
        loop {
            self.refill();
            let needed = u128::from(tokens) * NANOS_PER_SEC;
            if self.scaled_tokens >= needed {
                self.scaled_tokens -= needed;
                return Ok(());
            }
            let deficit_scaled = needed - self.scaled_tokens;
            let rate = u128::from(self.rate);
            // deficit_scaled is in token-nanos; dividing by rate gives nanos to wait.
            let wait_nanos = deficit_scaled / rate;
            // Cap at 1 second — the loop re-checks, so overshooting is fine.
            let capped = wait_nanos.min(NANOS_PER_SEC);
            // Safe: capped <= 10^9 which fits in u64.
            let nanos_u64 = u64::try_from(capped).unwrap_or(1_000_000_000);
            tokio::time::sleep(Duration::from_nanos(nanos_u64)).await;
        }
    }

    /// Returns the current number of whole available tokens (after refilling).
    #[must_use]
    pub fn available(&mut self) -> u64 {
        self.refill();
        // Safe: whole tokens <= burst which is u64.
        let whole = self.scaled_tokens / NANOS_PER_SEC;
        u64::try_from(whole).unwrap_or(self.burst)
    }

    /// Returns the configured rate in tokens per second.
    #[must_use]
    pub fn rate(&self) -> u64 {
        self.rate
    }

    /// Returns the configured burst capacity.
    #[must_use]
    pub fn burst(&self) -> u64 {
        self.burst
    }

    /// Checks whether at least `tokens` are available without consuming.
    ///
    /// Refills based on elapsed time first.
    pub(crate) fn has(&mut self, tokens: u64) -> bool {
        self.refill();
        self.scaled_tokens >= u128::from(tokens) * NANOS_PER_SEC
    }

    /// Consumes tokens without availability check.
    ///
    /// The caller must verify availability with [`has`](Self::has) first.
    pub(crate) fn force_consume(&mut self, tokens: u64) {
        self.scaled_tokens = self
            .scaled_tokens
            .saturating_sub(u128::from(tokens) * NANOS_PER_SEC);
    }

    /// Refills tokens based on elapsed time since the last refill.
    ///
    /// `new_scaled = elapsed_nanos × rate`, capped at `burst × NANOS_PER_SEC`.
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed_nanos = now.duration_since(self.last_refill).as_nanos();
        if elapsed_nanos == 0 {
            return;
        }
        let new_scaled = elapsed_nanos.saturating_mul(u128::from(self.rate));
        let max_scaled = u128::from(self.burst) * NANOS_PER_SEC;
        self.scaled_tokens = self
            .scaled_tokens
            .saturating_add(new_scaled)
            .min(max_scaled);
        self.last_refill = now;
    }
}

#[cfg(test)]
#[path = "mod_test.rs"]
mod tests;
