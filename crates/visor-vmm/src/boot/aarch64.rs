//! `aarch64` ARM64 Linux boot protocol setup.
//!
//! Implements the ARM64 Linux boot protocol: loads a kernel Image into guest
//! memory, generates a Flattened Device Tree (FDT) describing the virtual
//! hardware, and returns a [`BootConfig`] with the vCPU entry point and FDT
//! address.
//!
//! # Boot Protocol
//!
//! The ARM64 Linux kernel expects:
//! - Kernel loaded as an ARM64 Image at `DRAM_MEM_START + text_offset`
//! - FDT blob placed at a 2 MiB-aligned address near the end of RAM
//! - vCPU 0: `PC = entry_point`, `X0 = fdt_addr`, `PSTATE = 0x3C5`
//!
//! # Boot Memory Map
//!
//! ```text
//! 0x0800_0000  GICv3 distributor MMIO (typical)
//! 0x080A_0000  GICv3 redistributor MMIO (typical)
//! 0x8000_0000  DRAM start (2 GiB)
//! 0x8020_0000  Kernel Image (DRAM + text_offset, default 2 MiB)
//! end - 2 MiB  FDT blob (2 MiB-aligned, up to FDT_MAX_SIZE)
//! ```

use std::path::Path;

use super::{BootConfig, BootError, mmap_file};
use crate::memory::GuestMemory;

// ── Memory Layout Constants ──────────────────────────────────────────────

/// Start of DRAM in the guest physical address space.
///
/// ARM64 convention places DRAM at 2 GiB (`0x8000_0000`). This matches
/// QEMU's virt machine and Firecracker's layout.
pub const DRAM_MEM_START: u64 = 0x8000_0000;

/// Default offset from DRAM base where the kernel Image is loaded.
///
/// Used when the Image header's `text_offset` field is zero.
/// Matches the Linux ARM64 boot protocol default (2 MiB).
pub const KERNEL_LOAD_OFFSET: u64 = 0x0020_0000;

/// Maximum size reserved for the Flattened Device Tree blob (2 MiB).
pub const FDT_MAX_SIZE: usize = 0x0020_0000;

/// Maximum kernel command line length in bytes (including NUL terminator).
pub const CMDLINE_MAX_SIZE: usize = 2048;

/// Initial PSTATE value for the boot vCPU.
///
/// D, A, I, F exception masks set (all exceptions masked) + `EL1h` mode.
/// See ARM Architecture Reference Manual § D1.6.4.
pub const PSTATE_FAULT_BITS_64: u64 = 0x3C5;

// ── ARM64 Image Header ───────────────────────────────────────────────────

/// ARM64 Image magic number (`"ARM\x64"` as little-endian `u32`).
const ARM64_IMAGE_MAGIC: u32 = 0x644d_5241;

/// Offset of the magic field in the ARM64 Image header.
const ARM64_IMAGE_MAGIC_OFFSET: usize = 0x38;

/// Offset of `text_offset` in the Image header.
const ARM64_IMAGE_TEXT_OFFSET_OFF: usize = 0x08;

/// Minimum ARM64 Image header size (64 bytes).
const ARM64_IMAGE_HEADER_SIZE: usize = 64;

// ── FDT Constants ────────────────────────────────────────────────────────

/// Phandle value for the GIC interrupt controller node.
const GIC_PHANDLE: u32 = 1;

/// FDT interrupt type: PPI (Private Peripheral Interrupt).
const GIC_FDT_IRQ_TYPE_PPI: u32 = 1;

/// FDT interrupt type: SPI (Shared Peripheral Interrupt).
const GIC_FDT_IRQ_TYPE_SPI: u32 = 0;
/// Interrupt trigger: level-sensitive, active-high.
const IRQ_TYPE_LEVEL_HI: u32 = 4;

/// Phandle value for the fixed clock node (used by PL011 UART).
const CLOCK_PHANDLE: u32 = 2;

/// PL011 UART MMIO base address.
///
/// Matches QEMU's virt machine layout (`pl011@9000000`).
pub const UART_BASE: u64 = 0x0900_0000;

/// PL011 UART MMIO region size (4 KiB).
pub const UART_SIZE: u64 = 0x1000;

/// GSI for the PL011 UART interrupt (SPI 4, hardware IRQ 36).
pub const UART_GSI: u32 = 4;

/// Fixed clock frequency for the PL011 UART (24 MHz).
const UART_CLOCK_FREQ: u32 = 24_000_000;

// ── Types ────────────────────────────────────────────────────────────────

/// Configuration for FDT generation.
///
/// Describes the virtual hardware topology that the FDT will advertise to
/// the guest kernel: memory size, CPU count, command line, and GIC addresses.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FdtConfig<'a> {
    /// Total guest RAM size in bytes.
    pub memory_size: u64,
    /// Number of virtual CPUs.
    pub num_cpus: u32,
    /// Kernel command line string.
    pub cmdline: &'a str,
    /// GIC distributor MMIO base address.
    pub gic_dist_addr: u64,
    /// GIC distributor MMIO region size.
    pub gic_dist_size: u64,
    /// GIC redistributor MMIO base address.
    pub gic_redist_addr: u64,
    /// GIC redistributor MMIO region size.
    pub gic_redist_size: u64,
    /// Virtio-mmio devices to advertise in the FDT.
    ///
    /// Each entry produces a `virtio_mmio@{addr}` node describing a
    /// virtio-mmio transport region with its SPI interrupt.
    pub mmio_devices: &'a [FdtMmioDevice],
}

/// Descriptor for a virtio-mmio device to include in the FDT.
///
/// Each entry produces a `virtio_mmio@{addr}` node in the device tree
/// with `compatible = "virtio,mmio"`, the MMIO region, and SPI interrupt.
#[derive(Debug, Clone, Copy)]
#[non_exhaustive]
pub struct FdtMmioDevice {
    /// Guest physical base address of the MMIO region.
    pub base_addr: u64,
    /// Size of the MMIO region in bytes.
    pub size: u64,
    /// GSI (Global System Interrupt) number for this device.
    ///
    /// Mapped to SPI number `gsi + 32` in the FDT interrupt property.
    pub gsi: u32,
}

impl FdtMmioDevice {
    /// Creates a new FDT MMIO device descriptor.
    #[must_use]
    pub const fn new(base_addr: u64, size: u64, gsi: u32) -> Self {
        Self {
            base_addr,
            size,
            gsi,
        }
    }
}

// ── Public API ───────────────────────────────────────────────────────────

/// Sets up everything needed to boot an ARM64 Linux kernel.
///
/// 1. Memory-maps and validates the kernel Image file
/// 2. Loads the Image into guest memory at `DRAM_MEM_START + text_offset`
/// 3. Generates an FDT describing the virtual hardware
/// 4. Writes the FDT blob to guest memory
///
/// Returns a [`BootConfig`] with the entry point and FDT address.
///
/// # Errors
///
/// Returns [`BootError`] if the kernel file cannot be read, is not a valid
/// ARM64 Image, exceeds guest memory, or if FDT generation fails.
pub fn configure_boot(
    memory: &GuestMemory,
    kernel_path: &Path,
    fdt_config: &FdtConfig<'_>,
) -> Result<BootConfig, BootError> {
    let kernel_data = mmap_file(kernel_path)?;
    let entry_point = load_arm64_image(memory, &kernel_data)?;

    let fdt_blob = create_fdt(fdt_config)?;
    let fdt_addr = get_fdt_addr(fdt_config.memory_size);

    memory.write_bytes(fdt_addr, &fdt_blob)?;

    Ok(BootConfig {
        entry_point,
        fdt_addr,
    })
}

/// Computes the guest physical address where the FDT should be placed.
///
/// Places the FDT at end of DRAM minus [`FDT_MAX_SIZE`], aligned down
/// to a 2 MiB boundary. The caller must ensure `memory_size > FDT_MAX_SIZE`.
#[must_use]
pub fn get_fdt_addr(memory_size: u64) -> u64 {
    let fdt_offset = memory_size.saturating_sub(FDT_MAX_SIZE as u64);
    // Align down to 2 MiB boundary
    (DRAM_MEM_START + fdt_offset) & !(0x20_0000 - 1)
}

/// Generates a Flattened Device Tree blob describing the virtual hardware.
///
/// The FDT contains:
/// - Root node (`compatible = "linux,dummy-virt"`)
/// - CPU nodes (one per vCPU, `compatible = "arm,arm-v8"`)
/// - Memory node
/// - Chosen node (with `bootargs`)
/// - `GICv3` interrupt controller
/// - Timer (`arm,armv8-timer`)
/// - PSCI (`arm,psci-1.0` with `arm,psci-0.2` fallback)
/// - Virtio-mmio device nodes (from [`FdtConfig::mmio_devices`])
/// - PL011 UART (`pl011@9000000`) with fixed clock
/// # Errors
///
/// Returns [`BootError::Fdt`] if FDT generation fails.
pub fn create_fdt(config: &FdtConfig<'_>) -> Result<Vec<u8>, BootError> {
    let mut fdt = vm_fdt::FdtWriter::new().map_err(|e| BootError::Fdt(e.to_string()))?;

    let root = fdt
        .begin_node("")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string("compatible", "linux,dummy-virt")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("#address-cells", 2)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("#size-cells", 2)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("interrupt-parent", GIC_PHANDLE)
        .map_err(|e| BootError::Fdt(e.to_string()))?;

    create_cpu_nodes(&mut fdt, config.num_cpus)?;
    create_memory_node(&mut fdt, config.memory_size)?;
    create_chosen_node(&mut fdt, config.cmdline)?;
    create_gic_node(
        &mut fdt,
        config.gic_dist_addr,
        config.gic_dist_size,
        config.gic_redist_addr,
        config.gic_redist_size,
    )?;
    create_timer_node(&mut fdt)?;
    create_psci_node(&mut fdt)?;
    create_virtio_mmio_nodes(&mut fdt, config.mmio_devices)?;

    create_clock_node(&mut fdt)?;
    create_uart_node(&mut fdt)?;
    fdt.end_node(root)
        .map_err(|e| BootError::Fdt(e.to_string()))?;

    fdt.finish().map_err(|e| BootError::Fdt(e.to_string()))
}

// ── ARM64 Image Loading ──────────────────────────────────────────────────

/// Validates an ARM64 Image header and loads the kernel into guest memory.
///
/// Returns the kernel entry point address (load address).
///
/// # Errors
///
/// Returns [`BootError::InvalidImage`] if the data is not a valid ARM64
/// Image, or [`BootError::SegmentOutOfBounds`] if the image exceeds guest
/// memory.
fn load_arm64_image(memory: &GuestMemory, data: &[u8]) -> Result<u64, BootError> {
    if data.len() < ARM64_IMAGE_HEADER_SIZE {
        return Err(BootError::InvalidImage {
            reason: "file too small for ARM64 Image header",
        });
    }

    // Validate magic at offset 0x38
    let magic = u32::from_le_bytes(
        data[ARM64_IMAGE_MAGIC_OFFSET..ARM64_IMAGE_MAGIC_OFFSET + 4]
            .try_into()
            .map_err(|_| BootError::InvalidImage {
                reason: "truncated magic field",
            })?,
    );
    if magic != ARM64_IMAGE_MAGIC {
        return Err(BootError::InvalidImage {
            reason: "bad ARM64 Image magic",
        });
    }

    // Read text_offset: offset from DRAM start where kernel expects to be loaded
    let text_offset = u64::from_le_bytes(
        data[ARM64_IMAGE_TEXT_OFFSET_OFF..ARM64_IMAGE_TEXT_OFFSET_OFF + 8]
            .try_into()
            .map_err(|_| BootError::InvalidImage {
                reason: "truncated text_offset",
            })?,
    );

    // Use text_offset if non-zero, otherwise default KERNEL_LOAD_OFFSET
    let load_offset = if text_offset != 0 {
        text_offset
    } else {
        KERNEL_LOAD_OFFSET
    };
    let load_addr = DRAM_MEM_START + load_offset;

    // Bounds check: file data must fit in guest memory
    let write_end =
        load_addr
            .checked_add(data.len() as u64)
            .ok_or(BootError::SegmentOutOfBounds {
                addr: load_addr,
                size: data.len() as u64,
            })?;
    let mem_end = memory.guest_base() + memory.size() as u64;
    if write_end > mem_end {
        return Err(BootError::SegmentOutOfBounds {
            addr: load_addr,
            size: data.len() as u64,
        });
    }

    // Copy the entire Image into guest memory
    memory.write_bytes(load_addr, data)?;

    // ARM64 Image starts executing at its load address
    Ok(load_addr)
}

// ── FDT Node Helpers ─────────────────────────────────────────────────────

/// Creates CPU nodes in the FDT.
///
/// Structure: `cpus { #address-cells=2; #size-cells=0; cpu@N { ... } }`
fn create_cpu_nodes(fdt: &mut vm_fdt::FdtWriter, num_cpus: u32) -> Result<(), BootError> {
    let cpus = fdt
        .begin_node("cpus")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("#address-cells", 2)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("#size-cells", 0)
        .map_err(|e| BootError::Fdt(e.to_string()))?;

    for i in 0..num_cpus {
        let cpu_name = format!("cpu@{i}");
        let cpu = fdt
            .begin_node(&cpu_name)
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        fdt.property_string("device_type", "cpu")
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        fdt.property_string("compatible", "arm,arm-v8")
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        fdt.property_string("enable-method", "psci")
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        fdt.property_u64("reg", u64::from(i))
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        fdt.end_node(cpu)
            .map_err(|e| BootError::Fdt(e.to_string()))?;
    }

    fdt.end_node(cpus)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    Ok(())
}

/// Creates the memory node in the FDT.
fn create_memory_node(fdt: &mut vm_fdt::FdtWriter, memory_size: u64) -> Result<(), BootError> {
    let mem_name = format!("memory@{DRAM_MEM_START:x}");
    let mem = fdt
        .begin_node(&mem_name)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string("device_type", "memory")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_array_u64("reg", &[DRAM_MEM_START, memory_size])
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.end_node(mem)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    Ok(())
}

/// Creates the chosen node with bootargs and stdout-path.
///
/// The `stdout-path` property tells the kernel which device to use for
/// console output before the full driver model initialises.
fn create_chosen_node(fdt: &mut vm_fdt::FdtWriter, cmdline: &str) -> Result<(), BootError> {
    let chosen = fdt
        .begin_node("chosen")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string("bootargs", cmdline)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    let stdout_path = format!("/pl011@{UART_BASE:x}");
    fdt.property_string("stdout-path", &stdout_path)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.end_node(chosen)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    Ok(())
}

/// Creates the `GICv3` interrupt controller node.
fn create_gic_node(
    fdt: &mut vm_fdt::FdtWriter,
    dist_addr: u64,
    dist_size: u64,
    redist_addr: u64,
    redist_size: u64,
) -> Result<(), BootError> {
    let gic_name = format!("intc@{dist_addr:x}");
    let gic = fdt
        .begin_node(&gic_name)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string("compatible", "arm,gic-v3")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_null("interrupt-controller")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("#interrupt-cells", 3)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_array_u64("reg", &[dist_addr, dist_size, redist_addr, redist_size])
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("phandle", GIC_PHANDLE)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.end_node(gic)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    Ok(())
}

/// Creates the ARM generic timer node.
///
/// Timer interrupts (all PPI, level-sensitive, active-high):
/// - Secure physical timer: IRQ 13
/// - Non-secure physical timer: IRQ 14
/// - Virtual timer: IRQ 11
/// - Hypervisor physical timer: IRQ 10
fn create_timer_node(fdt: &mut vm_fdt::FdtWriter) -> Result<(), BootError> {
    let timer = fdt
        .begin_node("timer")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string("compatible", "arm,armv8-timer")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_null("always-on")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    #[rustfmt::skip]
    fdt.property_array_u32("interrupts", &[
        GIC_FDT_IRQ_TYPE_PPI, 13, IRQ_TYPE_LEVEL_HI,
        GIC_FDT_IRQ_TYPE_PPI, 14, IRQ_TYPE_LEVEL_HI,
        GIC_FDT_IRQ_TYPE_PPI, 11, IRQ_TYPE_LEVEL_HI,
        GIC_FDT_IRQ_TYPE_PPI, 10, IRQ_TYPE_LEVEL_HI,
    ])
    .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.end_node(timer)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    Ok(())
}

/// Creates the PSCI (Power State Coordination Interface) node.
///
/// Advertises PSCI 1.0 with a 0.2 fallback so both newer and older
/// kernels can discover power-management via HVC calls.
fn create_psci_node(fdt: &mut vm_fdt::FdtWriter) -> Result<(), BootError> {
    let psci = fdt
        .begin_node("psci")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string_list(
        "compatible",
        vec!["arm,psci-1.0".into(), "arm,psci-0.2".into()],
    )
    .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string("method", "hvc")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.end_node(psci)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    Ok(())
}

/// Creates virtio-mmio device nodes in the FDT.
///
/// Each [`FdtMmioDevice`] produces a node like:
/// ```text
/// virtio_mmio@d0000000 {
///     compatible = "virtio,mmio";
///     reg = <0x0 0xd0000000 0x0 0x1000>;
///     interrupts = <0 37 4>;  // SPI 37, level-high
///     interrupt-parent = <&gic>;
///     dma-coherent;
/// }
/// ```
///
/// The `interrupts` property uses GIC SPI format: type=0 (SPI),
/// intid = GSI + 32, trigger = level-high (4).
fn create_virtio_mmio_nodes(
    fdt: &mut vm_fdt::FdtWriter,
    devices: &[FdtMmioDevice],
) -> Result<(), BootError> {
    for dev in devices {
        let node_name = format!("virtio_mmio@{:x}", dev.base_addr);
        let node = fdt
            .begin_node(&node_name)
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        fdt.property_string("compatible", "virtio,mmio")
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        fdt.property_null("dma-coherent")
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        // reg = <addr_hi addr_lo size_hi size_lo> (2 address cells, 2 size cells)
        fdt.property_array_u64("reg", &[dev.base_addr, dev.size])
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        // interrupts = <type spi_id trigger>
        // SPI number = GSI + 32, but in the FDT interrupts property the
        // SPI number is relative (intid - 32), so we use the GSI directly.
        fdt.property_array_u32(
            "interrupts",
            &[GIC_FDT_IRQ_TYPE_SPI, dev.gsi, IRQ_TYPE_LEVEL_HI],
        )
        .map_err(|e| BootError::Fdt(e.to_string()))?;
        fdt.property_u32("interrupt-parent", GIC_PHANDLE)
            .map_err(|e| BootError::Fdt(e.to_string()))?;
        fdt.end_node(node)
            .map_err(|e| BootError::Fdt(e.to_string()))?;
    }
    Ok(())
}

/// Creates a fixed-clock node for the PL011 UART.
///
/// The PL011 driver requires `uartclk` and `apb_pclk` clock references.
/// This provides a single 24 MHz fixed clock that serves both roles.
fn create_clock_node(fdt: &mut vm_fdt::FdtWriter) -> Result<(), BootError> {
    let clk = fdt
        .begin_node("apb-pclk")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string("compatible", "fixed-clock")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("#clock-cells", 0)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("clock-frequency", UART_CLOCK_FREQ)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string("clock-output-names", "clk24mhz")
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_u32("phandle", CLOCK_PHANDLE)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.end_node(clk)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    Ok(())
}

/// Creates a PL011 UART node in the FDT.
///
/// Describes the PL011 serial port at [`UART_BASE`] with SPI interrupt
/// [`UART_GSI`]. The kernel uses this to discover the serial console
/// device (visible as `ttyAMA0`).
fn create_uart_node(fdt: &mut vm_fdt::FdtWriter) -> Result<(), BootError> {
    let node_name = format!("pl011@{UART_BASE:x}");
    let node = fdt
        .begin_node(&node_name)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string_list(
        "compatible",
        vec!["arm,pl011".into(), "arm,primecell".into()],
    )
    .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_array_u64("reg", &[UART_BASE, UART_SIZE])
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_array_u32(
        "interrupts",
        &[GIC_FDT_IRQ_TYPE_SPI, UART_GSI, IRQ_TYPE_LEVEL_HI],
    )
    .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_array_u32("clocks", &[CLOCK_PHANDLE, CLOCK_PHANDLE])
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.property_string_list("clock-names", vec!["uartclk".into(), "apb_pclk".into()])
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    fdt.end_node(node)
        .map_err(|e| BootError::Fdt(e.to_string()))?;
    Ok(())
}

#[cfg(test)]
#[path = "aarch64_test.rs"]
mod tests;
