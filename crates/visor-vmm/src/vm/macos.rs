//! macOS (HVF / aarch64) boot path and run loop.

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use crate::devices::block::BlockDevice;
use crate::devices::fs::FsDevice;
use crate::devices::net::NetDevice;
use crate::devices::rng::RngDevice;
use crate::devices::serial::COM1_IRQ;
use crate::devices::vsock::VsockDevice;
use crate::devices::vsock_muxer::VsockMuxer;
use crate::memory::GuestMemory;
use crate::platform::VmOps;
use crate::platform::event::InterruptEvent;
use crate::transport::mmio::MmioTransport;

use super::{
    BootedVm, BootedVmInner, DeviceManager, ExitAction, ExitHandler, MEMORY_SLOT, MIN_MEMORY_MIB,
    SerialOutput, VcpuError, VmBootError, VmExit,
};

// ── Boot register configuration ──────────────────────────────────────

/// Configures initial boot registers on a freshly-created HVF vCPU.
///
/// HVF vCPUs are thread-affine and start with zeroed registers.
/// Unlike KVM, register state does NOT persist across destroy/recreate.
/// This must be called on the vCPU thread before entering the run loop.
///
/// Sets:
/// - **PC** = `entry_point` (kernel entry address)
/// - **X0** = `fdt_addr` (Flattened Device Tree base)
/// - **CPSR** = `PSTATE_FAULT_BITS_64` (`0x3c5`, `EL1h` with DAIF masked)
/// - **`MPIDR_EL1`** = `0x8000_0000 | vcpu_index` (bit 31 = RES1 per ARM spec)
/// - **vtimer offset** = `mach_absolute_time()` (monotonic epoch for guest timers)
///
/// Note: `HCR_EL2` and `CNTHCTL_EL2` are managed internally by Apple HVF
/// for non-nested VMs. Setting them explicitly requires EL2 enabled in the
/// VM configuration (nested virtualization) and is not needed for standard boot.
///
/// # Errors
///
/// Returns [`VcpuError`] if register reads or writes fail.
pub(crate) fn configure_vcpu_boot_regs(
    vcpu: &<crate::platform::HvfVm as VmOps>::Vcpu,
    entry_point: u64,
    fdt_addr: u64,
    vcpu_index: u64,
) -> Result<(), VcpuError> {
    use crate::boot::aarch64;
    use crate::platform::VcpuOps;

    // ARM reset value for SCTLR_EL1 from M1 boot ROM (QEMU target/arm/hvf/hvf.c:883-889).
    const SCTLR_EL1_RESET: u64 = 0x3090_0180;
    // ID_AA64PFR1_EL1.SME field [27:24] — must be masked on M3/M4.
    const SME_MASK: u64 = 0xF << 24;
    // 40-bit physical address range (1 TiB), safe for default HVF IPA.
    const PARANGE_40BIT: u64 = 0b0010;

    // 1. General-purpose registers: PC, X0 (FDT pointer), CPSR.
    let mut regs = vcpu
        .get_regs()
        .map_err(|e| VcpuError::Create(std::io::Error::other(e)))?;
    regs.pc = entry_point;
    regs.x[0] = fdt_addr;
    regs.cpsr = aarch64::PSTATE_FAULT_BITS_64;
    vcpu.set_regs(&regs)
        .map_err(|e| VcpuError::Create(std::io::Error::other(e)))?;

    // 2. MPIDR_EL1: Multiprocessor Affinity Register.
    //    Bit 31 is RES1 (reserved-as-one) per the ARM Architecture Reference Manual.
    //    Lower bits encode the affinity / logical CPU index so Linux can identify CPUs.
    //    Reference: libkrun sets MPIDR at src/hvf/src/lib.rs:360-365.
    let mpidr = 0x8000_0000_u64 | vcpu_index;
    vcpu.vcpu
        .set_sys_reg(applevisor::vcpu::SysReg::MPIDR_EL1, mpidr)
        .map_err(|e| VcpuError::SetRegs(std::io::Error::other(e)))?;

    // 3. ID_AA64PFR0_EL1: Advertise GICv3 system register interface.
    //    Bits [27:24] = 0b0001 indicates GICv3 sysreg support.
    //    Without this, the Linux GIC driver cannot initialize the interrupt
    //    controller, causing the kernel to panic before timer init.
    //    Reference: QEMU sets this at target/arm/hvf/hvf.c:1038-1040.
    //    Reference: libkrun sets AA64PFR0_EL1_GIC3EN at src/hvf/src/lib.rs:91-92.
    let pfr = vcpu
        .vcpu
        .get_sys_reg(applevisor::vcpu::SysReg::ID_AA64PFR0_EL1)
        .map_err(|e| VcpuError::Create(std::io::Error::other(e)))?;
    vcpu.vcpu
        .set_sys_reg(applevisor::vcpu::SysReg::ID_AA64PFR0_EL1, pfr | (1 << 24))
        .map_err(|e| VcpuError::SetRegs(std::io::Error::other(e)))?;

    // 4. SCTLR_EL1: System Control Register reset value.
    //    HVF starts with SCTLR_EL1 = 0, but ARM requires several RES1
    //    bits. QEMU uses the M1 boot ROM value 0x30900180:
    //      bits 28-29 = RES1, bit 23 = SPAN, bit 20 = TSCXT (RES1),
    //      bit 8 = SED (RES1), bit 7 = ITD (RES1).
    //    Without these, CPU behavior is architecturally UNPREDICTABLE.
    //    Reference: QEMU at target/arm/hvf/hvf.c:883-889.
    vcpu.vcpu
        .set_sys_reg(applevisor::vcpu::SysReg::SCTLR_EL1, SCTLR_EL1_RESET)
        .map_err(|e| VcpuError::SetRegs(std::io::Error::other(e)))?;

    // 5. ID_AA64PFR1_EL1: Mask out SME feature bits.
    //    On M3/M4, HVF exposes SME bits [27:24]. Linux attempts to use SME,
    //    fails, and hangs during early boot. This is defensive — on M1/M2
    //    where SME isn't present, the mask is a no-op.
    //    Reference: QEMU at target/arm/hvf/hvf.c:874-875.
    //    Reference: libkrun at src/hvf/src/lib.rs:431-453.
    let pfr1 = vcpu
        .vcpu
        .get_sys_reg(applevisor::vcpu::SysReg::ID_AA64PFR1_EL1)
        .map_err(|e| VcpuError::Create(std::io::Error::other(e)))?;
    vcpu.vcpu
        .set_sys_reg(applevisor::vcpu::SysReg::ID_AA64PFR1_EL1, pfr1 & !SME_MASK)
        .map_err(|e| VcpuError::SetRegs(std::io::Error::other(e)))?;

    // 6. ID_AA64MMFR0_EL1: Clamp PARange to match VM IPA size.
    //    The hardware may advertise a larger physical address range than
    //    what the VM's IPA size supports. If unclamped, Linux tries to map
    //    addresses beyond our IPA range and faults.
    //    We clamp to PARange 0b0010 (40-bit / 1 TiB) which is safe for
    //    the default HVF IPA size.
    //    Reference: QEMU at target/arm/hvf/hvf.c:1043-1050.
    let mmfr0 = vcpu
        .vcpu
        .get_sys_reg(applevisor::vcpu::SysReg::ID_AA64MMFR0_EL1)
        .map_err(|e| VcpuError::Create(std::io::Error::other(e)))?;
    let hw_parange = mmfr0 & 0xF;
    let clamped_parange = if hw_parange > PARANGE_40BIT {
        PARANGE_40BIT
    } else {
        hw_parange
    };
    vcpu.vcpu
        .set_sys_reg(
            applevisor::vcpu::SysReg::ID_AA64MMFR0_EL1,
            (mmfr0 & !0xF) | clamped_parange,
        )
        .map_err(|e| VcpuError::SetRegs(std::io::Error::other(e)))?;

    // 7. Virtual timer offset: capture the host monotonic time so the guest
    //    timer starts from a sane epoch. Without this, the Linux kernel timer
    //    subsystem misbehaves on Apple HVF.
    //    Reference: QEMU sets vtimer_offset at target/arm/hvf/hvf.c:2086-2095.
    //
    //    SAFETY: `mach_absolute_time` is a leaf function from libSystem that
    //    returns a monotonic nanosecond-ish timestamp. Always safe to call.
    #[allow(unsafe_code)]
    let vtimer_offset = unsafe { mach_absolute_time() };
    vcpu.vcpu
        .set_vtimer_offset(vtimer_offset)
        .map_err(|e| VcpuError::SetRegs(std::io::Error::other(e)))?;

    Ok(())
}

// FFI binding for `mach_absolute_time()` from libSystem.
//
// Returns a monotonic nanosecond-ish timestamp used as the vtimer epoch offset
// for HVF vCPUs. This matches QEMU's approach of capturing the host time at
// VM init so the guest virtual timer starts from a sane baseline.
#[allow(unsafe_code)]
unsafe extern "C" {
    fn mach_absolute_time() -> u64;
}

// ── Helper functions ─────────────────────────────────────────────────

/// Creates a [`VsockMuxer`] for the given device, extracting the TX notify handle.
///
/// # Errors
///
/// Returns [`VmBootError::Device`] if the muxer socket directory cannot be created.
fn create_vsock_muxer(
    device: Arc<Mutex<VsockDevice>>,
    guest_cid: u32,
    transport: Arc<Mutex<MmioTransport>>,
) -> Result<VsockMuxer, VmBootError> {
    let socket_dir = PathBuf::from("/var/run/visor/vsock");
    let tx_notify = {
        let dev = device
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        dev.tx_notify()
    };
    let rx_kick: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Ok(transport) = transport.lock() {
            let _ = transport.process_external_queue(0);
        }
    });
    VsockMuxer::new(device, u64::from(guest_cid), socket_dir, tx_notify, rx_kick)
        .map_err(|e| VmBootError::Device(format!("vsock muxer: {e}")))
}

/// Creates the net device with its MMIO transport, auto-selecting the backend.
///
/// On macOS 26+ with vmnet-helper installed, uses the rootless vmnet-helper
/// subprocess path. Otherwise falls back to direct vmnet (requires sudo).
///
/// Returns the MMIO transport (for bus registration and run-loop RX polling)
/// and the `AtomicBool` flag used for RX-ready signalling.
///
/// # Errors
///
/// Returns [`VmBootError::Device`] if the network backend or packet I/O creation fails.
fn create_net_device(
    vm: &crate::platform::HvfVm,
    memory: &Arc<GuestMemory>,
) -> Result<(Arc<Mutex<MmioTransport>>, Arc<AtomicBool>), VmBootError> {
    use crate::net::macos::{SendableInterface, VmnetHelperPacketIo, VmnetPacketIo};
    use crate::net::{NetworkBackend, PlatformNetworkBackend};
    use crate::platform::MacosEventFd;

    // Auto-select: vmnet-helper (rootless on macOS 26+) or direct vmnet (sudo).
    let use_helper = std::path::Path::new(crate::net::macos::VMNET_HELPER_PATH).exists()
        && crate::net::macos::is_macos_26_or_later().unwrap_or(false);

    let (packet_io, mac, net_has_pending): (Box<dyn crate::devices::net::PacketIo>, _, _) =
        if use_helper {
            let (helper_io, info) = VmnetHelperPacketIo::spawn()
                .map_err(|e| VmBootError::Device(format!("vmnet-helper: {e}")))?;
            let mac = parse_mac(&info.mac_address).unwrap_or_else(NetDevice::generate_mac);
            // Helper uses non-blocking socketpair; always signal pending so the
            // run loop polls on every vCPU exit.
            let flag = Arc::new(AtomicBool::new(true));
            (Box::new(helper_io), mac, flag)
        } else {
            // Fallback: direct vmnet (requires entitlement or root).
            let net_backend = PlatformNetworkBackend::new();
            let net_config = crate::net::backend::InterfaceConfig::new("visor0");
            let mut net_iface = net_backend
                .create_interface(&net_config)
                .map_err(|e| VmBootError::Device(format!("net interface: {e}")))?;
            let vmnet_iface = net_iface
                .take_interface()
                .ok_or_else(|| VmBootError::Device("vmnet interface missing".into()))?;
            let sendable = SendableInterface::new(vmnet_iface);
            let (vmnet_io, flag) = VmnetPacketIo::new(sendable)
                .map_err(|e| VmBootError::Device(format!("vmnet packet io: {e}")))?;
            (Box::new(vmnet_io), NetDevice::generate_mac(), flag)
        };

    let net_dev = NetDevice::with_packet_io(mac, packet_io);
    let net_arc = Arc::new(Mutex::new(net_dev));
    let mut net_mmio = MmioTransport::new(net_arc);
    let net_irqfd =
        MacosEventFd::new().map_err(|e| VmBootError::Device(format!("net irqfd: {e}")))?;
    vm.register_irqfd(&net_irqfd, 7)?;
    net_mmio.set_memory(Arc::clone(memory));
    net_mmio.set_irq_evt(Arc::new(net_irqfd));
    net_mmio.set_irq_deassert(vm.create_spi_deassert(32 + 7));
    let net_mmio_arc = Arc::new(Mutex::new(net_mmio));

    Ok((net_mmio_arc, net_has_pending))
}

/// Parses a MAC address string like `"aa:bb:cc:dd:ee:ff"` into 6 bytes.
fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.split(':').collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, part) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(part, 16).ok()?;
    }
    Some(mac)
}

fn register_mmio(
    bus: &mut crate::devices::bus::Bus,
    base: u64,
    size: u64,
    device: Arc<Mutex<dyn crate::devices::bus::BusDevice>>,
    label: &str,
) -> Result<(), VmBootError> {
    bus.register(base, size, device)
        .map_err(|e| VmBootError::Device(format!("register {label} MMIO: {e}")))
}

// ── Boot ─────────────────────────────────────────────────────────────

#[allow(clippy::too_many_lines)]
pub(super) fn boot_macos(config: &super::VmConfig<'_>) -> Result<BootedVm, VmBootError> {
    use crate::boot::aarch64::{self, DRAM_MEM_START, FdtConfig};
    use crate::platform::{HvfPlatform, MacosEventFd, Platform};
    // 1. HVF platform + IRQ chip
    let platform = HvfPlatform::new()?;
    let vm = platform.create_vm()?;
    vm.create_irq_chip()?;
    vm.create_pit()?;
    // 3. Guest memory — ARM64 DRAM starts at 0x8000_0000
    let effective_mib = config.memory_mib.max(MIN_MEMORY_MIB);
    let memory_bytes = effective_mib as usize * 1024 * 1024;
    let memory = if let Some(mem) = config.guest_memory.clone() {
        mem
    } else {
        Arc::new(GuestMemory::new(memory_bytes, DRAM_MEM_START)?)
    };
    vm.register_memory(
        MEMORY_SLOT,
        DRAM_MEM_START,
        memory_bytes as u64,
        memory.host_addr(),
    )?;
    // 4. Kernel boot setup (ARM64 Image + FDT)
    let mut fdt_mmio_devices = vec![
        aarch64::FdtMmioDevice::new(0xd000_0000, 0x1000, 5), // block
        aarch64::FdtMmioDevice::new(0xd000_1000, 0x1000, 6), // vsock
        aarch64::FdtMmioDevice::new(0xd000_2000, 0x1000, 7), // net
        aarch64::FdtMmioDevice::new(0xd000_3000, 0x1000, 8), // rng
    ];
    for (i, _) in config.shared_dirs.iter().enumerate() {
        let base =
            0xd000_4000_u64 + u64::from(u32::try_from(i).expect("too many shared dirs")) * 0x1000;
        let irq =
            9 + u32::try_from(i).map_err(|_| VmBootError::Device("too many shared dirs".into()))?;
        fdt_mmio_devices.push(aarch64::FdtMmioDevice::new(base, 0x1000, irq));
    }
    let fdt_config = FdtConfig {
        memory_size: memory_bytes as u64,
        num_cpus: 1,
        cmdline: config.cmdline,
        gic_dist_addr: 0x0800_0000,
        gic_dist_size: 0x0001_0000,
        gic_redist_addr: 0x080A_0000,
        gic_redist_size: 0x00F6_0000,
        mmio_devices: &fdt_mmio_devices,
    };
    let boot_config = aarch64::configure_boot(&memory, config.kernel_path, &fdt_config)?;
    if config.vcpus > 1 {
        tracing::warn!(
            requested_vcpus = config.vcpus,
            effective_vcpus = 1,
            "multi-vCPU not yet supported, capping to 1"
        );
    }
    // 5. Wire devices — MMIO-only on ARM64 (no PIO bus)
    let serial_output = SerialOutput::new();
    let serial_irq: Arc<dyn InterruptEvent> =
        Arc::new(MacosEventFd::new().map_err(|e| VmBootError::Device(format!("serial irq: {e}")))?);
    let serial =
        crate::devices::pl011::Pl011::new(Box::new(serial_output.clone()), Arc::clone(&serial_irq));
    vm.register_irqfd(serial_irq.as_ref(), COM1_IRQ)?;
    let serial_arc: Arc<Mutex<dyn crate::devices::bus::BusDevice>> = Arc::new(Mutex::new(serial));
    let block = BlockDevice::new(config.rootfs_path, false)
        .map_err(|e| VmBootError::Device(format!("block device: {e}")))?;
    let block_arc = Arc::new(Mutex::new(block));
    let mut blk_mmio = MmioTransport::new(block_arc);
    let blk_irqfd =
        MacosEventFd::new().map_err(|e| VmBootError::Device(format!("blk irqfd: {e}")))?;
    vm.register_irqfd(&blk_irqfd, 5)?;
    blk_mmio.set_memory(Arc::clone(&memory));
    blk_mmio.set_irq_evt(Arc::new(blk_irqfd));
    blk_mmio.set_irq_deassert(vm.create_spi_deassert(32 + 5));
    let blk_mmio_arc = Arc::new(Mutex::new(blk_mmio));
    let vsock = VsockDevice::new(u64::from(config.guest_cid));
    let vsock_arc: Arc<Mutex<VsockDevice>> = Arc::new(Mutex::new(vsock));
    let vsock_for_muxer = Arc::clone(&vsock_arc);
    let mut vsock_mmio = MmioTransport::new(vsock_arc);
    let vsock_irqfd =
        MacosEventFd::new().map_err(|e| VmBootError::Device(format!("vsock irqfd: {e}")))?;
    vm.register_irqfd(&vsock_irqfd, 6)?;
    let vsock_irq: Arc<dyn InterruptEvent> = Arc::new(vsock_irqfd);
    vsock_mmio.set_memory(Arc::clone(&memory));
    vsock_mmio.set_irq_evt(Arc::clone(&vsock_irq));
    vsock_mmio.set_irq_deassert(vm.create_spi_deassert(32 + 6));
    let vsock_mmio_arc = Arc::new(Mutex::new(vsock_mmio));

    // 5b. Net device — vmnet.framework backed
    let (net_mmio_arc, net_has_pending) = create_net_device(&vm, &memory)?;

    // 5c. RNG device — entropy source backed by /dev/urandom
    let rng_dev = RngDevice::new().map_err(|e| VmBootError::Device(format!("rng device: {e}")))?;
    let rng_arc = Arc::new(Mutex::new(rng_dev));
    let mut rng_mmio = MmioTransport::new(rng_arc);
    let rng_irqfd =
        MacosEventFd::new().map_err(|e| VmBootError::Device(format!("rng irqfd: {e}")))?;
    vm.register_irqfd(&rng_irqfd, 8)?;
    rng_mmio.set_memory(Arc::clone(&memory));
    rng_mmio.set_irq_evt(Arc::new(rng_irqfd));
    rng_mmio.set_irq_deassert(vm.create_spi_deassert(32 + 8));
    let rng_mmio_arc = Arc::new(Mutex::new(rng_mmio));
    // ARM64: serial on MMIO at 0x0900_0000, virtio at 0xd000_xxxx
    let mut device_mgr = DeviceManager::new();
    register_mmio(
        &mut device_mgr.mmio_bus,
        0x0900_0000,
        0x1000,
        serial_arc,
        "serial",
    )?;
    register_mmio(
        &mut device_mgr.mmio_bus,
        0xd000_0000,
        0x1000,
        blk_mmio_arc,
        "block",
    )?;
    register_mmio(
        &mut device_mgr.mmio_bus,
        0xd000_1000,
        0x1000,
        Arc::clone(&vsock_mmio_arc) as _,
        "vsock",
    )?;
    register_mmio(
        &mut device_mgr.mmio_bus,
        0xd000_2000,
        0x1000,
        Arc::clone(&net_mmio_arc) as _,
        "net",
    )?;
    register_mmio(
        &mut device_mgr.mmio_bus,
        0xd000_3000,
        0x1000,
        rng_mmio_arc,
        "rng",
    )?;

    // 5d. Virtio-fs devices (one per shared directory)
    for (i, shared_dir) in config.shared_dirs.iter().enumerate() {
        let tag = format!("visor-fs-{i}");
        let fs_dev = FsDevice::new(shared_dir, &tag)
            .map_err(|e| VmBootError::Device(format!("fs device: {e}")))?;
        let fs_arc = Arc::new(Mutex::new(fs_dev));
        let mut fs_mmio = MmioTransport::new(fs_arc);
        let fs_irqfd =
            MacosEventFd::new().map_err(|e| VmBootError::Device(format!("fs irqfd: {e}")))?;
        let irq =
            9 + u32::try_from(i).map_err(|_| VmBootError::Device("too many shared dirs".into()))?;
        vm.register_irqfd(&fs_irqfd, irq)?;
        fs_mmio.set_memory(Arc::clone(&memory));
        fs_mmio.set_irq_evt(Arc::new(fs_irqfd));
        fs_mmio.set_irq_deassert(vm.create_spi_deassert(32 + irq));
        let fs_mmio_arc = Arc::new(Mutex::new(fs_mmio));
        let base =
            0xd000_4000_u64 + u64::from(u32::try_from(i).expect("too many shared dirs")) * 0x1000;
        register_mmio(&mut device_mgr.mmio_bus, base, 0x1000, fs_mmio_arc, "fs")?;
    }

    // 6. Boot config deferred — HVF vCPUs are thread-affine and register state does NOT
    //    persist across destroy/recreate (unlike KVM). The entry_point, fdt_addr, and CPSR
    //    are stored in BootedVmInner and applied by configure_vcpu_boot_regs() on the
    //    vCPU thread in run_vcpu() / run_vcpu_with_handler().

    // 7. Vsock muxer — bridges guest vsock to host UDS
    let muxer = create_vsock_muxer(
        vsock_for_muxer,
        config.guest_cid,
        Arc::clone(&vsock_mmio_arc) as _,
    )?;

    Ok(BootedVm {
        memory,
        device_mgr,
        serial_output,
        kill_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vsock_muxer: Some(muxer),
        vsock_transport: Some(vsock_mmio_arc),
        net_transports: vec![net_mmio_arc],
        #[cfg(target_os = "macos")]
        net_has_pending: Some(net_has_pending),
        inner: BootedVmInner {
            platform,
            vm,
            cpu_init_mode: super::CpuInitMode::Boot {
                entry_point: boot_config.entry_point,
                fdt_addr: boot_config.fdt_addr,
            },
        },
    })
}

// ── Run loop ─────────────────────────────────────────────────────────

/// Internal run loop shared by `run_vcpu` and `run_vcpu_with_handler`.
pub(super) fn run_loop(
    vcpu: &mut <crate::platform::HvfVm as VmOps>::Vcpu,
    vm: &crate::platform::HvfVm,
    kill_flag: &Arc<std::sync::atomic::AtomicBool>,
    handler: &mut dyn ExitHandler,
    vsock_transport: Option<&Arc<Mutex<MmioTransport>>>,
    net_has_pending: Option<&Arc<AtomicBool>>,
    net_transport: Option<&Arc<Mutex<MmioTransport>>>,
) -> Result<(), VcpuError> {
    use crate::platform::VcpuOps;

    // Snapshot IRQ registrations — these are fixed at boot time.
    let irq_regs = vm.irq_registrations_snapshot();
    let mut exit_count: u64 = 0;

    loop {
        if kill_flag.load(std::sync::atomic::Ordering::Acquire) {
            tracing::info!(exit_count, "vCPU exiting (kill flag set)");
            return Ok(());
        }
        match vcpu.run() {
            Ok(exit) => {
                exit_count += 1;

                if let VmExit::MmioRead { addr, size } = &exit {
                    let mut buf = vec![0u8; *size];
                    handler.handle_mmio_read(*addr, &mut buf);
                    vcpu.complete_mmio_read(&buf)
                        .map_err(|e| VcpuError::Run(std::io::Error::other(e)))?;
                }
                // Skip handle_exit for MmioRead — already handled above.
                if !matches!(&exit, VmExit::MmioRead { .. }) {
                    match handler.handle_exit(exit) {
                        Ok(ExitAction::Continue) => {}
                        Ok(ExitAction::Stop) => {
                            tracing::info!(exit_count, "vCPU stopped (guest exit)");
                            return Ok(());
                        }
                        Err(e) => return Err(e),
                    }
                }
                // Poll registered IRQ events and inject pending SPI interrupts.
                // The SPI is level-triggered; the deassert happens when the guest
                // writes InterruptACK in MmioTransport (via irq_deassert callback).
                for &(kq_fd, gsi) in &irq_regs {
                    if crate::platform::poll_kqueue_fd(kq_fd).unwrap_or(false) {
                        vm.gic_set_spi(32 + gsi, true)
                            .map_err(|e| VcpuError::Run(std::io::Error::other(e)))?;
                    }
                }

                if let Some(transport) = vsock_transport
                    && let Ok(t) = transport.lock()
                {
                    let _ = t.process_external_queue(0);
                }

                // Poll net RX: if packets arrived, process the RX queue.
                // After processing, re-arm the flag so poll-based backends
                // (vmnet-helper socketpair) are checked on every vCPU exit.
                // For event-driven backends (direct vmnet), the callback also
                // sets this flag, making the re-arm a harmless no-op.
                if let (Some(flag), Some(transport)) = (net_has_pending, net_transport) {
                    if flag.swap(false, std::sync::atomic::Ordering::Acquire) {
                        if let Ok(t) = transport.lock() {
                            let _ = t.process_external_queue(0);
                        }
                    }
                    flag.store(true, std::sync::atomic::Ordering::Release);
                }
            }
            Err(e) => {
                return Err(VcpuError::Run(std::io::Error::other(e)));
            }
        }
    }
}

// ── Snapshot Restore ───────────────────────────────────────────────

/// Restores a microVM from a snapshot directory on macOS (HVF / aarch64).
///
/// This is the fast-path restore that skips kernel loading and rootfs
/// creation. Memory is restored via `mmap(MAP_PRIVATE)` COW from
/// `memory.bin`, and devices are re-created at the same MMIO addresses.
///
/// vCPU register restore is deferred to `run_vcpu()` via `CpuInitMode::Restore`
/// because HVF vCPUs are thread-affine (registers don't persist across
/// destroy/recreate).
///
/// # Errors
///
/// Returns [`VmBootError`] if any restore step fails.
#[allow(clippy::too_many_lines)]
pub(super) fn restore_macos(
    config: &super::SnapshotRestoreConfig,
) -> Result<BootedVm, super::VmBootError> {
    use crate::boot::aarch64::DRAM_MEM_START;
    use crate::platform::{HvfPlatform, MacosEventFd, Platform};

    let memory_path = config.snapshot_dir.join("memory.bin");
    if !memory_path.exists() {
        return Err(super::VmBootError::Snapshot(
            crate::snapshot::SnapshotError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("snapshot memory file not found: {}", memory_path.display()),
            )),
        ));
    }

    // 1. HVF platform + IRQ chip
    let platform = HvfPlatform::new()?;
    let vm = platform.create_vm()?;
    vm.create_irq_chip()?;
    vm.create_pit()?;

    // 2. Restore guest memory from snapshot via mmap COW (O(1))
    let effective_mib = config.memory_mib.max(super::MIN_MEMORY_MIB);
    let memory_bytes = effective_mib as usize * 1024 * 1024;
    let memory = Arc::new(crate::snapshot::restore_memory(
        &memory_path,
        memory_bytes,
        DRAM_MEM_START,
    )?);
    vm.register_memory(
        super::MEMORY_SLOT,
        DRAM_MEM_START,
        memory_bytes as u64,
        memory.host_addr(),
    )?;

    // 3. Wire devices at the SAME MMIO addresses as boot
    let serial_output = SerialOutput::new();
    let serial_irq: Arc<dyn InterruptEvent> = Arc::new(
        MacosEventFd::new().map_err(|e| super::VmBootError::Device(format!("serial irq: {e}")))?,
    );
    let serial =
        crate::devices::pl011::Pl011::new(Box::new(serial_output.clone()), Arc::clone(&serial_irq));
    vm.register_irqfd(serial_irq.as_ref(), COM1_IRQ)?;
    let serial_arc: Arc<Mutex<dyn crate::devices::bus::BusDevice>> = Arc::new(Mutex::new(serial));

    // Vsock device
    let vsock = VsockDevice::new(u64::from(config.guest_cid));
    let vsock_arc: Arc<Mutex<VsockDevice>> = Arc::new(Mutex::new(vsock));
    let vsock_for_muxer = Arc::clone(&vsock_arc);
    let mut vsock_mmio = MmioTransport::new(vsock_arc);
    let vsock_irqfd =
        MacosEventFd::new().map_err(|e| super::VmBootError::Device(format!("vsock irqfd: {e}")))?;
    vm.register_irqfd(&vsock_irqfd, 6)?;
    let vsock_irq: Arc<dyn InterruptEvent> = Arc::new(vsock_irqfd);
    vsock_mmio.set_memory(Arc::clone(&memory));
    vsock_mmio.set_irq_evt(Arc::clone(&vsock_irq));
    vsock_mmio.set_irq_deassert(vm.create_spi_deassert(32 + 6));
    let vsock_mmio_arc = Arc::new(Mutex::new(vsock_mmio));

    // Net device
    let (net_mmio_arc, net_has_pending) = create_net_device(&vm, &memory)?;

    // RNG device
    let rng_dev =
        RngDevice::new().map_err(|e| super::VmBootError::Device(format!("rng device: {e}")))?;
    let rng_arc = Arc::new(Mutex::new(rng_dev));
    let mut rng_mmio = MmioTransport::new(rng_arc);
    let rng_irqfd =
        MacosEventFd::new().map_err(|e| super::VmBootError::Device(format!("rng irqfd: {e}")))?;
    vm.register_irqfd(&rng_irqfd, 8)?;
    rng_mmio.set_memory(Arc::clone(&memory));
    rng_mmio.set_irq_evt(Arc::new(rng_irqfd));
    rng_mmio.set_irq_deassert(vm.create_spi_deassert(32 + 8));
    let rng_mmio_arc = Arc::new(Mutex::new(rng_mmio));

    // 4. Device manager — same MMIO layout as boot
    let mut device_mgr = DeviceManager::new();
    register_mmio(
        &mut device_mgr.mmio_bus,
        0x0900_0000,
        0x1000,
        serial_arc,
        "serial",
    )?;
    // Skip block device at 0xd000_0000 — snapshot memory has rootfs state.
    register_mmio(
        &mut device_mgr.mmio_bus,
        0xd000_1000,
        0x1000,
        Arc::clone(&vsock_mmio_arc) as _,
        "vsock",
    )?;
    register_mmio(
        &mut device_mgr.mmio_bus,
        0xd000_2000,
        0x1000,
        Arc::clone(&net_mmio_arc) as _,
        "net",
    )?;
    register_mmio(
        &mut device_mgr.mmio_bus,
        0xd000_3000,
        0x1000,
        rng_mmio_arc,
        "rng",
    )?;

    // Virtio-fs devices (same layout as boot)
    for (i, shared_dir) in config.shared_dirs.iter().enumerate() {
        let tag = format!("visor-fs-{i}");
        let fs_dev = FsDevice::new(shared_dir, &tag)
            .map_err(|e| super::VmBootError::Device(format!("fs device: {e}")))?;
        let fs_arc = Arc::new(Mutex::new(fs_dev));
        let mut fs_mmio = MmioTransport::new(fs_arc);
        let fs_irqfd = MacosEventFd::new()
            .map_err(|e| super::VmBootError::Device(format!("fs irqfd: {e}")))?;
        let irq = 9 + u32::try_from(i)
            .map_err(|_| super::VmBootError::Device("too many shared dirs".into()))?;
        vm.register_irqfd(&fs_irqfd, irq)?;
        fs_mmio.set_memory(Arc::clone(&memory));
        fs_mmio.set_irq_evt(Arc::new(fs_irqfd));
        fs_mmio.set_irq_deassert(vm.create_spi_deassert(32 + irq));
        let fs_mmio_arc = Arc::new(Mutex::new(fs_mmio));
        let base = 0xd000_4000_u64
            + u64::from(
                u32::try_from(i)
                    .map_err(|_| super::VmBootError::Device("too many shared dirs".into()))?,
            ) * 0x1000;
        register_mmio(&mut device_mgr.mmio_bus, base, 0x1000, fs_mmio_arc, "fs")?;
    }

    // 5. Vsock muxer
    let muxer = create_vsock_muxer(
        vsock_for_muxer,
        config.guest_cid,
        Arc::clone(&vsock_mmio_arc) as _,
    )?;

    // 6. CPU init mode: Restore (registers will be loaded in run_vcpu from snapshot)
    //    On macOS, HVF vCPU registers don't persist across destroy/recreate,
    //    so we load them in run_vcpu() by reading cpu_state.json.
    //    For now, we use CpuInitMode::Restore which skips configure_vcpu_boot_regs().
    //    The actual register restore from cpu_state.json happens when the vCPU thread
    //    calls snapshot::restore_cpu() before entering the run loop.

    Ok(BootedVm {
        memory,
        device_mgr,
        serial_output,
        kill_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vsock_muxer: Some(muxer),
        vsock_transport: Some(vsock_mmio_arc),
        net_transports: vec![net_mmio_arc],
        #[cfg(target_os = "macos")]
        net_has_pending: Some(net_has_pending),
        inner: BootedVmInner {
            platform,
            vm,
            cpu_init_mode: super::CpuInitMode::Restore,
        },
    })
}
