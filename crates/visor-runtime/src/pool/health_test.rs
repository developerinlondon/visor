use std::sync::Arc;
use std::time::Duration;

use crate::api::sse::EventBroadcaster;
use crate::backend::{VmInfo, VmState};

use super::*;

// ── HealthStatus display ─────────────────────────────────────────

#[test]
fn health_status_healthy_displays_correctly() {
    let status = HealthStatus::Healthy;
    assert_eq!(format!("{status}"), "healthy");
}

#[test]
fn health_status_unhealthy_includes_reason() {
    let status = HealthStatus::Unhealthy("ping timeout".to_owned());
    let display = format!("{status}");
    assert!(display.contains("ping timeout"), "got: {display}");
}

#[test]
fn health_status_unknown_displays_correctly() {
    let status = HealthStatus::Unknown;
    assert_eq!(format!("{status}"), "unknown");
}

// ── HealthStatus serialization ───────────────────────────────────

#[test]
fn health_status_serializes_healthy() {
    let json = serde_json::to_value(&HealthStatus::Healthy).unwrap();
    assert_eq!(json, "healthy");
}

#[test]
fn health_status_serializes_unhealthy_with_reason() {
    let json = serde_json::to_value(HealthStatus::Unhealthy("timeout".to_owned())).unwrap();
    assert_eq!(json, serde_json::json!({"unhealthy": "timeout"}));
}

#[test]
fn health_status_serializes_unknown() {
    let json = serde_json::to_value(&HealthStatus::Unknown).unwrap();
    assert_eq!(json, "unknown");
}

#[test]
fn health_status_deserializes_round_trip() {
    let statuses = vec![
        HealthStatus::Healthy,
        HealthStatus::Unhealthy("test reason".to_owned()),
        HealthStatus::Unknown,
    ];
    for status in statuses {
        let json = serde_json::to_string(&status).unwrap();
        let deserialized: HealthStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(format!("{deserialized}"), format!("{status}"));
    }
}

// ── HealthCheckConfig defaults ───────────────────────────────────

#[test]
fn health_check_config_default_timeout_is_2s() {
    let config = HealthCheckConfig::default();
    assert_eq!(config.ping_timeout, Duration::from_secs(2));
}

#[test]
fn health_check_config_default_interval_is_30s() {
    let config = HealthCheckConfig::default();
    assert_eq!(config.check_interval, Duration::from_secs(30));
}

#[test]
fn health_check_config_default_failure_threshold_is_3() {
    let config = HealthCheckConfig::default();
    assert_eq!(config.failure_threshold, 3);
}

// ── Mock VsockHealthPinger ───────────────────────────────────────

/// Mock pinger that returns configurable success/failure per CID.
struct MockVsockPinger {
    /// CIDs that will succeed; all others fail.
    healthy_cids: Vec<u32>,
}

#[async_trait::async_trait]
impl VsockHealthPinger for MockVsockPinger {
    async fn ping(&self, cid: u32, _timeout: Duration) -> anyhow::Result<()> {
        if self.healthy_cids.contains(&cid) {
            Ok(())
        } else {
            anyhow::bail!("mock ping failed for CID {cid}")
        }
    }
}

fn mock_pinger(healthy_cids: Vec<u32>) -> Arc<dyn VsockHealthPinger> {
    Arc::new(MockVsockPinger { healthy_cids })
}

// ── HealthChecker single VM check ────────────────────────────────

#[tokio::test]
async fn health_checker_reports_healthy_for_responding_vm() {
    let pinger = mock_pinger(vec![3]);
    let config = HealthCheckConfig::default();
    let checker = HealthChecker::new(pinger, config);

    let status = checker.check_vm(3).await;
    assert!(
        matches!(status, HealthStatus::Healthy),
        "expected Healthy, got: {status}"
    );
}

#[tokio::test]
async fn health_checker_reports_unhealthy_for_non_responding_vm() {
    let pinger = mock_pinger(vec![]); // no healthy CIDs
    let config = HealthCheckConfig::default();
    let checker = HealthChecker::new(pinger, config);

    let status = checker.check_vm(99).await;
    assert!(
        matches!(status, HealthStatus::Unhealthy(_)),
        "expected Unhealthy, got: {status}"
    );
}

// ── HealthCheckLoop tracking ─────────────────────────────────────

fn test_vm_info(id: &str, cid: u32) -> (VmInfo, u32) {
    let info = VmInfo::new(
        id.to_owned(),
        "alpine:latest".to_owned(),
        VmState::Running,
        "1970-01-01T00:00:00Z".to_owned(),
        512,
        1,
    );
    (info, cid)
}

#[tokio::test]
async fn health_loop_tracks_consecutive_failures() {
    let pinger = mock_pinger(vec![]); // all pings fail
    let config = HealthCheckConfig {
        ping_timeout: Duration::from_millis(10),
        check_interval: Duration::from_millis(10),
        failure_threshold: 3,
    };
    let events = Arc::new(EventBroadcaster::new(16));
    let health_loop = HealthCheckLoop::new(
        HealthChecker::new(pinger, config.clone()),
        events.clone(),
        config,
    );

    // Simulate a running VM
    let (vm_info, cid) = test_vm_info("vm-fail", 3);
    let running_vms: Vec<(String, u32)> = vec![(vm_info.id.clone(), cid)];

    // Run check 3 times — should reach threshold
    for _ in 0..3 {
        health_loop.check_all(&running_vms).await;
    }

    let statuses = health_loop.statuses().await;
    let status = statuses.get("vm-fail").expect("vm-fail should have status");
    assert!(
        matches!(status, HealthStatus::Unhealthy(_)),
        "expected Unhealthy after 3 failures, got: {status}"
    );
}

#[tokio::test]
async fn health_loop_resets_failures_on_success() {
    let pinger = mock_pinger(vec![3]); // CID 3 is healthy
    let config = HealthCheckConfig {
        ping_timeout: Duration::from_millis(10),
        check_interval: Duration::from_millis(10),
        failure_threshold: 3,
    };
    let events = Arc::new(EventBroadcaster::new(16));
    let health_loop = HealthCheckLoop::new(
        HealthChecker::new(pinger, config.clone()),
        events.clone(),
        config,
    );

    let running_vms: Vec<(String, u32)> = vec![("vm-ok".to_owned(), 3)];

    // Run check once
    health_loop.check_all(&running_vms).await;

    let statuses = health_loop.statuses().await;
    let status = statuses.get("vm-ok").expect("vm-ok should have status");
    assert!(
        matches!(status, HealthStatus::Healthy),
        "expected Healthy, got: {status}"
    );
}

#[tokio::test]
async fn health_loop_emits_event_on_unhealthy_transition() {
    let pinger = mock_pinger(vec![]); // all pings fail
    let config = HealthCheckConfig {
        ping_timeout: Duration::from_millis(10),
        check_interval: Duration::from_millis(10),
        failure_threshold: 2, // lower threshold for faster test
    };
    let events = Arc::new(EventBroadcaster::new(16));
    let mut rx = events.subscribe();

    let health_loop = HealthCheckLoop::new(
        HealthChecker::new(pinger, config.clone()),
        events.clone(),
        config,
    );

    let running_vms: Vec<(String, u32)> = vec![("vm-event".to_owned(), 5)];

    // Run 2 checks to hit threshold
    for _ in 0..2 {
        health_loop.check_all(&running_vms).await;
    }

    // Should have received an unhealthy event
    let event = rx.try_recv().expect("should have received event");
    assert_eq!(event.event_type, "vm.health.unhealthy");
    assert_eq!(event.vm_id, "vm-event");
}

#[tokio::test]
async fn health_loop_emits_event_on_recovery() {
    // Start with failing pinger, then switch to healthy
    let config = HealthCheckConfig {
        ping_timeout: Duration::from_millis(10),
        check_interval: Duration::from_millis(10),
        failure_threshold: 1, // immediate threshold
    };
    let events = Arc::new(EventBroadcaster::new(16));
    let mut rx = events.subscribe();

    // Phase 1: fail
    let pinger_fail = mock_pinger(vec![]);
    let health_loop = HealthCheckLoop::new(
        HealthChecker::new(pinger_fail, config.clone()),
        events.clone(),
        config.clone(),
    );
    let running_vms: Vec<(String, u32)> = vec![("vm-recover".to_owned(), 3)];
    health_loop.check_all(&running_vms).await;

    // Consume the unhealthy event
    let event = rx.try_recv().expect("should have unhealthy event");
    assert_eq!(event.event_type, "vm.health.unhealthy");

    // Phase 2: recover — swap pinger to healthy
    health_loop
        .replace_checker(HealthChecker::new(
            mock_pinger(vec![3]),
            health_loop.config().clone(),
        ))
        .await;
    health_loop.check_all(&running_vms).await;

    // Should get a recovery event
    let event = rx.try_recv().expect("should have recovery event");
    assert_eq!(event.event_type, "vm.health.recovered");
    assert_eq!(event.vm_id, "vm-recover");
}

#[tokio::test]
async fn health_loop_removes_stale_vms() {
    let pinger = mock_pinger(vec![3]);
    let config = HealthCheckConfig::default();
    let events = Arc::new(EventBroadcaster::new(16));
    let health_loop = HealthCheckLoop::new(
        HealthChecker::new(pinger, config.clone()),
        events.clone(),
        config,
    );

    // Check with vm-a
    let running_vms: Vec<(String, u32)> = vec![("vm-a".to_owned(), 3)];
    health_loop.check_all(&running_vms).await;

    assert!(health_loop.statuses().await.contains_key("vm-a"));

    // Now check with empty list — vm-a should be cleaned up
    health_loop.check_all(&[]).await;
    assert!(!health_loop.statuses().await.contains_key("vm-a"));
}

// ── VmHealthReport serialization ─────────────────────────────────

#[test]
fn vm_health_report_serializes_correctly() {
    let report = VmHealthReport {
        vm_id: "test-vm".to_owned(),
        status: HealthStatus::Healthy,
        consecutive_failures: 0,
    };
    let json = serde_json::to_value(&report).unwrap();
    assert_eq!(json["vm_id"], "test-vm");
    assert_eq!(json["status"], "healthy");
    assert_eq!(json["consecutive_failures"], 0);
}
