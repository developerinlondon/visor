use super::*;

// ── x86_64 tests ────────────────────────────────────────────────────

#[cfg(target_arch = "x86_64")]
#[test]
fn standard_regs_default_is_zeroed() {
    let regs = StandardRegs::default();
    assert_eq!(regs.rax, 0);
    assert_eq!(regs.rbx, 0);
    assert_eq!(regs.rcx, 0);
    assert_eq!(regs.rdx, 0);
    assert_eq!(regs.rsi, 0);
    assert_eq!(regs.rdi, 0);
    assert_eq!(regs.rsp, 0);
    assert_eq!(regs.rbp, 0);
    assert_eq!(regs.r8, 0);
    assert_eq!(regs.r9, 0);
    assert_eq!(regs.r10, 0);
    assert_eq!(regs.r11, 0);
    assert_eq!(regs.r12, 0);
    assert_eq!(regs.r13, 0);
    assert_eq!(regs.r14, 0);
    assert_eq!(regs.r15, 0);
    assert_eq!(regs.rip, 0);
    assert_eq!(regs.rflags, 0);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn special_regs_default_is_zeroed() {
    let sregs = SpecialRegs::default();
    assert_eq!(sregs.cr0, 0);
    assert_eq!(sregs.cr2, 0);
    assert_eq!(sregs.cr3, 0);
    assert_eq!(sregs.cr4, 0);
    assert_eq!(sregs.cr8, 0);
    assert_eq!(sregs.efer, 0);
    assert_eq!(sregs.apic_base, 0);
    assert_eq!(sregs.cs, SegmentReg::default());
    assert_eq!(sregs.ds, SegmentReg::default());
    assert_eq!(sregs.es, SegmentReg::default());
    assert_eq!(sregs.fs, SegmentReg::default());
    assert_eq!(sregs.gs, SegmentReg::default());
    assert_eq!(sregs.ss, SegmentReg::default());
    assert_eq!(sregs.tr, SegmentReg::default());
    assert_eq!(sregs.ldt, SegmentReg::default());
    assert_eq!(sregs.gdt, DescriptorTable::default());
    assert_eq!(sregs.idt, DescriptorTable::default());
    assert_eq!(sregs.interrupt_bitmap, [0u64; 4]);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn segment_reg_default_is_zeroed() {
    let seg = SegmentReg::default();
    assert_eq!(seg.base, 0);
    assert_eq!(seg.limit, 0);
    assert_eq!(seg.selector, 0);
    assert_eq!(seg.type_, 0);
    assert_eq!(seg.present, 0);
    assert_eq!(seg.dpl, 0);
    assert_eq!(seg.db, 0);
    assert_eq!(seg.s, 0);
    assert_eq!(seg.l, 0);
    assert_eq!(seg.g, 0);
    assert_eq!(seg.avl, 0);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn descriptor_table_default_is_zeroed() {
    let dt = DescriptorTable::default();
    assert_eq!(dt.base, 0);
    assert_eq!(dt.limit, 0);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn standard_regs_equality() {
    let a = StandardRegs {
        rax: 1,
        rbx: 2,
        rip: 0xDEAD_BEEF,
        rflags: 0x202,
        ..Default::default()
    };
    let b = a.clone();
    assert_eq!(a, b);
}

#[cfg(target_arch = "x86_64")]
#[test]
fn special_regs_equality() {
    let a = SpecialRegs {
        cr0: 0x8000_0011,
        cr3: 0x1000,
        efer: 0x501,
        ..Default::default()
    };
    let b = a.clone();
    assert_eq!(a, b);
}

// ── KVM round-trip conversions (Linux-only) ─────────────────────────

#[cfg(target_os = "linux")]
mod kvm_roundtrip {
    use super::*;
    use kvm_bindings::{kvm_regs, kvm_sregs};

    #[test]
    fn standard_regs_roundtrip_through_kvm_regs() {
        let original = StandardRegs {
            rax: 0x1111_1111_1111_1111,
            rbx: 0x2222_2222_2222_2222,
            rcx: 0x3333_3333_3333_3333,
            rdx: 0x4444_4444_4444_4444,
            rsi: 0x5555_5555_5555_5555,
            rdi: 0x6666_6666_6666_6666,
            rsp: 0x7777_7777_7777_7777,
            rbp: 0x8888_8888_8888_8888,
            r8: 0x9999_9999_9999_9999,
            r9: 0xAAAA_AAAA_AAAA_AAAA,
            r10: 0xBBBB_BBBB_BBBB_BBBB,
            r11: 0xCCCC_CCCC_CCCC_CCCC,
            r12: 0xDDDD_DDDD_DDDD_DDDD,
            r13: 0xEEEE_EEEE_EEEE_EEEE,
            r14: 0xFFFF_FFFF_FFFF_FFFF,
            r15: 0x0123_4567_89AB_CDEF,
            rip: 0xDEAD_BEEF_CAFE_BABE,
            rflags: 0x0000_0000_0000_0202,
        };

        let kvm: kvm_regs = original.clone().into();
        let roundtripped: StandardRegs = kvm.into();
        assert_eq!(original, roundtripped);
    }

    #[test]
    fn special_regs_roundtrip_preserves_control_regs() {
        let original = SpecialRegs {
            cr0: 0x8000_0011,
            cr2: 0xDEAD_0000,
            cr3: 0x0000_1000,
            cr4: 0x0000_0020,
            cr8: 0x0000_000F,
            efer: 0x0000_0501,
            apic_base: 0xFEE0_0900,
            gdt: DescriptorTable {
                base: 0x0000_0500,
                limit: 0x001F,
            },
            idt: DescriptorTable {
                base: 0x0000_0000,
                limit: 0x0007,
            },
            interrupt_bitmap: [0x01, 0x02, 0x03, 0x04],
            ..Default::default()
        };

        let kvm: kvm_sregs = original.clone().into();
        let roundtripped: SpecialRegs = kvm.into();

        assert_eq!(original.cr0, roundtripped.cr0);
        assert_eq!(original.cr2, roundtripped.cr2);
        assert_eq!(original.cr3, roundtripped.cr3);
        assert_eq!(original.cr4, roundtripped.cr4);
        assert_eq!(original.cr8, roundtripped.cr8);
        assert_eq!(original.efer, roundtripped.efer);
        assert_eq!(original.apic_base, roundtripped.apic_base);
        assert_eq!(original.gdt, roundtripped.gdt);
        assert_eq!(original.idt, roundtripped.idt);
        assert_eq!(original.interrupt_bitmap, roundtripped.interrupt_bitmap);
    }
}

// ── aarch64 tests ───────────────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
#[test]
fn standard_regs_default_is_zeroed() {
    let regs = StandardRegs::default();
    for i in 0..31 {
        assert_eq!(regs.x[i], 0, "x[{i}] should be zero");
    }
    assert_eq!(regs.sp, 0);
    assert_eq!(regs.pc, 0);
    assert_eq!(regs.cpsr, 0);
    assert_eq!(regs.fpcr, 0);
    assert_eq!(regs.fpsr, 0);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn special_regs_default_is_zeroed() {
    let sregs = SpecialRegs::default();
    assert_eq!(sregs.sctlr_el1, 0);
    assert_eq!(sregs.ttbr0_el1, 0);
    assert_eq!(sregs.ttbr1_el1, 0);
    assert_eq!(sregs.tcr_el1, 0);
    assert_eq!(sregs.mair_el1, 0);
    assert_eq!(sregs.vbar_el1, 0);
    assert_eq!(sregs.spsr_el1, 0);
    assert_eq!(sregs.elr_el1, 0);
    assert_eq!(sregs.sp_el0, 0);
    assert_eq!(sregs.sp_el1, 0);
    assert_eq!(sregs.esr_el1, 0);
    assert_eq!(sregs.far_el1, 0);
    assert_eq!(sregs.par_el1, 0);
    assert_eq!(sregs.cpacr_el1, 0);
    assert_eq!(sregs.midr_el1, 0);
    assert_eq!(sregs.mpidr_el1, 0);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn standard_regs_equality() {
    let mut a = StandardRegs::default();
    a.x[0] = 0x1234;
    a.pc = 0xDEAD_BEEF;
    a.sp = 0x8000;
    a.cpsr = 0x3C5;
    let b = a.clone();
    assert_eq!(a, b);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn special_regs_equality() {
    let a = SpecialRegs {
        sctlr_el1: 0x3050_5085,
        tcr_el1: 0x1_0000_351C,
        mair_el1: 0xFF44_0C04_0400,
        ..Default::default()
    };
    let b = a.clone();
    assert_eq!(a, b);
}

#[cfg(target_arch = "aarch64")]
#[test]
fn standard_regs_individual_gpr_set() {
    let mut regs = StandardRegs::default();
    for i in 0..31 {
        regs.x[i] = (i as u64) * 0x1111;
    }
    for i in 0..31 {
        assert_eq!(regs.x[i], (i as u64) * 0x1111);
    }
}

// ── Display tests (aarch64) ────────────────────────────────────────

#[cfg(target_arch = "aarch64")]
#[test]
fn standard_regs_display_shows_pc_sp_cpsr() {
    let mut regs = StandardRegs::default();
    regs.pc = 0x8000_0000;
    regs.sp = 0xFFFF_0000;
    regs.cpsr = 0x3C5;
    let output = format!("{regs}");
    assert!(output.contains("PC"), "should contain PC: {output}");
    assert!(
        output.contains("0x0000000080000000"),
        "should contain pc value: {output}"
    );
    assert!(output.contains("SP"), "should contain SP: {output}");
    assert!(output.contains("CPSR"), "should contain CPSR: {output}");
}

#[cfg(target_arch = "aarch64")]
#[test]
fn standard_regs_display_shows_gprs() {
    let mut regs = StandardRegs::default();
    regs.x[0] = 0x42;
    regs.x[30] = 0xFF;
    let output = format!("{regs}");
    assert!(output.contains("X0"), "should contain X0: {output}");
    assert!(output.contains("X30"), "should contain X30: {output}");
}

#[cfg(target_arch = "aarch64")]
#[test]
fn standard_regs_display_default_all_zeros() {
    let regs = StandardRegs::default();
    let output = format!("{regs}");
    // All values should be zero
    assert!(
        output.contains("0x0000000000000000"),
        "should show zero values: {output}"
    );
}

#[cfg(target_arch = "aarch64")]
#[test]
fn special_regs_display_shows_system_regs() {
    let mut sregs = SpecialRegs::default();
    sregs.sctlr_el1 = 0x3050_5085;
    sregs.tcr_el1 = 0x1_0000_351C;
    sregs.vbar_el1 = 0xFFFF_0000_0000_0000;
    let output = format!("{sregs}");
    assert!(
        output.contains("SCTLR_EL1"),
        "should contain SCTLR_EL1: {output}"
    );
    assert!(
        output.contains("TCR_EL1"),
        "should contain TCR_EL1: {output}"
    );
    assert!(
        output.contains("VBAR_EL1"),
        "should contain VBAR_EL1: {output}"
    );
    assert!(
        output.contains("TTBR0_EL1"),
        "should contain TTBR0_EL1: {output}"
    );
    assert!(
        output.contains("MAIR_EL1"),
        "should contain MAIR_EL1: {output}"
    );
}

#[cfg(target_arch = "aarch64")]
#[test]
fn special_regs_display_default_all_zeros() {
    let sregs = SpecialRegs::default();
    let output = format!("{sregs}");
    assert!(
        output.contains("SCTLR_EL1"),
        "should have system reg labels: {output}"
    );
    assert!(
        output.contains("0x0000000000000000"),
        "should show zero values: {output}"
    );
}
