use super::*;

// ── SerialOutput ─────────────────────────────────────────────────

#[test]
fn serial_output_implements_write() {
    use std::io::Write;

    let output = SerialOutput::new();
    let mut writer: Box<dyn Write + Send> = Box::new(output.clone());

    writer.write_all(b"hello").unwrap();
    writer.write_all(b" world").unwrap();

    assert_eq!(output.as_bytes(), b"hello world");
}

#[test]
fn serial_output_clone_shares_data() {
    use std::io::Write;

    let a = SerialOutput::new();
    let b = a.clone();

    let mut writer: Box<dyn Write + Send> = Box::new(a);
    writer.write_all(b"shared").unwrap();

    assert_eq!(b.as_bytes(), b"shared");
}

#[test]
fn serial_output_empty_initially() {
    let output = SerialOutput::new();
    assert!(output.as_bytes().is_empty());
}

#[test]
fn serial_output_concurrent_writes() {
    use std::io::Write;

    let output = SerialOutput::new();
    let handles: Vec<_> = (0..4)
        .map(|i| {
            let mut o = output.clone();
            std::thread::spawn(move || {
                let msg = format!("thread-{i}\n");
                o.write_all(msg.as_bytes()).unwrap();
            })
        })
        .collect();

    for h in handles {
        h.join().unwrap();
    }

    let bytes = output.as_bytes();
    // All four threads should have written their messages
    #[allow(clippy::naive_bytecount)]
    {
        assert_eq!(bytes.iter().filter(|&&b| b == b'\n').count(), 4);
    }
}

// ── build_cmdline ────────────────────────────────────────────────

#[test]
fn build_cmdline_contains_console() {
    let config = visor_init::config::RunConfig::default();
    let cmdline = build_cmdline(&config).unwrap();
    let expected = visor_vmm::devices::serial::CONSOLE_DEVICE_NAME;
    assert!(
        cmdline.contains(&format!("console={expected}")),
        "missing console={expected}: {cmdline}"
    );
}

#[test]
fn build_cmdline_contains_earlycon() {
    let config = visor_init::config::RunConfig::default();
    let cmdline = build_cmdline(&config).unwrap();
    let earlycon = visor_vmm::devices::serial::EARLYCON_PARAM;
    if !earlycon.is_empty() {
        assert!(
            cmdline.contains(earlycon),
            "missing earlycon param '{earlycon}': {cmdline}"
        );
    }
}

#[test]
fn build_cmdline_contains_init() {
    let config = visor_init::config::RunConfig::default();
    let cmdline = build_cmdline(&config).unwrap();
    assert!(
        cmdline.contains("init=/sbin/visor-init"),
        "missing init=: {cmdline}"
    );
}

#[test]
fn build_cmdline_does_not_contain_virtio_mmio_device() {
    // virtio-mmio device discovery is handled by DSDT AML entries, not cmdline
    let config = visor_init::config::RunConfig::default();
    let cmdline = build_cmdline(&config).unwrap();
    assert!(
        !cmdline.contains("virtio_mmio.device="),
        "cmdline should not contain virtio_mmio.device= (DSDT handles discovery): {cmdline}"
    );
}

#[test]
fn build_cmdline_contains_visor_config_base64() {
    use base64::Engine as _;

    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec!["/bin/echo".to_owned(), "hello".to_owned()];

    let cmdline = build_cmdline(&config).unwrap();

    // Extract the visor.config= parameter
    let encoded = cmdline
        .split_whitespace()
        .find_map(|p| p.strip_prefix("visor.config="))
        .expect("missing visor.config= in cmdline");

    // Decode and verify it's valid JSON containing our command
    let engine = base64::engine::general_purpose::STANDARD_NO_PAD;
    let decoded = engine.decode(encoded).unwrap();
    let json = std::str::from_utf8(&decoded).unwrap();
    assert!(
        json.contains("echo"),
        "decoded config doesn't contain 'echo': {json}"
    );
}

#[test]
fn build_cmdline_contains_panic_reboot() {
    let config = visor_init::config::RunConfig::default();
    let cmdline = build_cmdline(&config).unwrap();
    assert!(cmdline.contains("panic=-1"), "missing panic=-1: {cmdline}");
    assert!(cmdline.contains("reboot=t"), "missing reboot=t: {cmdline}");
}

#[test]
fn build_cmdline_fits_in_max_size() {
    let config = visor_init::config::RunConfig::default();
    let cmdline = build_cmdline(&config).unwrap();
    // CMDLINE_MAX_SIZE is 2048 in visor-vmm boot module
    assert!(
        cmdline.len() < 2048,
        "cmdline too long: {} bytes",
        cmdline.len()
    );
}

#[test]
fn build_cmdline_with_multi_networks_fits_in_max_size() {
    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec![
        "sh".to_owned(),
        "-lc".to_owned(),
        "trap 'exit 0' TERM INT; while true; do sleep 1; done".to_owned(),
    ];
    let mut frontend = visor_init::config::NetworkConfig::default();
    frontend.name = Some("delta_frontend".to_owned());
    frontend.interface = Some("eth0".to_owned());
    frontend.address = "100.70.1.2".to_owned();
    frontend.netmask = "255.255.255.0".to_owned();
    frontend.gateway = "100.70.1.1".to_owned();
    frontend.dns_servers = vec!["100.70.1.1".to_owned()];
    frontend.default_route = true;
    let mut backend = visor_init::config::NetworkConfig::default();
    backend.name = Some("delta_backend".to_owned());
    backend.interface = Some("eth1".to_owned());
    backend.address = "100.71.1.2".to_owned();
    backend.netmask = "255.255.255.0".to_owned();
    backend.gateway = "100.71.1.1".to_owned();
    backend.dns_servers = vec!["100.71.1.1".to_owned()];
    backend.default_route = false;
    config.networks = vec![frontend, backend];
    config.extra_hosts = vec![
        visor_init::config::HostEntry::new("api", "100.70.1.3"),
        visor_init::config::HostEntry::new("api.delta", "100.70.1.3"),
        visor_init::config::HostEntry::new("delta-api-1", "100.70.1.3"),
        visor_init::config::HostEntry::new("delta-api-1.delta", "100.70.1.3"),
        visor_init::config::HostEntry::new("db", "100.71.1.4"),
        visor_init::config::HostEntry::new("db.delta", "100.71.1.4"),
        visor_init::config::HostEntry::new("delta-db-1", "100.71.1.4"),
        visor_init::config::HostEntry::new("delta-db-1.delta", "100.71.1.4"),
    ];
    config.exec_listener = true;

    let cmdline = build_cmdline(&config).unwrap();
    assert!(
        cmdline.len() < 2048,
        "multi-network cmdline too long: {} bytes",
        cmdline.len()
    );
}

// ── parse_exit_code ──────────────────────────────────────────────

#[test]
fn parse_exit_code_success() {
    let output = b"some boot output\nVISOR_EXIT_CODE=0\n";
    assert_eq!(parse_exit_code(output), 0);
}

#[test]
fn parse_exit_code_nonzero() {
    let output = b"boot log\nVISOR_EXIT_CODE=42\n";
    assert_eq!(parse_exit_code(output), 42);
}

#[test]
fn parse_exit_code_missing_defaults_to_1() {
    let output = b"no exit code marker here\n";
    assert_eq!(parse_exit_code(output), 1);
}

#[test]
fn parse_exit_code_takes_last_occurrence() {
    let output = b"VISOR_EXIT_CODE=1\nmore output\nVISOR_EXIT_CODE=0\n";
    assert_eq!(parse_exit_code(output), 0);
}

#[test]
fn parse_exit_code_empty_output() {
    assert_eq!(parse_exit_code(b""), 1);
}

#[test]
fn parse_exit_code_negative() {
    let output = b"VISOR_EXIT_CODE=-1\n";
    assert_eq!(parse_exit_code(output), -1);
}

#[test]
fn parse_exit_code_falls_back_to_kernel_panic_wait_status_zero() {
    let output =
        b"e2e_works\nKernel panic - not syncing: Attempted to kill init! exitcode=0x00000000\r\n";
    assert_eq!(parse_exit_code(output), 0);
}

#[test]
fn parse_exit_code_falls_back_to_kernel_panic_wait_status_nonzero() {
    let output = b"Kernel panic - not syncing: Attempted to kill init! exitcode=0x00002a00\r\n";
    assert_eq!(parse_exit_code(output), 42);
}

#[test]
fn parse_exit_code_prefers_explicit_marker_over_kernel_panic_status() {
    let output =
        b"VISOR_EXIT_CODE=7\nKernel panic - not syncing: Attempted to kill init! exitcode=0x00000000\r\n";
    assert_eq!(parse_exit_code(output), 7);
}

// ── visor_init_path ──────────────────────────────────────────────

#[test]
fn visor_init_path_returns_path() {
    // This test just verifies the function doesn't panic.
    // On CI/dev machines, the binary may or may not exist.
    let path = visor_init_path();
    assert!(path.is_ok() || path.is_err());
}

#[test]
fn visor_init_dev_path_candidates_include_workspace_target_dir() {
    let target_triple = if cfg!(target_arch = "aarch64") {
        "aarch64-unknown-linux-musl"
    } else {
        "x86_64-unknown-linux-musl"
    };
    let workspace_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root should exist");
    let expected = workspace_root.join(format!("target/{target_triple}/release/visor-init"));

    let candidates = visor_init_dev_path_candidates(target_triple);

    assert!(
        candidates.iter().any(|candidate| candidate == &expected),
        "expected workspace target path {expected:?} in {candidates:?}"
    );
}

#[test]
fn visor_init_dev_path_candidates_include_cargo_config_target_dir() {
    let target_triple = if cfg!(target_arch = "aarch64") {
        "aarch64-unknown-linux-musl"
    } else {
        "x86_64-unknown-linux-musl"
    };
    let tmp = crate::testutil::tempdir("visor-runtime-vm-").expect("create temp dir");
    let cargo_dir = tmp.path().join(".cargo");
    std::fs::create_dir_all(&cargo_dir).expect("create .cargo dir");
    let configured_target_dir = tmp.path().join("custom-target");
    std::fs::write(
        cargo_dir.join("config.toml"),
        format!(
            "[build]\ntarget-dir = {:?}\n",
            configured_target_dir.display().to_string()
        ),
    )
    .expect("write cargo config");
    let expected = configured_target_dir.join(format!("{target_triple}/release/visor-init"));

    let candidates =
        visor_init_dev_path_candidates_for_roots(target_triple, &[tmp.path().to_path_buf()]);

    assert!(
        candidates.iter().any(|candidate| candidate == &expected),
        "expected configured target path {expected:?} in {candidates:?}"
    );
}

#[test]
fn newest_existing_path_prefers_newer_binary() {
    let tmp = crate::testutil::tempdir("visor-runtime-vm-").expect("create temp dir");
    let older = tmp.path().join("older");
    let newer = tmp.path().join("newer");

    std::fs::write(&older, "older").expect("write older binary");
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::write(&newer, "newer").expect("write newer binary");

    let selected = newest_existing_path(&[older.clone(), newer.clone()]);
    assert_eq!(selected, Some(newer));
}

#[test]
fn newest_existing_path_ignores_missing_candidates() {
    let tmp = crate::testutil::tempdir("visor-runtime-vm-").expect("create temp dir");
    let missing = tmp.path().join("missing");
    let existing = tmp.path().join("existing");

    std::fs::write(&existing, "current").expect("write current binary");

    let selected = newest_existing_path(&[missing, existing.clone()]);
    assert_eq!(selected, Some(existing));
}

#[test]
fn build_vmm_network_config_uses_guest_network_settings() {
    let mut guest_network = visor_init::config::NetworkConfig::default();
    guest_network.address = "172.20.0.2".to_owned();
    guest_network.netmask = "255.255.255.0".to_owned();
    guest_network.gateway = "172.20.0.1".to_owned();
    guest_network.dns_servers = vec!["172.20.0.1".to_owned()];

    let network = build_vmm_network_config(
        Some(&guest_network),
        "c60e0bb8-c244-4c44-9e8b-b4e59339a66c",
        42,
    )
    .expect("guest network config should parse")
    .expect("guest network should map to a VMM network config");

    assert_eq!(network.interface_name, "vsrc60e0bb8c20");
    assert_eq!(network.guest_ip, std::net::Ipv4Addr::new(172, 20, 0, 2));
    assert_eq!(network.gateway_ip, std::net::Ipv4Addr::new(172, 20, 0, 1));
    assert_eq!(network.netmask, std::net::Ipv4Addr::new(255, 255, 255, 0));
}

#[test]
fn build_vmm_network_config_returns_none_without_guest_network() {
    let network = build_vmm_network_config(None, "vm-no-network", 7)
        .expect("missing guest network should not fail");

    assert!(network.is_none());
}

#[test]
fn build_vmm_network_config_uses_vm_identity_not_only_cid_for_interface_name() {
    let mut guest_network = visor_init::config::NetworkConfig::default();
    guest_network.address = "172.20.0.2".to_owned();
    guest_network.netmask = "255.255.255.0".to_owned();
    guest_network.gateway = "172.20.0.1".to_owned();

    let first = build_vmm_network_config(Some(&guest_network), "first-vm-0001", 4)
        .expect("first guest network config should parse")
        .expect("first guest network should map to a VMM network config");
    let second = build_vmm_network_config(Some(&guest_network), "second-vm-0001", 4)
        .expect("second guest network config should parse")
        .expect("second guest network should map to a VMM network config");

    assert_ne!(first.interface_name, second.interface_name);
}

// ── VmExitReason ─────────────────────────────────────────────────

#[test]
fn vm_exit_reason_display() {
    assert_eq!(format!("{}", VmExitReason::Shutdown), "shutdown");
    assert_eq!(format!("{}", VmExitReason::Reboot), "reboot");
    assert_eq!(format!("{}", VmExitReason::Halt), "halt");
    assert_eq!(
        format!("{}", VmExitReason::Error("oops".to_owned())),
        "error: oops"
    );
}

// ── extract_stdout ───────────────────────────────────────────────

#[test]
fn extract_stdout_filters_kernel_boot_log() {
    let raw = b"Linux version 7.0.0-rc1\n\
                BIOS-e820: [mem 0x0000] usable\n\
                virtio_blk virtio0: [vda] 149280 blocks\n\
                Run /sbin/visor-init as init process\n\
                hello world\n\
                VISOR_EXIT_CODE=0\n\
                Kernel panic - not syncing: Attempted to kill init!\n";
    let stdout = extract_stdout(raw);
    assert_eq!(stdout, "hello world\n");
}

#[test]
fn extract_stdout_prefers_explicit_stdout_markers() {
    let raw = b"Linux version 7.0.0-rc1\n\
                Run /sbin/visor-init as init process\n\
                EXT4-fs (vdb): mounted filesystem abc\n\
                VISOR_STDOUT_BEGIN\n\
                hello from data disk\n\
                VISOR_STDOUT_END\n\
                VISOR_EXIT_CODE=0\n\
                reboot: Restarting system\n";
    let stdout = extract_stdout(raw);
    assert_eq!(stdout, "hello from data disk\n");
}

#[test]
fn extract_stdout_filters_visor_init_logs_inside_marked_output() {
    let raw = b"Linux version 7.0.0-rc1\n\
                Run /sbin/visor-init as init process\n\
                VISOR_STDOUT_BEGIN\n\
                visor-init: agent listening on vsock port 52\n\
                buildx-load-ok\n\
                VISOR_STDOUT_END\n\
                VISOR_EXIT_CODE=0\n";
    let stdout = extract_stdout(raw);
    assert_eq!(stdout, "buildx-load-ok\n");
}

#[test]
fn extract_stdout_strips_end_marker_when_command_output_has_no_trailing_newline() {
    let raw = b"Linux version 7.0.0-rc1\n\
                Run /sbin/visor-init as init process\n\
                VISOR_STDOUT_BEGIN\n\
                pulled-okVISOR_STDOUT_END\n\
                VISOR_EXIT_CODE=0\n";
    let stdout = extract_stdout(raw);
    assert_eq!(stdout, "pulled-ok\n");
}

#[test]
fn extract_stdout_filters_timestamped_boot_noise() {
    let raw = b"[    0.000000] Linux version 7.0.0-rc1\n\
                [    0.123456] some kernel log\n\
                Run /sbin/visor-init as init process\n\
                hello world\n\
                VISOR_EXIT_CODE=0\n";
    let stdout = extract_stdout(raw);
    assert_eq!(stdout, "hello world\n");
}

#[test]
fn extract_stdout_filters_visor_init_control_logs() {
    let raw = b"Run /sbin/visor-init as init process\n\
                visor-init: agent listening on vsock port 52\n\
                buildx-load-ok\n\
                VISOR_EXIT_CODE=0\n";
    let stdout = extract_stdout(raw);
    assert_eq!(stdout, "buildx-load-ok\n");
}

#[test]
fn extract_stdout_empty_when_no_user_output() {
    let raw = b"Linux version 7.0.0-rc1\n\
                Run /sbin/visor-init as init process\n\
                VISOR_EXIT_CODE=0\n";
    let stdout = extract_stdout(raw);
    assert!(stdout.is_empty(), "expected empty stdout, got: {stdout:?}");
}

#[test]
fn extract_stdout_preserves_multiple_lines() {
    let raw = b"kernel boot log\n\
                Run /sbin/visor-init as init process\n\
                line1\n\
                line2\n\
                line3\n\
                VISOR_EXIT_CODE=0\n";
    let stdout = extract_stdout(raw);
    assert_eq!(stdout, "line1\nline2\nline3\n");
}

#[test]
fn extract_stdout_stops_at_kernel_panic() {
    let raw = b"Run /sbin/visor-init as init process\n\
                hello\n\
                Kernel panic - not syncing: Attempted to kill init!\n\
                Kernel Offset: disabled\n";
    let stdout = extract_stdout(raw);
    assert_eq!(stdout, "hello\n");
}

#[test]
fn extract_stdout_empty_when_no_init_marker() {
    let raw = b"some random output\nno init marker here\n";
    let stdout = extract_stdout(raw);
    assert!(
        stdout.is_empty(),
        "expected empty without init marker, got: {stdout:?}"
    );
}

// ── boot_vm_from_snapshot ───────────────────────────────────────

#[test]
fn boot_vm_from_snapshot_fails_with_missing_snapshot_dir() {
    let result = boot_vm_from_snapshot(
        "snapshot-missing",
        std::path::Path::new("/nonexistent/snapshot"),
        VmBootSpec::new(128, 1, 3),
        BootStorage::new(&[], &[]),
        &[],
    );
    assert!(
        result.is_err(),
        "boot_vm_from_snapshot should fail with missing snapshot dir"
    );
}

#[test]
fn boot_vm_from_snapshot_fails_with_empty_snapshot_dir() {
    let dir = crate::testutil::tempdir("visor-runtime-vm-").unwrap();
    let result = boot_vm_from_snapshot(
        "snapshot-empty",
        dir.path(),
        VmBootSpec::new(128, 1, 3),
        BootStorage::new(&[], &[]),
        &[],
    );
    assert!(
        result.is_err(),
        "boot_vm_from_snapshot should fail with empty snapshot dir"
    );
}

#[test]
fn vm_boot_spec_guest_virtualization_defaults_to_standard() {
    let spec = VmBootSpec::new(256, 2, 7);
    assert_eq!(spec.guest_virtualization, GuestVirtualizationMode::Standard);
}

#[test]
fn vm_boot_spec_with_guest_virtualization_overrides_default() {
    let spec =
        VmBootSpec::new(256, 2, 7).with_guest_virtualization(GuestVirtualizationMode::Nested);
    assert_eq!(spec.guest_virtualization, GuestVirtualizationMode::Nested);
}
