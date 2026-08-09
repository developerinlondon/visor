//! Integration tests for the VM boot pipeline.
//!
//! These tests boot real KVM microVMs to verify the full stack:
//! kernel loading, ACPI table parsing, virtio-blk discovery,
//! ext4 root mount, and init execution.
//!
//! **Requires**: `/dev/kvm` (runs on AX41 dev machine).

use std::io::{Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::Path;
use std::process::Command;
use std::time::Duration;

use serial_test::serial;
use visor_runtime::oci::rootfs::RootfsBuilder;

fn boot_test_tempdir() -> std::io::Result<tempfile::TempDir> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("visor-runtime-boot-tests");
    std::fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix("visor-runtime-boot-")
        .tempdir_in(root)
        .map_err(std::io::Error::from)
}

/// Creates a minimal empty ext4 filesystem image for boot testing.
///
/// The image contains no `/sbin/visor-init`, so the kernel will panic
/// after successfully mounting it — which is the expected behavior
/// for verifying the boot chain up to init execution.
fn create_minimal_rootfs(path: &Path) {
    let status = Command::new("mke2fs")
        .args([
            "-t",
            "ext4",
            "-F", // force — don't prompt when target is a regular file
            "-q", // quiet — suppress superblock/group-descriptor noise
            "-L",
            "test-rootfs",
            path.to_str().expect("rootfs path must be valid UTF-8"),
            "65536", // 64 MiB in 1K-blocks
        ])
        .status()
        .expect("mke2fs not found — install e2fsprogs");

    assert!(status.success(), "mke2fs failed to create test rootfs");
}

fn build_guest_rootfs(path: &Path, binaries: &[&str]) {
    let rootfs_dir = boot_test_tempdir().expect("create temp rootfs dir");
    copy_guest_binary_to(
        rootfs_dir.path(),
        &visor_runtime::vm::visor_init_path().expect("locate visor-init"),
        "/sbin/visor-init",
    );
    for binary in binaries {
        let binary_path = Path::new(binary);
        copy_guest_binary(rootfs_dir.path(), binary_path);
        for dependency in dynamic_dependencies(binary_path) {
            copy_guest_binary(rootfs_dir.path(), &dependency);
        }
    }

    RootfsBuilder::new(rootfs_dir.path(), path)
        .build()
        .expect("build guest rootfs");
}

fn copy_guest_binary(rootfs_dir: &Path, source: &Path) {
    let destination = rootfs_dir.join(
        source
            .strip_prefix("/")
            .expect("guest binary source must be absolute"),
    );
    copy_guest_binary_path(rootfs_dir, source, &destination);
}

fn copy_guest_binary_to(rootfs_dir: &Path, source: &Path, destination: &str) {
    let destination = rootfs_dir.join(
        destination
            .strip_prefix('/')
            .expect("guest binary destination must be absolute"),
    );
    copy_guest_binary_path(rootfs_dir, source, &destination);
}

fn copy_guest_binary_path(rootfs_dir: &Path, source: &Path, destination: &Path) {
    assert!(
        destination.starts_with(rootfs_dir),
        "guest destination must stay within the rootfs"
    );
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent).expect("create guest binary parent dir");
    }
    std::fs::copy(source, &destination).expect("copy guest binary into rootfs");
}

fn dynamic_dependencies(binary: &Path) -> Vec<std::path::PathBuf> {
    let output = Command::new("ldd")
        .arg(binary)
        .output()
        .expect("ldd must be available");
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        if stderr.contains("not a dynamic executable") {
            return Vec::new();
        }
        panic!("ldd failed for {}: {stderr}", binary.display());
    }

    let mut dependencies = std::collections::BTreeSet::new();
    let text = String::from_utf8_lossy(&output.stdout);
    for token in text.split_whitespace() {
        if token.starts_with('/') {
            let dependency = std::path::PathBuf::from(token);
            if dependency.is_file() {
                dependencies.insert(dependency);
            }
        }
    }

    dependencies.into_iter().collect()
}

fn reserve_local_port() -> u16 {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind local ephemeral port");
    listener.local_addr().expect("read ephemeral port").port()
}

async fn wait_for_http_response(
    host_ip: Ipv4Addr,
    host_port: u16,
    timeout: Duration,
) -> anyhow::Result<String> {
    let deadline = std::time::Instant::now() + timeout;
    let addr = std::net::SocketAddrV4::new(host_ip, host_port);

    loop {
        match TcpStream::connect_timeout(&addr.into(), Duration::from_millis(250)) {
            Ok(mut stream) => {
                stream.set_read_timeout(Some(Duration::from_secs(1)))?;
                stream.set_write_timeout(Some(Duration::from_secs(1)))?;
                stream
                    .write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n")?;
                let mut response = String::new();
                stream.read_to_string(&mut response)?;
                return Ok(response);
            }
            Err(_) if std::time::Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(error) => {
                return Err(error).map_err(Into::into);
            }
        }
    }
}

/// Full boot chain: kernel → ACPI → virtio-blk → ext4 → init.
///
/// Boots a microVM with an empty rootfs and verifies that all
/// critical boot milestones appear in serial output. The kernel
/// will panic when `/sbin/visor-init` is not found — this is
/// expected and confirms the boot chain completed successfully.
#[tokio::test]
async fn boot_vm_full_chain_reaches_init() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    create_minimal_rootfs(&rootfs);

    let config = visor_init::config::RunConfig::default();
    let mut handle = visor_runtime::vm::boot_vm(
        "boot-exit",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, 3),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("boot_vm should succeed — check /dev/kvm access");

    // Wait for the VM to complete. The kernel will:
    // 1. Boot and parse ACPI tables
    // 2. Discover virtio-blk via MMIO
    // 3. Mount the ext4 rootfs
    // 4. Try to exec /sbin/visor-init → ENOENT
    // 5. Kernel panic → reboot=t → triple fault → KVM_EXIT_SHUTDOWN
    let completion_rx = handle
        .completion_rx
        .take()
        .expect("completion_rx should exist");
    let exit_info = tokio::time::timeout(Duration::from_secs(30), completion_rx)
        .await
        .expect("VM boot timed out after 30s — guest likely hung")
        .expect("completion channel closed unexpectedly");

    // Join the vCPU thread to clean up
    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }

    // The VM exits via shutdown (kernel panic → reboot → triple fault)
    assert!(
        matches!(exit_info.reason, visor_runtime::vm::VmExitReason::Shutdown),
        "expected shutdown exit, got: {:?}",
        exit_info.reason
    );

    // Verify all critical boot milestones from serial output
    let serial_bytes = handle.serial_output.as_bytes();
    let serial = String::from_utf8_lossy(&serial_bytes);

    // Milestone 1: Kernel started
    assert!(
        serial.contains("Linux version"),
        "kernel did not start. Serial output:\n{serial}"
    );

    // Milestone 2: ACPI tables loaded (RSDP → XSDT → FADT → DSDT → MADT)
    assert!(
        serial.contains("ACPI: 1 ACPI AML tables successfully acquired and loaded"),
        "ACPI tables not loaded. Serial output:\n{serial}"
    );

    // Milestone 3: Virtio-blk device discovered via MMIO
    assert!(
        serial.contains("virtio_blk virtio0"),
        "virtio-blk device not discovered. Serial output:\n{serial}"
    );

    // Milestone 4: Root filesystem mounted
    assert!(
        serial.contains("EXT4-fs (vda): mounted"),
        "ext4 rootfs not mounted. Serial output:\n{serial}"
    );

    // Milestone 5: Init reached (kernel tried to exec visor-init)
    assert!(
        serial.contains("Run /sbin/visor-init as init process"),
        "init not reached. Serial output:\n{serial}"
    );
}

/// Verifies serial output is valid UTF-8 and contains the kernel version string.
///
/// This is a lighter-weight smoke test that just verifies the
/// serial capture path works correctly.
#[tokio::test]
async fn boot_vm_serial_captures_kernel_version() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    create_minimal_rootfs(&rootfs);

    let config = visor_init::config::RunConfig::default();
    let mut handle = visor_runtime::vm::boot_vm(
        "boot-serial",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, 3),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("boot_vm should succeed");

    let completion_rx = handle
        .completion_rx
        .take()
        .expect("completion_rx should exist");
    let _exit_info = tokio::time::timeout(Duration::from_secs(30), completion_rx)
        .await
        .expect("VM boot timed out")
        .expect("completion channel closed");

    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }

    let serial_bytes = handle.serial_output.as_bytes();

    // Serial output should be valid UTF-8 (kernel log is ASCII)
    let serial = std::str::from_utf8(&serial_bytes).expect("serial output should be valid UTF-8");

    // Should contain the kernel version from our mainline 7.0-rc1 build
    assert!(
        serial.contains("Linux version 7."),
        "missing kernel version string. Serial output:\n{serial}"
    );

    // Should show KVM paravirtualization detected
    assert!(
        serial.contains("Hypervisor detected: KVM"),
        "KVM paravirt not detected. Serial output:\n{serial}"
    );

    // Should show the visor hostname
    assert!(
        serial.contains("visor"),
        "visor hostname not found. Serial output:\n{serial}"
    );
}

#[tokio::test]
async fn boot_vm_process_limit_one_counts_only_the_workload() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    build_guest_rootfs(&rootfs, &["/usr/bin/busybox"]);

    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec![
        "/usr/bin/busybox".to_owned(),
        "cat".to_owned(),
        "/proc/self/cgroup".to_owned(),
    ];
    config.process_limit = Some(1);

    let mut handle = visor_runtime::vm::boot_vm(
        "boot-process-limit",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, 3),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("boot_vm should succeed");

    let completion_rx = handle
        .completion_rx
        .take()
        .expect("completion_rx should exist");
    let exit_info = tokio::time::timeout(Duration::from_secs(30), completion_rx)
        .await
        .expect("VM boot timed out")
        .expect("completion channel closed");

    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }

    let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
    assert_eq!(exit_info.exit_code, 0, "serial output:\n{serial}");
    let stdout = visor_runtime::vm::extract_stdout(&handle.serial_output.as_bytes());
    assert_eq!(stdout.trim(), "0::/visor", "serial output:\n{serial}");

    config.cmd = vec![
        "/usr/bin/busybox".to_owned(),
        "sh".to_owned(),
        "-c".to_owned(),
        "if /usr/bin/busybox sleep 0 & then exit 1; else exit 0; fi".to_owned(),
    ];
    let mut fork_handle = visor_runtime::vm::boot_vm(
        "boot-process-limit-fork",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, 4),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("second boot_vm should succeed");
    let fork_completion_rx = fork_handle
        .completion_rx
        .take()
        .expect("completion_rx should exist");
    let fork_exit_info = tokio::time::timeout(Duration::from_secs(30), fork_completion_rx)
        .await
        .expect("fork-limit VM boot timed out")
        .expect("completion channel closed");

    if let Some(thread) = fork_handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }

    let fork_serial = String::from_utf8_lossy(&fork_handle.serial_output.as_bytes()).into_owned();
    assert_eq!(
        fork_exit_info.exit_code, 0,
        "the workload should observe a rejected excess process\nserial output:\n{fork_serial}"
    );
}

#[tokio::test]
#[serial]
async fn boot_vm_agent_mode_accepts_vsock_ping() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    build_guest_rootfs(&rootfs, &[]);

    let cid = 54;
    let mut config = visor_init::config::RunConfig::default();
    config.mode = "agent".to_owned();

    let mut handle = visor_runtime::vm::boot_vm(
        "boot-agent",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, cid),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("boot_vm should succeed");

    let backend = visor_vmm::comms::create_comms_backend();
    let connect_result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match visor_runtime::vsock::client::VsockClient::connect(
                &backend,
                cid,
                visor_runtime::vsock::client::VSOCK_AGENT_PORT,
            )
            .await
            {
                Ok(client) => return Ok(client),
                Err(error) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    if matches!(
                        error,
                        visor_runtime::vsock::client::VsockError::Timeout { .. }
                    ) {
                        return Err(error);
                    }
                }
            }
        }
    })
    .await;

    let mut client = match connect_result {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "guest agent should accept host vsock connection: {error}\nserial output:\n{serial}"
            );
        }
        Err(_) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!("guest agent did not become reachable within 20s\nserial output:\n{serial}");
        }
    };

    let pong = client
        .ping()
        .await
        .expect("guest agent should respond to ping");
    assert_eq!(pong, "pong");
    client
        .shutdown()
        .await
        .expect("guest agent should accept shutdown");

    let completion_rx = handle
        .completion_rx
        .take()
        .expect("completion_rx should exist");
    let exit_info = tokio::time::timeout(Duration::from_secs(20), completion_rx)
        .await
        .expect("agent VM shutdown timed out")
        .expect("completion channel closed");

    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }

    let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
    assert_eq!(exit_info.exit_code, 0, "serial output:\n{serial}");
}

#[tokio::test]
#[serial]
async fn boot_vm_run_mode_with_exec_listener_accepts_vsock_exec() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    build_guest_rootfs(&rootfs, &["/usr/bin/busybox"]);

    let cid = 55;
    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec![
        "/usr/bin/busybox".to_owned(),
        "sleep".to_owned(),
        "60".to_owned(),
    ];
    config.exec_listener = true;

    let mut handle = visor_runtime::vm::boot_vm(
        "boot-http",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, cid),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("boot_vm should succeed");

    let backend = visor_vmm::comms::create_comms_backend();
    let connect_result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match visor_runtime::vsock::client::VsockClient::connect(
                &backend,
                cid,
                visor_runtime::vsock::client::VSOCK_AGENT_PORT,
            )
            .await
            {
                Ok(client) => return Ok(client),
                Err(error) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    if matches!(
                        error,
                        visor_runtime::vsock::client::VsockError::Timeout { .. }
                    ) {
                        return Err(error);
                    }
                }
            }
        }
    })
    .await;

    let mut client = match connect_result {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "run-mode guest should accept host vsock connection: {error}\nserial output:\n{serial}"
            );
        }
        Err(_) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "run-mode guest agent did not become reachable within 20s\nserial output:\n{serial}"
            );
        }
    };

    let result = client
        .exec(
            vec![
                "/usr/bin/busybox".to_owned(),
                "echo".to_owned(),
                "exec-ok".to_owned(),
            ],
            Vec::new(),
            "/".to_owned(),
        )
        .await
        .expect("guest exec listener should execute commands");

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.trim(), "exec-ok");

    handle
        .kill_flag
        .store(true, std::sync::atomic::Ordering::Release);
    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }
}

#[tokio::test]
#[serial]
async fn boot_vm_named_network_exec_listener_accepts_vsock_exec() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    build_guest_rootfs(&rootfs, &["/usr/bin/busybox"]);

    let cid = 58;
    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec![
        "/usr/bin/busybox".to_owned(),
        "sleep".to_owned(),
        "60".to_owned(),
    ];
    config.exec_listener = true;
    let mut network = visor_init::config::NetworkConfig::default();
    network.name = Some("delta_frontend".to_owned());
    network.interface = Some("eth0".to_owned());
    network.address = "100.70.1.2".to_owned();
    network.netmask = "255.255.255.0".to_owned();
    network.gateway = "100.70.1.1".to_owned();
    network.dns_servers = vec!["100.70.1.1".to_owned()];
    network.default_route = true;
    config.network = Some(network);

    let mut handle = visor_runtime::vm::boot_vm(
        "boot-named-net-exec",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, cid),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("boot_vm should succeed");

    let backend = visor_vmm::comms::create_comms_backend();
    let connect_result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match visor_runtime::vsock::client::VsockClient::connect(
                &backend,
                cid,
                visor_runtime::vsock::client::VSOCK_AGENT_PORT,
            )
            .await
            {
                Ok(client) => return Ok(client),
                Err(error) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    if matches!(
                        error,
                        visor_runtime::vsock::client::VsockError::Timeout { .. }
                    ) {
                        return Err(error);
                    }
                }
            }
        }
    })
    .await;

    let mut client = match connect_result {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "named-network guest should accept host vsock connection: {error}\nserial output:\n{serial}"
            );
        }
        Err(_) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "named-network exec did not become reachable within 20s\nserial output:\n{serial}"
            );
        }
    };

    let result = client
        .exec(
            vec![
                "/usr/bin/busybox".to_owned(),
                "echo".to_owned(),
                "named-network-ok".to_owned(),
            ],
            Vec::new(),
            "/".to_owned(),
        )
        .await
        .expect("named-network guest exec listener should execute commands");

    assert_eq!(result.exit_code, 0);
    assert_eq!(result.stdout.trim(), "named-network-ok");

    handle
        .kill_flag
        .store(true, std::sync::atomic::Ordering::Release);
    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }
}

#[tokio::test]
#[serial]
async fn boot_vm_exec_listener_guest_sees_eth0_and_reaches_gateway() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    build_guest_rootfs(&rootfs, &["/usr/bin/busybox"]);

    let cid = 57;
    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec![
        "/usr/bin/busybox".to_owned(),
        "sleep".to_owned(),
        "60".to_owned(),
    ];
    config.exec_listener = true;

    let mut network = visor_init::config::NetworkConfig::default();
    network.address = "172.21.34.2".to_owned();
    network.netmask = "255.255.255.252".to_owned();
    network.gateway = "172.21.34.1".to_owned();
    network.dns_servers = vec!["172.21.34.1".to_owned()];
    config.network = Some(network);

    let mut handle = visor_runtime::vm::boot_vm(
        "boot-net-exec",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, cid),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("boot_vm should succeed");

    let backend = visor_vmm::comms::create_comms_backend();
    let connect_result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match visor_runtime::vsock::client::VsockClient::connect(
                &backend,
                cid,
                visor_runtime::vsock::client::VSOCK_AGENT_PORT,
            )
            .await
            {
                Ok(client) => return Ok(client),
                Err(error) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    if matches!(
                        error,
                        visor_runtime::vsock::client::VsockError::Timeout { .. }
                    ) {
                        return Err(error);
                    }
                }
            }
        }
    })
    .await;

    let mut client = match connect_result {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "run-mode guest should accept host vsock connection: {error}\nserial output:\n{serial}"
            );
        }
        Err(_) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "networked guest agent did not become reachable within 20s\nserial output:\n{serial}"
            );
        }
    };

    let result = client
        .exec(
            vec![
                "/usr/bin/busybox".to_owned(),
                "sh".to_owned(),
                "-lc".to_owned(),
                "cat /proc/net/dev; echo ---; /usr/bin/busybox ping -c1 -W1 172.21.34.1".to_owned(),
            ],
            Vec::new(),
            "/".to_owned(),
        )
        .await
        .expect("guest exec listener should execute network diagnostics");

    handle
        .kill_flag
        .store(true, std::sync::atomic::Ordering::Release);
    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }

    assert_eq!(
        result.exit_code, 0,
        "guest should be able to ping its gateway\nstdout:\n{}\nstderr:\n{}",
        result.stdout, result.stderr
    );
    assert!(
        result.stdout.contains("eth0:"),
        "guest should expose eth0 in /proc/net/dev\nstdout:\n{}",
        result.stdout
    );
}

#[tokio::test]
#[serial]
async fn boot_vm_run_mode_exec_stream_returns_docker_raw_output() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    build_guest_rootfs(&rootfs, &["/usr/bin/busybox"]);

    let cid = 56;
    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec![
        "/usr/bin/busybox".to_owned(),
        "sleep".to_owned(),
        "60".to_owned(),
    ];
    config.exec_listener = true;

    let mut handle = visor_runtime::vm::boot_vm(
        "boot-dns",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, cid),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("boot_vm should succeed");

    let backend = visor_vmm::comms::create_comms_backend();
    let connect_result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match visor_runtime::vsock::client::VsockClient::connect_exec_stream(
                &backend,
                cid,
                visor_runtime::vsock::client::VSOCK_AGENT_PORT,
                vec![
                    "/usr/bin/busybox".to_owned(),
                    "echo".to_owned(),
                    "stream-ok".to_owned(),
                ],
                Vec::new(),
                "/".to_owned(),
                false,
            )
            .await
            {
                Ok(stream) => return Ok(stream),
                Err(error) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    if matches!(error, visor_runtime::vsock::client::VsockError::Rpc { .. })
                        || matches!(error, visor_runtime::vsock::client::VsockError::Protocol(_))
                    {
                        return Err(error);
                    }
                }
            }
        }
    })
    .await;

    let mut stream = match connect_result {
        Ok(Ok(stream)) => stream,
        Ok(Err(error)) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "run-mode guest should accept streaming exec: {error}\nserial output:\n{serial}"
            );
        }
        Err(_) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!("streaming exec did not become reachable within 20s\nserial output:\n{serial}");
        }
    };

    let mut header = [0u8; 8];
    tokio::io::AsyncReadExt::read_exact(&mut stream, &mut header)
        .await
        .expect("stream should include Docker frame header");
    assert_eq!(header[0], 1, "stdout frame type should be 1");
    let payload_len = u32::from_be_bytes([header[4], header[5], header[6], header[7]]) as usize;
    let mut payload = vec![0u8; payload_len];
    tokio::io::AsyncReadExt::read_exact(&mut stream, &mut payload)
        .await
        .expect("stream should include Docker frame payload");
    assert_eq!(String::from_utf8(payload).unwrap().trim(), "stream-ok");
    let eof = tokio::time::timeout(Duration::from_secs(5), async {
        let mut buffer = [0u8; 1];
        tokio::io::AsyncReadExt::read(&mut stream, &mut buffer).await
    })
    .await
    .expect("stream should close after command exit")
    .expect("stream EOF read should succeed");
    assert_eq!(eof, 0, "stream should reach EOF after command exit");

    handle
        .kill_flag
        .store(true, std::sync::atomic::Ordering::Release);
    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }
}

#[tokio::test]
async fn boot_vm_mounts_read_only_staged_bind_volume() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    build_guest_rootfs(&rootfs, &["/bin/sh", "/bin/cat"]);

    let shared_dir = tmp.path().join("shared");
    std::fs::create_dir_all(&shared_dir).unwrap();
    std::fs::write(shared_dir.join("message.txt"), "hello from virtiofs\n").unwrap();
    let staged_disk = tmp.path().join("bind-volume.ext4");
    RootfsBuilder::new(&shared_dir, &staged_disk)
        .build()
        .expect("build staged bind volume");

    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec![
        "/bin/sh".to_owned(),
        "-lc".to_owned(),
        "cat /workspace/message.txt".to_owned(),
    ];
    let mut volume = visor_init::config::VolumeConfig::default();
    volume.guest_path = "/workspace".to_owned();
    volume.read_only = true;
    volume.device_path = "/dev/vdb".to_owned();
    volume.fs_type = "ext4".to_owned();
    config.volumes = vec![volume];

    let mut handle = visor_runtime::vm::boot_vm(
        "boot-ro-volume",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, 3),
        visor_runtime::vm::BootStorage::new(
            &[],
            &[visor_vmm::vm::DataDiskConfig::new(staged_disk, true)],
        ),
    )
    .expect("boot_vm should succeed");

    let completion_rx = handle
        .completion_rx
        .take()
        .expect("completion_rx should exist");
    let exit_info = tokio::time::timeout(Duration::from_secs(30), completion_rx)
        .await
        .expect("VM boot timed out")
        .expect("completion channel closed");

    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }

    let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
    assert_eq!(exit_info.exit_code, 0, "serial output:\n{serial}");
    let stdout = visor_runtime::vm::extract_stdout(&handle.serial_output.as_bytes());
    assert_eq!(
        stdout.trim(),
        "hello from virtiofs",
        "serial output:\n{serial}"
    );
}

#[tokio::test]
async fn boot_vm_mounts_file_backed_data_disk() {
    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    build_guest_rootfs(&rootfs, &["/bin/sh", "/bin/cat"]);

    let disk_source = tmp.path().join("disk-source");
    std::fs::create_dir_all(&disk_source).unwrap();
    std::fs::write(disk_source.join("message.txt"), "hello from data disk\n").unwrap();
    let data_disk = tmp.path().join("data.ext4");
    RootfsBuilder::new(&disk_source, &data_disk)
        .build()
        .expect("build file-backed data disk");

    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec![
        "/bin/sh".to_owned(),
        "-lc".to_owned(),
        "cat /data/message.txt".to_owned(),
    ];
    let mut volume = visor_init::config::VolumeConfig::default();
    volume.guest_path = "/data".to_owned();
    volume.device_path = "/dev/vdb".to_owned();
    volume.fs_type = "ext4".to_owned();
    config.volumes = vec![volume];

    let mut handle = visor_runtime::vm::boot_vm(
        "boot-rw-volume",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, 3),
        visor_runtime::vm::BootStorage::new(
            &[],
            &[visor_vmm::vm::DataDiskConfig::new(data_disk, false)],
        ),
    )
    .expect("boot_vm should succeed");

    let completion_rx = handle
        .completion_rx
        .take()
        .expect("completion_rx should exist");
    let exit_info = tokio::time::timeout(Duration::from_secs(30), completion_rx)
        .await
        .expect("VM boot timed out")
        .expect("completion channel closed");

    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }

    let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
    assert_eq!(exit_info.exit_code, 0, "serial output:\n{serial}");
    let stdout = visor_runtime::vm::extract_stdout(&handle.serial_output.as_bytes());
    assert_eq!(
        stdout.trim(),
        "hello from data disk",
        "serial output:\n{serial}"
    );
}

#[tokio::test]
#[serial]
async fn boot_vm_linux_port_forward_reaches_guest_http_server() {
    use visor_vmm::net::{NetworkBackend as _, PlatformNetworkBackend};

    let tmp = boot_test_tempdir().unwrap();
    let rootfs = tmp.path().join("rootfs.ext4");
    build_guest_rootfs(&rootfs, &["/usr/bin/busybox"]);

    let guest_ip = Ipv4Addr::new(172, 21, 33, 2);
    let host_port = reserve_local_port();

    let mut config = visor_init::config::RunConfig::default();
    config.cmd = vec![
        "/usr/bin/busybox".to_owned(),
        "sleep".to_owned(),
        "60".to_owned(),
    ];
    config.exec_listener = true;
    let mut network = visor_init::config::NetworkConfig::default();
    network.address = guest_ip.to_string();
    network.netmask = "255.255.255.252".to_owned();
    network.gateway = "172.21.33.1".to_owned();
    network.dns_servers = vec!["172.21.33.1".to_owned()];
    config.network = Some(network);

    let mut handle = visor_runtime::vm::boot_vm(
        "boot-resolv",
        &config,
        &rootfs,
        visor_runtime::vm::VmBootSpec::new(256, 1, 33),
        visor_runtime::vm::BootStorage::new(&[], &[]),
    )
    .expect("boot_vm should succeed");

    let backend = visor_vmm::comms::create_comms_backend();
    let connect_result = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            match visor_runtime::vsock::client::VsockClient::connect(
                &backend,
                33,
                visor_runtime::vsock::client::VSOCK_AGENT_PORT,
            )
            .await
            {
                Ok(client) => return Ok(client),
                Err(error) => {
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    if matches!(
                        error,
                        visor_runtime::vsock::client::VsockError::Timeout { .. }
                    ) {
                        return Err(error);
                    }
                }
            }
        }
    })
    .await;

    let mut client = match connect_result {
        Ok(Ok(client)) => client,
        Ok(Err(error)) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "guest exec listener should accept host vsock connection: {error}\nserial output:\n{serial}"
            );
        }
        Err(_) => {
            handle
                .kill_flag
                .store(true, std::sync::atomic::Ordering::Release);
            if let Some(thread) = handle.thread.take() {
                thread.join().expect("vCPU thread panicked");
            }
            let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
            panic!(
                "guest exec listener did not become reachable within 20s\nserial output:\n{serial}"
            );
        }
    };

    let httpd_result = client
        .exec(
            vec![
                "/usr/bin/busybox".to_owned(),
                "sh".to_owned(),
                "-lc".to_owned(),
                "mkdir -p /srv && printf 'hello from guest\\n' > /srv/index.html && /usr/bin/busybox httpd -p 8080 -h /srv && echo httpd-started".to_owned(),
            ],
            Vec::new(),
            "/".to_owned(),
        )
        .await
        .expect("guest exec listener should start the HTTP server");

    assert_eq!(
        httpd_result.exit_code, 0,
        "httpd setup should succeed\nstdout:\n{}\nstderr:\n{}",
        httpd_result.stdout, httpd_result.stderr
    );

    let net_backend = PlatformNetworkBackend::new();
    let mappings = vec![
        visor_vmm::net::PortMapping::from_spec(&format!("{host_port}:8080/tcp"), guest_ip)
            .expect("build VMM port-forward mapping"),
    ];
    let port_forward_handle = net_backend
        .setup_port_forward(&mappings)
        .expect("install Linux port-forward rule");

    let probe_result =
        wait_for_http_response(Ipv4Addr::LOCALHOST, host_port, Duration::from_secs(20)).await;

    drop(port_forward_handle);
    handle
        .kill_flag
        .store(true, std::sync::atomic::Ordering::Release);
    if let Some(thread) = handle.thread.take() {
        thread.join().expect("vCPU thread panicked");
    }

    let serial = String::from_utf8_lossy(&handle.serial_output.as_bytes()).into_owned();
    let response = match probe_result {
        Ok(response) => response,
        Err(forward_error) => {
            let direct_guest_result =
                wait_for_http_response(guest_ip, 8080, Duration::from_secs(5)).await;
            panic!(
                "host should reach forwarded guest HTTP server: {forward_error}\n\
                 httpd start stdout:\n{}\n\
                 httpd start stderr:\n{}\n\
                 direct guest probe: {direct_guest_result:?}\n\
                 serial output:\n{serial}",
                httpd_result.stdout, httpd_result.stderr
            );
        }
    };
    assert!(
        response.contains("200 OK"),
        "expected successful HTTP response, got:\n{response}"
    );
    assert!(
        response.contains("hello from guest"),
        "expected guest body in forwarded response, got:\n{response}"
    );
}
