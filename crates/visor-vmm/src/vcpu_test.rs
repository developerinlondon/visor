//! Tests for vCPU creation, register setup, and run loop.

use std::path::PathBuf;

use crate::platform::{KvmPlatform, KvmVm, Platform};

use super::*;
use crate::boot;
use crate::boot::BootConfig;
use crate::guest_virtualization::GuestVirtualizationMode;
use crate::memory::GuestMemory;

// ── Test Helpers ───────────────────────────────────────────────────────────

/// Creates a KVM platform, VM, and memory for testing.
/// Returns (`KvmPlatform`, `KvmVm`, `GuestMemory`) with 64 MiB of memory at address 0.
fn setup_vm() -> (KvmPlatform, KvmVm, GuestMemory) {
    let platform = KvmPlatform::new().unwrap();
    let vm = platform.create_vm().unwrap();
    let memory = GuestMemory::new(64 * 1024 * 1024, 0).unwrap();
    memory.register(&vm, 0).unwrap();
    (platform, vm, memory)
}

/// A test exit handler that records exits and stops after a limit.
struct RecordingHandler {
    exits: Vec<VmExit>,
    max_exits: usize,
}

impl RecordingHandler {
    fn new(max_exits: usize) -> Self {
        Self {
            exits: Vec::new(),
            max_exits,
        }
    }
}

impl ExitHandler for RecordingHandler {
    fn handle_exit(&mut self, exit: VmExit) -> Result<ExitAction, VcpuError> {
        self.exits.push(exit);
        if self.exits.len() >= self.max_exits {
            Ok(ExitAction::Stop)
        } else {
            Ok(ExitAction::Continue)
        }
    }
}

/// A handler that responds to `IoIn` on a specific port with a fixed byte.
struct IoReadHandler {
    port: u16,
    response: u8,
    exits: Vec<VmExit>,
    max_exits: usize,
}

impl IoReadHandler {
    fn new(port: u16, response: u8, max_exits: usize) -> Self {
        Self {
            port,
            response,
            exits: Vec::new(),
            max_exits,
        }
    }
}

impl ExitHandler for IoReadHandler {
    fn handle_exit(&mut self, exit: VmExit) -> Result<ExitAction, VcpuError> {
        self.exits.push(exit);
        if self.exits.len() >= self.max_exits {
            Ok(ExitAction::Stop)
        } else {
            Ok(ExitAction::Continue)
        }
    }

    fn handle_io_read(&mut self, port: u16, data: &mut [u8]) {
        if port == self.port && !data.is_empty() {
            data[0] = self.response;
        }
    }
}

// ── vCPU Creation Tests ────────────────────────────────────────────────────

#[test]
fn create_vcpu_succeeds() {
    let (_platform, vm, _memory) = setup_vm();
    let vcpu = Vcpu::new(&vm, 0).unwrap();
    assert_eq!(vcpu.index(), 0);
}

#[test]
fn create_multiple_vcpus() {
    let (_platform, vm, _memory) = setup_vm();
    let vcpu0 = Vcpu::new(&vm, 0).unwrap();
    let vcpu1 = Vcpu::new(&vm, 1).unwrap();
    assert_eq!(vcpu0.index(), 0);
    assert_eq!(vcpu1.index(), 1);
}

// ── Register Setup Tests ───────────────────────────────────────────────────

#[test]
fn configure_regs_from_boot_config() {
    let (platform, vm, _memory) = setup_vm();
    let vcpu = Vcpu::new(&vm, 0).unwrap();

    let config = BootConfig {
        entry_point: 0x10_0000,
        stack_pointer: boot::BOOT_STACK_POINTER,
        boot_params_addr: boot::ZERO_PAGE_START,
        pml4_addr: boot::PML4_START,
    };

    vcpu.configure_regs(platform.kvm(), &config, GuestVirtualizationMode::Standard)
        .unwrap();

    // Verify general-purpose regs
    let regs = vcpu.fd().get_regs().unwrap();
    assert_eq!(regs.rip, 0x10_0000);
    assert_eq!(regs.rsp, boot::BOOT_STACK_POINTER);
    assert_eq!(regs.rbp, boot::BOOT_STACK_POINTER);
    assert_eq!(regs.rsi, boot::ZERO_PAGE_START);
    assert_eq!(regs.rflags, 2);
}

#[test]
fn configure_regs_sets_sregs_for_long_mode() {
    let (platform, vm, _memory) = setup_vm();
    let vcpu = Vcpu::new(&vm, 0).unwrap();

    let config = BootConfig {
        entry_point: 0x10_0000,
        stack_pointer: boot::BOOT_STACK_POINTER,
        boot_params_addr: boot::ZERO_PAGE_START,
        pml4_addr: boot::PML4_START,
    };

    vcpu.configure_regs(platform.kvm(), &config, GuestVirtualizationMode::Standard)
        .unwrap();

    let sregs = vcpu.fd().get_sregs().unwrap();

    // CR0 must have PE and PG set
    assert_ne!(sregs.cr0 & boot::X86_CR0_PE, 0, "CR0.PE not set");
    assert_ne!(sregs.cr0 & boot::X86_CR0_PG, 0, "CR0.PG not set");

    // CR3 = PML4 address
    assert_eq!(sregs.cr3, boot::PML4_START);

    // CR4 must have PAE
    assert_ne!(sregs.cr4 & boot::X86_CR4_PAE, 0, "CR4.PAE not set");

    // EFER must have LME and LMA
    assert_ne!(sregs.efer & boot::EFER_LME, 0, "EFER.LME not set");
    assert_ne!(sregs.efer & boot::EFER_LMA, 0, "EFER.LMA not set");

    // GDT base and limit
    assert_eq!(sregs.gdt.base, boot::BOOT_GDT_OFFSET);

    // Code segment selector = 0x08 (index 1 * 8)
    assert_eq!(sregs.cs.selector, 0x08);
    assert_eq!(sregs.cs.base, 0);

    // Data segment selector = 0x10 (index 2 * 8)
    assert_eq!(sregs.ds.selector, 0x10);
}

#[test]
fn configure_regs_sets_fpu() {
    let (platform, vm, _memory) = setup_vm();
    let vcpu = Vcpu::new(&vm, 0).unwrap();

    let config = BootConfig {
        entry_point: 0x10_0000,
        stack_pointer: boot::BOOT_STACK_POINTER,
        boot_params_addr: boot::ZERO_PAGE_START,
        pml4_addr: boot::PML4_START,
    };

    vcpu.configure_regs(platform.kvm(), &config, GuestVirtualizationMode::Standard)
        .unwrap();

    let fpu = vcpu.fd().get_fpu().unwrap();
    assert_eq!(fpu.fcw, 0x37f);
}

// ── HLT Exit Test ──────────────────────────────────────────────────────────
//
// Write a tiny x86 program that immediately executes HLT, then verify
// the vCPU exits with VmExit::Halt.

#[test]
fn vcpu_hlt_exit() {
    let (platform, vm, memory) = setup_vm();
    let mut vcpu = Vcpu::new(&vm, 0).unwrap();

    // Write a HLT instruction at 0x10_0000
    // HLT = 0xf4
    memory.write_bytes(0x10_0000, &[0xf4]).unwrap();

    // Set up page tables and GDT in guest memory
    boot::x86_64::configure_boot_memory(&memory, "").unwrap();

    let config = BootConfig {
        entry_point: 0x10_0000,
        stack_pointer: boot::BOOT_STACK_POINTER,
        boot_params_addr: boot::ZERO_PAGE_START,
        pml4_addr: boot::PML4_START,
    };

    vcpu.configure_regs(platform.kvm(), &config, GuestVirtualizationMode::Standard)
        .unwrap();

    let exit = vcpu.run_once().unwrap();
    assert_eq!(exit, VmExit::Halt);
}

// ── I/O Port Exit Test ─────────────────────────────────────────────────────
//
// Write x86 code that does `out 0x3f8, al` (serial port write).

#[test]
fn vcpu_io_out_exit() {
    let (platform, vm, memory) = setup_vm();
    let mut vcpu = Vcpu::new(&vm, 0).unwrap();

    // x86-64 instructions (64-bit long mode, imm32 encoding):
    //   mov edx, 0x03f8  → BA F8 03 00 00  (opcode + imm32)
    //   mov al, 0x41     → B0 41
    //   out dx, al       → EE
    //   hlt              → F4
    let code: &[u8] = &[
        0xBA, 0xF8, 0x03, 0x00, 0x00, // mov edx, 0x03f8
        0xB0, 0x41, // mov al, 0x41
        0xEE, // out dx, al
        0xF4, // hlt
    ];
    memory.write_bytes(0x10_0000, code).unwrap();

    boot::x86_64::configure_boot_memory(&memory, "").unwrap();

    let config = BootConfig {
        entry_point: 0x10_0000,
        stack_pointer: boot::BOOT_STACK_POINTER,
        boot_params_addr: boot::ZERO_PAGE_START,
        pml4_addr: boot::PML4_START,
    };

    vcpu.configure_regs(platform.kvm(), &config, GuestVirtualizationMode::Standard)
        .unwrap();

    // First exit should be IoOut on port 0x3f8
    let exit = vcpu.run_once().unwrap();
    assert_eq!(
        exit,
        VmExit::IoOut {
            port: 0x3f8,
            data: ExitData::from_slice(&[0x41]),
        }
    );

    // Second exit should be HLT
    let exit = vcpu.run_once().unwrap();
    assert_eq!(exit, VmExit::Halt);
}

// ── I/O Port In Exit Test ──────────────────────────────────────────────────

#[test]
fn vcpu_io_in_exit() {
    let (platform, vm, memory) = setup_vm();
    let mut vcpu = Vcpu::new(&vm, 0).unwrap();

    // x86-64 instructions (64-bit long mode, imm32 encoding):
    //   mov edx, 0x03f8  → BA F8 03 00 00  (opcode + imm32)
    //   in al, dx        → EC
    //   hlt              → F4
    let code: &[u8] = &[
        0xBA, 0xF8, 0x03, 0x00, 0x00, // mov edx, 0x03f8
        0xEC, // in al, dx
        0xF4, // hlt
    ];
    memory.write_bytes(0x10_0000, code).unwrap();

    boot::x86_64::configure_boot_memory(&memory, "").unwrap();

    let config = BootConfig {
        entry_point: 0x10_0000,
        stack_pointer: boot::BOOT_STACK_POINTER,
        boot_params_addr: boot::ZERO_PAGE_START,
        pml4_addr: boot::PML4_START,
    };

    vcpu.configure_regs(platform.kvm(), &config, GuestVirtualizationMode::Standard)
        .unwrap();

    // First exit should be IoIn on port 0x3f8
    let exit = vcpu.run_once().unwrap();
    assert_eq!(
        exit,
        VmExit::IoIn {
            port: 0x3f8,
            size: 1,
        }
    );
}

// ── Run Loop with Handler ──────────────────────────────────────────────────

#[test]
fn run_loop_stops_on_handler_decision() {
    let (platform, vm, memory) = setup_vm();
    let mut vcpu = Vcpu::new(&vm, 0).unwrap();

    // x86-64: out dx, al; hlt
    let code: &[u8] = &[
        0xBA, 0xF8, 0x03, 0x00, 0x00, // mov edx, 0x03f8
        0xB0, 0x48, // mov al, 'H'
        0xEE, // out dx, al
        0xF4, // hlt
    ];
    memory.write_bytes(0x10_0000, code).unwrap();

    boot::x86_64::configure_boot_memory(&memory, "").unwrap();

    let config = BootConfig {
        entry_point: 0x10_0000,
        stack_pointer: boot::BOOT_STACK_POINTER,
        boot_params_addr: boot::ZERO_PAGE_START,
        pml4_addr: boot::PML4_START,
    };

    vcpu.configure_regs(platform.kvm(), &config, GuestVirtualizationMode::Standard)
        .unwrap();

    // Stop after 2 exits (IoOut + Halt)
    let mut handler = RecordingHandler::new(2);
    let kill_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    vcpu.run_loop(&mut handler, &kill_flag).unwrap();

    assert_eq!(handler.exits.len(), 2);
    assert!(matches!(
        handler.exits[0],
        VmExit::IoOut { port: 0x3f8, .. }
    ));
    assert_eq!(handler.exits[1], VmExit::Halt);
}

// ── IoIn Data Writeback Test ─────────────────────────────────────────────
//
// Verify that `run_loop` calls `handle_io_read` on the handler so the
// guest receives actual device data (not KVM's default 0xFF).
//
// The test program reads a byte from port 0x3f8 into AL, then writes AL
// to port 0x3f9. The IoReadHandler responds with 0x42 on port 0x3f8.
// If writeback works, the IoOut on 0x3f9 will carry data 0x42.

#[test]
fn run_loop_io_in_writeback_from_handler() {
    let (platform, vm, memory) = setup_vm();
    let mut vcpu = Vcpu::new(&vm, 0).unwrap();

    // x86-64 instructions:
    //   mov edx, 0x03f8  ; port for IoIn
    //   in al, dx         ; read from device → AL
    //   mov edx, 0x03f9  ; port for IoOut (different port to distinguish)
    //   out dx, al        ; write AL to output port
    //   hlt
    let code: &[u8] = &[
        0xBA, 0xF8, 0x03, 0x00, 0x00, // mov edx, 0x03f8
        0xEC, // in al, dx
        0xBA, 0xF9, 0x03, 0x00, 0x00, // mov edx, 0x03f9
        0xEE, // out dx, al
        0xF4, // hlt
    ];
    memory.write_bytes(0x10_0000, code).unwrap();

    boot::x86_64::configure_boot_memory(&memory, "").unwrap();

    let config = BootConfig {
        entry_point: 0x10_0000,
        stack_pointer: boot::BOOT_STACK_POINTER,
        boot_params_addr: boot::ZERO_PAGE_START,
        pml4_addr: boot::PML4_START,
    };

    vcpu.configure_regs(platform.kvm(), &config, GuestVirtualizationMode::Standard)
        .unwrap();

    // IoReadHandler responds with 0x42 on port 0x3f8
    let mut handler = IoReadHandler::new(0x3f8, 0x42, 3);
    let kill_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    vcpu.run_loop(&mut handler, &kill_flag).unwrap();

    // Expect 3 exits: IoIn(0x3f8), IoOut(0x3f9, 0x42), Halt
    assert_eq!(handler.exits.len(), 3);

    // First exit: IoIn on port 0x3f8
    assert!(matches!(
        handler.exits[0],
        VmExit::IoIn {
            port: 0x3f8,
            size: 1
        }
    ));

    // Second exit: IoOut on port 0x3f9 with the data we injected (0x42)
    match handler.exits[1] {
        VmExit::IoOut { port, ref data } => {
            assert_eq!(port, 0x3f9);
            assert_eq!(
                data.as_bytes(),
                &[0x42],
                "IoIn writeback failed: guest should have received 0x42"
            );
        }
        ref other => panic!("expected IoOut, got {other:?}"),
    }

    // Third exit: Halt
    assert_eq!(handler.exits[2], VmExit::Halt);
}

// ── GDT Segment Helper Tests ──────────────────────────────────────────────

#[test]
fn kvm_segment_from_gdt_code_segment() {
    let entry = gdt_entry(boot::GDT_FLAGS_CODE, 0, 0xf_ffff);
    let seg = kvm_segment_from_gdt(entry, 1);

    assert_eq!(seg.selector, 0x08);
    assert_eq!(seg.base, 0);
    assert_eq!(seg.limit, 0xffff_ffff); // G flag scales 0xfffff to 4 GiB
    assert_eq!(seg.type_, 0xb); // code, readable, accessed
    assert_eq!(seg.present, 1);
    assert_eq!(seg.dpl, 0);
    assert_eq!(seg.s, 1);
    assert_eq!(seg.l, 1); // 64-bit
    assert_eq!(seg.db, 0); // must be 0 in long mode
    assert_eq!(seg.g, 1); // granularity
}

#[test]
fn kvm_segment_from_gdt_data_segment() {
    let entry = gdt_entry(boot::GDT_FLAGS_DATA, 0, 0xf_ffff);
    let seg = kvm_segment_from_gdt(entry, 2);

    assert_eq!(seg.selector, 0x10);
    assert_eq!(seg.base, 0);
    assert_eq!(seg.limit, 0xffff_ffff);
    assert_eq!(seg.type_, 0x3); // data, writable, accessed
    assert_eq!(seg.present, 1);
    assert_eq!(seg.s, 1);
    assert_eq!(seg.g, 1);
}

// ── Real Kernel Boot Test ──────────────────────────────────────────────────
//
// Boots the pre-built kernel and verifies it starts executing (doesn't
// triple-fault). Without a serial UART device (Layer 6), the kernel spins
// waiting for UART ready, so we can only assert it produces I/O exits
// rather than an immediate Shutdown/FailEntry.

#[test]
fn boot_real_kernel_starts_executing() {
    let kernel_path = PathBuf::from("/var/lib/visor/kernel/vmlinux-x86_64");
    if !kernel_path.exists() {
        return;
    }

    let (platform, vm, memory) = setup_vm();

    let config = boot::x86_64::configure_boot(
        &memory,
        &kernel_path,
        "console=ttyS0 reboot=k panic=1 noapic",
    )
    .unwrap();

    let mut vcpu = Vcpu::new(&vm, 0).unwrap();
    vcpu.configure_regs(platform.kvm(), &config, GuestVirtualizationMode::Standard)
        .unwrap();

    // Run a small number of exits to verify the kernel starts executing.
    // A triple-fault would produce an immediate Shutdown on the very first exit.
    // Any I/O exit (port or MMIO) proves the kernel is running real code.
    let got_io = matches!(
        vcpu.run_once(),
        Ok(VmExit::IoIn { .. }
            | VmExit::IoOut { .. }
            | VmExit::MmioRead { .. }
            | VmExit::MmioWrite { .. })
    );

    assert!(
        got_io,
        "expected kernel to produce I/O exits, but it halted or shut down immediately"
    );
}
