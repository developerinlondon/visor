//! Diagnostic tool: boots a microVM and dumps serial output.
//!
//! Usage: `vm_debug <rootfs.ext4> [timeout_secs]`
//!
//! Boots the kernel with a minimal rootfs, runs for up to 10 seconds,
//! then dumps everything the serial port captured. This isolates the
//! VM boot path from the OCI pipeline, daemon, and HTTP layers.
//!
//! Works on both Linux (KVM / `x86_64`) and macOS (HVF / `aarch64`).

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use visor_init::config::RunConfig;
use visor_vmm::devices::DeviceManager;
use visor_vmm::vm::{
    ExitAction, ExitHandler, VcpuError, VmConfig, VmExit, boot, run_vcpu_with_handler,
};

/// Wraps [`DeviceManager`] with exit logging and a timeout guard.
struct DebugHandler {
    inner: DeviceManager,
    start: Instant,
    timeout: Duration,
    exit_count: u64,
    last_printed: u64,
}

impl ExitHandler for DebugHandler {
    fn handle_exit(&mut self, exit: VmExit) -> Result<ExitAction, VcpuError> {
        self.exit_count += 1;

        match &exit {
            VmExit::IoOut { port, data } => {
                // Only log non-serial ports to avoid flooding.
                if !is_serial_port(*port) {
                    eprintln!(
                        "[{:>6}] IoOut port={:#06x} data={:02x?}",
                        self.exit_count,
                        port,
                        data.as_bytes()
                    );
                }
            }
            VmExit::MmioWrite { addr, data } => {
                if is_virtio_mmio_range(*addr) {
                    let offset = *addr - VIRTIO_MMIO_BASE;
                    let name = mmio_register_name(offset);
                    eprintln!(
                        "[{:>6}] MmioWrite {:#05x} ({}) = {:02x?}",
                        self.exit_count,
                        offset,
                        name,
                        data.as_bytes()
                    );
                } else {
                    eprintln!(
                        "[{:>6}] MmioWrite addr={:#010x} len={}",
                        self.exit_count,
                        addr,
                        data.len()
                    );
                }
            }
            VmExit::MmioRead { addr, size } => {
                if !is_virtio_mmio_range(*addr) {
                    eprintln!(
                        "[{:>6}] MmioRead  addr={:#010x} size={}",
                        self.exit_count, addr, size
                    );
                }
                // Virtio MMIO reads are logged in handle_mmio_read after value is filled.
            }
            VmExit::Shutdown => eprintln!("[{:>6}] *** SHUTDOWN ***", self.exit_count),
            VmExit::Reboot => eprintln!("[{:>6}] *** REBOOT ***", self.exit_count),
            VmExit::Halt => {
                if self.exit_count - self.last_printed >= 10000 {
                    eprintln!("[{:>6}] Halt (periodic)", self.exit_count);
                    self.last_printed = self.exit_count;
                }
            }
            VmExit::IoIn { port, .. } => {
                if !is_serial_port(*port) {
                    eprintln!("[{:>6}] IoIn  port={:#06x}", self.exit_count, port);
                }
            }
            _ => {
                eprintln!("[{:>6}] Unknown exit: {exit:?}", self.exit_count);
            }
        }

        // Check timeout.
        if self.start.elapsed() > self.timeout {
            eprintln!(
                "\n=== TIMEOUT after {:?} ({} exits) ===",
                self.timeout, self.exit_count
            );
            return Ok(ExitAction::Stop);
        }

        self.inner.handle_exit(exit)
    }

    fn handle_io_read(&mut self, port: u16, data: &mut [u8]) {
        self.inner.handle_io_read(port, data);
    }

    fn handle_mmio_read(&mut self, addr: u64, data: &mut [u8]) {
        self.inner.handle_mmio_read(addr, data);
        if is_virtio_mmio_range(addr) {
            let offset = addr - VIRTIO_MMIO_BASE;
            let name = mmio_register_name(offset);
            eprintln!(
                "[{:>6}] MmioRead  {:#05x} ({}) => {:02x?}",
                self.exit_count, offset, name, data
            );
        }
    }
}

// ── Constants ─────────────────────────────────────────────────────────

/// Base address of the first virtio-MMIO device region.
const VIRTIO_MMIO_BASE: u64 = 0xd000_0000;

/// End of the first virtio-MMIO device region (exclusive).
const VIRTIO_MMIO_END: u64 = 0xd000_1000;

/// Serial console parameter: `ttyS0` on `x86_64`, `ttyAMA0` on `aarch64`.
#[cfg(target_arch = "x86_64")]
const CONSOLE_TTY: &str = "ttyS0";

#[cfg(target_arch = "aarch64")]
const CONSOLE_TTY: &str = "ttyAMA0";

// ── Helpers ───────────────────────────────────────────────────────────

/// Returns `true` if `addr` falls within the first virtio-MMIO region.
const fn is_virtio_mmio_range(addr: u64) -> bool {
    addr >= VIRTIO_MMIO_BASE && addr < VIRTIO_MMIO_END
}

/// Returns `true` if `port` is in the COM1 serial I/O range (`x86_64` only).
fn is_serial_port(port: u16) -> bool {
    // COM1 is 0x3F8..0x400 (8 ports). On aarch64 serial is MMIO, so this
    // will simply never match any IoIn/IoOut exits.
    let base = visor_vmm::devices::serial::COM1_PORT_BASE;
    let count = visor_vmm::devices::serial::COM1_PORT_COUNT;
    port >= base && (u64::from(port) < u64::from(base) + count)
}

/// Maps virtio MMIO register offsets to human-readable names.
fn mmio_register_name(offset: u64) -> &'static str {
    match offset {
        0x00 => "Magic",
        0x04 => "Version",
        0x08 => "DeviceID",
        0x0C => "VendorID",
        0x10 => "DevFeatures",
        0x14 => "DevFeaturesSel",
        0x20 => "DrvFeatures",
        0x24 => "DrvFeaturesSel",
        0x30 => "QueueSel",
        0x34 => "QueueNumMax",
        0x38 => "QueueNum",
        0x44 => "QueueReady",
        0x50 => "QueueNotify",
        0x60 => "IntStatus",
        0x64 => "IntACK",
        0x70 => "Status",
        0x80 => "QueueDescLow",
        0x84 => "QueueDescHigh",
        0x90 => "QueueAvailLow",
        0x94 => "QueueAvailHigh",
        0xA0 => "QueueUsedLow",
        0xA4 => "QueueUsedHigh",
        0xFC => "ConfigGen",
        o if o >= 0x100 => "Config",
        _ => "Unknown",
    }
}

// ── Main ──────────────────────────────────────────────────────────────

fn main() -> anyhow::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        eprintln!("Usage: vm_debug <rootfs.ext4> [timeout_secs]");
        eprintln!("\nBoots a microVM and dumps serial output.");
        eprintln!("This isolates VM boot from the daemon/OCI pipeline.");
        std::process::exit(1);
    }

    let rootfs_path = PathBuf::from(&args[1]);
    let timeout_secs: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(10);

    if !rootfs_path.is_file() {
        eprintln!("Error: rootfs not found: {}", rootfs_path.display());
        std::process::exit(1);
    }

    eprintln!("=== visor vm_debug ===");
    eprintln!("rootfs:  {}", rootfs_path.display());
    eprintln!("timeout: {timeout_secs}s");
    eprintln!();

    // Build a minimal RunConfig.
    let mut run_config = RunConfig::default();
    run_config.cmd = vec!["echo".into(), "hello".into()];

    // Build kernel command line.
    let json = run_config.to_json()?;
    let engine = base64::engine::general_purpose::STANDARD_NO_PAD;
    let encoded = base64::Engine::encode(&engine, json.as_bytes());
    let cmdline = format!(
        "console={CONSOLE_TTY} earlyprintk=serial reboot=t panic=-1 root=/dev/vda rw \
         init=/sbin/visor-init visor.config={encoded}"
    );
    eprintln!("cmdline: {}", &cmdline[..80.min(cmdline.len())]);
    eprintln!();

    // Boot the VM via the portable facade.
    let kernel_path = visor_kernel::kernel_path();
    eprintln!("kernel:  {}", kernel_path.display());

    let vm_config = VmConfig::new(
        &kernel_path,
        &cmdline,
        &rootfs_path,
        256, // 256 MiB RAM
        1,   // 1 vCPU
        100, // guest CID
    );

    let mut booted = boot(&vm_config).context("failed to boot VM")?;
    eprintln!("VM booted successfully");
    eprintln!();

    // Take ownership of the device manager to wrap in DebugHandler.
    let device_mgr = std::mem::replace(&mut booted.device_mgr, DeviceManager::new());

    let mut handler = DebugHandler {
        inner: device_mgr,
        start: Instant::now(),
        timeout: Duration::from_secs(timeout_secs),
        exit_count: 0,
        last_printed: 0,
    };

    // Arm a timer thread to set the kill flag after the timeout.
    // This ensures the hypervisor run loop terminates even if no VM exits
    // trigger the timeout check (e.g., guest is stuck in a tight HLT loop).
    let kill_flag = booted.kill_flag.clone();
    let timeout = Duration::from_secs(timeout_secs);
    std::thread::spawn(move || {
        std::thread::sleep(timeout);
        kill_flag.store(true, std::sync::atomic::Ordering::Release);
    });

    eprintln!("=== Starting vCPU run loop (timeout: {timeout_secs}s) ===\n");

    let result = run_vcpu_with_handler(&mut booted, &mut handler);
    let elapsed = handler.start.elapsed();

    eprintln!(
        "\n=== vCPU stopped after {:.2}s ({} exits) ===",
        elapsed.as_secs_f64(),
        handler.exit_count
    );

    match &result {
        Ok(run_result) => {
            eprintln!("result: Ok");

            // Dump registers via portable Display impls.
            eprintln!("\n=== vCPU REGISTER DUMP ===");
            if let Some(regs) = &run_result.regs {
                eprintln!("{regs}");
            } else {
                eprintln!("  (standard registers not available)");
            }
            if let Some(sregs) = &run_result.sregs {
                eprintln!("{sregs}");
            } else {
                eprintln!("  (special registers not available)");
            }
        }
        Err(e) => {
            eprintln!("result: Err({e})");
        }
    }

    // Dump serial output.
    let serial_bytes = booted.serial_output.as_bytes();
    eprintln!("\n=== SERIAL OUTPUT ({} bytes) ===\n", serial_bytes.len());
    let text = String::from_utf8_lossy(&serial_bytes);
    println!("{text}");

    // Propagate any error from the run loop.
    result
        .map(|_| ())
        .map_err(|e| anyhow::anyhow!("vCPU run failed: {e}"))
}
