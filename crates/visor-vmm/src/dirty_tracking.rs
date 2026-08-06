//! Dirty page tracking for live migration's iterative pre-copy algorithm.
//!
//! Tracks which guest memory pages are modified between iterations so the
//! migration system can re-transfer only changed pages. Each bit in the
//! [`DirtyBitmap`] represents a 4 KiB page — set means modified since the
//! last collection.
//!
//! # Design
//!
//! The module separates pure data structures from KVM operations:
//!
//! - [`DirtyBitmap`] — bitmap data structure, fully testable without KVM
//! - [`DirtyRateEstimator`] — dirty rate calculation from sample pairs
//! - [`DirtyTracker`] — orchestrates bitmap collection and rate estimation
//!
//! In production, `DirtyTracker` obtains bitmaps from `KVM_GET_DIRTY_LOG`.
//! For testing, bitmaps are injected via [`DirtyTracker::collect_from_bitmap`].

/// Page size for dirty tracking (4 KiB).
pub const PAGE_SIZE: usize = 4096;

/// Errors from dirty page tracking operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum DirtyTrackingError {
    /// KVM ioctl failed.
    #[error("KVM dirty tracking ioctl failed: {0}")]
    Kvm(std::io::Error),

    /// Memory region not configured for dirty tracking.
    #[error("memory region {slot} not configured for dirty tracking")]
    NotEnabled {
        /// KVM memory slot number.
        slot: u32,
    },

    /// Bitmap size mismatch.
    #[error("bitmap size mismatch: expected {expected} bytes, got {actual}")]
    BitmapSizeMismatch {
        /// Expected bitmap size in bytes.
        expected: usize,
        /// Actual bitmap size in bytes.
        actual: usize,
    },
}

/// Bitmap tracking dirty (modified) guest memory pages.
///
/// Each bit represents a 4 KiB page. Bit set = page modified since last clear.
/// A 256 MiB VM needs an 8,192-byte bitmap (256 MiB / 4 KiB / 8 bits).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct DirtyBitmap {
    /// Raw bitmap bytes (1 bit per 4 KiB page).
    bitmap: Vec<u8>,
    /// Total number of pages tracked.
    page_count: usize,
}

impl DirtyBitmap {
    /// Creates a new clean bitmap for `memory_size_bytes` of guest RAM.
    ///
    /// Pages are tracked at 4 KiB granularity. Non-page-aligned sizes are
    /// rounded up.
    #[must_use]
    pub fn new(memory_size_bytes: usize) -> Self {
        let page_count = memory_size_bytes.div_ceil(PAGE_SIZE);
        let byte_count = page_count.div_ceil(8);
        Self {
            bitmap: vec![0u8; byte_count],
            page_count,
        }
    }

    /// Creates a bitmap from raw bytes (e.g., from `KVM_GET_DIRTY_LOG`).
    ///
    /// # Errors
    ///
    /// Returns [`DirtyTrackingError::BitmapSizeMismatch`] if the byte slice
    /// length does not match the expected bitmap size for `memory_size_bytes`.
    pub fn from_raw(bytes: Vec<u8>, memory_size_bytes: usize) -> Result<Self, DirtyTrackingError> {
        let page_count = memory_size_bytes.div_ceil(PAGE_SIZE);
        let expected = page_count.div_ceil(8);
        if bytes.len() != expected {
            return Err(DirtyTrackingError::BitmapSizeMismatch {
                expected,
                actual: bytes.len(),
            });
        }
        Ok(Self {
            bitmap: bytes,
            page_count,
        })
    }

    /// Returns whether the page at `page_index` is dirty.
    #[must_use]
    pub fn is_dirty(&self, page_index: usize) -> bool {
        if page_index >= self.page_count {
            return false;
        }
        let byte_idx = page_index / 8;
        let bit_idx = page_index % 8;
        (self.bitmap[byte_idx] >> bit_idx) & 1 == 1
    }

    /// Marks a page as dirty.
    pub fn set_dirty(&mut self, page_index: usize) {
        if page_index >= self.page_count {
            return;
        }
        let byte_idx = page_index / 8;
        let bit_idx = page_index % 8;
        self.bitmap[byte_idx] |= 1 << bit_idx;
    }

    /// Clears all dirty bits.
    pub fn clear(&mut self) {
        self.bitmap.fill(0);
    }

    /// Returns the number of dirty pages.
    #[must_use]
    pub fn dirty_count(&self) -> usize {
        self.bitmap.iter().map(|b| b.count_ones() as usize).sum()
    }

    /// Returns total page count tracked.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.page_count
    }

    /// Returns the bitmap size in bytes.
    #[must_use]
    pub fn bitmap_size(&self) -> usize {
        self.bitmap.len()
    }

    /// Returns an iterator over dirty page indices.
    pub fn dirty_pages(&self) -> impl Iterator<Item = usize> + '_ {
        let page_count = self.page_count;
        self.bitmap
            .iter()
            .enumerate()
            .flat_map(move |(byte_idx, byte)| {
                let byte_val = *byte;
                (0..8u32).filter_map(move |bit| {
                    let page_idx = byte_idx * 8 + bit as usize;
                    if (byte_val >> bit) & 1 == 1 && page_idx < page_count {
                        Some(page_idx)
                    } else {
                        None
                    }
                })
            })
    }

    /// Merges another bitmap into this one (OR operation).
    ///
    /// Bits set in `other` become set in `self`. The bitmaps must cover the
    /// same memory size; extra bytes in `other` are ignored, missing bytes
    /// are treated as zero.
    pub fn merge(&mut self, other: &DirtyBitmap) {
        for (dst, src) in self.bitmap.iter_mut().zip(other.bitmap.iter()) {
            *dst |= *src;
        }
    }
}

/// Estimates the rate of dirty pages per second.
///
/// Tracks samples over time to compute a running dirty rate.
/// Live migration uses this to detect convergence (dirty rate dropping).
#[derive(Debug)]
#[non_exhaustive]
pub struct DirtyRateEstimator {
    /// Previous sample: (`timestamp_ms`, `dirty_count`).
    last_sample: Option<(u64, usize)>,
}

impl DirtyRateEstimator {
    /// Creates a new rate estimator with no samples.
    #[must_use]
    pub fn new() -> Self {
        Self { last_sample: None }
    }

    /// Records a new sample and returns the estimated rate (pages/sec).
    ///
    /// Returns `None` if this is the first sample (need 2 for a rate).
    pub fn sample(&mut self, timestamp_ms: u64, dirty_count: usize) -> Option<f64> {
        let rate = self.last_sample.map(|(prev_ts, _prev_count)| {
            let dt_ms = timestamp_ms.saturating_sub(prev_ts);
            if dt_ms == 0 {
                return 0.0;
            }
            // Clamp to u32 for lossless f64 conversion. Practical VMs have
            // <2^32 dirty pages (<16 TiB) and intervals <49 days.
            let count = u32::try_from(dirty_count.min(u32::MAX as usize)).unwrap_or(u32::MAX);
            let dt = u32::try_from(dt_ms.min(u64::from(u32::MAX))).unwrap_or(u32::MAX);
            f64::from(count) / (f64::from(dt) / 1000.0)
        });
        self.last_sample = Some((timestamp_ms, dirty_count));
        rate
    }

    /// Resets the estimator, discarding all previous samples.
    pub fn reset(&mut self) {
        self.last_sample = None;
    }
}

impl Default for DirtyRateEstimator {
    fn default() -> Self {
        Self::new()
    }
}

/// A point-in-time snapshot of dirty page state.
#[derive(Debug)]
#[non_exhaustive]
pub struct DirtySnapshot {
    /// The dirty bitmap.
    pub bitmap: DirtyBitmap,
    /// Dirty rate estimate (pages/sec), `None` if insufficient data.
    pub rate: Option<f64>,
    /// Number of dirty pages in this snapshot.
    pub dirty_count: usize,
}

/// High-level dirty page tracker for a VM.
///
/// Wraps bitmap management and rate estimation for use by the migration system.
#[non_exhaustive]
pub struct DirtyTracker {
    /// Memory size in bytes.
    memory_size: usize,
    /// KVM memory region slot number.
    slot: u32,
    /// Rate estimator.
    rate_estimator: DirtyRateEstimator,
    /// Last computed rate.
    last_rate: Option<f64>,
}

impl DirtyTracker {
    /// Creates a new tracker for the given memory size and KVM slot.
    #[must_use]
    pub fn new(memory_size: usize, slot: u32) -> Self {
        Self {
            memory_size,
            slot,
            rate_estimator: DirtyRateEstimator::new(),
            last_rate: None,
        }
    }

    /// Collects dirty pages from a provided bitmap.
    ///
    /// In production, the caller obtains the bitmap from `KVM_GET_DIRTY_LOG`
    /// and passes it here. This separation keeps the KVM dependency out of
    /// unit tests.
    ///
    /// # Errors
    ///
    /// Returns [`DirtyTrackingError::BitmapSizeMismatch`] if the bitmap does
    /// not match the configured memory size.
    pub fn collect_from_bitmap(
        &mut self,
        bitmap: DirtyBitmap,
        timestamp_ms: u64,
    ) -> Result<DirtySnapshot, DirtyTrackingError> {
        let expected_pages = self.memory_size.div_ceil(PAGE_SIZE);
        if bitmap.page_count() != expected_pages {
            return Err(DirtyTrackingError::BitmapSizeMismatch {
                expected: expected_pages.div_ceil(8),
                actual: bitmap.bitmap_size(),
            });
        }
        let dirty_count = bitmap.dirty_count();
        let rate = self.rate_estimator.sample(timestamp_ms, dirty_count);
        self.last_rate = rate;

        Ok(DirtySnapshot {
            bitmap,
            rate,
            dirty_count,
        })
    }

    /// Returns the current estimated dirty rate (pages/sec).
    #[must_use]
    pub fn current_rate(&self) -> Option<f64> {
        self.last_rate
    }

    /// Returns the KVM memory slot number.
    #[must_use]
    pub fn slot(&self) -> u32 {
        self.slot
    }
}

#[cfg(test)]
#[path = "dirty_tracking_test.rs"]
mod tests;
