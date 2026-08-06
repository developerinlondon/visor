use std::sync::Arc;
use std::sync::atomic::Ordering;

use super::*;

// ── VmMetrics ────────────────────────────────────────────────────────────────

#[test]
fn vm_metrics_default_is_all_zeros() {
    let m = VmMetrics::default();
    assert_eq!(m.cpu_time_us, 0);
    assert_eq!(m.memory_rss_bytes, 0);
    assert_eq!(m.disk_read_bytes, 0);
    assert_eq!(m.disk_write_bytes, 0);
    assert_eq!(m.disk_read_ops, 0);
    assert_eq!(m.disk_write_ops, 0);
    assert_eq!(m.net_rx_bytes, 0);
    assert_eq!(m.net_tx_bytes, 0);
    assert_eq!(m.net_rx_packets, 0);
    assert_eq!(m.net_tx_packets, 0);
}

#[test]
fn vm_metrics_clone_preserves_all_fields() {
    let m = VmMetrics {
        cpu_time_us: 1_000_000,
        memory_rss_bytes: 256 * 1024 * 1024,
        disk_read_bytes: 4096,
        disk_write_bytes: 8192,
        disk_read_ops: 10,
        disk_write_ops: 20,
        net_rx_bytes: 5000,
        net_tx_bytes: 6000,
        net_rx_packets: 50,
        net_tx_packets: 60,
    };
    assert_eq!(m, m.clone());
}

#[test]
fn vm_metrics_debug_contains_struct_name() {
    let m = VmMetrics::default();
    let debug = format!("{m:?}");
    assert!(debug.contains("VmMetrics"));
    assert!(debug.contains("cpu_time_us"));
}

#[test]
fn vm_metrics_serde_round_trip() {
    let original = VmMetrics {
        cpu_time_us: 123_456,
        memory_rss_bytes: 1024 * 1024,
        disk_read_bytes: 4096,
        disk_write_bytes: 8192,
        disk_read_ops: 10,
        disk_write_ops: 20,
        net_rx_bytes: 5000,
        net_tx_bytes: 6000,
        net_rx_packets: 50,
        net_tx_packets: 60,
    };
    let json = serde_json::to_string(&original).unwrap();
    let deserialized: VmMetrics = serde_json::from_str(&json).unwrap();
    assert_eq!(original, deserialized);
}

// ── MetricsError ─────────────────────────────────────────────────────────────

#[test]
fn metrics_error_proc_read_display() {
    let err = MetricsError::ProcRead {
        path: "/proc/123/stat".to_owned(),
        source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
    };
    let msg = err.to_string();
    assert!(msg.contains("/proc/123/stat"));
    assert!(msg.contains("no such file"));
}

#[test]
fn metrics_error_proc_parse_display() {
    let err = MetricsError::ProcParse {
        path: "/proc/123/stat".to_owned(),
        reason: "missing fields".to_owned(),
    };
    let msg = err.to_string();
    assert!(msg.contains("/proc/123/stat"));
    assert!(msg.contains("missing fields"));
}

// ── DeviceCounters ───────────────────────────────────────────────────────────

#[test]
fn device_counters_default_is_all_zeros() {
    let c = DeviceCounters::default();
    assert_eq!(c.disk_read_bytes.load(Ordering::Relaxed), 0);
    assert_eq!(c.disk_write_bytes.load(Ordering::Relaxed), 0);
    assert_eq!(c.disk_read_ops.load(Ordering::Relaxed), 0);
    assert_eq!(c.disk_write_ops.load(Ordering::Relaxed), 0);
    assert_eq!(c.net_rx_bytes.load(Ordering::Relaxed), 0);
    assert_eq!(c.net_tx_bytes.load(Ordering::Relaxed), 0);
    assert_eq!(c.net_rx_packets.load(Ordering::Relaxed), 0);
    assert_eq!(c.net_tx_packets.load(Ordering::Relaxed), 0);
}

#[test]
fn device_counters_atomic_increment() {
    let c = DeviceCounters::default();
    c.disk_read_bytes.fetch_add(4096, Ordering::Relaxed);
    c.net_rx_packets.fetch_add(1, Ordering::Relaxed);
    assert_eq!(c.disk_read_bytes.load(Ordering::Relaxed), 4096);
    assert_eq!(c.net_rx_packets.load(Ordering::Relaxed), 1);
}

// ── Parsing: CPU time ────────────────────────────────────────────────────────

#[test]
fn parse_cpu_time_us_valid_stat_line() {
    // Fields after comm: state ppid pgrp session tty_nr tpgid flags
    //   minflt cminflt majflt cmajflt utime(11) stime(12) ...
    let stat = "1234 (test) S 1 1234 1234 0 -1 4194304 5000 0 100 0 150 30 0 0 20 0 1 0 12345";
    let result = parse_cpu_time_us(stat, "/proc/1234/stat").unwrap();
    // (150 + 30) * 1_000_000 / 100 = 1_800_000
    assert_eq!(result, 1_800_000);
}

#[test]
fn parse_cpu_time_us_comm_with_spaces() {
    let stat =
        "1234 (my process) S 1 1234 1234 0 -1 4194304 5000 0 100 0 200 100 0 0 20 0 1 0 12345";
    let result = parse_cpu_time_us(stat, "/proc/1234/stat").unwrap();
    // (200 + 100) * 1_000_000 / 100 = 3_000_000
    assert_eq!(result, 3_000_000);
}

#[test]
fn parse_cpu_time_us_comm_with_parentheses() {
    let stat = "1234 (bash (old)) S 1 1234 1234 0 -1 4194304 5000 0 100 0 50 25 0 0 20 0 1 0 12345";
    let result = parse_cpu_time_us(stat, "/proc/1234/stat").unwrap();
    // (50 + 25) * 1_000_000 / 100 = 750_000
    assert_eq!(result, 750_000);
}

#[test]
fn parse_cpu_time_us_missing_close_paren_returns_error() {
    let stat = "1234 (test S 1 1234";
    let result = parse_cpu_time_us(stat, "/proc/1234/stat");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("parenthesis"));
}

#[test]
fn parse_cpu_time_us_too_few_fields_returns_error() {
    let stat = "1234 (test) S 1 1234";
    let result = parse_cpu_time_us(stat, "/proc/1234/stat");
    assert!(result.is_err());
}

#[test]
fn parse_cpu_time_us_invalid_utime_returns_error() {
    let stat = "1234 (test) S 1 1234 1234 0 -1 4194304 5000 0 100 0 INVALID 30 0 0 20 0 1 0";
    let result = parse_cpu_time_us(stat, "/proc/1234/stat");
    assert!(result.is_err());
}

// ── Parsing: RSS ─────────────────────────────────────────────────────────────

#[test]
fn parse_rss_bytes_valid_smaps() {
    let smaps = "\
5632701a4000-7fff656e9000 ---p 00000000 00:00 0          [rollup]
Rss:                1024 kB
Pss:                 512 kB
Shared_Clean:        256 kB
";
    let result = parse_rss_bytes(smaps, "/proc/1234/smaps_rollup").unwrap();
    assert_eq!(result, 1024 * 1024);
}

#[test]
fn parse_rss_bytes_large_value() {
    let smaps = "Rss:             1048576 kB\n";
    let result = parse_rss_bytes(smaps, "/proc/1234/smaps_rollup").unwrap();
    assert_eq!(result, 1_048_576 * 1024);
}

#[test]
fn parse_rss_bytes_zero() {
    let smaps = "Rss:                   0 kB\n";
    let result = parse_rss_bytes(smaps, "/proc/1234/smaps_rollup").unwrap();
    assert_eq!(result, 0);
}

#[test]
fn parse_rss_bytes_missing_field_returns_error() {
    let smaps = "Pss:                 512 kB\nShared_Clean:        256 kB\n";
    let result = parse_rss_bytes(smaps, "/proc/1234/smaps_rollup");
    assert!(result.is_err());
    let err_msg = result.unwrap_err().to_string();
    assert!(err_msg.contains("Rss"));
}

#[test]
fn parse_rss_bytes_invalid_value_returns_error() {
    let smaps = "Rss:              abc kB\n";
    let result = parse_rss_bytes(smaps, "/proc/1234/smaps_rollup");
    assert!(result.is_err());
}

// ── MetricsCollector trait (mock) ────────────────────────────────────────────

struct MockCollector {
    metrics: VmMetrics,
}

impl MetricsCollector for MockCollector {
    fn collect(&self) -> Result<VmMetrics, MetricsError> {
        Ok(self.metrics.clone())
    }
}

#[test]
fn mock_collector_returns_expected_metrics() {
    let expected = VmMetrics {
        cpu_time_us: 42,
        memory_rss_bytes: 1024,
        ..VmMetrics::default()
    };
    let collector = MockCollector {
        metrics: expected.clone(),
    };
    let result = collector.collect().unwrap();
    assert_eq!(result, expected);
}

#[test]
fn mock_collector_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MockCollector>();
}

// ── KvmMetricsCollector ──────────────────────────────────────────────────────

#[test]
fn kvm_collector_stores_pid() {
    let counters = Arc::new(DeviceCounters::default());
    let collector = KvmMetricsCollector::new(1234, counters);
    assert_eq!(collector.pid(), 1234);
}

#[test]
fn kvm_collector_is_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<KvmMetricsCollector>();
}

#[cfg(target_os = "linux")]
#[test]
fn kvm_collector_collect_self_process() {
    let pid = std::process::id();
    let counters = Arc::new(DeviceCounters::default());
    counters.disk_read_bytes.store(4096, Ordering::Relaxed);
    counters.net_rx_packets.store(100, Ordering::Relaxed);

    let collector = KvmMetricsCollector::new(pid, counters);
    let metrics = collector.collect().unwrap();

    // CPU time may be 0 if the test binary hasn't consumed a full clock tick.
    // The important assertion is that collect() succeeds and returns valid data.
    assert!(
        metrics.memory_rss_bytes > 0,
        "memory_rss_bytes should be > 0 for our own process"
    );
    // Device counters should be read from the atomics.
    assert_eq!(metrics.disk_read_bytes, 4096);
    assert_eq!(metrics.net_rx_packets, 100);
}

#[test]
fn kvm_collector_collect_invalid_pid_returns_error() {
    let counters = Arc::new(DeviceCounters::default());
    let collector = KvmMetricsCollector::new(u32::MAX, counters);
    let result = collector.collect();
    assert!(result.is_err());
}

#[cfg(target_os = "linux")]
#[test]
fn kvm_collector_reads_all_device_counters() {
    let pid = std::process::id();
    let counters = Arc::new(DeviceCounters::default());
    counters.disk_read_bytes.store(1000, Ordering::Relaxed);
    counters.disk_write_bytes.store(2000, Ordering::Relaxed);
    counters.disk_read_ops.store(10, Ordering::Relaxed);
    counters.disk_write_ops.store(20, Ordering::Relaxed);
    counters.net_rx_bytes.store(3000, Ordering::Relaxed);
    counters.net_tx_bytes.store(4000, Ordering::Relaxed);
    counters.net_rx_packets.store(30, Ordering::Relaxed);
    counters.net_tx_packets.store(40, Ordering::Relaxed);

    let collector = KvmMetricsCollector::new(pid, counters);
    let metrics = collector.collect().unwrap();

    assert_eq!(metrics.disk_read_bytes, 1000);
    assert_eq!(metrics.disk_write_bytes, 2000);
    assert_eq!(metrics.disk_read_ops, 10);
    assert_eq!(metrics.disk_write_ops, 20);
    assert_eq!(metrics.net_rx_bytes, 3000);
    assert_eq!(metrics.net_tx_bytes, 4000);
    assert_eq!(metrics.net_rx_packets, 30);
    assert_eq!(metrics.net_tx_packets, 40);
}
