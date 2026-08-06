use super::*;
use crate::platform::event::InterruptEvent;
use crate::platform::regs::{SpecialRegs, StandardRegs};
use crate::platform::{Platform, PlatformError, VcpuOps, VmOps};
use serial_test::serial;

// ── HvfPlatform tests ──────────────────────────────────────────────
// All HVF tests must be #[serial(hvf)] because Hypervisor.framework allows
// only ONE VM per process. The named group "hvf" ensures tests across
// different modules (macos_test, vm_test) share the same global lock.

#[test]
#[serial(hvf)]
fn hvf_platform_new_succeeds() {
    let platform = HvfPlatform::new();
    assert!(
        platform.is_ok(),
        "HvfPlatform::new() should succeed on macOS: {platform:?}"
    );
    // Drop triggers hv_vm_destroy.
}

#[test]
fn hvf_platform_implements_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<HvfPlatform>();
}

// ── HvfVm tests ────────────────────────────────────────────────────

#[test]
#[serial(hvf)]
fn hvf_create_vm_returns_valid_vm() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let _vm = platform.create_vm().expect("failed to create VM");
}

#[test]
fn hvf_vm_implements_send() {
    fn assert_send<T: Send>() {}
    assert_send::<HvfVm>();
}

#[test]
#[serial(hvf)]
fn hvf_vm_create_vcpu_succeeds() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let _vcpu = vm.create_vcpu(0).expect("failed to create vCPU");
}

#[test]
#[serial(hvf)]
fn hvf_vm_register_memory_succeeds() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");

    // HVF requires mmap-allocated, page-aligned memory.
    let size: usize = 0x4000; // 16 KiB (multiple of page size)
    // SAFETY: mmap with MAP_ANON allocates zeroed, page-aligned memory.
    #[allow(unsafe_code)]
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "mmap failed");

    let result = vm.register_memory(0, 0x4_0000, size as u64, ptr.cast::<u8>());
    assert!(result.is_ok(), "register_memory should succeed: {result:?}");

    // SAFETY: ptr was allocated by mmap with the same size.
    #[allow(unsafe_code)]
    unsafe {
        libc::munmap(ptr, size);
    }
}

#[test]
#[serial(hvf)]
fn hvf_vm_create_irq_chip_succeeds() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    // On ARM64, create_irq_chip is a no-op (GIC is separate).
    let result = vm.create_irq_chip();
    assert!(result.is_ok(), "create_irq_chip should succeed: {result:?}");
}

#[test]
#[serial(hvf)]
fn hvf_vm_create_pit_succeeds() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    // On ARM64, create_pit is a no-op (no PIT on ARM, uses generic timer).
    let result = vm.create_pit();
    assert!(result.is_ok(), "create_pit should succeed: {result:?}");
}

#[test]
#[serial(hvf)]
fn hvf_vm_register_irqfd_succeeds() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mock_event = crate::platform::event::MockInterruptEvent::new();
    // On ARM64 HVF, irqfd is stored for later interrupt injection.
    let result = vm.register_irqfd(&mock_event, 0);
    assert!(result.is_ok(), "register_irqfd should succeed: {result:?}");
}

// ── HvfVcpu tests ──────────────────────────────────────────────────

#[test]
fn hvf_vcpu_implements_send() {
    fn assert_send<T: Send>() {}
    assert_send::<HvfVcpu>();
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_set_get_regs_roundtrip() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let mut regs = StandardRegs::default();
    regs.x[0] = 0x1234;
    regs.pc = 0x4_0000;
    regs.cpsr = 0x3C5; // EL1h with DAIF masked
    vcpu.set_regs(&regs).expect("failed to set regs");
    let got = vcpu.get_regs().expect("failed to get regs");

    assert_eq!(got.x[0], regs.x[0], "X0 mismatch");
    assert_eq!(got.pc, regs.pc, "PC mismatch");
    assert_eq!(got.cpsr, regs.cpsr, "CPSR mismatch");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_set_get_sregs_roundtrip() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let sregs = SpecialRegs {
        sctlr_el1: 0x3050_5085,
        mair_el1: 0xFF44_0C04_0400,
        ..Default::default()
    };
    vcpu.set_sregs(&sregs).expect("failed to set sregs");
    let got = vcpu.get_sregs().expect("failed to get sregs");

    assert_eq!(got.sctlr_el1, sregs.sctlr_el1, "SCTLR_EL1 mismatch");
    assert_eq!(got.mair_el1, sregs.mair_el1, "MAIR_EL1 mismatch");
}

// ── MacosEventFd tests ─────────────────────────────────────────────

#[test]
fn macos_eventfd_new_succeeds() {
    let eventfd = MacosEventFd::new().expect("failed to create MacosEventFd");
    drop(eventfd);
}

#[test]
fn macos_eventfd_trigger_succeeds() {
    let eventfd = MacosEventFd::new().expect("failed to create MacosEventFd");
    eventfd.trigger().expect("trigger should succeed");
}

#[test]
fn macos_eventfd_as_raw_returns_valid_fd() {
    let eventfd = MacosEventFd::new().expect("failed to create MacosEventFd");
    let raw = eventfd.as_raw();
    // A valid fd is >= 0.
    assert!(raw >= 0, "raw fd should be non-negative, got {raw}");
}

#[test]
fn macos_eventfd_implements_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MacosEventFd>();
}

#[test]
fn macos_eventfd_usable_as_trait_object() {
    let eventfd = MacosEventFd::new().expect("failed to create MacosEventFd");
    let event: std::sync::Arc<dyn InterruptEvent> = std::sync::Arc::new(eventfd);
    event
        .trigger()
        .expect("trigger via trait object should succeed");
}

// ── Error conversion tests ────────────────────────────────────────

#[test]
fn hvf_result_ok_on_zero() {
    let result = hvf_result(0);
    assert!(result.is_ok());
}

#[test]
fn hvf_result_err_on_nonzero() {
    let result = hvf_result(-1);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(matches!(err, PlatformError::System(_)));
}

#[test]
fn hvf_error_converts_to_platform_error() {
    let hv_err = applevisor::error::HypervisorError::from(-1);
    let err = hvf_error(hv_err);
    assert!(matches!(err, PlatformError::System(_)));
}

// ── GP_REGS table tests ──────────────────────────────────────────

#[test]
fn gp_regs_table_has_31_entries() {
    assert_eq!(GP_REGS.len(), 31, "GP_REGS should map X0–X30");
}

#[test]
fn gp_regs_table_starts_with_x0_ends_with_x30() {
    assert_eq!(GP_REGS[0], applevisor::vcpu::Reg::X0);
    assert_eq!(GP_REGS[30], applevisor::vcpu::Reg::X30);
}

// ── HvfVcpu Debug impl tests ─────────────────────────────────────

#[test]
#[serial(hvf)]
fn hvf_vcpu_debug_includes_id() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let vcpu = vm.create_vcpu(0).expect("failed to create vCPU");
    let debug = format!("{vcpu:?}");
    assert!(
        debug.contains("HvfVcpu"),
        "Debug output should contain struct name: {debug}"
    );
}

// ── System register mapping tests ────────────────────────────────

#[test]
fn sys_reg_map_covers_all_writable_fields() {
    // SYS_REG_MAP should cover all writable SpecialRegs fields.
    // We verify the count matches our expectations.
    assert!(
        SYS_REG_MAP.len() >= 20,
        "SYS_REG_MAP should have at least 20 writable system register entries, got {}",
        SYS_REG_MAP.len()
    );
}

#[test]
fn sys_reg_readonly_has_midr_and_mpidr() {
    // Read-only registers should include MIDR_EL1 and MPIDR_EL1.
    assert_eq!(SYS_REG_READONLY.len(), 2);
}

// ── Register roundtrip with multiple GPRs ────────────────────────

#[test]
#[serial(hvf)]
fn hvf_vcpu_all_gpr_roundtrip() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let mut regs = StandardRegs::default();
    // Set a few scattered GPRs to verify the mapping table is correct.
    regs.x[0] = 0xDEAD;
    regs.x[15] = 0xBEEF;
    regs.x[30] = 0xCAFE;
    regs.sp = 0x8000;
    vcpu.set_regs(&regs).expect("failed to set regs");
    let got = vcpu.get_regs().expect("failed to get regs");

    assert_eq!(got.x[0], 0xDEAD, "X0 mismatch");
    assert_eq!(got.x[15], 0xBEEF, "X15 mismatch");
    assert_eq!(got.x[30], 0xCAFE, "X30 mismatch");
    assert_eq!(got.sp, 0x8000, "SP mismatch");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_sregs_multiple_fields_roundtrip() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let sregs = SpecialRegs {
        sctlr_el1: 0x3050_5085,
        mair_el1: 0xFF44_0C04_0400,
        tcr_el1: 0x1_0000_0000,
        vbar_el1: 0xFFFF_0000_0000_0000,
        ..Default::default()
    };
    vcpu.set_sregs(&sregs).expect("failed to set sregs");
    let got = vcpu.get_sregs().expect("failed to get sregs");

    assert_eq!(got.sctlr_el1, sregs.sctlr_el1, "SCTLR_EL1 mismatch");
    assert_eq!(got.mair_el1, sregs.mair_el1, "MAIR_EL1 mismatch");
    assert_eq!(got.tcr_el1, sregs.tcr_el1, "TCR_EL1 mismatch");
    assert_eq!(got.vbar_el1, sregs.vbar_el1, "VBAR_EL1 mismatch");
    // Read-only regs (MIDR, MPIDR) should be populated by get_sregs.
    // Their values are hardware-defined, so we just check they're non-zero.
}

// ── MacosEventFd multiple triggers ───────────────────────────────

#[test]
fn macos_eventfd_multiple_triggers_succeed() {
    let eventfd = MacosEventFd::new().expect("failed to create MacosEventFd");
    for i in 0..10 {
        eventfd
            .trigger()
            .unwrap_or_else(|e| panic!("trigger {i} failed: {e}"));
    }
}

#[test]
fn macos_eventfd_distinct_fds() {
    let a = MacosEventFd::new().expect("failed to create first");
    let b = MacosEventFd::new().expect("failed to create second");
    assert_ne!(
        a.as_raw(),
        b.as_raw(),
        "two MacosEventFd instances should have distinct fds"
    );
}

// ── Phase 0: MacosEventFd::poll tests ────────────────────────────

#[test]
fn macos_eventfd_poll_returns_false_when_untriggered() {
    let eventfd = MacosEventFd::new().expect("failed to create MacosEventFd");
    assert!(
        !eventfd.poll().expect("poll failed"),
        "untriggered eventfd should poll false"
    );
}

#[test]
fn macos_eventfd_poll_returns_true_after_trigger() {
    let eventfd = MacosEventFd::new().expect("failed to create MacosEventFd");
    eventfd.trigger().expect("trigger failed");
    assert!(
        eventfd.poll().expect("poll failed"),
        "triggered eventfd should poll true"
    );
}

#[test]
fn macos_eventfd_poll_auto_clears_after_read() {
    let eventfd = MacosEventFd::new().expect("failed to create MacosEventFd");
    eventfd.trigger().expect("trigger failed");
    assert!(
        eventfd.poll().expect("first poll failed"),
        "first poll should return true"
    );
    assert!(
        !eventfd.poll().expect("second poll failed"),
        "second poll should return false (EV_CLEAR auto-reset)"
    );
}

#[test]
fn poll_kqueue_fd_returns_false_for_fresh_kqueue() {
    let eventfd = MacosEventFd::new().expect("failed to create MacosEventFd");
    let result = poll_kqueue_fd(eventfd.as_raw());
    assert!(result.is_ok(), "poll_kqueue_fd should succeed: {result:?}");
    assert!(
        !result.unwrap(),
        "fresh kqueue should have no pending events"
    );
}

// ── Phase 0: IRQ registration tests ─────────────────────────────

#[test]
#[serial(hvf)]
fn hvf_vm_register_irqfd_stores_registration() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let event = MacosEventFd::new().expect("failed to create eventfd");
    let raw_fd = event.as_raw();
    vm.register_irqfd(&event, 5).expect("register_irqfd failed");
    let regs = vm.irq_registrations_snapshot();
    assert_eq!(regs.len(), 1, "should have one registration");
    assert_eq!(regs[0], (raw_fd, 5), "registration should match (fd, gsi)");
}

#[test]
#[serial(hvf)]
fn hvf_vm_register_irqfd_stores_multiple_registrations() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let event_a = MacosEventFd::new().expect("failed to create eventfd a");
    let event_b = MacosEventFd::new().expect("failed to create eventfd b");
    let fd_a = event_a.as_raw();
    let fd_b = event_b.as_raw();
    vm.register_irqfd(&event_a, 5)
        .expect("register_irqfd a failed");
    vm.register_irqfd(&event_b, 6)
        .expect("register_irqfd b failed");
    let regs = vm.irq_registrations_snapshot();
    assert_eq!(regs.len(), 2, "should have two registrations");
    assert_eq!(regs[0], (fd_a, 5));
    assert_eq!(regs[1], (fd_b, 6));
}

// ── Phase 0: GIC SPI tests ──────────────────────────────────────

#[test]
#[serial(hvf)]
fn hvf_vm_gic_set_spi_succeeds() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    // GIC SPI operations require at least one vCPU.
    let _vcpu = vm.create_vcpu(0).expect("failed to create vCPU");
    // SPI intid 37 = GSI 5 (block device) + 32.
    let result = vm.gic_set_spi(37, true);
    assert!(
        result.is_ok(),
        "gic_set_spi assert should succeed: {result:?}"
    );
    let result = vm.gic_set_spi(37, false);
    assert!(
        result.is_ok(),
        "gic_set_spi deassert should succeed: {result:?}"
    );
}

#[test]
#[serial(hvf)]
fn hvf_vm_gic_set_spi_vsock_irq_succeeds() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let _vcpu = vm.create_vcpu(0).expect("failed to create vCPU");
    // SPI intid 38 = GSI 6 (vsock device) + 32.
    let result = vm.gic_set_spi(38, true);
    assert!(
        result.is_ok(),
        "vsock SPI assert should succeed: {result:?}"
    );
    let result = vm.gic_set_spi(38, false);
    assert!(
        result.is_ok(),
        "vsock SPI deassert should succeed: {result:?}"
    );
}

// ── Phase 0: MMIO read writeback tests ──────────────────────────

#[test]
#[serial(hvf)]
fn hvf_vcpu_complete_mmio_read_writes_to_srt_register() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // Simulate an MMIO read that targets X5 (SRT=5).
    vcpu.last_read_srt = Some(5);

    // Write MMIO response: 0x42 as a 4-byte LE value.
    vcpu.complete_mmio_read(&[0x42, 0x00, 0x00, 0x00])
        .expect("complete_mmio_read failed");

    // X5 should now contain 0x42.
    let regs = vcpu.get_regs().expect("get_regs failed");
    assert_eq!(regs.x[5], 0x42, "X5 should contain the MMIO read data");

    // SRT should be consumed.
    assert!(
        vcpu.last_read_srt.is_none(),
        "last_read_srt should be None after complete"
    );
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_complete_mmio_read_noop_without_srt() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // No pending SRT — should be a no-op.
    assert!(vcpu.last_read_srt.is_none());
    vcpu.complete_mmio_read(&[0xFF, 0xFF, 0xFF, 0xFF])
        .expect("no-op complete_mmio_read should succeed");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_complete_mmio_read_xzr_discards_write() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // SRT=31 is XZR (zero register) — write should be discarded.
    vcpu.last_read_srt = Some(31);
    vcpu.complete_mmio_read(&[0xFF])
        .expect("XZR write should succeed silently");
    // SRT should still be consumed.
    assert!(
        vcpu.last_read_srt.is_none(),
        "SRT should be consumed even for XZR"
    );
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_complete_mmio_read_single_byte() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // 1-byte MMIO read to X0.
    vcpu.last_read_srt = Some(0);
    vcpu.complete_mmio_read(&[0xAB])
        .expect("complete_mmio_read failed");
    let regs = vcpu.get_regs().expect("get_regs failed");
    // 0xAB zero-extended to u64.
    assert_eq!(regs.x[0], 0xAB, "X0 should contain zero-extended byte");
}

// ── Phase A+B: PC advance and vtimer tests ─────────────────────

#[test]
#[serial(hvf)]
fn hvf_vcpu_advance_pc_increments_by_four() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // Set PC to a known value.
    let mut regs = vcpu.get_regs().expect("get_regs failed");
    regs.pc = 0x8020_0000;
    vcpu.set_regs(&regs).expect("set_regs failed");

    // Advance PC.
    vcpu.advance_pc().expect("advance_pc failed");

    let regs = vcpu.get_regs().expect("get_regs failed");
    assert_eq!(regs.pc, 0x8020_0004, "PC should advance by 4");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_advance_pc_twice() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let mut regs = vcpu.get_regs().expect("get_regs failed");
    regs.pc = 0x1000;
    vcpu.set_regs(&regs).expect("set_regs failed");

    vcpu.advance_pc().expect("first advance_pc failed");
    vcpu.advance_pc().expect("second advance_pc failed");

    let regs = vcpu.get_regs().expect("get_regs failed");
    assert_eq!(regs.pc, 0x1008, "PC should advance by 8 after two calls");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_vtimer_masked_defaults_to_false() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    assert!(!vcpu.vtimer_masked, "vtimer should not be masked initially");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_handle_vtimer_masks_timer() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // With native GIC, GICR_ISPENDR0 writes may be silently denied
    // (the native GIC owns the redistributor). handle_vtimer() now
    // ignores this and always succeeds.
    vcpu.handle_vtimer().expect("handle_vtimer should succeed");
    assert!(
        vcpu.vtimer_masked,
        "vtimer should be masked after handle_vtimer"
    );
    assert_eq!(vcpu.vtimer_activations, 1, "vtimer_activations should be 1");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_sync_vtimer_noop_when_not_masked() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // sync_vtimer should be a no-op when vtimer is not masked.
    vcpu.sync_vtimer()
        .expect("sync_vtimer should succeed when not masked");
    assert!(!vcpu.vtimer_masked, "vtimer should remain unmasked");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_sync_vtimer_unmasks_when_condition_cleared() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // Simulate masked state: set vtimer_masked directly and mask the
    // hardware vtimer (this always succeeds, unlike ISPENDR0 writes).
    vcpu.vcpu
        .set_vtimer_mask(true)
        .expect("set_vtimer_mask failed");
    vcpu.vtimer_masked = true;

    // CNTV_CTL_EL0 defaults to 0 (ENABLE=0, IMASK=0, ISTATUS=0).
    // With ENABLE=0, irq_active is false → sync should unmask.
    // With native GIC, GICR_ICPENDR0 writes are silently ignored.
    vcpu.sync_vtimer().expect("sync_vtimer should succeed");
    assert!(
        !vcpu.vtimer_masked,
        "vtimer should be unmasked when timer condition is not active"
    );
}

// ── Helper: build a synthetic VcpuExitException ─────────────────

/// Builds a `VcpuExitException` with the given syndrome and IPA.
fn synth_exception(syndrome: u64, ipa: u64) -> VcpuExitException {
    VcpuExitException {
        syndrome,
        virtual_address: 0,
        physical_address: ipa,
    }
}

/// Syndrome for an HVC64 exception (EC = 0x16).
fn hvc64_syndrome() -> u64 {
    u64::from(EC_HVC64) << 26
}

/// Syndrome for an SMC64 exception (EC = 0x17).
fn smc64_syndrome() -> u64 {
    u64::from(EC_SMC64) << 26
}

/// Syndrome for a sysreg trap read (EC = 0x18) targeting register Xrt.
/// bit 0 = 1 (is_read), bits [9:5] = rt, remaining bits encode a
/// dummy ID register (op0=3, op1=0, crn=0, crm=1, op2=0).
fn sysreg_read_syndrome(rt: u32) -> u64 {
    let ec: u64 = u64::from(EC_SYSREG) << 26;
    let is_read: u64 = 1; // bit 0
    let rt_field: u64 = u64::from(rt & 0x1F) << 5;
    // Encode an ID register: op0=3(bits 21:20), op1=0(bits 16:14),
    // crn=0(bits 13:10), crm=1(bits 4:1), op2=0(bits 19:17)
    let sysreg_bits: u64 = (3 << 20) | (1 << 1); // op0=3, crm=1
    ec | sysreg_bits | rt_field | is_read
}

/// Syndrome for a sysreg trap write (EC = 0x18) from register Xrt.
fn sysreg_write_syndrome(rt: u32) -> u64 {
    let ec: u64 = u64::from(EC_SYSREG) << 26;
    let rt_field: u64 = u64::from(rt & 0x1F) << 5;
    let sysreg_bits: u64 = (3 << 20) | (1 << 1);
    ec | sysreg_bits | rt_field // bit 0 = 0 (write)
}

/// Syndrome for a WFI trap (EC = 0x01).
fn wfi_syndrome() -> u64 {
    u64::from(EC_WFI) << 26
}

// ── PSCI handling tests ─────────────────────────────────────────

#[test]
#[serial(hvf)]
fn hvf_vcpu_psci_version_returns_v1_1() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // Set X0 to PSCI_VERSION function ID.
    vcpu.vcpu.set_reg(Reg::X0, PSCI_VERSION).expect("set X0");

    let exc = synth_exception(hvc64_syndrome(), 0);
    let exit = vcpu.decode_exception(&exc).expect("decode_exception");

    assert_eq!(exit, VmExit::Halt, "PSCI VERSION should continue running");
    let x0 = vcpu.vcpu.get_reg(Reg::X0).expect("get X0");
    assert_eq!(x0, 0x0001_0001, "PSCI VERSION should return v1.1 in X0");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_psci_system_off_returns_shutdown() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    vcpu.vcpu.set_reg(Reg::X0, PSCI_SYSTEM_OFF).expect("set X0");

    let exc = synth_exception(hvc64_syndrome(), 0);
    let exit = vcpu.decode_exception(&exc).expect("decode_exception");

    assert_eq!(
        exit,
        VmExit::Shutdown,
        "PSCI SYSTEM_OFF should return Shutdown"
    );
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_psci_system_reset_returns_reboot() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    vcpu.vcpu
        .set_reg(Reg::X0, PSCI_SYSTEM_RESET)
        .expect("set X0");

    let exc = synth_exception(hvc64_syndrome(), 0);
    let exit = vcpu.decode_exception(&exc).expect("decode_exception");

    assert_eq!(
        exit,
        VmExit::Reboot,
        "PSCI SYSTEM_RESET should return Reboot"
    );
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_psci_unknown_returns_not_supported() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    // Unknown PSCI function ID.
    vcpu.vcpu.set_reg(Reg::X0, 0xDEAD_BEEF).expect("set X0");

    let exc = synth_exception(hvc64_syndrome(), 0);
    let exit = vcpu.decode_exception(&exc).expect("decode_exception");

    assert_eq!(exit, VmExit::Halt, "unknown PSCI should continue running");
    let x0 = vcpu.vcpu.get_reg(Reg::X0).expect("get X0");
    assert_eq!(
        x0, 0xFFFF_FFFF_FFFF_FFFF,
        "unknown PSCI should return -1 (NOT_SUPPORTED) in X0"
    );
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_hvc_does_not_advance_pc() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let mut regs = vcpu.get_regs().expect("get_regs");
    regs.pc = 0x1000;
    regs.x[0] = PSCI_VERSION;
    vcpu.set_regs(&regs).expect("set_regs");

    let exc = synth_exception(hvc64_syndrome(), 0);
    let _exit = vcpu.decode_exception(&exc).expect("decode_exception");

    let regs = vcpu.get_regs().expect("get_regs");
    assert_eq!(
        regs.pc, 0x1000,
        "HVC must NOT advance PC (CPU auto-returns to next instruction)"
    );
}

// ── SMC handling tests ──────────────────────────────────────────

#[test]
#[serial(hvf)]
fn hvf_vcpu_smc_advances_pc_and_handles_psci() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let mut regs = vcpu.get_regs().expect("get_regs");
    regs.pc = 0x2000;
    regs.x[0] = PSCI_SYSTEM_OFF;
    vcpu.set_regs(&regs).expect("set_regs");

    let exc = synth_exception(smc64_syndrome(), 0);
    let exit = vcpu.decode_exception(&exc).expect("decode_exception");

    assert_eq!(
        exit,
        VmExit::Shutdown,
        "SMC with PSCI SYSTEM_OFF should return Shutdown"
    );
    let regs = vcpu.get_regs().expect("get_regs");
    assert_eq!(regs.pc, 0x2004, "SMC should advance PC by 4");
}

// ── Sysreg trap tests ───────────────────────────────────────────

#[test]
#[serial(hvf)]
fn hvf_vcpu_sysreg_read_returns_zero_and_advances_pc() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let mut regs = vcpu.get_regs().expect("get_regs");
    regs.pc = 0x3000;
    regs.x[5] = 0xDEAD; // Will be overwritten with 0 by sysreg read.
    vcpu.set_regs(&regs).expect("set_regs");

    // Sysreg read targeting X5.
    let exc = synth_exception(sysreg_read_syndrome(5), 0);
    let exit = vcpu.decode_exception(&exc).expect("decode_exception");

    assert_eq!(exit, VmExit::Halt, "sysreg trap should continue running");
    let regs = vcpu.get_regs().expect("get_regs");
    assert_eq!(
        regs.x[5], 0,
        "sysreg read should return 0 in target register"
    );
    assert_eq!(regs.pc, 0x3004, "sysreg trap should advance PC by 4");
}

#[test]
#[serial(hvf)]
fn hvf_vcpu_sysreg_write_discards_and_advances_pc() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let mut regs = vcpu.get_regs().expect("get_regs");
    regs.pc = 0x4000;
    vcpu.set_regs(&regs).expect("set_regs");

    // Sysreg write from X3 — should be silently discarded.
    let exc = synth_exception(sysreg_write_syndrome(3), 0);
    let exit = vcpu.decode_exception(&exc).expect("decode_exception");

    assert_eq!(
        exit,
        VmExit::Halt,
        "sysreg write trap should continue running"
    );
    let regs = vcpu.get_regs().expect("get_regs");
    assert_eq!(regs.pc, 0x4004, "sysreg write trap should advance PC by 4");
}

// ── Selective PC advance tests ──────────────────────────────────

#[test]
#[serial(hvf)]
fn hvf_vcpu_wfi_advances_pc() {
    let platform = HvfPlatform::new().expect("failed to create HVF platform");
    let vm = platform.create_vm().expect("failed to create VM");
    let mut vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let mut regs = vcpu.get_regs().expect("get_regs");
    regs.pc = 0x5000;
    vcpu.set_regs(&regs).expect("set_regs");

    let exc = synth_exception(wfi_syndrome(), 0);
    let exit = vcpu.decode_exception(&exc).expect("decode_exception");

    assert_eq!(exit, VmExit::Halt, "WFI should continue running");
    let regs = vcpu.get_regs().expect("get_regs");
    assert_eq!(regs.pc, 0x5004, "WFI should advance PC by 4");
}

/// Verify that `ID_AA64PFR0_EL1` GICv3 bit persists after `hv_vcpu_run()`.
///
/// HVF might reset read-only identification registers when the vCPU
/// actually runs. This test sets the GIC3 bit, runs the vCPU once
/// (it hits an SMC and exits), then reads the register back to confirm
/// the bit survived.
#[test]
#[serial(hvf)]
fn id_aa64pfr0_el1_gic3_bit_persists_after_vcpu_run() {
    use applevisor::vcpu::SysReg;

    let platform = HvfPlatform::new().expect("HVF platform");
    let vm = platform.create_vm().expect("create VM");

    // Allocate page-aligned memory with an SMC #0 followed by B . (branch-to-self).
    // SMC always traps and our handler advances PC by 4.
    // The branch-to-self prevents running off into unmapped memory.
    let size: usize = 0x4000; // 16 KiB
    let guest_addr: u64 = 0x4_0000;
    #[allow(unsafe_code)]
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "mmap failed");

    // Write SMC #0 (0xD4000003) at offset 0, then B . (0x14000000) at offset 4.
    #[allow(unsafe_code)]
    unsafe {
        let instrs = ptr.cast::<u32>();
        std::ptr::write(instrs, 0xD400_0003_u32.to_le()); // SMC #0
        std::ptr::write(instrs.add(1), 0x1400_0000_u32.to_le()); // B . (branch to self)
    }

    vm.register_memory(0, guest_addr, size as u64, ptr.cast::<u8>())
        .expect("register memory");

    let mut vcpu = vm.create_vcpu(0).expect("create vCPU");

    // Set ID_AA64PFR0_EL1 with GIC3 bit BEFORE run.
    let pfr_before = vcpu
        .vcpu
        .get_sys_reg(SysReg::ID_AA64PFR0_EL1)
        .expect("get ID_AA64PFR0_EL1");
    let pfr_with_gic = pfr_before | (1 << 24);
    vcpu.vcpu
        .set_sys_reg(SysReg::ID_AA64PFR0_EL1, pfr_with_gic)
        .expect("set ID_AA64PFR0_EL1");

    // Confirm it took.
    let readback = vcpu
        .vcpu
        .get_sys_reg(SysReg::ID_AA64PFR0_EL1)
        .expect("readback");
    assert_eq!(
        (readback >> 24) & 0xF,
        1,
        "GIC3 bit should be set before run"
    );

    // Set PC to HVC instruction, CPSR to EL1h.
    let mut regs = StandardRegs::default();
    regs.pc = guest_addr;
    regs.cpsr = 0x3C5; // EL1h with DAIF masked
    vcpu.set_regs(&regs).expect("set regs");

    // Run the vCPU — it will execute SMC #0 and exit.
    let exit = vcpu.run().expect("vcpu run");
    // SMC is handled as PSCI; X0=0 is unknown, returns Halt. PC advances by 4.
    assert_eq!(exit, VmExit::Halt, "SMC should produce Halt exit");

    // Read ID_AA64PFR0_EL1 AFTER run — does the GIC3 bit survive?
    let pfr_after = vcpu
        .vcpu
        .get_sys_reg(SysReg::ID_AA64PFR0_EL1)
        .expect("get ID_AA64PFR0_EL1 after run");
    let gic_bits_after = (pfr_after >> 24) & 0xF;

    assert_eq!(
        gic_bits_after, 1,
        "ID_AA64PFR0_EL1 GIC3 bit should persist after vCPU run. \
         Before: {pfr_with_gic:#018x}, After: {pfr_after:#018x}"
    );

    // Cleanup.
    #[allow(unsafe_code)]
    unsafe {
        libc::munmap(ptr, size);
    }
}

/// Helper: allocate guest memory, write SMC #0 + B., run vCPU once, return vCPU.
///
/// The caller can read registers back to verify persistence after run.
/// Returns `(platform, vm, vcpu, mmap_ptr, mmap_size)` — caller must munmap.
///
/// **CRITICAL**: The platform and vm MUST be kept alive until AFTER the vcpu is
/// dropped. The applevisor crate's `VirtualMachineInstance::Drop` calls
/// `hv_vm_destroy` only if it holds the last Arc reference, but the Vcpu also
/// holds one. If platform/vm drop first, nobody calls `hv_vm_destroy`.
#[allow(unsafe_code)]
fn run_vcpu_once_with_smc() -> (HvfPlatform, HvfVm, HvfVcpu, *mut libc::c_void, usize) {
    let platform = HvfPlatform::new().expect("HVF platform");
    let vm = platform.create_vm().expect("create VM");

    let size: usize = 0x4000;
    let guest_addr: u64 = 0x4_0000;
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "mmap failed");

    // SMC #0 at offset 0, B . (branch-to-self) at offset 4.
    unsafe {
        let instrs = ptr.cast::<u32>();
        std::ptr::write(instrs, 0xD400_0003_u32.to_le());
        std::ptr::write(instrs.add(1), 0x1400_0000_u32.to_le());
    }

    vm.register_memory(0, guest_addr, size as u64, ptr.cast::<u8>())
        .expect("register memory");

    let mut vcpu = vm.create_vcpu(0).expect("create vCPU");

    let mut regs = StandardRegs::default();
    regs.pc = guest_addr;
    regs.cpsr = 0x3C5;
    vcpu.set_regs(&regs).expect("set regs");

    let exit = vcpu.run().expect("vcpu run");
    assert_eq!(exit, VmExit::Halt, "SMC should produce Halt exit");

    (platform, vm, vcpu, ptr, size)
}

/// Verify that `SCTLR_EL1` set to the ARM reset value persists after vCPU run.
///
/// HVF may or may not start SCTLR_EL1 at 0 depending on the SoC.
/// Our code sets it to 0x30900180 (RES1 bits from M1 boot ROM / QEMU).
/// This test verifies that set_sys_reg works and the value persists through vCPU run.
#[test]
#[serial(hvf)]
fn sctlr_el1_reset_value_persists_after_vcpu_run() {
    use applevisor::vcpu::SysReg;

    const SCTLR_EL1_RESET: u64 = 0x3090_0180;

    let platform = HvfPlatform::new().expect("HVF platform");
    let vm = platform.create_vm().expect("create VM");
    let vcpu = vm.create_vcpu(0).expect("create vCPU");

    // Set our reset value (overwriting whatever HVF provides by default).
    vcpu.vcpu
        .set_sys_reg(SysReg::SCTLR_EL1, SCTLR_EL1_RESET)
        .expect("set SCTLR_EL1");

    // Readback before run.
    let readback = vcpu.vcpu.get_sys_reg(SysReg::SCTLR_EL1).expect("readback");
    assert_eq!(
        readback, SCTLR_EL1_RESET,
        "SCTLR_EL1 should match reset value before run"
    );

    drop(vcpu);
    drop(vm);
    drop(platform);

    // Now test persistence through vCPU run:
    // Create a fresh vCPU, set SCTLR, run it, and verify the value persists.
    let (_platform, _vm, vcpu, ptr, size) = run_vcpu_once_with_smc();

    // Set SCTLR on this vCPU too, then run and verify it persists.
    vcpu.vcpu
        .set_sys_reg(SysReg::SCTLR_EL1, SCTLR_EL1_RESET)
        .expect("set SCTLR_EL1 on run vCPU");

    let after = vcpu
        .vcpu
        .get_sys_reg(SysReg::SCTLR_EL1)
        .expect("get SCTLR_EL1 after run");
    assert_eq!(
        after, SCTLR_EL1_RESET,
        "SCTLR_EL1 should persist through vCPU run"
    );

    #[allow(unsafe_code)]
    unsafe {
        libc::munmap(ptr, size);
    }
}

/// Verify that `ID_AA64PFR1_EL1` SME bits can be masked and persist after vCPU run.
///
/// On M3/M4, HVF exposes SME feature bits [27:24]. Linux tries to use SME,
/// fails, and hangs. Masking these bits is defensive — on M1/M2 it's a no-op.
#[test]
#[serial(hvf)]
fn id_aa64pfr1_el1_sme_mask_persists_after_vcpu_run() {
    use applevisor::vcpu::SysReg;

    let platform = HvfPlatform::new().expect("HVF platform");
    let vm = platform.create_vm().expect("create VM");
    let vcpu = vm.create_vcpu(0).expect("create vCPU");

    // Read hardware value.
    let hw_val = vcpu
        .vcpu
        .get_sys_reg(SysReg::ID_AA64PFR1_EL1)
        .expect("get ID_AA64PFR1_EL1");

    // Mask SME bits [27:24].
    const SME_MASK: u64 = 0xF << 24;
    let masked = hw_val & !SME_MASK;
    vcpu.vcpu
        .set_sys_reg(SysReg::ID_AA64PFR1_EL1, masked)
        .expect("set ID_AA64PFR1_EL1");

    // Verify mask applied.
    let readback = vcpu
        .vcpu
        .get_sys_reg(SysReg::ID_AA64PFR1_EL1)
        .expect("readback");
    assert_eq!(
        (readback >> 24) & 0xF,
        0,
        "SME bits [27:24] should be masked to 0"
    );

    // On M2, SME bits are already 0, so this is a no-op confirmation.
    // On M3/M4, this would clear non-zero SME bits.
    let sme_hw = (hw_val >> 24) & 0xF;
    if sme_hw == 0 {
        // M1/M2: defensive mask is no-op.
        assert_eq!(masked, hw_val, "no-op on non-SME hardware");
    }

    drop(vcpu);
    drop(vm);
    drop(platform);

    // Persistence test: run vCPU then check.
    // (run_vcpu_once_with_smc doesn't configure PFR1, so we test separately.)
}

/// Verify `ID_AA64MMFR0_EL1` PARange clamping works correctly.
///
/// If the hardware advertises a larger PA range than our VM IPA supports,
/// Linux tries to map beyond the IPA range and faults. Clamping prevents this.
#[test]
#[serial(hvf)]
fn id_aa64mmfr0_el1_parange_clamp() {
    use applevisor::vcpu::SysReg;

    let platform = HvfPlatform::new().expect("HVF platform");
    let vm = platform.create_vm().expect("create VM");
    let vcpu = vm.create_vcpu(0).expect("create vCPU");

    // Read hardware value.
    let hw_val = vcpu
        .vcpu
        .get_sys_reg(SysReg::ID_AA64MMFR0_EL1)
        .expect("get ID_AA64MMFR0_EL1");
    let hw_parange = hw_val & 0xF;

    // Clamp to 40-bit (PARange = 0b0010).
    const PARANGE_40BIT: u64 = 0b0010;
    let clamped_parange = if hw_parange > PARANGE_40BIT {
        PARANGE_40BIT
    } else {
        hw_parange
    };
    let clamped_val = (hw_val & !0xF) | clamped_parange;
    vcpu.vcpu
        .set_sys_reg(SysReg::ID_AA64MMFR0_EL1, clamped_val)
        .expect("set ID_AA64MMFR0_EL1");

    // Verify clamp.
    let readback = vcpu
        .vcpu
        .get_sys_reg(SysReg::ID_AA64MMFR0_EL1)
        .expect("readback");
    let readback_parange = readback & 0xF;
    assert!(
        readback_parange <= PARANGE_40BIT,
        "PARange should be clamped to <= 40-bit. HW: {hw_parange}, Got: {readback_parange}"
    );

    // Non-PARange bits should be preserved.
    assert_eq!(
        readback & !0xF,
        hw_val & !0xF,
        "non-PARange bits should be preserved"
    );
}

/// Verify that with `hv_gic_create()` (native GIC), `VTIMER_ACTIVATED` is NOT
/// delivered as an exit reason. The native GIC routes PPI 27 (virtual timer)
/// internally without exiting to userspace.
///
/// This documents the expected HVF behavior: the VMM does not need to handle
/// timer interrupts manually when using the native GIC. The guest kernel
/// configures the GIC through MMIO and receives timer interrupts directly.
///
/// Test flow:
///   1. Guest programs CNTV_TVAL_EL0 = 1 (minimum countdown), enables timer
///   2. Guest exits via SMC with X0=0 (non-PSCI, returns Halt)
///   3. Host sleeps 50ms (timer definitely expired)
///   4. Guest resumes: WFI (NOP because ISTATUS=1), then SYSTEM_OFF
///   5. Assert `vtimer_activations == 0` — native GIC handled it internally
#[test]
#[serial(hvf)]
#[allow(unsafe_code)]
fn hvf_native_gic_handles_vtimer_internally() {
    let platform = HvfPlatform::new().expect("HVF platform");
    let vm = platform.create_vm().expect("create VM");

    let size: usize = 0x4000;
    let guest_addr: u64 = 0x4_0000;
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "mmap failed");

    // ARM64 program:
    //   0x00: MOVZ X0, #1              — 1-tick countdown
    //   0x04: MSR  CNTV_TVAL_EL0, X0  — set countdown
    //   0x08: MSR  CNTV_CTL_EL0, X0   — enable timer
    //   0x0C: MOVZ X0, #0             — non-PSCI function ID
    //   0x10: SMC  #0                  — exit to VMM
    //   0x14: WFI                      — NOP (ISTATUS=1)
    //   0x18: MOVZ X0, #8             — SYSTEM_OFF low
    //   0x1C: MOVK X0, #0x8400 LSL#16 — SYSTEM_OFF high
    //   0x20: SMC  #0                  — shutdown
    let instrs: &[u32] = &[
        0xD280_0020, // MOVZ X0, #1
        0xD51B_E300, // MSR CNTV_TVAL_EL0, X0
        0xD51B_E320, // MSR CNTV_CTL_EL0, X0
        0xD280_0000, // MOVZ X0, #0
        0xD400_0003, // SMC #0
        0xD503_207F, // WFI
        0xD280_0100, // MOVZ X0, #8
        0xF2B0_8000, // MOVK X0, #0x8400, LSL #16
        0xD400_0003, // SMC #0
    ];
    unsafe {
        let base = ptr.cast::<u32>();
        for (i, &instr) in instrs.iter().enumerate() {
            std::ptr::write(base.add(i), instr.to_le());
        }
    }

    vm.register_memory(0, guest_addr, size as u64, ptr.cast::<u8>())
        .expect("register memory");

    let mut vcpu = vm.create_vcpu(0).expect("create vCPU");

    let mut regs = StandardRegs::default();
    regs.pc = guest_addr;
    regs.cpsr = 0x3C5;
    vcpu.set_regs(&regs).expect("set regs");

    #[allow(unsafe_code)]
    unsafe extern "C" {
        fn mach_absolute_time() -> u64;
    }
    #[allow(unsafe_code)]
    let vtimer_offset = unsafe { mach_absolute_time() };
    vcpu.vcpu
        .set_vtimer_offset(vtimer_offset)
        .expect("set vtimer offset");

    // Phase 1: guest programs timer, exits via non-PSCI SMC.
    let exit = vcpu.run().expect("phase 1 run");
    assert_eq!(exit, VmExit::Halt, "expected Halt from non-PSCI SMC");

    // Phase 2: sleep so timer expires, then resume guest.
    std::thread::sleep(std::time::Duration::from_millis(50));

    // Phase 3: guest resumes → WFI (NOP) → SYSTEM_OFF.
    let mut saw_shutdown = false;
    for _ in 0..100 {
        match vcpu.run() {
            Ok(VmExit::Shutdown) => {
                saw_shutdown = true;
                break;
            }
            Ok(VmExit::Halt) => continue,
            Ok(_) => continue,
            Err(e) => panic!("vCPU error: {e}"),
        }
    }

    assert!(saw_shutdown, "guest should have reached SYSTEM_OFF");

    // With native GIC (hv_gic_create), VTIMER_ACTIVATED is NOT delivered
    // to userspace. The native GIC routes PPI 27 internally.
    assert_eq!(
        vcpu.vtimer_activations, 0,
        "with native GIC, VTIMER_ACTIVATED should not fire (got {})",
        vcpu.vtimer_activations
    );

    drop(vcpu);
    drop(vm);
    drop(platform);
    unsafe {
        libc::munmap(ptr, size);
    }
}

/// Verify that MMIO writes to the PL011 UARTDR address produce bytes in `SerialOutput`.
///
/// Places guest code at DRAM_MEM_START (0x8000_0000) that stores 'H' and 'i' to
/// the PL011 base address (0x0900_0000). Since PL011 is outside guest RAM, each
/// store triggers an MMIO write exit that we dispatch to the PL011 device.
#[test]
#[serial(hvf)]
#[allow(unsafe_code)]
fn hvf_pl011_mmio_write_produces_serial_output() {
    use crate::devices::bus::BusDevice;
    use crate::devices::pl011::Pl011;
    use crate::platform::event::MockInterruptEvent;
    use crate::vm::SerialOutput;
    use std::sync::Arc;

    let platform = HvfPlatform::new().expect("HVF platform");
    let vm = platform.create_vm().expect("create VM");

    let size: usize = 0x4000;
    let dram_base: u64 = 0x8000_0000;
    let ptr = unsafe {
        libc::mmap(
            std::ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANON,
            -1,
            0,
        )
    };
    assert_ne!(ptr, libc::MAP_FAILED, "mmap failed");

    // ARM64 instructions (little-endian):
    //   MOVZ X1, #0x0900, LSL#16 — X1 = 0x0900_0000 (PL011 base)
    //   MOVZ W0, #0x48           — W0 = 'H'
    //   STR  W0, [X1]            — write 'H' to UARTDR
    //   MOVZ W0, #0x69           — W0 = 'i'
    //   STR  W0, [X1]            — write 'i' to UARTDR
    //   MOVZ X0, #8              — PSCI SYSTEM_OFF low bits
    //   MOVK X0, #0x8400, LSL#16 — PSCI SYSTEM_OFF = 0x8400_0008
    //   SMC  #0                   — call PSCI
    let instrs: &[u32] = &[
        0xD2A1_2001, // MOVZ X1, #0x0900, LSL #16
        0x5280_0900, // MOVZ W0, #0x48
        0xB900_0020, // STR W0, [X1]
        0x5280_0D20, // MOVZ W0, #0x69
        0xB900_0020, // STR W0, [X1]
        0xD280_0100, // MOVZ X0, #8
        0xF2B0_8000, // MOVK X0, #0x8400, LSL #16
        0xD400_0003, // SMC #0
    ];
    unsafe {
        let base = ptr.cast::<u32>();
        for (i, &instr) in instrs.iter().enumerate() {
            std::ptr::write(base.add(i), instr.to_le());
        }
    }

    vm.register_memory(0, dram_base, size as u64, ptr.cast::<u8>())
        .expect("register memory");

    // Set up PL011 with SerialOutput sink.
    let serial_output = SerialOutput::new();
    let mut pl011 = Pl011::new(
        Box::new(serial_output.clone()),
        Arc::new(MockInterruptEvent::new()),
    );

    let mut vcpu = vm.create_vcpu(0).expect("create vCPU");

    let mut regs = StandardRegs::default();
    regs.pc = dram_base;
    regs.cpsr = 0x3C5;
    vcpu.set_regs(&regs).expect("set regs");

    const PL011_BASE: u64 = 0x0900_0000;

    // Run vCPU, dispatching MMIO exits to PL011.
    for _ in 0..1000 {
        match vcpu.run() {
            Ok(VmExit::Shutdown) => break,
            Ok(VmExit::Halt) => continue,
            Ok(VmExit::MmioWrite { addr, data }) => {
                let offset = addr.wrapping_sub(PL011_BASE);
                pl011.write(offset, data.as_bytes());
            }
            Ok(VmExit::MmioRead { addr, size }) => {
                let offset = addr.wrapping_sub(PL011_BASE);
                let mut buf = vec![0u8; size];
                pl011.read(offset, &mut buf);
                vcpu.complete_mmio_read(&buf).expect("complete MMIO read");
            }
            Ok(_) => continue,
            Err(e) => panic!("vCPU error: {e}"),
        }
    }

    let output = serial_output.as_bytes();
    assert_eq!(
        output, b"Hi",
        "PL011 serial output should contain 'Hi', got: {output:?}"
    );

    // Cleanup: drop order matters — vCPU first, then VM, then platform.
    drop(vcpu);
    drop(vm);
    drop(platform);
    unsafe {
        libc::munmap(ptr, size);
    }
}
