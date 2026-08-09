//! Linux (KVM / `x86_64`) boot path and run loop.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::devices::block::BlockDevice;
use crate::devices::fs::FsDevice;
use crate::devices::net::NetDevice;
use crate::devices::rng::RngDevice;
use crate::devices::serial::{COM1_IRQ, COM1_PORT_BASE, COM1_PORT_COUNT, SerialDevice};
use crate::devices::vsock::VsockDevice;
use crate::devices::vsock_muxer::VsockMuxer;
use crate::guest_virtualization::{GuestVirtualizationMode, validate_guest_virtualization};
use crate::memory::GuestMemory;
use crate::net::NetworkInterface;
use crate::platform::event::InterruptEvent;
use crate::platform::{KvmPlatform, KvmVm, LinuxEventFd, Platform, VmOps};
use crate::transport::VirtioDevice;
use crate::transport::mmio::MmioTransport;

use super::{
    BootedVm, BootedVmInner, ExitHandler, MEMORY_SLOT, MIN_MEMORY_MIB, SerialOutput, VcpuError,
    VmBootError,
};

/// `x86_64` guest memory starts at physical address 0.
const GUEST_BASE_ADDR: u64 = 0;

/// MMIO layout for virtio devices (Firecracker convention).
const MMIO_BASE: u64 = 0xd000_0000;
const MMIO_SIZE: u64 = 0x1000;
const MMIO_IRQ: u32 = 5;
const ROOTFS_MMIO_BASE: u64 = MMIO_BASE;
const ROOTFS_MMIO_SIZE: u64 = MMIO_SIZE;
const ROOTFS_MMIO_IRQ: u32 = MMIO_IRQ;

/// Offset of `acpi_rsdp_addr` in the Linux `boot_params` struct.
const BP_ACPI_RSDP_ADDR: u64 = 0x70;

pub(super) struct NetworkResources {
    #[allow(dead_code)]
    interface: crate::net::linux::LinuxNetworkInterface,
    #[allow(dead_code)]
    nat: Option<crate::net::linux::LinuxNatHandle>,
}

type DeviceManagerBundle = (
    super::DeviceManager,
    SerialOutput,
    Arc<Mutex<MmioTransport>>,
    VsockMuxer,
    Vec<Arc<Mutex<MmioTransport>>>,
    Vec<NetworkResources>,
);

pub(super) fn boot_linux(config: &super::VmConfig<'_>) -> Result<BootedVm, VmBootError> {
    use crate::boot::x86_64::configure_boot;

    validate_guest_virtualization(config.guest_virtualization)?;
    let (platform, vm) = create_platform_vm()?;
    let memory = create_guest_memory(&vm, config.memory_mib)?;
    let boot_config = configure_boot(&memory, config.kernel_path, config.cmdline)?;
    write_acpi_tables(
        &memory,
        config.vcpus,
        config.shared_dirs.len(),
        config.data_disks.len(),
        effective_networks(config).len(),
    )?;
    let (device_mgr, serial_output, vsock_transport, vsock_muxer, net_transports, networks) =
        create_boot_device_manager(
            &vm,
            &memory,
            config.rootfs_path,
            config.guest_cid,
            &config.shared_dirs,
            &config.data_disks,
            effective_networks(config),
        )?;

    let vcpu = crate::vcpu::Vcpu::new(&vm, 0)?;
    vcpu.configure_regs(platform.kvm(), &boot_config, config.guest_virtualization)?;

    Ok(BootedVm {
        memory,
        device_mgr,
        serial_output,
        kill_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vsock_muxer: Some(vsock_muxer),
        vsock_transport: Some(vsock_transport),
        net_transports,
        inner: BootedVmInner {
            platform,
            vm,
            vcpu,
            networks,
        },
    })
}

/// Internal run loop shared by `run_vcpu` and `run_vcpu_with_handler`.
pub(super) fn run_loop(
    vcpu: &mut crate::vcpu::Vcpu,
    kill_flag: &Arc<std::sync::atomic::AtomicBool>,
    handler: &mut dyn ExitHandler,
    vsock_transport: Option<&Arc<Mutex<MmioTransport>>>,
    net_transports: &[Arc<Mutex<MmioTransport>>],
) -> Result<(), VcpuError> {
    loop {
        if kill_flag.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(());
        }

        let exit = vcpu.run_once_with_handler(handler)?;
        let action = handler.handle_exit(exit)?;
        if action == super::ExitAction::Stop {
            return Ok(());
        }

        if let Some(transport) = vsock_transport
            && let Ok(locked) = transport.lock()
        {
            let _ = locked.process_external_queue(0);
        }

        for transport in net_transports {
            if let Ok(locked) = transport.lock() {
                let _ = locked.process_external_queue(0);
            }
        }
    }
}

// ── Snapshot Restore ───────────────────────────────────────────────

/// Restores a microVM from a snapshot directory on Linux (KVM / `x86_64`).
///
/// This is the fast-path restore that skips kernel loading and rootfs
/// creation. Memory is restored via `mmap(MAP_PRIVATE)` COW from
/// `memory.bin`, and vCPU registers are loaded from `cpu_state.json`.
///
/// The restored KVM vCPU is retained inside [`BootedVm`] so `run_vcpu()`
/// uses the same initialized vCPU rather than recreating slot `0`.
///
/// # Errors
///
/// Returns [`VmBootError`] if any restore step fails.
pub(super) fn restore_linux(
    config: &super::SnapshotRestoreConfig,
) -> Result<BootedVm, VmBootError> {
    validate_guest_virtualization(config.guest_virtualization)?;
    let memory_path = snapshot_memory_path(&config.snapshot_dir)?;
    let (platform, vm) = create_platform_vm()?;
    let memory = restore_guest_memory(&vm, &memory_path, config.memory_mib)?;
    let rootfs_path = snapshot_rootfs_path(&config.snapshot_dir)?;
    let (device_mgr, serial_output, vsock_transport, vsock_muxer, net_transports, networks) =
        create_restore_device_manager(
            &vm,
            &memory,
            &rootfs_path,
            config.guest_cid,
            &config.shared_dirs,
            &config.data_disks,
            effective_restore_networks(config),
        )?;
    let vcpu = restore_snapshot_vcpu(
        &vm,
        &config.snapshot_dir,
        platform.kvm(),
        config.guest_virtualization,
    )?;

    Ok(BootedVm {
        memory,
        device_mgr,
        serial_output,
        kill_flag: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        vsock_muxer: Some(vsock_muxer),
        vsock_transport: Some(vsock_transport),
        net_transports,
        inner: BootedVmInner {
            platform,
            vm,
            vcpu,
            networks,
        },
    })
}

fn create_platform_vm() -> Result<(KvmPlatform, KvmVm), VmBootError> {
    let platform = KvmPlatform::new()?;
    let vm = platform.create_vm()?;
    vm.create_irq_chip()?;
    vm.create_pit()?;
    Ok((platform, vm))
}

fn create_guest_memory(vm: &KvmVm, memory_mib: u32) -> Result<Arc<GuestMemory>, VmBootError> {
    let memory_bytes = memory_bytes(memory_mib);
    let memory = Arc::new(GuestMemory::new(memory_bytes, GUEST_BASE_ADDR)?);
    register_guest_memory(vm, &memory, memory_bytes)?;
    Ok(memory)
}

fn restore_guest_memory(
    vm: &KvmVm,
    memory_path: &Path,
    memory_mib: u32,
) -> Result<Arc<GuestMemory>, VmBootError> {
    let memory_bytes = memory_bytes(memory_mib);
    let memory = Arc::new(crate::snapshot::restore_memory(
        memory_path,
        memory_bytes,
        GUEST_BASE_ADDR,
    )?);
    register_guest_memory(vm, &memory, memory_bytes)?;
    Ok(memory)
}

fn register_guest_memory(
    vm: &KvmVm,
    memory: &GuestMemory,
    memory_bytes: usize,
) -> Result<(), VmBootError> {
    vm.register_memory(
        MEMORY_SLOT,
        GUEST_BASE_ADDR,
        memory_bytes as u64,
        memory.host_addr(),
    )?;
    Ok(())
}

fn memory_bytes(memory_mib: u32) -> usize {
    memory_mib.max(MIN_MEMORY_MIB) as usize * 1024 * 1024
}

fn write_acpi_tables(
    memory: &GuestMemory,
    requested_vcpus: u32,
    shared_dir_count: usize,
    data_disk_count: usize,
    network_count: usize,
) -> Result<(), VmBootError> {
    let mmio_devices = build_acpi_mmio_devices(shared_dir_count, data_disk_count, network_count)?;
    let rsdp_addr = crate::acpi::create_acpi_tables(
        memory,
        effective_linux_vcpus(requested_vcpus),
        &mmio_devices,
    )
    .map_err(|e| VmBootError::Device(format!("ACPI tables: {e}")))?;
    memory.write_bytes(
        crate::boot::ZERO_PAGE_START + BP_ACPI_RSDP_ADDR,
        &rsdp_addr.to_le_bytes(),
    )?;
    Ok(())
}

fn effective_linux_vcpus(requested_vcpus: u32) -> u8 {
    let effective_vcpus = 1;
    if requested_vcpus > u32::from(effective_vcpus) {
        tracing::warn!(
            requested_vcpus,
            effective_vcpus,
            "multi-vCPU not yet supported, capping to 1"
        );
    }
    effective_vcpus
}

fn build_acpi_mmio_devices(
    shared_dir_count: usize,
    data_disk_count: usize,
    network_count: usize,
) -> Result<Vec<crate::acpi::MmioDeviceInfo>, VmBootError> {
    let mut mmio_devices = vec![crate::acpi::MmioDeviceInfo::new(
        ROOTFS_MMIO_BASE,
        ROOTFS_MMIO_SIZE,
        ROOTFS_MMIO_IRQ,
    )];

    for index in 0..data_disk_count {
        let (base, irq) = extra_block_mmio_location(index)?;
        mmio_devices.push(crate::acpi::MmioDeviceInfo::new(base, MMIO_SIZE, irq));
    }

    let (vsock_base, vsock_irq) = vsock_mmio_location(data_disk_count)?;
    mmio_devices.push(crate::acpi::MmioDeviceInfo::new(
        vsock_base, MMIO_SIZE, vsock_irq,
    ));
    let (rng_base, rng_irq) = rng_mmio_location(data_disk_count)?;
    mmio_devices.push(crate::acpi::MmioDeviceInfo::new(
        rng_base, MMIO_SIZE, rng_irq,
    ));

    for index in 0..network_count {
        let (net_base, net_irq) = net_mmio_location(index, data_disk_count)?;
        mmio_devices.push(crate::acpi::MmioDeviceInfo::new(
            net_base, MMIO_SIZE, net_irq,
        ));
    }

    for index in 0..shared_dir_count {
        let (base, irq) = fs_mmio_location(index, data_disk_count, network_count)?;
        mmio_devices.push(crate::acpi::MmioDeviceInfo::new(base, MMIO_SIZE, irq));
    }

    Ok(mmio_devices)
}

fn extra_block_mmio_location(index: usize) -> Result<(u64, u32), VmBootError> {
    let slot = index
        .checked_add(1)
        .ok_or_else(|| VmBootError::Device("too many data disks".into()))?;
    mmio_slot_location(slot, "too many data disks")
}

fn vsock_mmio_location(data_disk_count: usize) -> Result<(u64, u32), VmBootError> {
    let slot = data_disk_count
        .checked_add(1)
        .ok_or_else(|| VmBootError::Device("too many data disks".into()))?;
    mmio_slot_location(slot, "too many data disks")
}

fn rng_mmio_location(data_disk_count: usize) -> Result<(u64, u32), VmBootError> {
    let slot = data_disk_count
        .checked_add(2)
        .ok_or_else(|| VmBootError::Device("too many data disks".into()))?;
    mmio_slot_location(slot, "too many data disks")
}

fn net_mmio_location(index: usize, data_disk_count: usize) -> Result<(u64, u32), VmBootError> {
    let slot = data_disk_count
        .checked_add(3)
        .and_then(|base| base.checked_add(index))
        .ok_or_else(|| VmBootError::Device("too many data disks".into()))?;
    mmio_slot_location(slot, "too many data disks")
}

fn fs_mmio_location(
    index: usize,
    data_disk_count: usize,
    network_count: usize,
) -> Result<(u64, u32), VmBootError> {
    let base_slot = data_disk_count
        .checked_add(3)
        .and_then(|base| base.checked_add(network_count))
        .ok_or_else(|| VmBootError::Device("too many shared dirs".into()))?;
    let slot = base_slot
        .checked_add(index)
        .ok_or_else(|| VmBootError::Device("too many shared dirs".into()))?;
    mmio_slot_location(slot, "too many shared dirs")
}

fn mmio_slot_location(slot_index: usize, message: &str) -> Result<(u64, u32), VmBootError> {
    let slot = u32::try_from(slot_index).map_err(|_| VmBootError::Device(message.to_owned()))?;
    Ok((MMIO_BASE + u64::from(slot) * MMIO_SIZE, MMIO_IRQ + slot))
}

fn create_boot_device_manager(
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    rootfs_path: &Path,
    guest_cid: u32,
    shared_dirs: &[PathBuf],
    data_disks: &[super::DataDiskConfig],
    networks: &[super::NetworkConfig],
) -> Result<DeviceManagerBundle, VmBootError> {
    let (serial_output, serial) = create_serial_device(vm)?;
    let rootfs_mmio =
        create_block_transport(vm, memory, rootfs_path, false, ROOTFS_MMIO_IRQ, "rootfs")?;
    let (vsock_mmio, vsock_muxer) = create_vsock_device(vm, memory, guest_cid, data_disks.len())?;
    let rng_mmio = create_rng_transport(vm, memory, data_disks.len())?;
    let (net_mmio, network_resources) =
        create_network_transports(vm, memory, guest_cid, networks, data_disks.len())?;
    let mut device_mgr =
        create_common_device_manager(serial, &vsock_mmio, rng_mmio, &net_mmio, data_disks.len())?;

    register_mmio_transport(
        &mut device_mgr,
        ROOTFS_MMIO_BASE,
        ROOTFS_MMIO_SIZE,
        rootfs_mmio,
        "block MMIO",
    )?;
    register_data_disks(&mut device_mgr, vm, memory, data_disks)?;
    register_shared_fs_devices(
        &mut device_mgr,
        vm,
        memory,
        shared_dirs,
        data_disks.len(),
        networks.len(),
    )?;

    Ok((
        device_mgr,
        serial_output,
        vsock_mmio,
        vsock_muxer,
        net_mmio,
        network_resources,
    ))
}

fn create_restore_device_manager(
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    rootfs_path: &Path,
    guest_cid: u32,
    shared_dirs: &[PathBuf],
    data_disks: &[super::DataDiskConfig],
    networks: &[super::NetworkConfig],
) -> Result<DeviceManagerBundle, VmBootError> {
    let (serial_output, serial) = create_serial_device(vm)?;
    let rootfs_mmio =
        create_block_transport(vm, memory, rootfs_path, false, ROOTFS_MMIO_IRQ, "rootfs")?;
    let (vsock_mmio, vsock_muxer) = create_vsock_device(vm, memory, guest_cid, data_disks.len())?;
    let rng_mmio = create_rng_transport(vm, memory, data_disks.len())?;
    let (net_mmio, network_resources) =
        create_network_transports(vm, memory, guest_cid, networks, data_disks.len())?;
    let mut device_mgr =
        create_common_device_manager(serial, &vsock_mmio, rng_mmio, &net_mmio, data_disks.len())?;

    register_mmio_transport(
        &mut device_mgr,
        ROOTFS_MMIO_BASE,
        ROOTFS_MMIO_SIZE,
        rootfs_mmio,
        "block MMIO",
    )?;
    register_data_disks(&mut device_mgr, vm, memory, data_disks)?;
    register_shared_fs_devices(
        &mut device_mgr,
        vm,
        memory,
        shared_dirs,
        data_disks.len(),
        networks.len(),
    )?;

    Ok((
        device_mgr,
        serial_output,
        vsock_mmio,
        vsock_muxer,
        net_mmio,
        network_resources,
    ))
}

fn create_serial_device(
    vm: &KvmVm,
) -> Result<(SerialOutput, Arc<Mutex<SerialDevice>>), VmBootError> {
    let serial_output = SerialOutput::new();
    let serial_irq: Arc<dyn InterruptEvent> =
        Arc::new(LinuxEventFd::new().map_err(|e| VmBootError::Device(format!("serial irq: {e}")))?);
    let serial = SerialDevice::new(Box::new(serial_output.clone()), Arc::clone(&serial_irq));
    vm.register_irqfd(serial_irq.as_ref(), COM1_IRQ)?;

    Ok((serial_output, Arc::new(Mutex::new(serial))))
}

fn create_common_device_manager(
    serial: Arc<Mutex<SerialDevice>>,
    vsock_mmio: &Arc<Mutex<MmioTransport>>,
    rng_mmio: Arc<Mutex<MmioTransport>>,
    net_mmio: &[Arc<Mutex<MmioTransport>>],
    data_disk_count: usize,
) -> Result<super::DeviceManager, VmBootError> {
    let mut device_mgr = super::DeviceManager::new();
    device_mgr
        .pio_bus
        .register(u64::from(COM1_PORT_BASE), COM1_PORT_COUNT, serial)
        .map_err(|e| VmBootError::Device(format!("register serial PIO: {e}")))?;
    let (vsock_base, _) = vsock_mmio_location(data_disk_count)?;
    register_mmio_transport(
        &mut device_mgr,
        vsock_base,
        MMIO_SIZE,
        Arc::clone(vsock_mmio),
        "vsock MMIO",
    )?;
    let (rng_base, _) = rng_mmio_location(data_disk_count)?;
    register_mmio_transport(&mut device_mgr, rng_base, MMIO_SIZE, rng_mmio, "rng MMIO")?;
    for (index, net_transport) in net_mmio.iter().enumerate() {
        let (net_base, _) = net_mmio_location(index, data_disk_count)?;
        register_mmio_transport(
            &mut device_mgr,
            net_base,
            MMIO_SIZE,
            Arc::clone(net_transport),
            "net MMIO",
        )?;
    }
    Ok(device_mgr)
}

fn create_vsock_device(
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    guest_cid: u32,
    data_disk_count: usize,
) -> Result<(Arc<Mutex<MmioTransport>>, VsockMuxer), VmBootError> {
    let (_, irq) = vsock_mmio_location(data_disk_count)?;
    let vsock = Arc::new(Mutex::new(VsockDevice::new(u64::from(guest_cid))));
    let tx_notify = {
        let device = vsock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        device.tx_notify()
    };

    let transport_device: Arc<Mutex<dyn VirtioDevice>> = vsock.clone();
    let mut transport = MmioTransport::new(transport_device);
    let irqfd =
        LinuxEventFd::new().map_err(|e| VmBootError::Device(format!("vsock irqfd: {e}")))?;
    let irq_evt: Arc<dyn InterruptEvent> = Arc::new(irqfd);
    vm.register_irqfd(irq_evt.as_ref(), irq)?;
    transport.set_memory(Arc::clone(memory));
    transport.set_irq_evt(Arc::clone(&irq_evt));
    let transport = Arc::new(Mutex::new(transport));
    let rx_transport = Arc::clone(&transport);
    let rx_kick: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        if let Ok(transport) = rx_transport.lock() {
            let _ = transport.process_external_queue(0);
        }
    });
    let muxer = VsockMuxer::new(
        vsock,
        u64::from(guest_cid),
        crate::comms::muxer::MuxerCommsBackend::configured_socket_dir(),
        tx_notify,
        rx_kick,
    )
    .map_err(|e| VmBootError::Device(format!("vsock muxer: {e}")))?;

    Ok((transport, muxer))
}

fn create_rng_transport(
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    data_disk_count: usize,
) -> Result<Arc<Mutex<MmioTransport>>, VmBootError> {
    let (_, irq) = rng_mmio_location(data_disk_count)?;
    let rng = Arc::new(Mutex::new(
        RngDevice::new().map_err(|e| VmBootError::Device(format!("rng device: {e}")))?,
    ));
    create_mmio_transport(vm, memory, rng, irq, "rng")
}

fn create_block_transport(
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    disk_path: &Path,
    read_only: bool,
    irq: u32,
    label: &str,
) -> Result<Arc<Mutex<MmioTransport>>, VmBootError> {
    let block = Arc::new(Mutex::new(
        BlockDevice::new(disk_path, read_only)
            .map_err(|e| VmBootError::Device(format!("{label} block device: {e}")))?,
    ));
    create_mmio_transport(vm, memory, block, irq, label)
}

fn create_network_transports(
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    guest_cid: u32,
    configs: &[super::NetworkConfig],
    data_disk_count: usize,
) -> Result<(Vec<Arc<Mutex<MmioTransport>>>, Vec<NetworkResources>), VmBootError> {
    let mut transports = Vec::with_capacity(configs.len());
    let mut resources = Vec::with_capacity(configs.len());

    for (index, config) in configs.iter().enumerate() {
        let (transport, attachment_resources) =
            create_network_transport(vm, memory, guest_cid, config, data_disk_count, index)?;
        transports.push(transport);
        resources.push(attachment_resources);
    }

    Ok((transports, resources))
}

fn create_network_transport(
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    guest_cid: u32,
    config: &super::NetworkConfig,
    data_disk_count: usize,
    attachment_index: usize,
) -> Result<(Arc<Mutex<MmioTransport>>, NetworkResources), VmBootError> {
    use crate::net::{NetworkBackend as _, PlatformNetworkBackend};

    let (_, irq) = net_mmio_location(attachment_index, data_disk_count)?;
    let net_backend = PlatformNetworkBackend::new();
    let mut interface_config = crate::net::InterfaceConfig::new(&config.interface_name)
        .with_ip(config.gateway_ip)
        .with_netmask(config.netmask);
    if let Some(bridge_name) = config.bridge_name.as_deref() {
        interface_config = interface_config.with_bridge(bridge_name);
    }
    let interface = net_backend
        .create_interface(&interface_config)
        .map_err(|e| VmBootError::Device(format!("net interface: {e}")))?;
    let nat = if let Some(bridge_name) = config.bridge_name.as_deref() {
        crate::net::linux::ensure_shared_bridge_nat(bridge_name, &config.subnet_cidr())
            .map_err(|e| VmBootError::Device(format!("net shared bridge nat: {e}")))?;
        None
    } else {
        let nat_config = crate::net::NatConfig::new(interface.name(), &config.subnet_cidr());
        let nat = net_backend
            .setup_nat(&nat_config)
            .map_err(|e| VmBootError::Device(format!("net nat: {e}")))?;
        Some(nat)
    };
    let packet_io = crate::net::linux::TapPacketIo::open(interface.name())
        .map_err(|e| VmBootError::Device(format!("tap packet io: {e}")))?;
    let net_device = Arc::new(Mutex::new(NetDevice::with_packet_io(
        guest_mac(guest_cid, attachment_index),
        Box::new(packet_io),
    )));
    let net_transport = create_mmio_transport(vm, memory, net_device, irq, "net")?;

    Ok((net_transport, NetworkResources { interface, nat }))
}

fn create_mmio_transport<D>(
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    device: Arc<Mutex<D>>,
    irq: u32,
    label: &str,
) -> Result<Arc<Mutex<MmioTransport>>, VmBootError>
where
    D: VirtioDevice + 'static,
{
    let mut transport = MmioTransport::new(device);
    let irqfd =
        LinuxEventFd::new().map_err(|e| VmBootError::Device(format!("{label} irqfd: {e}")))?;
    vm.register_irqfd(&irqfd, irq)?;
    transport.set_memory(Arc::clone(memory));
    transport.set_irq_evt(Arc::new(irqfd));
    Ok(Arc::new(Mutex::new(transport)))
}

fn register_shared_fs_devices(
    device_mgr: &mut super::DeviceManager,
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    shared_dirs: &[PathBuf],
    data_disk_count: usize,
    network_count: usize,
) -> Result<(), VmBootError> {
    for (index, shared_dir) in shared_dirs.iter().enumerate() {
        let (base, irq) = fs_mmio_location(index, data_disk_count, network_count)?;
        let tag = format!("visor-fs-{index}");
        let fs = Arc::new(Mutex::new(
            FsDevice::new(shared_dir, &tag)
                .map_err(|e| VmBootError::Device(format!("fs device: {e}")))?,
        ));
        let transport = create_mmio_transport(vm, memory, fs, irq, "fs")?;
        register_mmio_transport(device_mgr, base, MMIO_SIZE, transport, "fs MMIO")?;
    }

    Ok(())
}

fn effective_networks<'a>(config: &'a super::VmConfig<'_>) -> &'a [super::NetworkConfig] {
    if !config.networks.is_empty() {
        config.networks.as_slice()
    } else {
        config.network.as_ref().map_or(&[], std::slice::from_ref)
    }
}

fn effective_restore_networks(config: &super::SnapshotRestoreConfig) -> &[super::NetworkConfig] {
    if !config.networks.is_empty() {
        config.networks.as_slice()
    } else {
        config.network.as_ref().map_or(&[], std::slice::from_ref)
    }
}

fn register_data_disks(
    device_mgr: &mut super::DeviceManager,
    vm: &KvmVm,
    memory: &Arc<GuestMemory>,
    data_disks: &[super::DataDiskConfig],
) -> Result<(), VmBootError> {
    for (index, disk) in data_disks.iter().enumerate() {
        let (base, irq) = extra_block_mmio_location(index)?;
        let transport =
            create_block_transport(vm, memory, &disk.path, disk.read_only, irq, "data disk")?;
        register_mmio_transport(device_mgr, base, MMIO_SIZE, transport, "data disk MMIO")?;
    }

    Ok(())
}

fn guest_mac(guest_cid: u32, attachment_index: usize) -> [u8; 6] {
    let attachment = u8::try_from(attachment_index).unwrap_or(u8::MAX);
    [
        0x02,
        0x56,
        attachment,
        ((guest_cid >> 16) & 0xff) as u8,
        ((guest_cid >> 8) & 0xff) as u8,
        (guest_cid & 0xff) as u8,
    ]
}

fn register_mmio_transport(
    device_mgr: &mut super::DeviceManager,
    base: u64,
    size: u64,
    transport: Arc<Mutex<MmioTransport>>,
    label: &str,
) -> Result<(), VmBootError> {
    device_mgr
        .mmio_bus
        .register(base, size, transport)
        .map_err(|e| VmBootError::Device(format!("register {label}: {e}")))?;
    Ok(())
}

fn snapshot_memory_path(snapshot_dir: &Path) -> Result<PathBuf, VmBootError> {
    let memory_path = snapshot_dir.join("memory.bin");
    if memory_path.exists() {
        Ok(memory_path)
    } else {
        Err(VmBootError::Snapshot(crate::snapshot::SnapshotError::Io(
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("snapshot memory file not found: {}", memory_path.display()),
            ),
        )))
    }
}

fn snapshot_rootfs_path(snapshot_dir: &Path) -> Result<PathBuf, VmBootError> {
    let rootfs_path = snapshot_dir.join("rootfs.ext4");
    if rootfs_path.exists() {
        Ok(rootfs_path)
    } else {
        Err(VmBootError::Snapshot(crate::snapshot::SnapshotError::Io(
            std::io::Error::new(
                std::io::ErrorKind::NotFound,
                format!("snapshot rootfs file not found: {}", rootfs_path.display()),
            ),
        )))
    }
}

fn restore_snapshot_vcpu(
    vm: &KvmVm,
    snapshot_dir: &Path,
    kvm: &kvm_ioctls::Kvm,
    guest_virtualization: GuestVirtualizationMode,
) -> Result<crate::vcpu::Vcpu, VmBootError> {
    let cpu_path = snapshot_dir.join("cpu_state.json");
    let vcpu = crate::vcpu::Vcpu::new(vm, 0)?;
    vcpu.configure_cpuid(kvm, guest_virtualization)?;

    if cpu_path.exists() {
        let cpu_json =
            std::fs::read_to_string(&cpu_path).map_err(crate::snapshot::SnapshotError::Io)?;
        let cpu_snap = crate::snapshot::deserialize_cpu_state(&cpu_json)?;
        crate::snapshot::restore_cpu(vcpu.fd(), &cpu_snap)?;
    }

    Ok(vcpu)
}

#[cfg(test)]
#[path = "linux_test.rs"]
mod tests;
