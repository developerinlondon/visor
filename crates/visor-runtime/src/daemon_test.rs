use super::*;

#[test]
fn default_config_listen_addr() {
    let config = DaemonConfig::default();
    assert_eq!(config.listen_addr, "0.0.0.0:7800");
}

#[test]
fn custom_config_listen_addr() {
    let config = DaemonConfig {
        listen_addr: "0.0.0.0:9090".to_owned(),
    };
    assert_eq!(config.listen_addr, "0.0.0.0:9090");
}

#[test]
fn config_clone_is_independent() {
    let original = DaemonConfig {
        listen_addr: "10.0.0.1:8080".to_owned(),
    };
    let mut cloned = original.clone();
    cloned.listen_addr = "10.0.0.2:8081".to_owned();
    assert_eq!(original.listen_addr, "10.0.0.1:8080");
    assert_eq!(cloned.listen_addr, "10.0.0.2:8081");
}

#[test]
fn config_debug_format() {
    let config = DaemonConfig::default();
    let debug = format!("{config:?}");
    assert!(debug.contains("0.0.0.0:7800"));
    assert!(debug.contains("DaemonConfig"));
}

#[test]
fn init_tracing_does_not_panic() {
    // tracing subscriber can only be set once per process. Use try_init
    // pattern via a manual guard — if another test already initialized,
    // the global subscriber is already set and this is a no-op.
    use tracing_subscriber::{EnvFilter, fmt};
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    let _ = fmt().with_env_filter(filter).with_target(true).try_init();
}

#[test]
fn embedded_dns_listen_ip_matches_platform_expectations() {
    let ip = embedded_dns_listen_ip();
    if cfg!(target_os = "linux") {
        assert_eq!(ip, std::net::Ipv4Addr::UNSPECIFIED);
    } else {
        assert_eq!(ip, std::net::Ipv4Addr::LOCALHOST);
    }
}

#[tokio::test]
async fn docker_service_discovery_bridge_registers_and_unregisters_names() {
    let registry = Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new()));
    let bridge = DockerDnsServiceDiscovery::new(Arc::clone(&registry));
    let ip = std::net::Ipv4Addr::new(172, 20, 0, 1);

    visor_docker::ServiceDiscovery::register_name(&bridge, "api", ip).await;
    assert_eq!(registry.read().await.resolve("api"), Some(ip));

    visor_docker::ServiceDiscovery::unregister_name(&bridge, "api").await;
    assert_eq!(registry.read().await.resolve("api"), None);
}

#[tokio::test]
async fn docker_service_discovery_bridge_snapshots_registered_names() {
    let registry = Arc::new(tokio::sync::RwLock::new(crate::net::dns::DnsRegistry::new()));
    registry
        .write()
        .await
        .register("api", std::net::Ipv4Addr::new(172, 20, 0, 1));
    let bridge = DockerDnsServiceDiscovery::new(Arc::clone(&registry));

    let snapshot = visor_docker::ServiceDiscovery::snapshot_names(&bridge).await;

    assert_eq!(
        snapshot,
        vec![("api".to_owned(), std::net::Ipv4Addr::new(172, 20, 0, 1))]
    );
}

#[test]
fn visor_linux_interface_names_filters_only_visor_taps() {
    let output = "\
1: lo: <LOOPBACK,UP,LOWER_UP> mtu 65536 qdisc noqueue state UNKNOWN mode DEFAULT group default qlen 1000\n\
2: eth0@if3: <BROADCAST,MULTICAST,UP,LOWER_UP> mtu 1500 qdisc fq_codel state UP mode DEFAULT group default qlen 1000\n\
174: vsrbbc304ad84d2: <NO-CARRIER,BROADCAST,MULTICAST,UP> mtu 1500 qdisc fq_codel state DOWN mode DEFAULT group default qlen 1000\n\
175: vsr5: <NO-CARRIER,BROADCAST,MULTICAST,UP> mtu 1500 qdisc fq_codel state DOWN mode DEFAULT group default qlen 1000\n\
";

    let names = visor_linux_interface_names(output);

    assert_eq!(names, vec!["vsrbbc304ad84d2".to_owned(), "vsr5".to_owned()]);
}

// ═════════════════════════════════════════════════════════════════════
// VM Persistence Integration Tests
// ═════════════════════════════════════════════════════════════════════

use crate::backend::{ExecutionBackend, PortMapping, VmConfig, VmInfo, VmState, VmmBackend};
use crate::state::persistence;

// ── Helpers ─────────────────────────────────────────────────────

fn sample_vm_info(id: &str, name: &str, state: VmState) -> VmInfo {
    let mut info = VmInfo::new(
        id.to_owned(),
        "alpine:latest".to_owned(),
        state,
        "2026-03-01T12:00:00Z".to_owned(),
        256,
        2,
    );
    info.name = Some(name.to_owned());
    info.ports = vec![PortMapping::new(8080, 80)];
    info
}

fn vm_info_to_meta(info: &VmInfo) -> persistence::VmMeta {
    persistence::VmMeta {
        id: info.id.clone(),
        name: info.name.clone(),
        image: info.image.clone(),
        config: {
            let mut cfg = VmConfig::new(info.image.clone());
            cfg.memory_mib = info.memory_mib;
            cfg.vcpus = info.vcpus;
            cfg.name = info.name.clone();
            cfg.ports = info.ports.clone();
            cfg.detach = true;
            cfg
        },
        created_at: info.created_at.clone(),
        cid: 0,
        memory_mib: info.memory_mib,
        vcpus: info.vcpus,
        ports: info.ports.clone(),
    }
}

// ── Snapshot → Restore round-trip ──────────────────────────────

/// Full daemon lifecycle: register running VMs → snapshot to disk →
/// fresh backend → restore from disk → verify restored as Stopped.
#[tokio::test]
async fn persistence_snapshot_restore_round_trip() {
    let dir = crate::testutil::tempdir("visor-runtime-daemon-").unwrap();
    let base = dir.path().to_path_buf();

    // Simulate daemon with 2 running VMs
    let backend = VmmBackend::new();
    let vm_a = sample_vm_info("vm-round-a", "alpha", VmState::Running);
    let vm_b = sample_vm_info("vm-round-b", "bravo", VmState::Running);
    backend.insert_vm(vm_a).await;
    backend.insert_vm(vm_b).await;

    // Snapshot (daemon shutdown path)
    let running_ids = backend.running_vm_ids().await;
    assert_eq!(running_ids.len(), 2, "should see 2 running VMs");

    for id in &running_ids {
        let info = backend.get_vm_info(id).await.unwrap();
        let meta = vm_info_to_meta(&info);
        persistence::save_vm_meta(&base.join(id), &meta).unwrap();
    }

    // Fresh backend (daemon restart)
    let fresh = VmmBackend::new();
    assert!(fresh.list().await.unwrap().is_empty());

    // Restore (daemon startup path)
    let vm_ids = persistence::scan_state_dir(&base).unwrap();
    assert_eq!(vm_ids.len(), 2);

    for vm_id in &vm_ids {
        let meta = persistence::load_vm_meta(&base.join(vm_id)).unwrap();
        let mut restored = VmInfo::new(
            meta.id.clone(),
            meta.image.clone(),
            VmState::Stopped,
            meta.created_at.clone(),
            meta.memory_mib,
            meta.vcpus,
        );
        restored.name = meta.name.clone();
        restored.ports = meta.ports.clone();
        fresh.restore_vm(restored).await;
    }

    // Verify
    let vms = fresh.list().await.unwrap();
    assert_eq!(vms.len(), 2);
    for info in &vms {
        assert_eq!(info.state, VmState::Stopped, "restored VMs must be Stopped");
        assert_eq!(info.image, "alpine:latest");
        assert_eq!(info.memory_mib, 256);
        assert_eq!(info.vcpus, 2);
        assert_eq!(info.ports.len(), 1);
        assert_eq!(info.ports[0].host_port, 8080);
    }
}

#[tokio::test]
async fn clean_shutdown_sequence_persists_metadata_before_vm_teardown() {
    let dir = crate::testutil::tempdir("visor-runtime-daemon-").unwrap();
    let base = dir.path().to_path_buf();

    let backend = VmmBackend::new();
    backend
        .insert_vm(sample_vm_info(
            "vm-clean-stop",
            "clean-stop",
            VmState::Running,
        ))
        .await;

    let saved = persist_running_vm_state(&backend, &base).await;
    backend.shutdown_all_running_vms().await;

    assert_eq!(saved, 1, "expected one running VM to be persisted");
    let persisted = persistence::scan_state_dir(&base).unwrap();
    assert_eq!(persisted, vec!["vm-clean-stop".to_owned()]);

    let restored_meta = persistence::load_vm_meta(&base.join("vm-clean-stop")).unwrap();
    assert_eq!(restored_meta.id, "vm-clean-stop");
    assert_eq!(restored_meta.name.as_deref(), Some("clean-stop"));

    let info = backend.get_vm_info("vm-clean-stop").await.unwrap();
    assert_eq!(info.state, VmState::Stopped);
}

// ── Only running VMs are snapshotted ───────────────────────────

/// Stopped and failed VMs are NOT included in the snapshot.
#[tokio::test]
async fn persistence_snapshot_skips_non_running_vms() {
    let dir = crate::testutil::tempdir("visor-runtime-daemon-").unwrap();
    let base = dir.path().to_path_buf();

    let backend = VmmBackend::new();
    backend
        .insert_vm(sample_vm_info("vm-run", "runner", VmState::Running))
        .await;
    backend
        .insert_vm(sample_vm_info("vm-stop", "stopper", VmState::Stopped))
        .await;
    backend
        .insert_vm(sample_vm_info("vm-fail", "failer", VmState::Failed))
        .await;

    let running_ids = backend.running_vm_ids().await;
    assert_eq!(running_ids.len(), 1);
    assert_eq!(running_ids[0], "vm-run");

    for id in &running_ids {
        let info = backend.get_vm_info(id).await.unwrap();
        let meta = vm_info_to_meta(&info);
        persistence::save_vm_meta(&base.join(id), &meta).unwrap();
    }

    let persisted = persistence::scan_state_dir(&base).unwrap();
    assert_eq!(persisted.len(), 1, "only running VM should be persisted");
    assert_eq!(persisted[0], "vm-run");
}

// ── Crash recovery before restore ─────────────────────────────

/// Incomplete state dirs (missing vm_meta.json) are cleaned up before
/// restoring valid VMs.
#[tokio::test]
async fn persistence_restore_with_crash_recovery() {
    let dir = crate::testutil::tempdir("visor-runtime-daemon-").unwrap();
    let base = dir.path().to_path_buf();

    // One valid VM
    let meta = vm_info_to_meta(&sample_vm_info("vm-good", "good", VmState::Running));
    persistence::save_vm_meta(&base.join("vm-good"), &meta).unwrap();

    // One incomplete dir (crash mid-write)
    std::fs::create_dir_all(base.join("vm-crashed")).unwrap();

    let removed = persistence::cleanup_incomplete(&base).unwrap();
    assert_eq!(removed, 1);

    let vm_ids = persistence::scan_state_dir(&base).unwrap();
    assert_eq!(vm_ids.len(), 1);
    assert_eq!(vm_ids[0], "vm-good");

    // Restore into fresh backend
    let backend = VmmBackend::new();
    let loaded = persistence::load_vm_meta(&base.join("vm-good")).unwrap();
    backend
        .restore_vm({
            let mut info = VmInfo::new(
                loaded.id.clone(),
                loaded.image.clone(),
                VmState::Stopped,
                loaded.created_at.clone(),
                loaded.memory_mib,
                loaded.vcpus,
            );
            info.name = loaded.name.clone();
            info.ports = loaded.ports.clone();
            info
        })
        .await;

    let vms = backend.list().await.unwrap();
    assert_eq!(vms.len(), 1);
    assert_eq!(vms[0].id, "vm-good");
    assert_eq!(vms[0].state, VmState::Stopped);
}

// ── Edge cases ─────────────────────────────────────────────────

/// Empty state dir produces zero restored VMs.
#[tokio::test]
async fn persistence_restore_empty_state_dir_is_noop() {
    let dir = crate::testutil::tempdir("visor-runtime-daemon-").unwrap();
    let ids = persistence::scan_state_dir(dir.path()).unwrap();
    assert!(ids.is_empty());
}

/// Nonexistent state dir produces zero restored VMs.
#[tokio::test]
async fn persistence_restore_nonexistent_dir_is_noop() {
    let base = std::path::PathBuf::from("/tmp/visor-test-nonexistent-98765");
    assert!(!base.exists());
    let ids = persistence::scan_state_dir(&base).unwrap();
    assert!(ids.is_empty());
}

/// Port mappings survive the full snapshot → restore cycle.
#[tokio::test]
async fn persistence_port_mappings_preserved() {
    let dir = crate::testutil::tempdir("visor-runtime-daemon-").unwrap();
    let base = dir.path().to_path_buf();

    let mut vm = sample_vm_info("vm-ports", "ported", VmState::Running);
    vm.ports = vec![
        PortMapping::new(8080, 80),
        PortMapping::new(3000, 3000),
        PortMapping::new(5432, 5432),
    ];

    let meta = vm_info_to_meta(&vm);
    persistence::save_vm_meta(&base.join("vm-ports"), &meta).unwrap();

    let loaded = persistence::load_vm_meta(&base.join("vm-ports")).unwrap();
    assert_eq!(loaded.ports.len(), 3);
    assert_eq!(loaded.ports[0].host_port, 8080);
    assert_eq!(loaded.ports[0].guest_port, 80);
    assert_eq!(loaded.ports[1].host_port, 3000);
    assert_eq!(loaded.ports[2].host_port, 5432);
}

/// Repeated snapshots overwrite (not duplicate) state.
#[tokio::test]
async fn persistence_repeated_snapshots_overwrite() {
    let dir = crate::testutil::tempdir("visor-runtime-daemon-").unwrap();
    let base = dir.path().to_path_buf();

    let vm = sample_vm_info("vm-repeat", "repeater", VmState::Running);
    let meta = vm_info_to_meta(&vm);

    // Snapshot twice
    persistence::save_vm_meta(&base.join("vm-repeat"), &meta).unwrap();
    persistence::save_vm_meta(&base.join("vm-repeat"), &meta).unwrap();

    let ids = persistence::scan_state_dir(&base).unwrap();
    assert_eq!(
        ids.len(),
        1,
        "repeated snapshots must overwrite, not duplicate"
    );

    let loaded = persistence::load_vm_meta(&base.join("vm-repeat")).unwrap();
    assert_eq!(loaded.id, "vm-repeat");
}
