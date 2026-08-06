use clap::Parser;

use super::*;

// ── visor start ──────────────────────────────────────────────────

#[test]
fn start_defaults() {
    let cli = Cli::try_parse_from(["visor", "start"]).unwrap();
    match cli.command {
        Command::Start(args) => {
            assert_eq!(args.listen, "0.0.0.0:7800");
            assert!(!args.foreground);
        }
        other => panic!("expected Start, got {other:?}"),
    }
}

#[test]
fn start_custom_listen() {
    let cli = Cli::try_parse_from(["visor", "start", "--listen", "0.0.0.0:9000"]).unwrap();
    match cli.command {
        Command::Start(args) => {
            assert_eq!(args.listen, "0.0.0.0:9000");
        }
        other => panic!("expected Start, got {other:?}"),
    }
}

#[test]
fn start_foreground() {
    let cli = Cli::try_parse_from(["visor", "start", "--foreground"]).unwrap();
    match cli.command {
        Command::Start(args) => {
            assert!(args.foreground);
        }
        other => panic!("expected Start, got {other:?}"),
    }
}

// ── visor run ────────────────────────────────────────────────────

#[test]
fn run_image_and_cmd() {
    let cli = Cli::try_parse_from(["visor", "run", "alpine", "echo", "hello"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.image, "alpine");
            assert_eq!(args.cmd, vec!["echo", "hello"]);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_all_args() {
    let cli = Cli::try_parse_from([
        "visor",
        "run",
        "-e",
        "KEY=VALUE",
        "-m",
        "1024",
        "--cpus",
        "2",
        "--name",
        "test",
        "alpine",
    ])
    .unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.image, "alpine");
            assert_eq!(args.env, vec!["KEY=VALUE"]);
            assert_eq!(args.memory, 1024);
            assert_eq!(args.cpus, 2);
            assert_eq!(args.name.as_deref(), Some("test"));
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_port_mapping() {
    let cli = Cli::try_parse_from(["visor", "run", "-p", "8080:80", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.port, vec!["8080:80"]);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_network_flag() {
    let cli = Cli::try_parse_from(["visor", "run", "--network", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert!(args.network);
            assert!(!args.no_network);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_no_network_flag() {
    let cli = Cli::try_parse_from(["visor", "run", "--no-network", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert!(args.no_network);
            assert!(!args.network);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_defaults() {
    let cli = Cli::try_parse_from(["visor", "run", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.memory, 512);
            assert_eq!(args.cpus, 1);
            assert!(args.name.is_none());
            assert!(args.env.is_empty());
            assert!(!args.network);
            assert!(!args.no_network);
            assert!(args.port.is_empty());
            assert!(args.cmd.is_empty());
            assert!(args.workdir.is_none());
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_workdir() {
    let cli = Cli::try_parse_from(["visor", "run", "-w", "/app", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.workdir.as_deref(), Some("/app"));
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

// ── visor exec ───────────────────────────────────────────────────

#[test]
fn exec_vm_id_and_cmd() {
    let cli = Cli::try_parse_from(["visor", "exec", "vm123", "ls", "-la"]).unwrap();
    match cli.command {
        Command::Exec(args) => {
            assert_eq!(args.vm_id, "vm123");
            assert_eq!(args.cmd, vec!["ls", "-la"]);
        }
        other => panic!("expected Exec, got {other:?}"),
    }
}
#[test]
fn exec_with_env() {
    let cli = Cli::try_parse_from(["visor", "exec", "-e", "KEY=VALUE", "vm123", "ls"]).unwrap();
    match cli.command {
        Command::Exec(args) => {
            assert_eq!(args.vm_id, "vm123");
            assert_eq!(args.cmd, vec!["ls"]);
            assert_eq!(args.env, vec!["KEY=VALUE"]);
        }
        other => panic!("expected Exec, got {other:?}"),
    }
}

#[test]
fn exec_with_workdir() {
    let cli = Cli::try_parse_from(["visor", "exec", "-w", "/tmp", "vm123", "pwd"]).unwrap();
    match cli.command {
        Command::Exec(args) => {
            assert_eq!(args.vm_id, "vm123");
            assert_eq!(args.cmd, vec!["pwd"]);
            assert_eq!(args.workdir.as_deref(), Some("/tmp"));
        }
        other => panic!("expected Exec, got {other:?}"),
    }
}

#[test]
fn exec_with_env_and_workdir() {
    let cli = Cli::try_parse_from([
        "visor", "exec", "-e", "FOO=bar", "-w", "/app", "vm123", "echo", "hello",
    ])
    .unwrap();
    match cli.command {
        Command::Exec(args) => {
            assert_eq!(args.vm_id, "vm123");
            assert_eq!(args.cmd, vec!["echo", "hello"]);
            assert_eq!(args.env, vec!["FOO=bar"]);
            assert_eq!(args.workdir.as_deref(), Some("/app"));
        }
        other => panic!("expected Exec, got {other:?}"),
    }
}

#[test]
fn exec_multiple_env() {
    let cli = Cli::try_parse_from([
        "visor",
        "exec",
        "-e",
        "KEY1=val1",
        "-e",
        "KEY2=val2",
        "vm123",
        "env",
    ])
    .unwrap();
    match cli.command {
        Command::Exec(args) => {
            assert_eq!(args.vm_id, "vm123");
            assert_eq!(args.cmd, vec!["env"]);
            assert_eq!(args.env, vec!["KEY1=val1", "KEY2=val2"]);
        }
        other => panic!("expected Exec, got {other:?}"),
    }
}

// ── visor ps ─────────────────────────────────────────────────────

#[test]
fn ps_variant() {
    let cli = Cli::try_parse_from(["visor", "ps"]).unwrap();
    assert!(matches!(cli.command, Command::Ps));
}

// ── visor stop ───────────────────────────────────────────────────

#[test]
fn stop_vm_id() {
    let cli = Cli::try_parse_from(["visor", "stop", "vm123"]).unwrap();
    match cli.command {
        Command::Stop(args) => {
            assert_eq!(args.vm_id, Some("vm123".to_owned()));
        }
        other => panic!("expected Stop, got {other:?}"),
    }
}

#[test]
fn stop_no_args_stops_daemon() {
    let cli = Cli::try_parse_from(["visor", "stop"]).unwrap();
    match cli.command {
        Command::Stop(args) => {
            assert!(args.vm_id.is_none(), "expected no vm_id for daemon stop");
        }
        other => panic!("expected Stop, got {other:?}"),
    }
}

#[test]
fn stop_with_time_flag() {
    let cli = Cli::try_parse_from(["visor", "stop", "-t", "5", "vm123"]).unwrap();
    match cli.command {
        Command::Stop(args) => {
            assert_eq!(args.vm_id, Some("vm123".to_owned()));
            assert_eq!(args.time, 5);
        }
        other => panic!("expected Stop, got {other:?}"),
    }
}

#[test]
fn stop_with_long_time_flag() {
    let cli = Cli::try_parse_from(["visor", "stop", "--time", "30", "vm123"]).unwrap();
    match cli.command {
        Command::Stop(args) => {
            assert_eq!(args.time, 30);
        }
        other => panic!("expected Stop, got {other:?}"),
    }
}

#[test]
fn stop_default_time_is_10() {
    let cli = Cli::try_parse_from(["visor", "stop", "vm123"]).unwrap();
    match cli.command {
        Command::Stop(args) => {
            assert_eq!(args.time, 10);
        }
        other => panic!("expected Stop, got {other:?}"),
    }
}

#[test]
fn ensure_interactive_vm_running_accepts_running_vm() {
    let vm = crate::backend::VmInfo::new(
        "vm-123".to_owned(),
        "alpine:latest".to_owned(),
        crate::backend::VmState::Running,
        "2026-03-09T00:00:00Z".to_owned(),
        128,
        1,
    );

    assert!(ensure_interactive_vm_running(&vm, "vm-123", "shell").is_ok());
}

#[test]
fn ensure_interactive_vm_running_rejects_stopped_vm_with_start_hint() {
    let vm = crate::backend::VmInfo::new(
        "vm-123".to_owned(),
        "alpine:latest".to_owned(),
        crate::backend::VmState::Stopped,
        "2026-03-09T00:00:00Z".to_owned(),
        128,
        1,
    );

    let err = ensure_interactive_vm_running(&vm, "quick_maxwell", "shell")
        .expect_err("stopped VM should be rejected");

    let text = err.to_string();
    assert!(text.contains("cannot open shell"));
    assert!(text.contains("quick_maxwell"));
    assert!(text.contains("visor start quick_maxwell"));
}

#[test]
fn ensure_interactive_vm_running_rejects_creating_vm_without_start_hint() {
    let vm = crate::backend::VmInfo::new(
        "vm-123".to_owned(),
        "alpine:latest".to_owned(),
        crate::backend::VmState::Creating,
        "2026-03-09T00:00:00Z".to_owned(),
        128,
        1,
    );

    let err = ensure_interactive_vm_running(&vm, "vm-123", "console")
        .expect_err("creating VM should be rejected");

    let text = err.to_string();
    assert!(text.contains("cannot open console"));
    assert!(text.contains("still creating"));
    assert!(!text.contains("visor start"));
}

// ── visor info ───────────────────────────────────────────────────

#[test]
fn info_variant() {
    let cli = Cli::try_parse_from(["visor", "info"]).unwrap();
    assert!(matches!(cli.command, Command::Info));
}

// ── visor shell ──────────────────────────────────────────────────

#[test]
fn shell_vm_id() {
    let cli = Cli::try_parse_from(["visor", "shell", "vm123"]).unwrap();
    match cli.command {
        Command::Shell(args) => {
            assert_eq!(args.vm_id, "vm123");
        }
        other => panic!("expected Shell, got {other:?}"),
    }
}

// ── Global --addr ────────────────────────────────────────────────

#[test]
fn global_addr_default() {
    let cli = Cli::try_parse_from(["visor", "ps"]).unwrap();
    assert_eq!(cli.addr, "http://127.0.0.1:7800");
}

#[test]
fn global_addr_custom() {
    let cli = Cli::try_parse_from(["visor", "--addr", "http://10.0.0.1:9000", "ps"]).unwrap();
    assert_eq!(cli.addr, "http://10.0.0.1:9000");
}

// ── Port mapping parser ─────────────────────────────────────────

#[test]
fn parse_port_mapping_valid() {
    let mapping = parse_port_mapping("8080:80").unwrap();
    assert_eq!(mapping.host_port, 8080);
    assert_eq!(mapping.guest_port, 80);
    assert_eq!(mapping.protocol, "tcp");
}

#[test]
fn parse_port_mapping_high_ports() {
    let mapping = parse_port_mapping("65535:443").unwrap();
    assert_eq!(mapping.host_port, 65535);
    assert_eq!(mapping.guest_port, 443);
}

#[test]
fn parse_port_mapping_invalid_format() {
    let result = parse_port_mapping("invalid");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("host:guest"),
        "error should mention format: {err}"
    );
}

#[test]
fn parse_port_mapping_invalid_host_port() {
    let result = parse_port_mapping("abc:80");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("host port"),
        "error should mention host port: {err}"
    );
}

#[test]
fn parse_port_mapping_invalid_guest_port() {
    let result = parse_port_mapping("8080:abc");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("guest port"),
        "error should mention guest port: {err}"
    );
}

#[test]
fn parse_port_mapping_too_many_colons() {
    let result = parse_port_mapping("8080:80:tcp");
    assert!(result.is_err());
}

// ── HTTP client ──────────────────────────────────────────────────

#[test]
fn http_client_builds_successfully() {
    let client = http_client();
    assert!(client.is_ok());
}

// ── visor restart ────────────────────────────────────────────────

#[test]
fn restart_defaults() {
    let cli = Cli::try_parse_from(["visor", "restart"]).unwrap();
    match cli.command {
        Command::Restart(args) => {
            assert_eq!(args.listen, "0.0.0.0:7800");
        }
        other => panic!("expected Restart, got {other:?}"),
    }
}

#[test]
fn restart_custom_listen() {
    let cli = Cli::try_parse_from(["visor", "restart", "--listen", "0.0.0.0:9000"]).unwrap();
    match cli.command {
        Command::Restart(args) => {
            assert_eq!(args.listen, "0.0.0.0:9000");
        }
        other => panic!("expected Restart, got {other:?}"),
    }
}

// ── visor rm ─────────────────────────────────────────────────────

#[test]
fn rm_single_id() {
    let cli = Cli::try_parse_from(["visor", "rm", "vm123"]).unwrap();
    match cli.command {
        Command::Rm(args) => {
            assert_eq!(args.vm_ids, vec!["vm123"]);
        }
        other => panic!("expected Rm, got {other:?}"),
    }
}

#[test]
fn rm_multiple_ids() {
    let cli = Cli::try_parse_from(["visor", "rm", "vm1", "vm2", "vm3"]).unwrap();
    match cli.command {
        Command::Rm(args) => {
            assert_eq!(args.vm_ids, vec!["vm1", "vm2", "vm3"]);
        }
        other => panic!("expected Rm, got {other:?}"),
    }
}

#[test]
fn rm_requires_at_least_one_id() {
    let result = Cli::try_parse_from(["visor", "rm"]);
    assert!(result.is_err());
}

// ── visor logs ───────────────────────────────────────────────────

#[test]
fn logs_vm_id() {
    let cli = Cli::try_parse_from(["visor", "logs", "vm123"]).unwrap();
    match cli.command {
        Command::Logs(args) => {
            assert_eq!(args.vm_id, "vm123");
        }
        other => panic!("expected Logs, got {other:?}"),
    }
}

// ── visor inspect ────────────────────────────────────────────────

#[test]
fn inspect_vm_id() {
    let cli = Cli::try_parse_from(["visor", "inspect", "vm123"]).unwrap();
    match cli.command {
        Command::Inspect(args) => {
            assert_eq!(args.vm_id, "vm123");
        }
        other => panic!("expected Inspect, got {other:?}"),
    }
}

// ── visor kill ───────────────────────────────────────────────────

#[test]
fn kill_vm_id() {
    let cli = Cli::try_parse_from(["visor", "kill", "vm123"]).unwrap();
    match cli.command {
        Command::Kill(args) => {
            assert_eq!(args.vm_id, "vm123");
        }
        other => panic!("expected Kill, got {other:?}"),
    }
}

// ── visor pull ───────────────────────────────────────────────────

#[test]
fn pull_image() {
    let cli = Cli::try_parse_from(["visor", "pull", "alpine:latest"]).unwrap();
    match cli.command {
        Command::Pull(args) => {
            assert_eq!(args.image, "alpine:latest");
        }
        other => panic!("expected Pull, got {other:?}"),
    }
}

// ── visor rmi ────────────────────────────────────────────────────

#[test]
fn rmi_single_image() {
    let cli = Cli::try_parse_from(["visor", "rmi", "alpine:latest"]).unwrap();
    match cli.command {
        Command::Rmi(args) => {
            assert_eq!(args.images, vec!["alpine:latest"]);
        }
        other => panic!("expected Rmi, got {other:?}"),
    }
}

#[test]
fn rmi_multiple_images() {
    let cli = Cli::try_parse_from(["visor", "rmi", "alpine:latest", "ubuntu:22.04"]).unwrap();
    match cli.command {
        Command::Rmi(args) => {
            assert_eq!(args.images, vec!["alpine:latest", "ubuntu:22.04"]);
        }
        other => panic!("expected Rmi, got {other:?}"),
    }
}

#[test]
fn rmi_requires_at_least_one_image() {
    let result = Cli::try_parse_from(["visor", "rmi"]);
    assert!(result.is_err());
}

// ── visor run --detach ───────────────────────────────────────────

#[test]
fn run_detach_flag() {
    let cli = Cli::try_parse_from(["visor", "run", "-d", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert!(args.detach);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_detach_long_flag() {
    let cli = Cli::try_parse_from(["visor", "run", "--detach", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert!(args.detach);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_no_detach_by_default() {
    let cli = Cli::try_parse_from(["visor", "run", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert!(!args.detach);
            assert!(!args.nested_virt);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_nested_virt_flag() {
    let cli = Cli::try_parse_from(["visor", "run", "--nested-virt", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert!(args.nested_virt);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

// ── Volume mount parser ─────────────────────────────────────────

#[test]
fn parse_volume_mount_host_guest() {
    let mount = parse_volume_mount("/host/path:/guest/path").unwrap();
    assert_eq!(mount.host_path, "/host/path");
    assert_eq!(mount.guest_path, "/guest/path");
    assert!(!mount.read_only);
}

#[test]
fn parse_volume_mount_host_guest_ro() {
    let mount = parse_volume_mount("/host/path:/guest/path:ro").unwrap();
    assert_eq!(mount.host_path, "/host/path");
    assert_eq!(mount.guest_path, "/guest/path");
    assert!(mount.read_only);
}

#[test]
fn parse_volume_mount_rejects_empty() {
    let result = parse_volume_mount("");
    assert!(result.is_err());
}

#[test]
fn parse_volume_mount_rejects_no_colon() {
    let result = parse_volume_mount("/foo");
    assert!(result.is_err());
}

#[test]
fn parse_volume_mount_rejects_empty_host() {
    let result = parse_volume_mount(":/guest");
    assert!(result.is_err());
}

#[test]
fn parse_volume_mount_rejects_relative_guest() {
    let result = parse_volume_mount("/host:relative");
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("absolute"),
        "error should mention absolute: {err}"
    );
}

// ── Volume CLI flag ─────────────────────────────────────────────

#[test]
fn run_volume_flag() {
    let cli = Cli::try_parse_from(["visor", "run", "-v", "/host:/guest", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.volume, vec!["/host:/guest"]);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_multiple_volumes() {
    let cli =
        Cli::try_parse_from(["visor", "run", "-v", "/a:/b", "-v", "/c:/d:ro", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert_eq!(args.volume, vec!["/a:/b", "/c:/d:ro"]);
        }
        other => panic!("expected Run, got {other:?}"),
    }
}

#[test]
fn run_volume_defaults_empty() {
    let cli = Cli::try_parse_from(["visor", "run", "alpine"]).unwrap();
    match cli.command {
        Command::Run(args) => {
            assert!(args.volume.is_empty());
        }
        other => panic!("expected Run, got {other:?}"),
    }
}
