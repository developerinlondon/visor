//! Per-VM metrics collection.
//!
//! Provides a [`MetricsCollector`] trait for gathering CPU time, memory usage,
//! disk I/O counters, and network counters for individual VMs.
//!
//! # Architecture
//!
//! ```text
//! MetricsCollector (trait)
//! └── KvmMetricsCollector
//!     ├── /proc/{pid}/stat        → cpu_time_us
//!     ├── /proc/{pid}/smaps_rollup → memory_rss_bytes
//!     └── DeviceCounters (Arc)    → disk + network I/O
//! ```
//!
//! CPU and memory metrics are read from procfs. Disk and network counters
//! come from shared [`DeviceCounters`] that virtio device implementations
//! increment atomically on each I/O operation.

use std::fs;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Clock ticks per second on Linux x86\_64.
///
/// This is the value returned by `sysconf(_SC_CLK_TCK)`, which is universally
/// 100 on Linux x86\_64 systems. Used to convert `/proc/*/stat` jiffies to
/// microseconds.
const CLK_TCK: u64 = 100;

/// Microseconds per second.
const MICROS_PER_SEC: u64 = 1_000_000;

/// Errors from metrics collection operations.
///
/// # Errors
///
/// Returned when procfs files cannot be read or their contents are malformed.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum MetricsError {
    /// Failed to read a `/proc` file.
    #[error("failed to read {path}: {source}")]
    ProcRead {
        /// Path that could not be read.
        path: String,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// Failed to parse a `/proc` file's contents.
    #[error("failed to parse {path}: {reason}")]
    ProcParse {
        /// Path whose contents could not be parsed.
        path: String,
        /// Description of what went wrong.
        reason: String,
    },
}

/// Point-in-time VM performance metrics snapshot.
///
/// All counters are cumulative since VM start unless otherwise noted.
/// Created by [`MetricsCollector::collect`].
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub struct VmMetrics {
    /// Total CPU time consumed in microseconds (user + system).
    pub cpu_time_us: u64,

    /// Resident set size in bytes.
    pub memory_rss_bytes: u64,

    /// Total bytes read from block devices.
    pub disk_read_bytes: u64,

    /// Total bytes written to block devices.
    pub disk_write_bytes: u64,

    /// Total read operations on block devices.
    pub disk_read_ops: u64,

    /// Total write operations on block devices.
    pub disk_write_ops: u64,

    /// Total bytes received on network interfaces.
    pub net_rx_bytes: u64,

    /// Total bytes transmitted on network interfaces.
    pub net_tx_bytes: u64,

    /// Total packets received on network interfaces.
    pub net_rx_packets: u64,

    /// Total packets transmitted on network interfaces.
    pub net_tx_packets: u64,
}

/// Collects performance metrics for a single VM.
///
/// Implementations must be thread-safe (`Send + Sync`) because metrics are
/// typically collected from an async HTTP handler (Prometheus scrape endpoint).
///
/// # Errors
///
/// Implementations return [`MetricsError`] if underlying data sources
/// (procfs files, device counters) are unavailable or unparseable.
pub trait MetricsCollector: Send + Sync {
    /// Collect a point-in-time snapshot of VM metrics.
    ///
    /// # Errors
    ///
    /// Returns [`MetricsError`] if procfs data cannot be read or parsed.
    fn collect(&self) -> Result<VmMetrics, MetricsError>;
}

/// Shared atomic counters for virtio device I/O tracking.
///
/// Block and network device implementations increment these counters
/// on each I/O operation. The metrics collector reads them atomically
/// to produce a consistent snapshot.
///
/// # Example
///
/// ```
/// use std::sync::Arc;
/// use std::sync::atomic::Ordering;
/// use visor_vmm::metrics::DeviceCounters;
///
/// let counters = Arc::new(DeviceCounters::default());
/// counters.disk_read_bytes.fetch_add(4096, Ordering::Relaxed);
/// assert_eq!(counters.disk_read_bytes.load(Ordering::Relaxed), 4096);
/// ```
#[derive(Debug, Default)]
#[non_exhaustive]
pub struct DeviceCounters {
    /// Total bytes read from virtio-blk devices.
    pub disk_read_bytes: AtomicU64,

    /// Total bytes written to virtio-blk devices.
    pub disk_write_bytes: AtomicU64,

    /// Total read operations on virtio-blk devices.
    pub disk_read_ops: AtomicU64,

    /// Total write operations on virtio-blk devices.
    pub disk_write_ops: AtomicU64,

    /// Total bytes received on virtio-net devices.
    pub net_rx_bytes: AtomicU64,

    /// Total bytes transmitted on virtio-net devices.
    pub net_tx_bytes: AtomicU64,

    /// Total packets received on virtio-net devices.
    pub net_rx_packets: AtomicU64,

    /// Total packets transmitted on virtio-net devices.
    pub net_tx_packets: AtomicU64,
}

/// Collects VM metrics from KVM and `/proc` on Linux.
///
/// Reads CPU time from `/proc/{pid}/stat`, memory RSS from
/// `/proc/{pid}/smaps_rollup`, and device I/O from shared
/// [`DeviceCounters`].
///
/// # Example
///
/// ```no_run
/// use std::sync::Arc;
/// use visor_vmm::metrics::{DeviceCounters, KvmMetricsCollector, MetricsCollector};
///
/// let counters = Arc::new(DeviceCounters::default());
/// let collector = KvmMetricsCollector::new(std::process::id(), counters);
/// let metrics = collector.collect().expect("failed to collect metrics");
/// println!("CPU time: {} us", metrics.cpu_time_us);
/// ```
#[derive(Debug)]
#[non_exhaustive]
pub struct KvmMetricsCollector {
    /// Process ID of the VM (used to read `/proc` entries).
    pid: u32,
    /// Shared device I/O counters from virtio devices.
    counters: Arc<DeviceCounters>,
}

impl KvmMetricsCollector {
    /// Create a new collector for the given process ID.
    ///
    /// The `counters` are shared with virtio device implementations and
    /// updated atomically on each I/O operation.
    #[must_use]
    pub fn new(pid: u32, counters: Arc<DeviceCounters>) -> Self {
        Self { pid, counters }
    }

    /// Returns the process ID being monitored.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.pid
    }
}

impl MetricsCollector for KvmMetricsCollector {
    fn collect(&self) -> Result<VmMetrics, MetricsError> {
        let stat_path = format!("/proc/{}/stat", self.pid);
        let stat_content =
            fs::read_to_string(&stat_path).map_err(|source| MetricsError::ProcRead {
                path: stat_path.clone(),
                source,
            })?;
        let cpu_time_us = parse_cpu_time_us(&stat_content, &stat_path)?;

        let smaps_path = format!("/proc/{}/smaps_rollup", self.pid);
        let memory_rss_bytes = match fs::read_to_string(&smaps_path) {
            Ok(content) => parse_rss_bytes(&content, &smaps_path)?,
            Err(source) => {
                // Fall back to /proc/{pid}/status if smaps_rollup is unavailable.
                let status_path = format!("/proc/{}/status", self.pid);
                let status_content =
                    fs::read_to_string(&status_path).map_err(|source| MetricsError::ProcRead {
                        path: status_path.clone(),
                        source,
                    })?;
                parse_rss_from_status(&status_content, &status_path).map_err(|_| {
                    // Return the original smaps_rollup error if status parsing also fails.
                    MetricsError::ProcRead {
                        path: smaps_path,
                        source,
                    }
                })?
            }
        };

        Ok(VmMetrics {
            cpu_time_us,
            memory_rss_bytes,
            disk_read_bytes: self.counters.disk_read_bytes.load(Ordering::Relaxed),
            disk_write_bytes: self.counters.disk_write_bytes.load(Ordering::Relaxed),
            disk_read_ops: self.counters.disk_read_ops.load(Ordering::Relaxed),
            disk_write_ops: self.counters.disk_write_ops.load(Ordering::Relaxed),
            net_rx_bytes: self.counters.net_rx_bytes.load(Ordering::Relaxed),
            net_tx_bytes: self.counters.net_tx_bytes.load(Ordering::Relaxed),
            net_rx_packets: self.counters.net_rx_packets.load(Ordering::Relaxed),
            net_tx_packets: self.counters.net_tx_packets.load(Ordering::Relaxed),
        })
    }
}

// ── Parsing helpers (pub(crate) for testing) ─────────────────────────────────

/// Parse CPU time in microseconds from `/proc/{pid}/stat` content.
///
/// Extracts `utime` (field 14) and `stime` (field 15) from the stat line,
/// converts from clock ticks to microseconds.
pub(crate) fn parse_cpu_time_us(stat_content: &str, path: &str) -> Result<u64, MetricsError> {
    // The comm field (field 2) is enclosed in parentheses and may contain
    // spaces, parentheses, and other special characters. Find the LAST ')'
    // to reliably delimit it.
    let comm_end = stat_content
        .rfind(')')
        .ok_or_else(|| MetricsError::ProcParse {
            path: path.to_owned(),
            reason: "no closing parenthesis in comm field".to_owned(),
        })?;

    // Fields after comm start 2 characters past the closing paren: ") S ..."
    let after_comm = stat_content
        .get(comm_end + 2..)
        .ok_or_else(|| MetricsError::ProcParse {
            path: path.to_owned(),
            reason: "stat line truncated after comm field".to_owned(),
        })?;

    let fields: Vec<&str> = after_comm.split_whitespace().collect();

    // After comm: state(0) ppid(1) pgrp(2) session(3) tty_nr(4) tpgid(5)
    // flags(6) minflt(7) cminflt(8) majflt(9) cmajflt(10) utime(11) stime(12)
    let min_fields = 13;
    if fields.len() < min_fields {
        return Err(MetricsError::ProcParse {
            path: path.to_owned(),
            reason: format!(
                "expected at least {min_fields} fields after comm, got {}",
                fields.len()
            ),
        });
    }

    let utime: u64 = fields[11].parse().map_err(|_| MetricsError::ProcParse {
        path: path.to_owned(),
        reason: format!("invalid utime value: {}", fields[11]),
    })?;

    let stime: u64 = fields[12].parse().map_err(|_| MetricsError::ProcParse {
        path: path.to_owned(),
        reason: format!("invalid stime value: {}", fields[12]),
    })?;

    Ok((utime + stime) * MICROS_PER_SEC / CLK_TCK)
}

/// Parse RSS in bytes from `/proc/{pid}/smaps_rollup` content.
///
/// Looks for the `Rss:` line and converts the kilobyte value to bytes.
pub(crate) fn parse_rss_bytes(smaps_content: &str, path: &str) -> Result<u64, MetricsError> {
    parse_kb_field(smaps_content, "Rss:", path)
}

/// Parse RSS in bytes from `/proc/{pid}/status` content.
///
/// Looks for the `VmRSS:` line and converts the kilobyte value to bytes.
/// Used as a fallback when `smaps_rollup` is unavailable.
pub(crate) fn parse_rss_from_status(status_content: &str, path: &str) -> Result<u64, MetricsError> {
    parse_kb_field(status_content, "VmRSS:", path)
}

/// Parse a kilobyte field from a `/proc` file.
///
/// Matches lines like `FieldName:    1234 kB` and returns the value in bytes.
fn parse_kb_field(content: &str, field_prefix: &str, path: &str) -> Result<u64, MetricsError> {
    for line in content.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix(field_prefix) {
            let rest = rest.trim();
            let value_str = rest.strip_suffix("kB").map_or(rest, str::trim);
            let kb: u64 = value_str.parse().map_err(|_| MetricsError::ProcParse {
                path: path.to_owned(),
                reason: format!("invalid {field_prefix} value: {value_str}"),
            })?;
            return Ok(kb * 1024);
        }
    }

    Err(MetricsError::ProcParse {
        path: path.to_owned(),
        reason: format!("{field_prefix} field not found"),
    })
}

#[cfg(test)]
#[path = "metrics_test.rs"]
mod tests;
