use std::collections::HashMap;

use super::*;
use crate::api::routes::info::{
    GuestCapabilities, LifecycleCapabilities, ObservabilityCapabilities, SystemCapabilities,
};
use crate::pool::manager::{ImagePoolStatus, PoolStatus};

fn sample_system_info() -> SystemInfo {
    SystemInfo {
        version: "0.0.7".to_owned(),
        mode: "kvm".to_owned(),
        uptime_secs: 3661,
        vm_count: 2,
        kernel_version: "7.0.0-visor".to_owned(),
        kernel_size_bytes: 2 * 1024 * 1024,
        kernel_sha256: "deadbeef".to_owned(),
        capabilities: SystemCapabilities {
            guest: GuestCapabilities {
                networking: true,
                volume_mounts: true,
                snapshot_restore: true,
            },
            lifecycle: LifecycleCapabilities {
                warm_pool: true,
                health_monitoring: true,
            },
            observability: ObservabilityCapabilities {
                metrics: true,
                vm_runtime_metrics: false,
                seccomp_sandbox: false,
            },
        },
    }
}

#[test]
fn format_duration_secs_renders_hours_minutes_and_seconds() {
    assert_eq!(format_duration_secs(3661), "1h 1m 1s");
    assert_eq!(format_duration_secs(59), "59s");
}

#[test]
fn format_system_info_includes_pool_summary() {
    let mut images = HashMap::new();
    images.insert(
        "alpine:latest".to_owned(),
        ImagePoolStatus {
            available: 1,
            target: 3,
        },
    );
    images.insert(
        "nginx:alpine".to_owned(),
        ImagePoolStatus {
            available: 2,
            target: 2,
        },
    );

    let rendered = format_system_info(
        &sample_system_info(),
        Some(&PoolStatus { images, total: 3 }),
    );

    assert!(rendered.contains("Version: 0.0.7"));
    assert!(rendered.contains("Uptime: 1h 1m 1s"));
    assert!(rendered.contains("Warm pool: enabled"));
    assert!(rendered.contains("Health monitoring: enabled"));
    assert!(rendered.contains("Per-VM runtime metrics: disabled"));
    assert!(rendered.contains("Warm Pool State:"));
    assert!(rendered.contains("Available: 3"));
    assert!(rendered.contains("Target: 5"));
    assert!(rendered.contains("alpine:latest 1/3"));
    assert!(rendered.contains("nginx:alpine 2/2"));
}

#[test]
fn format_system_info_marks_missing_pool() {
    let rendered = format_system_info(&sample_system_info(), None);

    assert!(rendered.contains("Warm Pool State:"));
    assert!(rendered.contains("Not configured"));
}
