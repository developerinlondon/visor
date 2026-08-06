//! visor-init — Guest PID 1 for visor microVMs.
//!
//! Boot sequence:
//! 1. Mount essential filesystems (/proc, /sys, /dev)
//! 2. Create essential device nodes
//! 3. Read run configuration from kernel cmdline (`visor.config=<base64-json>`)
//! 4. Configure networking (if config provides it)
//! 5. Mount volumes (if any)
//! 6. Write guest `/etc/resolv.conf` and `/etc/hosts`
//! 7. Check mode: if `"agent"`, start vsock listener; otherwise execute command
//! 8. Execute the user's command (run mode)
//! 9. Reap zombies and forward signals until child exits
//! 10. Print exit code marker for host capture

// visor-init only runs inside Linux guest VMs. On non-Linux hosts it still
// needs to compile (it is a workspace member) but must not be executed.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("visor-init is a Linux guest binary and cannot run on this platform.");
    std::process::exit(1);
}

#[cfg(target_os = "linux")]
fn main() {
    if let Err(e) = run() {
        // Print the full error chain to stdout (serial console) since
        // stderr may not be visible in the early guest environment.
        println!("visor-init: FATAL: {e:?}");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
fn run() -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::time::Duration;

    /// Give the guest serial path a brief window to deliver the exit marker
    /// before PID 1 powers the VM off.
    const EXIT_MARKER_GRACE_PERIOD: Duration = Duration::from_millis(100);

    // Step 1: Mount essential filesystems
    visor_init::mount::mount_initial_filesystems()
        .context("failed to mount initial filesystems")?;

    // Step 2: Create essential device nodes (/dev/null, /dev/zero, /dev/urandom)
    visor_init::mount::create_essential_devices().context("failed to create essential devices")?;

    // Step 3: Read configuration from kernel cmdline
    let config = visor_init::config::RunConfig::from_kernel_cmdline();

    // Step 4: Configure networking (if provided)
    let networks = config.effective_networks();
    if !networks.is_empty() {
        visor_init::network::configure_loopback().context("failed to configure loopback")?;
        for (index, net_config) in networks.iter().enumerate() {
            let setup = visor_init::network::NetworkSetup::from_config(net_config, index)
                .context("failed to parse network config")?;
            visor_init::network::configure_network(&setup)
                .context("failed to configure network")?;
        }
    }

    // Step 5: Mount volumes
    visor_init::volume::mount_volumes(&config.volumes).context("failed to mount volumes")?;

    // Step 6: Write guest network config files after final mount layout is in place.
    if !networks.is_empty() {
        visor_init::network::write_resolv_conf(&networks)
            .context("failed to write guest resolv.conf")?;
    }
    visor_init::network::write_hosts_file(&config.extra_hosts)
        .context("failed to write guest hosts file")?;

    // Step 7: Check operating mode
    if config.mode == "agent" {
        // Agent mode: start the vsock listener (loops forever, does not return)
        visor_init::listener::start_listener(config).context("agent listener failed")?;
        // start_listener loops forever; this is unreachable under normal operation.
        return Ok(());
    }

    // Run mode (default): execute the user's command
    if config.exec_listener {
        let listener_config = config.clone();
        std::thread::spawn(move || {
            if let Err(error) = visor_init::listener::start_listener(listener_config) {
                println!("visor-init: background agent listener failed: {error:?}");
            }
        });
    }

    // Step 8: Set up signal handling for PID 1
    visor_init::entrypoint::setup_signal_handlers().context("failed to set up signal handlers")?;

    // Step 9: Spawn the user's command
    write_stdout_marker(STDOUT_BEGIN_MARKER).context("write stdout begin marker")?;
    let params = visor_init::entrypoint::ExecParams::from_config(&config);
    let child_pid =
        visor_init::entrypoint::spawn_child(&params).context("failed to spawn child process")?;

    // Step 10: Wait for child, reaping zombies along the way
    let result = visor_init::entrypoint::wait_for_child(child_pid)
        .context("failed waiting for child process")?;
    write_stdout_marker(STDOUT_END_MARKER).context("write stdout end marker")?;

    // Final cleanup: reap any remaining zombies
    let _ = visor_init::entrypoint::reap_zombies();

    // Write the entire marker directly to the stdout fd so the host sees a
    // contiguous `VISOR_EXIT_CODE=<n>` line before any shutdown/panic text.
    if let Err(error) = write_exit_marker(result.exit_code) {
        eprintln!("visor-init: stdout drain failed before poweroff: {error}");
    }
    std::thread::sleep(EXIT_MARKER_GRACE_PERIOD);

    // Ask the guest kernel to reboot explicitly instead of relying on
    // "PID 1 exited" panic behavior. The guest kernel cmdline sets
    // `reboot=t`, so an explicit reboot becomes a clean hypervisor exit.
    match nix::sys::reboot::reboot(nix::sys::reboot::RebootMode::RB_AUTOBOOT) {
        Ok(never) => match never {},
        Err(error) => {
            eprintln!("visor-init: reboot failed after command exit: {error}");
            std::process::exit(result.exit_code);
        }
    }
}

#[cfg(target_os = "linux")]
const STDOUT_BEGIN_MARKER: &str = "VISOR_STDOUT_BEGIN";

#[cfg(target_os = "linux")]
const STDOUT_END_MARKER: &str = "VISOR_STDOUT_END";

#[cfg(target_os = "linux")]
fn write_stdout_marker(marker: &str) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::io::Write as _;

    let mut stdout = std::io::stdout().lock();
    writeln!(stdout, "{marker}").context("write stdout marker to serial console")?;
    stdout
        .flush()
        .context("flush stdout marker to serial console")?;
    Ok(())
}

#[cfg(target_os = "linux")]
fn write_exit_marker(exit_code: i32) -> anyhow::Result<()> {
    use anyhow::Context as _;
    use std::os::fd::AsFd as _;

    let marker = format!("\nVISOR_EXIT_CODE={exit_code}\n");
    let stdout = std::io::stdout();
    let stdout_fd = stdout.as_fd();
    let mut remaining = marker.as_bytes();

    while !remaining.is_empty() {
        let written =
            nix::unistd::write(stdout_fd, remaining).context("write exit marker to stdout")?;
        remaining = &remaining[written..];
    }

    nix::sys::termios::tcdrain(stdout_fd).context("drain stdout tty")?;
    Ok(())
}
