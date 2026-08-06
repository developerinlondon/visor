//! Portable register types for vCPUs.
//!
//! These types abstract over platform-specific register representations.
//! On `x86_64` they map to KVM's `kvm_regs`/`kvm_sregs` layout.
//! On `aarch64` they map to ARM64 general-purpose and system registers
//! as exposed by Apple's Hypervisor.framework.
//!
//! The struct fields are `cfg`-gated by `target_arch` so each architecture
//! gets the appropriate register set at compile time.

// ── General-purpose registers ──────────────────────────────────────

/// General-purpose registers.
///
/// On `x86_64`: RAX–R15, RIP, RFLAGS (18 registers).
/// On `aarch64`: X0–X30, SP, PC, CPSR, FPCR, FPSR (35 registers).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
#[cfg(target_arch = "x86_64")]
pub struct StandardRegs {
    /// Accumulator register.
    pub rax: u64,
    /// Base register.
    pub rbx: u64,
    /// Counter register.
    pub rcx: u64,
    /// Data register.
    pub rdx: u64,
    /// Source index register.
    pub rsi: u64,
    /// Destination index register.
    pub rdi: u64,
    /// Stack pointer.
    pub rsp: u64,
    /// Base pointer.
    pub rbp: u64,
    /// Extended register 8.
    pub r8: u64,
    /// Extended register 9.
    pub r9: u64,
    /// Extended register 10.
    pub r10: u64,
    /// Extended register 11.
    pub r11: u64,
    /// Extended register 12.
    pub r12: u64,
    /// Extended register 13.
    pub r13: u64,
    /// Extended register 14.
    pub r14: u64,
    /// Extended register 15.
    pub r15: u64,
    /// Instruction pointer.
    pub rip: u64,
    /// Flags register.
    pub rflags: u64,
}

/// General-purpose registers.
///
/// On `aarch64`: X0–X30, SP, PC, CPSR, FPCR, FPSR.
/// Maps to Apple Hypervisor.framework `hv_reg_t` values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
#[cfg(target_arch = "aarch64")]
pub struct StandardRegs {
    /// General-purpose registers X0–X30.
    pub x: [u64; 31],
    /// Stack pointer (SP / X31).
    pub sp: u64,
    /// Program counter.
    pub pc: u64,
    /// Current Program Status Register (PSTATE).
    pub cpsr: u64,
    /// Floating-point Control Register.
    pub fpcr: u64,
    /// Floating-point Status Register.
    pub fpsr: u64,
}

// ── Special (system) registers ─────────────────────────────────────

/// Special (system) registers for `x86_64`.
///
/// Contains segment registers, descriptor tables, control registers, and
/// model-specific registers needed for vCPU initialization.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
#[cfg(target_arch = "x86_64")]
pub struct SpecialRegs {
    /// Code segment.
    pub cs: SegmentReg,
    /// Data segment.
    pub ds: SegmentReg,
    /// Extra segment.
    pub es: SegmentReg,
    /// FS segment.
    pub fs: SegmentReg,
    /// GS segment.
    pub gs: SegmentReg,
    /// Stack segment.
    pub ss: SegmentReg,
    /// Task register.
    pub tr: SegmentReg,
    /// Local descriptor table register.
    pub ldt: SegmentReg,
    /// Global descriptor table.
    pub gdt: DescriptorTable,
    /// Interrupt descriptor table.
    pub idt: DescriptorTable,
    /// Control register 0 (protection enable, paging, etc.).
    pub cr0: u64,
    /// Control register 2 (page-fault linear address).
    pub cr2: u64,
    /// Control register 3 (page directory base / PML4).
    pub cr3: u64,
    /// Control register 4 (PAE, PSE, etc.).
    pub cr4: u64,
    /// Control register 8 (task priority).
    pub cr8: u64,
    /// Extended Feature Enable Register (long mode, NX, etc.).
    pub efer: u64,
    /// Local APIC base address.
    pub apic_base: u64,
    /// Interrupt bitmap (4 × 64 bits = 256 IRQ lines).
    pub interrupt_bitmap: [u64; 4],
}

/// Special (system) registers for `aarch64`.
///
/// Contains EL1 system registers needed for vCPU initialization
/// on ARM64 platforms (Apple Hypervisor.framework).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
#[cfg(target_arch = "aarch64")]
pub struct SpecialRegs {
    /// System Control Register (EL1).
    pub sctlr_el1: u64,
    /// Translation Table Base Register 0 (EL1).
    pub ttbr0_el1: u64,
    /// Translation Table Base Register 1 (EL1).
    pub ttbr1_el1: u64,
    /// Translation Control Register (EL1).
    pub tcr_el1: u64,
    /// Memory Attribute Indirection Register (EL1).
    pub mair_el1: u64,
    /// Vector Base Address Register (EL1).
    pub vbar_el1: u64,
    /// Saved Program Status Register (EL1).
    pub spsr_el1: u64,
    /// Exception Link Register (EL1).
    pub elr_el1: u64,
    /// Stack Pointer (EL0).
    pub sp_el0: u64,
    /// Stack Pointer (EL1).
    pub sp_el1: u64,
    /// Exception Syndrome Register (EL1).
    pub esr_el1: u64,
    /// Fault Address Register (EL1).
    pub far_el1: u64,
    /// Physical Address Register (EL1).
    pub par_el1: u64,
    /// Architectural Feature Access Control Register (EL1).
    pub cpacr_el1: u64,
    /// Counter-timer Kernel Control Register (EL1).
    pub cntkctl_el1: u64,
    /// Counter-timer Virtual Timer Control Register (EL0).
    pub cntv_ctl_el0: u64,
    /// Counter-timer Virtual Timer Compare Value (EL0).
    pub cntv_cval_el0: u64,
    /// Thread Pointer ID Register (EL0).
    pub tpidr_el0: u64,
    /// Thread Pointer ID Register, read-only (EL0).
    pub tpidrro_el0: u64,
    /// Thread Pointer ID Register (EL1).
    pub tpidr_el1: u64,
    /// Context ID Register (EL1).
    pub contextidr_el1: u64,
    /// Auxiliary Memory Attribute Indirection Register (EL1).
    pub amair_el1: u64,
    /// Auxiliary Fault Status Register 0 (EL1).
    pub afsr0_el1: u64,
    /// Auxiliary Fault Status Register 1 (EL1).
    pub afsr1_el1: u64,
    /// MIDR value (read-only, for reference).
    pub midr_el1: u64,
    /// Multiprocessor Affinity Register (read-only, set via MPIDR).
    pub mpidr_el1: u64,
}

// ── x86_64-only supporting types ───────────────────────────────────

/// Portable segment register descriptor.
///
/// Matches the fields of KVM's `kvm_segment` structure using portable types.
/// Only available on `x86_64`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
#[cfg(target_arch = "x86_64")]
pub struct SegmentReg {
    /// Segment base address.
    pub base: u64,
    /// Segment limit.
    pub limit: u32,
    /// Segment selector (index into GDT/LDT).
    pub selector: u16,
    /// Segment type (code/data, read/write/exec, accessed).
    pub type_: u8,
    /// Present bit.
    pub present: u8,
    /// Descriptor privilege level (0–3).
    pub dpl: u8,
    /// Default operation size (0 = 16-bit, 1 = 32-bit).
    pub db: u8,
    /// Descriptor type (0 = system, 1 = code/data).
    pub s: u8,
    /// Long mode flag (1 = 64-bit code segment).
    pub l: u8,
    /// Granularity (0 = byte, 1 = 4 KiB).
    pub g: u8,
    /// Available for system software.
    pub avl: u8,
}

/// Descriptor table register (GDT or IDT).
///
/// Only available on `x86_64`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
#[non_exhaustive]
#[cfg(target_arch = "x86_64")]
pub struct DescriptorTable {
    /// Linear base address of the table.
    pub base: u64,
    /// Table size in bytes minus one.
    pub limit: u16,
}

// ── Display implementations ─────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
impl std::fmt::Display for StandardRegs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "  RIP = {:#018x}  RSP = {:#018x}", self.rip, self.rsp)?;
        writeln!(f, "  RAX = {:#018x}  RBX = {:#018x}", self.rax, self.rbx)?;
        writeln!(f, "  RCX = {:#018x}  RDX = {:#018x}", self.rcx, self.rdx)?;
        writeln!(f, "  RSI = {:#018x}  RDI = {:#018x}", self.rsi, self.rdi)?;
        writeln!(f, "  R8  = {:#018x}  R9  = {:#018x}", self.r8, self.r9)?;
        writeln!(f, "  R10 = {:#018x}  R11 = {:#018x}", self.r10, self.r11)?;
        writeln!(f, "  R12 = {:#018x}  R13 = {:#018x}", self.r12, self.r13)?;
        writeln!(f, "  R14 = {:#018x}  R15 = {:#018x}", self.r14, self.r15)?;
        write!(
            f,
            "  RBP = {:#018x}  RFLAGS = {:#018x}",
            self.rbp, self.rflags
        )
    }
}

#[cfg(target_arch = "x86_64")]
impl std::fmt::Display for SpecialRegs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "  CR0  = {:#018x}  CR3  = {:#018x}", self.cr0, self.cr3)?;
        writeln!(f, "  CR4  = {:#018x}  EFER = {:#018x}", self.cr4, self.efer)?;
        writeln!(f, "  CR2  = {:#018x}  CR8  = {:#018x}", self.cr2, self.cr8)?;
        writeln!(f, "  APIC = {:#018x}", self.apic_base)?;
        writeln!(
            f,
            "  CS   = sel={:#06x} base={:#018x} limit={:#010x} type={:#04x}",
            self.cs.selector, self.cs.base, self.cs.limit, self.cs.type_,
        )?;
        writeln!(
            f,
            "  DS   = sel={:#06x} base={:#018x} limit={:#010x}",
            self.ds.selector, self.ds.base, self.ds.limit,
        )?;
        writeln!(
            f,
            "  GDT  = base={:#018x} limit={:#06x}",
            self.gdt.base, self.gdt.limit,
        )?;
        write!(
            f,
            "  IDT  = base={:#018x} limit={:#06x}",
            self.idt.base, self.idt.limit,
        )
    }
}

#[cfg(target_arch = "aarch64")]
impl std::fmt::Display for StandardRegs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "  PC   = {:#018x}  SP   = {:#018x}", self.pc, self.sp)?;
        writeln!(
            f,
            "  CPSR = {:#018x}  FPCR = {:#018x}",
            self.cpsr, self.fpcr
        )?;
        writeln!(f, "  FPSR = {:#018x}", self.fpsr)?;
        for i in (0..31).step_by(2) {
            if i + 1 < 31 {
                writeln!(
                    f,
                    "  X{:<3} = {:#018x}  X{:<3} = {:#018x}",
                    i,
                    self.x[i],
                    i + 1,
                    self.x[i + 1],
                )?;
            } else {
                write!(f, "  X{:<3} = {:#018x}", i, self.x[i])?;
            }
        }
        Ok(())
    }
}

#[cfg(target_arch = "aarch64")]
impl std::fmt::Display for SpecialRegs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        writeln!(f, "  SCTLR_EL1  = {:#018x}", self.sctlr_el1)?;
        writeln!(f, "  TTBR0_EL1  = {:#018x}", self.ttbr0_el1)?;
        writeln!(f, "  TTBR1_EL1  = {:#018x}", self.ttbr1_el1)?;
        writeln!(f, "  TCR_EL1    = {:#018x}", self.tcr_el1)?;
        writeln!(f, "  MAIR_EL1   = {:#018x}", self.mair_el1)?;
        writeln!(f, "  VBAR_EL1   = {:#018x}", self.vbar_el1)?;
        writeln!(f, "  SPSR_EL1   = {:#018x}", self.spsr_el1)?;
        writeln!(f, "  ELR_EL1    = {:#018x}", self.elr_el1)?;
        writeln!(f, "  SP_EL0     = {:#018x}", self.sp_el0)?;
        writeln!(f, "  SP_EL1     = {:#018x}", self.sp_el1)?;
        writeln!(f, "  ESR_EL1    = {:#018x}", self.esr_el1)?;
        writeln!(f, "  FAR_EL1    = {:#018x}", self.far_el1)?;
        writeln!(f, "  PAR_EL1    = {:#018x}", self.par_el1)?;
        writeln!(f, "  CPACR_EL1  = {:#018x}", self.cpacr_el1)?;
        writeln!(f, "  MIDR_EL1   = {:#018x}", self.midr_el1)?;
        write!(f, "  MPIDR_EL1  = {:#018x}", self.mpidr_el1)
    }
}

#[cfg(test)]
#[path = "regs_test.rs"]
mod tests;
