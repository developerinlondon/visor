//! Live migration pre-copy algorithm and protocol types.
//!
//! Provides the core migration logic without networking:
//!
//! 1. **Pre-copy algorithm** — iteratively transfers dirty pages until
//!    the dirty rate converges below a threshold.
//! 2. **Convergence detection** — monitors dirty page rate to decide
//!    when to enter the stop-and-copy phase.
//! 3. **Protocol messages** — serializable types for source↔destination
//!    communication during migration.
//!
//! # Design
//!
//! ```text
//! ┌──────────┐                          ┌──────────────┐
//! │  Source   │  MigrationInit ────────► │ Destination  │
//! │          │  ◄──── MigrationReady    │              │
//! │  PreCopy │  PageBatch ────────────► │              │
//! │  rounds  │  ◄──── PageBatchAck     │              │
//! │   ...    │  PageBatch ────────────► │              │
//! │          │  (converged)             │              │
//! │  pause   │  FinalState ───────────► │  resume VM   │
//! │  VM      │  ◄──── Resumed          │              │
//! └──────────┘                          └──────────────┘
//! ```
//!
//! The networking layer (in `visor-runtime`) serializes these messages
//! and sends them over the wire. This module is purely algorithmic.

use serde::{Deserialize, Serialize};

use crate::dirty_tracking::DirtySnapshot;

/// Errors from migration operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MigrationError {
    /// Dirty tracking failed during pre-copy.
    #[error("dirty tracking error: {0}")]
    DirtyTracking(#[from] crate::dirty_tracking::DirtyTrackingError),

    /// CPU template mismatch between source and destination.
    #[error("CPU template mismatch: source={src_template}, destination={dst_template}")]
    TemplateMismatch {
        /// Source CPU template name.
        src_template: String,
        /// Destination CPU template name.
        dst_template: String,
    },

    /// Migration was aborted.
    #[error("migration aborted: {reason}")]
    Aborted {
        /// Reason for aborting.
        reason: String,
    },

    /// Maximum pre-copy rounds exceeded without convergence.
    #[error("failed to converge after {rounds} pre-copy rounds")]
    ConvergenceFailed {
        /// Number of rounds attempted.
        rounds: u32,
    },

    /// Serialization error.
    #[error("serialization error: {0}")]
    Serialization(String),
}

/// Migration protocol messages exchanged between source and destination.
///
/// These are serialized to JSON (or another format) by the networking layer
/// in `visor-runtime`. This module only defines the types.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum MigrationMessage {
    /// Source → Destination: initialize migration.
    Init(MigrationInit),
    /// Destination → Source: ready to receive.
    Ready(MigrationReady),
    /// Source → Destination: batch of dirty pages.
    PageBatch(PageBatch),
    /// Destination → Source: batch acknowledged.
    PageBatchAck(PageBatchAck),
    /// Source → Destination: final VM state (stop-and-copy phase).
    FinalState(FinalState),
    /// Destination → Source: VM resumed successfully.
    Resumed {
        /// Whether the VM resumed successfully on the destination.
        success: bool,
    },
}

/// Migration initialization from source.
///
/// Sent as the first message to the destination to negotiate
/// compatibility before transferring any pages.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MigrationInit {
    /// VM memory size in bytes.
    pub memory_size: usize,
    /// CPU template name (must match destination).
    pub cpu_template: Option<String>,
    /// Number of vCPUs.
    pub vcpu_count: u32,
}

/// Destination ready response.
///
/// Sent after the destination validates the [`MigrationInit`] and
/// allocates resources for the incoming VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct MigrationReady {
    /// Unique session ID for this migration.
    pub session_id: String,
}

/// A batch of dirty pages for transfer.
///
/// Each round of the pre-copy algorithm produces one batch containing
/// pages that were dirtied since the previous round.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PageBatch {
    /// Pre-copy round number.
    pub round: u32,
    /// Page data: `(page_index, 4096-byte page content)`.
    pub pages: Vec<(usize, Vec<u8>)>,
}

/// Acknowledgment for a page batch.
///
/// Sent by the destination after receiving and applying a [`PageBatch`].
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PageBatchAck {
    /// Round number being acknowledged.
    pub round: u32,
    /// Number of pages received in this batch.
    pub received_count: usize,
}

/// Final VM state for the stop-and-copy phase.
///
/// Sent after the source VM is paused. Contains the CPU register state
/// and the last set of dirty pages that were modified between the final
/// pre-copy round and the VM pause.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct FinalState {
    /// Serialized CPU registers (format depends on architecture).
    pub cpu_state: Vec<u8>,
    /// Last dirty pages: `(page_index, 4096-byte page content)`.
    pub last_pages: Vec<(usize, Vec<u8>)>,
}

/// Detects when dirty page rate is low enough for stop-and-copy.
///
/// Monitors the dirty page rate reported by [`DirtySnapshot`] and
/// determines when the migration should transition from iterative
/// pre-copy to the final stop-and-copy phase.
#[derive(Debug)]
#[non_exhaustive]
pub struct ConvergenceDetector {
    /// Dirty rate threshold (pages/sec) below which migration can cut over.
    threshold_pages_per_sec: f64,
    /// Maximum pre-copy rounds before forcing cutover.
    max_rounds: u32,
}

impl ConvergenceDetector {
    /// Creates a detector with default threshold (50 pages/sec) and max rounds (30).
    #[must_use]
    pub fn new() -> Self {
        Self {
            threshold_pages_per_sec: 50.0,
            max_rounds: 30,
        }
    }

    /// Creates a detector with custom threshold and max rounds.
    #[must_use]
    pub fn with_config(threshold_pages_per_sec: f64, max_rounds: u32) -> Self {
        Self {
            threshold_pages_per_sec,
            max_rounds,
        }
    }

    /// Checks if migration should enter stop-and-copy phase.
    ///
    /// Returns `true` if any of these conditions hold:
    /// - `dirty_rate` is `Some(r)` where `r < threshold`
    /// - `round >= max_rounds` (forced cutover)
    ///
    /// Returns `false` if `dirty_rate` is `None` (insufficient data)
    /// and `round < max_rounds`.
    #[must_use]
    pub fn should_stop_and_copy(&self, dirty_rate: Option<f64>, round: u32) -> bool {
        if round >= self.max_rounds {
            return true;
        }
        match dirty_rate {
            Some(rate) => rate < self.threshold_pages_per_sec,
            None => false,
        }
    }
}

impl Default for ConvergenceDetector {
    fn default() -> Self {
        Self::new()
    }
}

/// Action to take after processing a dirty snapshot.
#[derive(Debug)]
#[non_exhaustive]
pub enum PreCopyAction {
    /// Send these dirty page indices to the destination.
    SendPages {
        /// Round number for this batch.
        round: u32,
        /// Indices of dirty pages to transfer.
        dirty_page_indices: Vec<usize>,
    },
    /// Converged — pause the VM and do final state transfer.
    StopAndCopy {
        /// Round number at which convergence was detected.
        round: u32,
    },
}

/// Pre-copy migration state machine.
///
/// Tracks the iterative pre-copy algorithm: each round collects dirty
/// pages, checks convergence, and either sends another batch or signals
/// that the VM should be paused for final state transfer.
///
/// # Example
///
/// ```rust,no_run
/// use visor_vmm::migration::{PreCopyState, PreCopyAction};
/// use visor_vmm::dirty_tracking::DirtySnapshot;
///
/// let mut state = PreCopyState::new();
/// // In a loop, collect dirty snapshots and process them:
/// // let snapshot = tracker.collect_from_bitmap(bitmap, timestamp)?;
/// // match state.process_snapshot(&snapshot) {
/// //     PreCopyAction::SendPages { dirty_page_indices, .. } => { /* send pages */ }
/// //     PreCopyAction::StopAndCopy { .. } => { /* pause VM, send final state */ }
/// // }
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct PreCopyState {
    /// Current round number (1-indexed after first `process_snapshot`).
    pub round: u32,
    /// Total pages transferred so far.
    pub total_pages_sent: usize,
    /// Convergence detector.
    convergence: ConvergenceDetector,
}

impl PreCopyState {
    /// Creates a new pre-copy state with default convergence settings.
    #[must_use]
    pub fn new() -> Self {
        Self {
            round: 0,
            total_pages_sent: 0,
            convergence: ConvergenceDetector::new(),
        }
    }

    /// Creates a new pre-copy state with a custom convergence detector.
    #[must_use]
    pub fn with_convergence(convergence: ConvergenceDetector) -> Self {
        Self {
            round: 0,
            total_pages_sent: 0,
            convergence,
        }
    }

    /// Processes a dirty snapshot and decides the next action.
    ///
    /// Increments the round counter, collects dirty page indices from the
    /// snapshot's bitmap, and checks convergence. Returns either
    /// [`PreCopyAction::SendPages`] with the page indices to transfer, or
    /// [`PreCopyAction::StopAndCopy`] when converged.
    ///
    /// # Convergence
    ///
    /// Stop-and-copy is triggered when:
    /// - The dirty rate drops below the threshold, OR
    /// - The maximum number of rounds is exceeded, OR
    /// - There are zero dirty pages (immediate convergence).
    pub fn process_snapshot(&mut self, snapshot: &DirtySnapshot) -> PreCopyAction {
        self.round += 1;
        let round = self.round;

        let dirty_page_indices: Vec<usize> = snapshot.bitmap.dirty_pages().collect();

        // Zero dirty pages means immediate convergence.
        if dirty_page_indices.is_empty() {
            return PreCopyAction::StopAndCopy { round };
        }

        // Check if we should stop-and-copy based on rate or max rounds.
        if self.convergence.should_stop_and_copy(snapshot.rate, round) {
            return PreCopyAction::StopAndCopy { round };
        }

        self.total_pages_sent += dirty_page_indices.len();

        PreCopyAction::SendPages {
            round,
            dirty_page_indices,
        }
    }
}

impl Default for PreCopyState {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[path = "migration_test.rs"]
mod tests;
