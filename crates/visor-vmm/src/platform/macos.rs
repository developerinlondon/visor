//! macOS Hypervisor.framework (HVF) implementation for ARM64.
//!
//! Implements [`Platform`], [`VmOps`], and [`VcpuOps`] traits using
//! the [`applevisor`] crate on `aarch64-apple-darwin`.
//!
//! The HVF model differs from KVM in important ways:
//! - One global VM per process (no multi-VM fd)
//! - No in-kernel IRQ chip — GIC is created separately via `VirtualMachine::with_gic`
//! - No irqfd — interrupt injection is explicit
//! - vCPUs are thread-affine (each must run on its own thread)

use applevisor::error::HypervisorError;
use applevisor::gic::GicConfig;
use applevisor::gic::GicRedistributorReg;
use applevisor::vcpu::{ExitReason, Reg, SysReg, Vcpu, VcpuExitException};
use applevisor::vm::{GicEnabled, VirtualMachine, VirtualMachineConfig, VirtualMachineInstance};

use super::regs::{SpecialRegs, StandardRegs};
use super::{ExitData, Platform, PlatformError, VcpuOps, VmExit, VmOps, event::InterruptEvent};

use super::event::RawEventHandle;
use std::sync::{Arc, Mutex};

// ── Minimal FFI for hv_vm_map ───────────────────────────────────────
//
// The `applevisor` crate allocates its own host memory for guest mappings
// (via `Memory::map`), but our `register_memory` trait provides an
// externally-owned host pointer. We declare only `hv_vm_map` here to
// map caller-provided memory into the guest IPA space. The Hypervisor
// framework is already linked by the `applevisor` crate.

#[allow(unsafe_code)]
unsafe extern "C" {
    fn hv_vm_map(addr: *const u8, ipa: u64, size: usize, flags: u64) -> i32;
}

/// Memory permission flags matching `Hypervisor.framework` constants.
const HV_MEMORY_READ: u64 = 1 << 0;
const HV_MEMORY_WRITE: u64 = 1 << 1;
const HV_MEMORY_EXEC: u64 = 1 << 2;

// ── Error conversion ────────────────────────────────────────────────

/// Converts an [`applevisor::error::HypervisorError`] into a [`PlatformError`].
fn hvf_error(err: HypervisorError) -> PlatformError {
    PlatformError::System(std::io::Error::other(err.to_string()))
}

/// Converts a raw HVF return code to a [`PlatformError`].
///
/// Returns `Ok(())` on success (0), or a [`PlatformError::System`] with
/// the error code encoded as a raw OS error.
fn hvf_result(ret: i32) -> Result<(), PlatformError> {
    if ret == 0 {
        Ok(())
    } else {
        Err(PlatformError::System(std::io::Error::from_raw_os_error(
            ret,
        )))
    }
}

// ── General-purpose register lookup table ───────────────────────────

/// Maps array index 0–30 to the corresponding [`Reg`] variant (X0–X30).
const GP_REGS: [Reg; 31] = [
    Reg::X0,
    Reg::X1,
    Reg::X2,
    Reg::X3,
    Reg::X4,
    Reg::X5,
    Reg::X6,
    Reg::X7,
    Reg::X8,
    Reg::X9,
    Reg::X10,
    Reg::X11,
    Reg::X12,
    Reg::X13,
    Reg::X14,
    Reg::X15,
    Reg::X16,
    Reg::X17,
    Reg::X18,
    Reg::X19,
    Reg::X20,
    Reg::X21,
    Reg::X22,
    Reg::X23,
    Reg::X24,
    Reg::X25,
    Reg::X26,
    Reg::X27,
    Reg::X28,
    Reg::X29,
    Reg::X30,
];

// ── System register mapping tables ──────────────────────────────────

/// System register mapping entry: `(SysReg, getter, setter)`.
type SysRegEntry = (SysReg, fn(&SpecialRegs) -> u64, fn(&mut SpecialRegs, u64));

/// System register mapping table: maps each [`SpecialRegs`] field to
/// its applevisor [`SysReg`] variant for read/write operations.
const SYS_REG_MAP: &[SysRegEntry] = &[
    (SysReg::SCTLR_EL1, |s| s.sctlr_el1, |s, v| s.sctlr_el1 = v),
    (SysReg::TTBR0_EL1, |s| s.ttbr0_el1, |s, v| s.ttbr0_el1 = v),
    (SysReg::TTBR1_EL1, |s| s.ttbr1_el1, |s, v| s.ttbr1_el1 = v),
    (SysReg::TCR_EL1, |s| s.tcr_el1, |s, v| s.tcr_el1 = v),
    (SysReg::MAIR_EL1, |s| s.mair_el1, |s, v| s.mair_el1 = v),
    (SysReg::VBAR_EL1, |s| s.vbar_el1, |s, v| s.vbar_el1 = v),
    (SysReg::SPSR_EL1, |s| s.spsr_el1, |s, v| s.spsr_el1 = v),
    (SysReg::ELR_EL1, |s| s.elr_el1, |s, v| s.elr_el1 = v),
    (SysReg::SP_EL0, |s| s.sp_el0, |s, v| s.sp_el0 = v),
    (SysReg::SP_EL1, |s| s.sp_el1, |s, v| s.sp_el1 = v),
    (SysReg::ESR_EL1, |s| s.esr_el1, |s, v| s.esr_el1 = v),
    (SysReg::FAR_EL1, |s| s.far_el1, |s, v| s.far_el1 = v),
    (SysReg::PAR_EL1, |s| s.par_el1, |s, v| s.par_el1 = v),
    (SysReg::CPACR_EL1, |s| s.cpacr_el1, |s, v| s.cpacr_el1 = v),
    (
        SysReg::CNTKCTL_EL1,
        |s| s.cntkctl_el1,
        |s, v| s.cntkctl_el1 = v,
    ),
    (
        SysReg::CNTV_CTL_EL0,
        |s| s.cntv_ctl_el0,
        |s, v| s.cntv_ctl_el0 = v,
    ),
    (
        SysReg::CNTV_CVAL_EL0,
        |s| s.cntv_cval_el0,
        |s, v| s.cntv_cval_el0 = v,
    ),
    (SysReg::TPIDR_EL0, |s| s.tpidr_el0, |s, v| s.tpidr_el0 = v),
    (
        SysReg::TPIDRRO_EL0,
        |s| s.tpidrro_el0,
        |s, v| s.tpidrro_el0 = v,
    ),
    (SysReg::TPIDR_EL1, |s| s.tpidr_el1, |s, v| s.tpidr_el1 = v),
    (
        SysReg::CONTEXTIDR_EL1,
        |s| s.contextidr_el1,
        |s, v| s.contextidr_el1 = v,
    ),
    (SysReg::AMAIR_EL1, |s| s.amair_el1, |s, v| s.amair_el1 = v),
    (SysReg::AFSR0_EL1, |s| s.afsr0_el1, |s, v| s.afsr0_el1 = v),
    (SysReg::AFSR1_EL1, |s| s.afsr1_el1, |s, v| s.afsr1_el1 = v),
];

/// Read-only system register entry: `(SysReg, setter)`.
type SysRegReadonlyEntry = (SysReg, fn(&mut SpecialRegs, u64));

/// Read-only system registers fetched in `get_sregs` but never written
/// in `set_sregs`.
const SYS_REG_READONLY: &[SysRegReadonlyEntry] = &[
    (SysReg::MIDR_EL1, |s, v| s.midr_el1 = v),
    (SysReg::MPIDR_EL1, |s, v| s.mpidr_el1 = v),
];

/// `ESR_ELx` exception class (EC) field is bits [31:26].
/// Data abort from a lower exception level.
const EC_DATA_ABORT_LOWER: u32 = 0x24;
/// HVC instruction execution in `AArch64`.
const EC_HVC64: u32 = 0x16;
/// SMC instruction execution in `AArch64`.
const EC_SMC64: u32 = 0x17;
/// System register access trap (MRS/MSR to trapped registers).
const EC_SYSREG: u32 = 0x18;
/// WFI/WFE trap exception class.
const EC_WFI: u32 = 0x01;

// ── PSCI function IDs (SMCCC / PSCI v1.1) ─────────────────────────

/// `PSCI_VERSION` — returns PSCI interface version.
const PSCI_VERSION: u64 = 0x8400_0000;
/// `PSCI_MIGRATE_INFO_TYPE` — TOS migration capability.
const PSCI_MIGRATE_INFO_TYPE: u64 = 0x8400_0006;
/// `PSCI_SYSTEM_OFF` — shut down the system.
const PSCI_SYSTEM_OFF: u64 = 0x8400_0008;
/// `PSCI_SYSTEM_RESET` — reset the system.
const PSCI_SYSTEM_RESET: u64 = 0x8400_0009;
/// `PSCI_FEATURES` — query supported PSCI functions.
const PSCI_FEATURES: u64 = 0x8400_000A;
/// `PSCI_CPU_ON` (64-bit) — power on a CPU core.
const _PSCI_CPU_ON_64: u64 = 0xC400_0003;

// ── System register syndrome mask ──────────────────────────────────
//
// Extracts the system register encoding from the ISS field of an
// EC=0x18 syndrome. Bit layout matches Apple HVF / ARM ARM:
//   op0[21:20] | op2[19:17] | op1[16:14] | CRn[13:10] | CRm[4:1]
const SYSREG_MASK: u32 = (0x3 << 20) | (0x7 << 17) | (0x7 << 14) | (0xF << 10) | (0xF << 1);
/// `CNTV_CTL_EL0` bit 0: timer enabled.
const VTIMER_CTL_ENABLE: u64 = 1 << 0;
/// `CNTV_CTL_EL0` bit 1: timer interrupt masked by guest.
const VTIMER_CTL_IMASK: u64 = 1 << 1;
/// `CNTV_CTL_EL0` bit 2: timer condition met.
const VTIMER_CTL_ISTATUS: u64 = 1 << 2;
/// Virtual timer PPI interrupt ID (`GICv3` PPI 27).
const VTIMER_PPI: u64 = 27;

// ── HVF Platform ───────────────────────────────────────────────────

/// Apple Hypervisor Framework platform for ARM64.
///
/// Wraps the process-global HVF VM created via [`VirtualMachine::new`].
/// On macOS there is exactly one VM per process. The VM is destroyed
/// when this value is dropped (via the inner [`VirtualMachineInstance`]).
#[derive(Debug)]
pub struct HvfPlatform {
    vm: VirtualMachineInstance<GicEnabled>,
}

impl Platform for HvfPlatform {
    type Vm = HvfVm;

    /// Creates the HVF platform by calling [`VirtualMachine::new`].
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if `hv_vm_create` fails
    /// (e.g. missing `com.apple.security.hypervisor` entitlement).
    fn new() -> Result<Self, PlatformError> {
        let vm_config = VirtualMachineConfig::default();
        let mut gic_config = GicConfig::new();
        gic_config
            .set_distributor_base(0x0800_0000)
            .map_err(hvf_error)?;
        gic_config
            .set_redistributor_base(0x080A_0000)
            .map_err(hvf_error)?;
        let vm = VirtualMachine::with_gic(vm_config, gic_config).map_err(hvf_error)?;
        Ok(Self { vm })
    }

    /// Returns a logical VM wrapper.
    ///
    /// HVF has a single global VM per process, so this returns a
    /// lightweight handle that clones the inner [`VirtualMachineInstance`]
    /// without additional system calls.
    ///
    /// # Errors
    ///
    /// This method does not fail on HVF.
    fn create_vm(&self) -> Result<Self::Vm, PlatformError> {
        Ok(HvfVm {
            vm: self.vm.clone(),
            irq_registrations: Mutex::new(Vec::new()),
        })
    }
}

// ── HVF VM ─────────────────────────────────────────────────────────

/// HVF virtual machine handle.
///
/// On ARM64 macOS, the VM is process-global. This is a logical wrapper
/// holding a cloned [`VirtualMachineInstance`] handle and providing
/// [`VmOps`] trait methods.
#[derive(Debug)]
pub struct HvfVm {
    vm: VirtualMachineInstance<GicEnabled>,
    irq_registrations: Mutex<Vec<(RawEventHandle, u32)>>,
}

impl VmOps for HvfVm {
    type Vcpu = HvfVcpu;

    /// No-op on ARM64. The IRQ chip (`GICv3`) is created separately
    /// via [`VirtualMachine::with_gic`] when needed.
    ///
    /// # Errors
    ///
    /// This method always succeeds on ARM64 HVF.
    fn create_irq_chip(&self) -> Result<(), PlatformError> {
        // ARM64 does not have an in-kernel APIC/IOAPIC. The GIC is
        // configured separately. Return Ok to satisfy the trait contract.
        Ok(())
    }

    /// No-op on ARM64. There is no PIT on ARM; the generic timer is
    /// used instead and is handled via `VTimer` exits.
    ///
    /// # Errors
    ///
    /// This method always succeeds on ARM64 HVF.
    fn create_pit(&self) -> Result<(), PlatformError> {
        // ARM64 uses the generic timer, not a PIT.
        Ok(())
    }

    /// Maps a host memory region into the guest physical address space.
    ///
    /// Uses a direct FFI call to `hv_vm_map` because the [`applevisor`]
    /// crate's [`Memory`] type allocates its own host memory, whereas our
    /// trait provides an externally-owned host pointer.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if `hv_vm_map` fails.
    #[allow(clippy::not_unsafe_ptr_arg_deref)]
    fn register_memory(
        &self,
        _slot: u32,
        guest_addr: u64,
        size: u64,
        host_addr: *mut u8,
    ) -> Result<(), PlatformError> {
        // SAFETY: The caller guarantees that host_addr points to a valid
        // memory region of at least `size` bytes that remains valid for
        // the lifetime of the VM. HVF maps it as RWX for the guest.
        #[allow(unsafe_code, clippy::cast_possible_truncation)]
        let ret = unsafe {
            hv_vm_map(
                host_addr,
                guest_addr,
                size as usize,
                HV_MEMORY_READ | HV_MEMORY_WRITE | HV_MEMORY_EXEC,
            )
        };
        hvf_result(ret)
    }

    /// Stores the interrupt event for later injection in the run loop.
    ///
    /// HVF does not have an irqfd mechanism like KVM. On ARM64,
    /// interrupts are injected explicitly via GIC SPI triggering.
    /// This method stores the event fd and GSI so the run loop can
    /// poll for pending events and inject via [`HvfVm::gic_set_spi`].
    ///
    /// # Errors
    ///
    /// This method always succeeds on ARM64 HVF.
    fn register_irqfd(&self, event: &dyn InterruptEvent, gsi: u32) -> Result<(), PlatformError> {
        let mut regs = self
            .irq_registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        regs.push((event.as_raw(), gsi));
        Ok(())
    }

    /// Creates a vCPU via [`VirtualMachineInstance::vcpu_create`].
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if `hv_vcpu_create` fails.
    fn create_vcpu(&self, _index: u64) -> Result<Self::Vcpu, PlatformError> {
        let vcpu = self.vm.vcpu_create().map_err(hvf_error)?;
        Ok(HvfVcpu {
            vcpu,
            last_read_srt: None,
            vtimer_masked: false,
            vtimer_activations: 0,
        })
    }
}

impl HvfVm {
    /// Triggers a Shared Peripheral Interrupt (SPI) on the GIC.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if the GIC SPI trigger fails.
    pub(crate) fn gic_set_spi(&self, intid: u32, level: bool) -> Result<(), PlatformError> {
        self.vm.gic_set_spi(intid, level).map_err(hvf_error)
    }

    /// Returns a snapshot of registered IRQ event associations.
    ///
    /// Each entry is a `(kqueue_fd, gsi)` pair registered via
    /// [`VmOps::register_irqfd`].
    pub(crate) fn irq_registrations_snapshot(&self) -> Vec<(RawEventHandle, u32)> {
        self.irq_registrations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Creates a closure that deasserts a level-triggered GIC SPI.
    ///
    /// Returns an `Arc<dyn Fn()>` that calls `gic_set_spi(intid, false)`.
    /// Used as the `irq_deassert` callback on [`MmioTransport`] so the
    /// guest's `InterruptACK` write properly deasserts the interrupt line.
    pub(crate) fn create_spi_deassert(&self, intid: u32) -> Arc<dyn Fn() + Send + Sync> {
        let vm = self.vm.clone();
        Arc::new(move || {
            let _ = vm.gic_set_spi(intid, false);
        })
    }

    /// Creates an [`IrqMonitorHandle`] for use by the IRQ monitor thread.
    ///
    /// Clones the underlying `VirtualMachineInstance` (Arc-based, cheap) so the
    /// monitor thread can call `hv_vcpus_exit` without borrowing `self`.
    pub(crate) fn irq_monitor_handle(&self, vcpu_handle: applevisor::vcpu::VcpuHandle) -> IrqMonitorHandle {
        IrqMonitorHandle {
            vm: self.vm.clone(),
            vcpu_handle,
        }
    }
}

/// Thread-safe handle for the IRQ monitor thread to kick vCPUs.
///
/// Owns a cloned `VirtualMachineInstance` (Arc-based) and a `VcpuHandle`,
/// both of which are `Send + Clone`. This avoids raw pointers or `SendPtr`
/// wrappers for cross-thread VM access.
pub(crate) struct IrqMonitorHandle {
    vm: VirtualMachineInstance<GicEnabled>,
    vcpu_handle: applevisor::vcpu::VcpuHandle,
}

impl IrqMonitorHandle {
    /// Forces the vCPU to exit `hv_vcpu_run()`, kicking it out of WFI.
    pub(crate) fn kick_vcpu(&self) -> Result<(), PlatformError> {
        self.vm.vcpus_exit(std::slice::from_ref(&self.vcpu_handle)).map_err(hvf_error)
    }
}

// ── HVF vCPU ───────────────────────────────────────────────────────

/// HVF virtual CPU handle for ARM64.
///
/// Wraps an applevisor [`Vcpu`] which manages the underlying HVF vCPU
/// lifetime. The vCPU is destroyed when this value is dropped.
pub struct HvfVcpu {
    pub(crate) vcpu: Vcpu,
    /// SRT (Source Register Transfer) from the last MMIO read data abort.
    /// Set by `decode_exception` for `MmioRead` exits, consumed by `complete_mmio_read`.
    last_read_srt: Option<u32>,
    /// Whether the vtimer is currently masked to prevent repeated `VTIMER_ACTIVATED` exits.
    /// Set on `VTIMER_ACTIVATED` exit, cleared when the guest acknowledges the timer interrupt.
    vtimer_masked: bool,
    /// Diagnostic counter: how many `VTIMER_ACTIVATED` exits have been handled.
    pub(crate) vtimer_activations: u64,
}

// Manual Debug impl because applevisor::Vcpu's Debug includes raw pointers
// that we don't want to expose in our public API.
impl std::fmt::Debug for HvfVcpu {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HvfVcpu")
            .field("id", &self.vcpu.id())
            .finish_non_exhaustive()
    }
}

// SAFETY: HvfVcpu wraps an applevisor Vcpu which holds an opaque vCPU ID
// (u64) and a pointer to framework-managed memory. While HVF vCPUs are
// thread-affine for run(), the handle itself can be safely sent between
// threads (the owning thread calls run()).
#[allow(unsafe_code)]
unsafe impl Send for HvfVcpu {}

impl VcpuOps for HvfVcpu {
    /// Sets ARM64 general-purpose registers (X0–X30, SP, PC, CPSR, FPCR, FPSR).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if any register write fails.
    fn set_regs(&self, regs: &StandardRegs) -> Result<(), PlatformError> {
        for (i, &reg) in GP_REGS.iter().enumerate() {
            self.vcpu.set_reg(reg, regs.x[i]).map_err(hvf_error)?;
        }

        self.vcpu.set_reg(Reg::PC, regs.pc).map_err(hvf_error)?;
        self.vcpu.set_reg(Reg::CPSR, regs.cpsr).map_err(hvf_error)?;
        self.vcpu.set_reg(Reg::FPCR, regs.fpcr).map_err(hvf_error)?;
        self.vcpu.set_reg(Reg::FPSR, regs.fpsr).map_err(hvf_error)?;

        // SP is accessed via the SP_EL0 system register on ARM64 HVF.
        self.vcpu
            .set_sys_reg(SysReg::SP_EL0, regs.sp)
            .map_err(hvf_error)?;

        Ok(())
    }

    /// Gets ARM64 general-purpose registers (X0–X30, SP, PC, CPSR, FPCR, FPSR).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if any register read fails.
    fn get_regs(&self) -> Result<StandardRegs, PlatformError> {
        let mut regs = StandardRegs::default();

        for (i, &reg) in GP_REGS.iter().enumerate() {
            regs.x[i] = self.vcpu.get_reg(reg).map_err(hvf_error)?;
        }

        regs.pc = self.vcpu.get_reg(Reg::PC).map_err(hvf_error)?;
        regs.cpsr = self.vcpu.get_reg(Reg::CPSR).map_err(hvf_error)?;
        regs.fpcr = self.vcpu.get_reg(Reg::FPCR).map_err(hvf_error)?;
        regs.fpsr = self.vcpu.get_reg(Reg::FPSR).map_err(hvf_error)?;

        // SP via SP_EL0 system register.
        regs.sp = self.vcpu.get_sys_reg(SysReg::SP_EL0).map_err(hvf_error)?;

        Ok(regs)
    }

    /// Sets ARM64 system registers (`SCTLR_EL1`, `TCR_EL1`, etc.).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if any system register write fails.
    fn set_sregs(&self, sregs: &SpecialRegs) -> Result<(), PlatformError> {
        for &(reg, getter, _) in SYS_REG_MAP {
            self.vcpu
                .set_sys_reg(reg, getter(sregs))
                .map_err(hvf_error)?;
        }
        // Read-only registers (MIDR, MPIDR) are NOT written.
        Ok(())
    }

    /// Gets ARM64 system registers (`SCTLR_EL1`, `TCR_EL1`, etc.).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if any system register read fails.
    fn get_sregs(&self) -> Result<SpecialRegs, PlatformError> {
        let mut sregs = SpecialRegs::default();

        for &(reg, _, setter) in SYS_REG_MAP {
            let val = self.vcpu.get_sys_reg(reg).map_err(hvf_error)?;
            setter(&mut sregs, val);
        }

        // Also read the read-only registers.
        for &(reg, setter) in SYS_REG_READONLY {
            let val = self.vcpu.get_sys_reg(reg).map_err(hvf_error)?;
            setter(&mut sregs, val);
        }

        Ok(sregs)
    }

    /// Runs the vCPU until a VM exit occurs.
    ///
    /// Maps HVF exit reasons to the portable [`VmExit`] enum and performs
    /// mandatory post-exit housekeeping:
    ///
    /// - **Exception exits** (`EXCEPTION`): decodes the syndrome (which also
    ///   selectively advances PC for ECs that require it) and syncs vtimer state.
    /// - **Timer exits** (`VTIMER_ACTIVATED`): injects a pending IRQ and masks
    ///   the vtimer to prevent repeated exits while the guest handles it.
    /// - **Other exits** (`CANCELED`, etc.): mapped to [`VmExit::Halt`].
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if `hv_vcpu_run` or any post-exit
    /// register operation fails.
    fn run(&mut self) -> Result<VmExit, PlatformError> {
        self.vcpu.run().map_err(hvf_error)?;

        let exit = self.vcpu.get_exit_info();

        match exit.reason {
            ExitReason::EXCEPTION => {
                let vm_exit = self.decode_exception(&exit.exception)?;
                // PC advance is handled selectively in decode_exception.
                // Check if a previously masked vtimer can be unmasked.
                self.sync_vtimer()?;
                Ok(vm_exit)
            }
            ExitReason::VTIMER_ACTIVATED => {
                // Guest virtual timer fired — inject IRQ and mask vtimer
                // to prevent repeated exits while the guest handles it.
                self.handle_vtimer()?;
                Ok(VmExit::Halt)
            }
            _ => Ok(VmExit::Halt),
        }
    }
}

impl HvfVcpu {
    /// Decodes an ARM64 exception syndrome into a [`VmExit`].
    ///
    /// Handles PC advance selectively: data aborts, SMC, sysreg traps, and
    /// WFI advance PC by 4. HVC does **not** advance PC (the CPU auto-returns
    /// to the next instruction after an HVC call).
    fn decode_exception(&mut self, exception: &VcpuExitException) -> Result<VmExit, PlatformError> {
        let ec = ((exception.syndrome >> 26) & 0x3F) as u32;
        let ipa = exception.physical_address;

        match ec {
            EC_DATA_ABORT_LOWER => {
                // ISS encoding for data aborts: bit 6 = WnR (write-not-read)
                let wnr = (exception.syndrome >> 6) & 1;
                // SAS (Syndrome Access Size) is bits [23:22]
                let sas = ((exception.syndrome >> 22) & 0x3) as usize;
                let access_size = 1usize << sas;
                // SRT (Source Register Transfer) is bits [20:16]
                let srt = ((exception.syndrome >> 16) & 0x1F) as u32;

                self.advance_pc()?;

                if wnr == 1 {
                    // Write: read the data from the SRT register.
                    let data = self.read_gpr(srt)?;
                    let bytes = data.to_le_bytes();
                    let len = access_size.min(8);
                    Ok(VmExit::MmioWrite {
                        addr: ipa,
                        data: ExitData::from_slice(&bytes[..len]),
                    })
                } else {
                    // Read: save SRT so complete_mmio_read can write the response
                    // to the correct guest register.
                    self.last_read_srt = Some(srt);
                    Ok(VmExit::MmioRead {
                        addr: ipa,
                        size: access_size,
                    })
                }
            }
            EC_HVC64 => {
                // PSCI calls pass function ID in X0 (NOT the HVC immediate).
                // HVC does NOT need PC advance — the CPU auto-returns to the
                // instruction after HVC.
                let psci_fn = self.vcpu.get_reg(Reg::X0).map_err(hvf_error)?;
                self.handle_psci(psci_fn)
            }
            EC_SMC64 => {
                // SMC traps to EL2 — we must advance PC past the SMC instruction.
                let psci_fn = self.vcpu.get_reg(Reg::X0).map_err(hvf_error)?;
                self.advance_pc()?;
                self.handle_psci(psci_fn)
            }
            EC_SYSREG => {
                // System register access trap (MRS/MSR to ID/PMU/ICC regs).
                self.handle_sysreg_trap(exception.syndrome)?;
                self.advance_pc()?;
                Ok(VmExit::Halt)
            }
            EC_WFI => {
                // WFI/WFE trap — advance PC and continue.
                self.advance_pc()?;
                Ok(VmExit::Halt)
            }
            // Other unrecognized exception classes: advance PC and continue.
            _ => {
                self.advance_pc()?;
                Ok(VmExit::Halt)
            }
        }
    }

    /// Reads a general-purpose register by its index (0–30 = X0–X30).
    fn read_gpr(&self, index: u32) -> Result<u64, PlatformError> {
        let Some(&reg) = GP_REGS.get(index as usize) else {
            return Ok(0); // XZR (index 31 or out-of-range)
        };
        self.vcpu.get_reg(reg).map_err(hvf_error)
    }

    /// Writes a general-purpose register by its index (0–30 = X0–X30).
    ///
    /// Index 31 (XZR) and out-of-range indices are silently discarded.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if the register write fails.
    fn write_gpr(&self, index: u32, value: u64) -> Result<(), PlatformError> {
        let Some(&reg) = GP_REGS.get(index as usize) else {
            return Ok(()); // XZR (index 31 or out-of-range)
        };
        self.vcpu.set_reg(reg, value).map_err(hvf_error)
    }

    /// Dispatches a PSCI call based on the function ID in X0.
    ///
    /// Handles PSCI v1.1 function IDs. Unknown functions return -1
    /// (`NOT_SUPPORTED`) in X0.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if a register write fails.
    fn handle_psci(&self, function_id: u64) -> Result<VmExit, PlatformError> {
        match function_id {
            PSCI_VERSION => {
                // PSCI v1.1 = major(1) << 16 | minor(1) = 0x0001_0001
                self.write_gpr(0, 0x0001_0001)?;
                Ok(VmExit::Halt)
            }
            PSCI_MIGRATE_INFO_TYPE => {
                // 2 = Trusted OS not present / migration not required.
                self.write_gpr(0, 2)?;
                Ok(VmExit::Halt)
            }
            PSCI_SYSTEM_OFF => Ok(VmExit::Shutdown),
            PSCI_SYSTEM_RESET => Ok(VmExit::Reboot),
            PSCI_FEATURES => {
                // 0 = function supported.
                self.write_gpr(0, 0)?;
                Ok(VmExit::Halt)
            }
            _ => {
                // Unknown PSCI function — return NOT_SUPPORTED (-1).
                #[allow(clippy::cast_sign_loss)]
                let not_supported = -1_i64 as u64;
                self.write_gpr(0, not_supported)?;
                Ok(VmExit::Halt)
            }
        }
    }

    /// Handles a system register access trap (EC 0x18).
    ///
    /// For reads: writes 0 to the destination register (safe default for
    /// ID, PMU, debug, and ICC registers). For writes: silently discards
    /// the value.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if a register write fails.
    fn handle_sysreg_trap(&self, syndrome: u64) -> Result<(), PlatformError> {
        let is_read = (syndrome & 1) != 0;
        let rt = ((syndrome >> 5) & 0x1F) as u32;

        #[allow(clippy::cast_possible_truncation)]
        let reg = (syndrome as u32) & SYSREG_MASK;

        if is_read {
            // Return 0 as safe default for unhandled system registers.
            self.write_gpr(rt, 0)?;
        }
        // Writes are silently discarded.

        tracing::debug!(
            is_read,
            rt,
            reg = format_args!("0x{reg:06x}"),
            "sysreg trap"
        );

        Ok(())
    }

    /// Writes MMIO read response data back to the guest register identified by
    /// the SRT (Source Register Transfer) saved during the data abort exit.
    ///
    /// After the device manager fills the response buffer via
    /// [`ExitHandler::handle_mmio_read`](crate::vm::ExitHandler::handle_mmio_read),
    /// the run loop calls this method to write the data to the guest register
    /// that performed the load instruction.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if the register write fails.
    pub fn complete_mmio_read(&mut self, data: &[u8]) -> Result<(), PlatformError> {
        let Some(srt) = self.last_read_srt.take() else {
            return Ok(());
        };
        // XZR (register index 31) is the zero register — writes are discarded.
        let Some(&reg) = GP_REGS.get(srt as usize) else {
            return Ok(());
        };
        // Zero-extend data to u64 (little-endian).
        let mut val_bytes = [0u8; 8];
        let len = data.len().min(8);
        val_bytes[..len].copy_from_slice(&data[..len]);
        let value = u64::from_le_bytes(val_bytes);
        self.vcpu.set_reg(reg, value).map_err(hvf_error)
    }

    /// Advances the program counter by 4 bytes (one `AArch64` instruction).
    ///
    /// Apple Hypervisor.framework does not auto-advance PC after exception
    /// VM exits. The VMM must manually step past the faulting instruction
    /// after handling data aborts, SMC, sysreg traps, and WFI/WFE. HVC
    /// does **not** require PC advance (the CPU auto-returns to the next
    /// instruction).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if the register read/write fails.
    fn advance_pc(&mut self) -> Result<(), PlatformError> {
        let pc = self.vcpu.get_reg(Reg::PC).map_err(hvf_error)?;
        self.vcpu.set_reg(Reg::PC, pc + 4).map_err(hvf_error)
    }

    /// Handles a `VTIMER_ACTIVATED` exit.
    ///
    /// **Note**: With `hv_gic_create()` (native GIC), HVF routes PPI 27
    /// internally and typically does NOT deliver `VTIMER_ACTIVATED` exits
    /// to userspace. This method exists as a fallback for configurations
    /// where the native GIC does not manage the timer interrupt.
    ///
    /// When called, attempts to pend PPI 27 via `GICR_ISPENDR0` and mask
    /// the vtimer. HVF may deny the `GICR_ISPENDR0` write (`HV_DENIED`)
    /// when the native GIC owns the redistributor; in that case the vtimer
    /// is still masked to prevent repeated exits, but the IRQ pend is a no-op
    /// (the native GIC handles it).
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if the vtimer mask operation fails.
    fn handle_vtimer(&mut self) -> Result<(), PlatformError> {
        tracing::debug!("vtimer: VTIMER_ACTIVATED — pending PPI 27 via GICR_ISPENDR0");
        // Attempt to pend PPI 27. With native GIC, this may be denied
        // (HV_DENIED) because the native GIC owns the redistributor.
        // That is fine — the native GIC handles the interrupt internally.
        let _ = self
            .vcpu
            .set_redistributor_reg(GicRedistributorReg::ISPENDR0, 1 << VTIMER_PPI);
        self.vcpu.set_vtimer_mask(true).map_err(hvf_error)?;
        self.vtimer_masked = true;
        self.vtimer_activations += 1;
        Ok(())
    }

    /// Checks whether a masked vtimer can be unmasked.
    ///
    /// Called after every exception exit. Reads `CNTV_CTL_EL0` to determine
    /// if the timer interrupt condition has been cleared by the guest.
    ///
    /// **Note**: With `hv_gic_create()` (native GIC), `vtimer_masked` is
    /// always false (because `VTIMER_ACTIVATED` never fires), so this method
    /// returns immediately as a no-op.
    ///
    /// # Errors
    ///
    /// Returns [`PlatformError::System`] if the system register read
    /// or vtimer mask operation fails.
    pub(crate) fn sync_vtimer(&mut self) -> Result<(), PlatformError> {
        if !self.vtimer_masked {
            return Ok(());
        }
        let ctl = self
            .vcpu
            .get_sys_reg(SysReg::CNTV_CTL_EL0)
            .map_err(hvf_error)?;
        let irq_active = (ctl & (VTIMER_CTL_ENABLE | VTIMER_CTL_IMASK | VTIMER_CTL_ISTATUS))
            == (VTIMER_CTL_ENABLE | VTIMER_CTL_ISTATUS);
        if !irq_active {
            tracing::debug!("vtimer: condition cleared — unmasking vtimer");
            // Attempt to clear PPI 27. With native GIC, this may be denied.
            let _ = self
                .vcpu
                .set_redistributor_reg(GicRedistributorReg::ICPENDR0, 1 << VTIMER_PPI);
            self.vcpu.set_vtimer_mask(false).map_err(hvf_error)?;
            self.vtimer_masked = false;
        }
        Ok(())
    }
}

/// Non-blocking poll of a kqueue fd for pending `EVFILT_USER` events.
///
/// Returns `true` if a triggered event was consumed, `false` if none pending.
/// The `EV_CLEAR` flag (set during kqueue registration) auto-resets the event
/// after reading, so a second call returns `false` unless re-triggered.
pub(crate) fn poll_kqueue_fd(kq: RawEventHandle) -> Result<bool, std::io::Error> {
    let timeout = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut eventlist = [libc::kevent {
        ident: 0,
        filter: 0,
        flags: 0,
        fflags: 0,
        data: 0,
        udata: std::ptr::null_mut(),
    }];

    // SAFETY: Valid kqueue fd, null changelist (read-only), zero timeout
    // for non-blocking poll.
    #[allow(unsafe_code)]
    let ret = unsafe {
        libc::kevent(
            kq,
            std::ptr::null(),
            0,
            eventlist.as_mut_ptr(),
            1,
            &raw const timeout,
        )
    };

    if ret < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(ret > 0)
}

// ── MacosEventFd ───────────────────────────────────────────────────

/// macOS interrupt event backed by `kqueue` with `EVFILT_USER`.
///
/// Provides an [`InterruptEvent`] implementation for macOS that uses
/// a kqueue file descriptor with a user-defined filter. This is the
/// macOS equivalent of Linux's `eventfd`.
pub struct MacosEventFd {
    kq: RawEventHandle,
}

impl MacosEventFd {
    /// Creates a new kqueue-backed event.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if `kqueue()` fails.
    pub fn new() -> Result<Self, std::io::Error> {
        // SAFETY: kqueue() creates a new kqueue file descriptor.
        #[allow(unsafe_code)]
        let kq = unsafe { libc::kqueue() };
        if kq < 0 {
            return Err(std::io::Error::last_os_error());
        }

        // Register an EVFILT_USER event (ident=1) so we can trigger it.
        let mut changelist = [libc::kevent {
            ident: 1,
            filter: libc::EVFILT_USER,
            flags: libc::EV_ADD | libc::EV_CLEAR,
            fflags: 0,
            data: 0,
            udata: std::ptr::null_mut(),
        }];

        // SAFETY: Valid kqueue fd with a properly initialized changelist.
        #[allow(unsafe_code)]
        let ret = unsafe {
            libc::kevent(
                kq,
                changelist.as_ptr(),
                1,
                changelist.as_mut_ptr(),
                0,
                std::ptr::null(),
            )
        };
        if ret < 0 {
            // SAFETY: Closing the kqueue fd we just created.
            #[allow(unsafe_code)]
            unsafe {
                libc::close(kq);
            }
            return Err(std::io::Error::last_os_error());
        }

        Ok(Self { kq })
    }

    /// Non-blocking check for a pending trigger event.
    ///
    /// Returns `true` if the event has been triggered since the last poll,
    /// `false` otherwise. The `EV_CLEAR` flag auto-resets the event on read.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if `kevent()` fails.
    pub fn poll(&self) -> Result<bool, std::io::Error> {
        poll_kqueue_fd(self.kq)
    }
}

impl InterruptEvent for MacosEventFd {
    /// Triggers the kqueue event by noting `EVFILT_USER`.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if `kevent()` fails.
    fn trigger(&self) -> Result<(), std::io::Error> {
        let changelist = [libc::kevent {
            ident: 1,
            filter: libc::EVFILT_USER,
            flags: 0,
            fflags: libc::NOTE_TRIGGER,
            data: 0,
            udata: std::ptr::null_mut(),
        }];

        // SAFETY: Triggering a registered EVFILT_USER event on a valid kqueue.
        #[allow(unsafe_code)]
        let ret = unsafe {
            libc::kevent(
                self.kq,
                changelist.as_ptr(),
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if ret < 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn as_raw(&self) -> RawEventHandle {
        self.kq
    }
}

impl Drop for MacosEventFd {
    fn drop(&mut self) {
        // SAFETY: Closing the kqueue fd we created in new().
        #[allow(unsafe_code)]
        unsafe {
            libc::close(self.kq);
        }
    }
}

// SAFETY: MacosEventFd holds only a kqueue fd (integer). kqueue fds can
// be safely shared between threads — kevent() is thread-safe on macOS.
#[allow(unsafe_code)]
unsafe impl Send for MacosEventFd {}
#[allow(unsafe_code)]
unsafe impl Sync for MacosEventFd {}

#[cfg(test)]
#[path = "macos_test.rs"]
mod tests;
