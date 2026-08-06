use std::collections::HashMap;

use super::*;
use crate::backend::{PortMapping, VmState};

fn sample_vm(id: &str) -> VmInfo {
    let mut vm = VmInfo::new(
        id.to_owned(),
        "nginx:alpine".to_owned(),
        VmState::Running,
        "2026-03-09T18:30:00Z".to_owned(),
        512,
        1,
    );
    vm.name = Some("web".to_owned());
    vm.cid = Some(7);
    vm.ports = vec![
        PortMapping::new(8080, 80),
        PortMapping::with_protocol(8443, 443, "tcp"),
    ];
    vm
}

#[test]
fn format_vm_ports_renders_all_mappings() {
    let rendered = format_vm_ports(&sample_vm("vm-1"));
    assert_eq!(rendered, "8080->80/tcp,8443->443/tcp");
}

#[test]
fn format_vm_health_includes_failure_count_for_unhealthy_vm() {
    let report = VmHealthReport {
        vm_id: "vm-1".to_owned(),
        status: HealthStatus::Unhealthy("timeout".to_owned()),
        consecutive_failures: 3,
    };

    assert_eq!(format_vm_health(Some(&report)), "unhealthy(3)");
}

#[test]
fn render_vm_table_includes_health_cid_and_ports_columns() {
    let vm = sample_vm("vm-1");
    let mut health = HashMap::new();
    health.insert(
        "vm-1".to_owned(),
        VmHealthReport {
            vm_id: "vm-1".to_owned(),
            status: HealthStatus::Healthy,
            consecutive_failures: 0,
        },
    );

    let rendered = render_vm_table(&[vm], &health);

    assert!(rendered.contains("HEALTH"));
    assert!(rendered.contains("CID"));
    assert!(rendered.contains("PORTS"));
    assert!(rendered.contains("healthy"));
    assert!(rendered.contains("8080->80/tcp,8443->443/tcp"));
    assert!(rendered.contains("7"));
}
