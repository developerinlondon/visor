//! vCPU creation, register initialization, and KVM run loop.
//!
//! Each [`Vcpu`] wraps a KVM vCPU file descriptor. After configuring
//! registers from a [`BootConfig`], call [`Vcpu::run_loop`] to enter
//! the KVM execution loop. VM exits are dispatched through the
//! [`ExitHandler`] trait.
//!
//! # Architecture
//!
//! ```text
//! ┌────────────┐     ┌─────────┐     ┌──────────────┐
//! │ BootConfig │────>│  Vcpu   │────>│  KVM_RUN     │
//! │ (Layer 4)  │     │ set_regs│     │  loop        │
//! └────────────┘     └─────────┘     └──┬───────────┘
//!                                       │ VmExit
//!                                       ▼
//!                                  ExitHandler
//!                                  (Layer 6+)
//! ```

// Re-export exit types from the canonical location (crate::vm).
pub use crate::vm::{ExitAction, ExitHandler, VcpuError};
pub use crate::vm::{ExitData, VM_EXIT_DATA_MAX, VmExit};

// ── KVM-specific vCPU ────────────────────────────────────────────────

#[cfg(target_os = "linux")]
use std::mem;

#[cfg(target_os = "linux")]
use kvm_bindings::{KVM_MAX_CPUID_ENTRIES, Msrs, kvm_fpu, kvm_msr_entry, kvm_regs, kvm_sregs};
#[cfg(target_os = "linux")]
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd};

#[cfg(target_os = "linux")]
use crate::boot::x86_64::gdt_entry;
#[cfg(target_os = "linux")]
use crate::boot::{self, BootConfig};
#[cfg(target_os = "linux")]
use crate::guest_virtualization::GuestVirtualizationMode;

#[cfg(target_os = "linux")]
/// A KVM virtual CPU.
///
/// Wraps a [`VcpuFd`] and provides methods to configure registers
/// and run the vCPU execution loop.
pub struct Vcpu {
    fd: VcpuFd,
    index: u64,
}

#[cfg(target_os = "linux")]
impl Vcpu {
    /// Creates a new vCPU on the given VM.
    ///
    /// # Errors
    ///
    /// Returns [`VcpuError::Create`] if the KVM ioctl fails.
    pub fn new(vm: &crate::platform::KvmVm, index: u64) -> Result<Self, VcpuError> {
        let fd = vm
            .fd
            .create_vcpu(index)
            .map_err(|e| VcpuError::Create(std::io::Error::from_raw_os_error(e.errno())))?;
        Ok(Self { fd, index })
    }

    /// Returns the vCPU index.
    #[must_use]
    pub fn index(&self) -> u64 {
        self.index
    }

    /// Returns a reference to the underlying `VcpuFd`.
    #[must_use]
    pub fn fd(&self) -> &VcpuFd {
        &self.fd
    }

    /// Configures all vCPU registers for booting a Linux kernel.
    ///
    /// Sets general-purpose registers, special registers (GDT, IDT, CR, EFER),
    /// FPU state, CPUID, and boot MSRs based on the provided [`BootConfig`].
    ///
    /// The `kvm` handle is used to obtain the host's supported CPUID, which
    /// is then configured on the vCPU.
    ///
    /// # Errors
    ///
    /// Returns [`VcpuError`] if any register setup ioctl fails.
    pub fn configure_regs(
        &self,
        kvm: &Kvm,
        config: &BootConfig,
        guest_virtualization: GuestVirtualizationMode,
    ) -> Result<(), VcpuError> {
        self.configure_cpuid(kvm, guest_virtualization)?;
        self.set_base_regs(config)?;
        self.set_special_regs(config)?;
        self.set_fpu()?;
        self.set_boot_msrs()?;
        Ok(())
    }

    /// Configures CPUID exposure for this vCPU.
    ///
    /// # Errors
    ///
    /// Returns [`VcpuError`] if KVM rejects the configured CPUID set.
    pub fn configure_cpuid(
        &self,
        kvm: &Kvm,
        guest_virtualization: GuestVirtualizationMode,
    ) -> Result<(), VcpuError> {
        self.set_cpuid(kvm, guest_virtualization)
    }

    /// Runs the vCPU in a loop, dispatching exits to the handler.
    ///
    /// For I/O and MMIO **read** exits, the handler's [`ExitHandler::handle_io_read`]
    /// and [`ExitHandler::handle_mmio_read`] methods are called while the KVM data
    /// buffer is still live, so device responses are written directly into the
    /// `kvm_run` shared memory region.
    ///
    /// The loop continues until the handler returns [`ExitAction::Stop`]
    /// or a fatal KVM error occurs.
    ///
    /// # Errors
    ///
    /// Returns [`VcpuError`] on KVM run failure or fatal exit.
    pub fn run_loop(
        &mut self,
        handler: &mut dyn ExitHandler,
        kill_flag: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<(), VcpuError> {
        loop {
            if kill_flag.load(std::sync::atomic::Ordering::Acquire) {
                return Ok(());
            }
            let exit = self.run_once_with_handler(handler)?;
            let action = handler.handle_exit(exit)?;
            if action == ExitAction::Stop {
                return Ok(());
            }
        }
    }

    /// Executes a single `KVM_RUN`, handles reads via the handler, and
    /// translates the exit reason.
    ///
    /// For `IoIn` and `MmioRead` exits, calls the handler's read methods
    /// while the `kvm_run` data buffer is still mutable, then returns the
    /// corresponding [`VmExit`].
    ///
    /// # Errors
    ///
    /// Returns [`VcpuError`] on KVM run failure or fatal exit.
    pub(crate) fn run_once_with_handler(
        &mut self,
        handler: &mut dyn ExitHandler,
    ) -> Result<VmExit, VcpuError> {
        match self.fd.run() {
            Ok(VcpuExit::IoIn(port, data)) => {
                handler.handle_io_read(port, data);
                Ok(VmExit::IoIn {
                    port,
                    size: data.len(),
                })
            }
            Ok(VcpuExit::IoOut(port, data)) => Ok(VmExit::IoOut {
                port,
                data: ExitData::from_slice(data),
            }),
            Ok(VcpuExit::MmioRead(addr, data)) => {
                handler.handle_mmio_read(addr, data);
                Ok(VmExit::MmioRead {
                    addr,
                    size: data.len(),
                })
            }
            Ok(VcpuExit::MmioWrite(addr, data)) => Ok(VmExit::MmioWrite {
                addr,
                data: ExitData::from_slice(data),
            }),
            Ok(VcpuExit::Shutdown) => Ok(VmExit::Shutdown),
            Ok(VcpuExit::SystemEvent(event_type, _flags)) => {
                // KVM_SYSTEM_EVENT_SHUTDOWN = 1, KVM_SYSTEM_EVENT_RESET = 2
                if event_type == 2 {
                    Ok(VmExit::Reboot)
                } else {
                    Ok(VmExit::Shutdown)
                }
            }
            Ok(VcpuExit::FailEntry(reason, cpu)) => Err(VcpuError::FailEntry { reason, cpu }),
            Ok(VcpuExit::InternalError) => Err(VcpuError::InternalError),
            // Hlt + unknown exits → halt. Shutdown/SystemEvent/FailEntry/InternalError
            // are matched above, so this only catches Hlt and future KVM exit types.
            Ok(VcpuExit::Hlt | _) => Ok(VmExit::Halt),
            Err(e) => {
                let errno = e.errno();
                if errno == libc::EAGAIN || errno == libc::EINTR {
                    Ok(VmExit::Halt)
                } else {
                    Err(VcpuError::Run(std::io::Error::from_raw_os_error(errno)))
                }
            }
        }
    }

    /// Executes a single `KVM_RUN` and translates the exit reason.
    ///
    /// **Note**: This method does NOT call read handlers. For `IoIn` and
    /// `MmioRead` exits, the guest receives KVM's default data (typically
    /// `0xFF`). Use [`run_loop`](Self::run_loop) for full device emulation.
    ///
    /// # Errors
    ///
    /// Returns [`VcpuError`] on KVM run failure or fatal exit.
    pub fn run_once(&mut self) -> Result<VmExit, VcpuError> {
        match self.fd.run() {
            Ok(VcpuExit::IoIn(port, data)) => Ok(VmExit::IoIn {
                port,
                size: data.len(),
            }),
            Ok(VcpuExit::IoOut(port, data)) => Ok(VmExit::IoOut {
                port,
                data: ExitData::from_slice(data),
            }),
            Ok(VcpuExit::MmioRead(addr, data)) => Ok(VmExit::MmioRead {
                addr,
                size: data.len(),
            }),
            Ok(VcpuExit::MmioWrite(addr, data)) => Ok(VmExit::MmioWrite {
                addr,
                data: ExitData::from_slice(data),
            }),
            Ok(VcpuExit::Shutdown) => Ok(VmExit::Shutdown),
            Ok(VcpuExit::SystemEvent(event_type, _flags)) => {
                // KVM_SYSTEM_EVENT_SHUTDOWN = 1, KVM_SYSTEM_EVENT_RESET = 2
                if event_type == 2 {
                    Ok(VmExit::Reboot)
                } else {
                    Ok(VmExit::Shutdown)
                }
            }
            Ok(VcpuExit::FailEntry(reason, cpu)) => Err(VcpuError::FailEntry { reason, cpu }),
            Ok(VcpuExit::InternalError) => Err(VcpuError::InternalError),
            // Hlt + unknown exits → halt. Shutdown/SystemEvent/FailEntry/InternalError
            // are matched above, so this only catches Hlt and future KVM exit types.
            Ok(VcpuExit::Hlt | _) => Ok(VmExit::Halt),
            Err(e) => {
                let errno = e.errno();
                if errno == libc::EAGAIN || errno == libc::EINTR {
                    Ok(VmExit::Halt)
                } else {
                    Err(VcpuError::Run(std::io::Error::from_raw_os_error(errno)))
                }
            }
        }
    }

    // ── Register Setup ─────────────────────────────────────────────────

    /// Sets CPUID entries from the host's supported set.
    fn set_cpuid(
        &self,
        kvm: &Kvm,
        guest_virtualization: GuestVirtualizationMode,
    ) -> Result<(), VcpuError> {
        let mut cpuid = kvm
            .get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)
            .map_err(|e| VcpuError::GetCpuid(std::io::Error::from_raw_os_error(e.errno())))?;
        crate::guest_virtualization::apply_supported_cpuid(
            guest_virtualization,
            cpuid.as_mut_slice(),
        );
        self.fd
            .set_cpuid2(&cpuid)
            .map_err(|e| VcpuError::SetCpuid(std::io::Error::from_raw_os_error(e.errno())))?;
        Ok(())
    }

    /// Sets general-purpose registers (RIP, RSP, RBP, RSI, RFLAGS).
    fn set_base_regs(&self, config: &BootConfig) -> Result<(), VcpuError> {
        let regs = kvm_regs {
            rflags: 0x0000_0000_0000_0002, // Reserved bit 1 must be set
            rip: config.entry_point,
            rsp: config.stack_pointer,
            rbp: config.stack_pointer,
            rsi: config.boot_params_addr,
            ..Default::default()
        };
        self.fd
            .set_regs(&regs)
            .map_err(|e| VcpuError::SetRegs(std::io::Error::from_raw_os_error(e.errno())))
    }

    /// Sets special registers: GDT, IDT, segment selectors, CR0/CR3/CR4, EFER.
    fn set_special_regs(&self, config: &BootConfig) -> Result<(), VcpuError> {
        let mut sregs: kvm_sregs = self
            .fd
            .get_sregs()
            .map_err(|e| VcpuError::GetSregs(std::io::Error::from_raw_os_error(e.errno())))?;

        // Build GDT entries (must match what write_gdt wrote to guest memory)
        let gdt_table: [u64; boot::BOOT_GDT_COUNT] = [
            gdt_entry(0, 0, 0),                           // NULL
            gdt_entry(boot::GDT_FLAGS_CODE, 0, 0xf_ffff), // CODE
            gdt_entry(boot::GDT_FLAGS_DATA, 0, 0xf_ffff), // DATA
            gdt_entry(boot::GDT_FLAGS_TSS, 0, 0xf_ffff),  // TSS
        ];

        let code_seg = kvm_segment_from_gdt(gdt_table[1], 1);
        let data_seg = kvm_segment_from_gdt(gdt_table[2], 2);
        let tss_seg = kvm_segment_from_gdt(gdt_table[3], 3);

        // GDT register
        sregs.gdt.base = boot::BOOT_GDT_OFFSET;
        sregs.gdt.limit = u16::try_from(mem::size_of_val(&gdt_table)).unwrap_or(u16::MAX) - 1;

        // IDT register
        sregs.idt.base = boot::BOOT_IDT_OFFSET;
        sregs.idt.limit = u16::try_from(mem::size_of::<u64>()).unwrap_or(u16::MAX) - 1;

        // Segment selectors
        sregs.cs = code_seg;
        sregs.ds = data_seg;
        sregs.es = data_seg;
        sregs.fs = data_seg;
        sregs.gs = data_seg;
        sregs.ss = data_seg;
        sregs.tr = tss_seg;

        // 64-bit long mode
        sregs.cr0 |= boot::X86_CR0_PE | boot::X86_CR0_PG;
        sregs.cr3 = config.pml4_addr;
        sregs.cr4 |= boot::X86_CR4_PAE;
        sregs.efer |= boot::EFER_LME | boot::EFER_LMA;

        self.fd
            .set_sregs(&sregs)
            .map_err(|e| VcpuError::SetSregs(std::io::Error::from_raw_os_error(e.errno())))
    }

    /// Sets FPU to the initial state expected by Linux.
    fn set_fpu(&self) -> Result<(), VcpuError> {
        let fpu = kvm_fpu {
            fcw: 0x37f,
            mxcsr: 0x1f80,
            ..Default::default()
        };
        self.fd
            .set_fpu(&fpu)
            .map_err(|e| VcpuError::SetFpu(std::io::Error::from_raw_os_error(e.errno())))
    }

    /// Sets boot MSRs required by the Linux kernel.
    fn set_boot_msrs(&self) -> Result<(), VcpuError> {
        let msr_default = |index| kvm_msr_entry {
            index,
            data: 0,
            ..Default::default()
        };

        let entries = [
            msr_default(MSR_IA32_SYSENTER_CS),
            msr_default(MSR_IA32_SYSENTER_ESP),
            msr_default(MSR_IA32_SYSENTER_EIP),
            msr_default(MSR_STAR),
            msr_default(MSR_CSTAR),
            msr_default(MSR_KERNEL_GS_BASE),
            msr_default(MSR_SYSCALL_MASK),
            msr_default(MSR_LSTAR),
            msr_default(MSR_IA32_TSC),
            kvm_msr_entry {
                index: MSR_IA32_MISC_ENABLE,
                data: MSR_IA32_MISC_ENABLE_FAST_STRING,
                ..Default::default()
            },
            kvm_msr_entry {
                index: MSR_MTRR_DEF_TYPE,
                // Enable MTRRs (bit 11) + write-back default type (6)
                data: (1 << 11) | 6,
                ..Default::default()
            },
        ];

        let msrs = Msrs::from_entries(&entries).map_err(|_| {
            VcpuError::SetMsrs(std::io::Error::other("failed to create MSR wrapper"))
        })?;

        let written = self
            .fd
            .set_msrs(&msrs)
            .map_err(|e| VcpuError::SetMsrs(std::io::Error::from_raw_os_error(e.errno())))?;

        let total = entries.len();
        if written != total {
            return Err(VcpuError::MsrsIncomplete { written, total });
        }

        Ok(())
    }
}

// ── GDT Segment Helpers ────────────────────────────────────────────────

#[cfg(target_os = "linux")]
/// Extracts the base address from a GDT entry.
fn get_base(entry: u64) -> u64 {
    ((entry & 0xFF00_0000_0000_0000) >> 32)
        | ((entry & 0x0000_00FF_0000_0000) >> 16)
        | ((entry & 0x0000_0000_FFFF_0000) >> 16)
}

#[cfg(target_os = "linux")]
/// Extracts the limit from a GDT entry, handling the G flag.
fn get_limit(entry: u64) -> u32 {
    #[allow(clippy::cast_possible_truncation)]
    let limit = (((entry & 0x000F_0000_0000_0000) >> 32) | (entry & 0x0000_0000_0000_FFFF)) as u32;

    if (entry & 0x0080_0000_0000_0000) != 0 {
        // G flag set — scale by 4 KiB
        (limit << 12) | 0xFFF
    } else {
        limit
    }
}

#[cfg(target_os = "linux")]
/// Builds a `kvm_segment` from a GDT entry and table index.
fn kvm_segment_from_gdt(entry: u64, table_index: u8) -> kvm_bindings::kvm_segment {
    kvm_bindings::kvm_segment {
        base: get_base(entry),
        limit: get_limit(entry),
        selector: u16::from(table_index) * 8,
        type_: ((entry & 0x0000_0F00_0000_0000) >> 40) as u8,
        present: ((entry & 0x0000_8000_0000_0000) >> 47) as u8,
        dpl: ((entry & 0x0000_6000_0000_0000) >> 45) as u8,
        db: ((entry & 0x0040_0000_0000_0000) >> 54) as u8,
        s: ((entry & 0x0000_1000_0000_0000) >> 44) as u8,
        l: ((entry & 0x0020_0000_0000_0000) >> 53) as u8,
        g: ((entry & 0x0080_0000_0000_0000) >> 55) as u8,
        avl: ((entry & 0x0010_0000_0000_0000) >> 52) as u8,
        padding: 0,
        unusable: u8::from(((entry & 0x0000_8000_0000_0000) >> 47) == 0),
    }
}

// ── MSR Constants ──────────────────────────────────────────────────────
// From linux/arch/x86/include/uapi/asm/msr-index.h

#[cfg(target_os = "linux")]
/// `IA32_SYSENTER_CS`
const MSR_IA32_SYSENTER_CS: u32 = 0x0000_0174;
#[cfg(target_os = "linux")]
/// `IA32_SYSENTER_ESP`
const MSR_IA32_SYSENTER_ESP: u32 = 0x0000_0175;
#[cfg(target_os = "linux")]
/// `IA32_SYSENTER_EIP`
const MSR_IA32_SYSENTER_EIP: u32 = 0x0000_0176;
#[cfg(target_os = "linux")]
/// `MSR_STAR` — syscall target address.
const MSR_STAR: u32 = 0xc000_0081;
#[cfg(target_os = "linux")]
/// `MSR_CSTAR` — compat mode syscall target.
const MSR_CSTAR: u32 = 0xc000_0083;
#[cfg(target_os = "linux")]
/// `MSR_KERNEL_GS_BASE` — swap target for GS base.
const MSR_KERNEL_GS_BASE: u32 = 0xc000_0102;
#[cfg(target_os = "linux")]
/// `MSR_SYSCALL_MASK` — syscall flag mask.
const MSR_SYSCALL_MASK: u32 = 0xc000_0084;
#[cfg(target_os = "linux")]
/// `MSR_LSTAR` — long mode syscall target.
const MSR_LSTAR: u32 = 0xc000_0082;
#[cfg(target_os = "linux")]
/// `IA32_TSC` — timestamp counter.
const MSR_IA32_TSC: u32 = 0x0000_0010;
#[cfg(target_os = "linux")]
/// `IA32_MISC_ENABLE` — miscellaneous feature control.
const MSR_IA32_MISC_ENABLE: u32 = 0x0000_01a0;
#[cfg(target_os = "linux")]
/// Fast string operations enable bit in `IA32_MISC_ENABLE`.
const MSR_IA32_MISC_ENABLE_FAST_STRING: u64 = 1;
#[cfg(target_os = "linux")]
/// `MTRRdefType` — default memory type register.
const MSR_MTRR_DEF_TYPE: u32 = 0x0000_02ff;

#[cfg(all(test, target_os = "linux"))]
#[path = "vcpu_test.rs"]
mod tests;
