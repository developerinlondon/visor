use std::path::Path;

use super::*;

// ── Linux tests ────────────────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux_tests {
    use super::*;
    use kvm_bindings::{kvm_fpu, kvm_regs, kvm_sregs};

    fn create_kvm_vcpu() -> (kvm_ioctls::Kvm, kvm_ioctls::VmFd, kvm_ioctls::VcpuFd) {
        let kvm = kvm_ioctls::Kvm::new().expect("open /dev/kvm");
        let vm = kvm.create_vm().expect("create VM");
        let vcpu = vm.create_vcpu(0).expect("create vCPU");
        (kvm, vm, vcpu)
    }

    #[test]
    fn save_cpu_returns_registers() {
        let (_kvm, _vm, vcpu) = create_kvm_vcpu();
        let snap = save_cpu(&vcpu).expect("save_cpu");

        // Freshly created vCPU should have zeroed regs (except RIP/RFLAGS on some KVM versions).
        // Just verify we got valid data back.
        assert_eq!(
            std::mem::size_of_val(&snap.regs),
            std::mem::size_of::<kvm_regs>()
        );
        assert_eq!(
            std::mem::size_of_val(&snap.sregs),
            std::mem::size_of::<kvm_sregs>()
        );
        assert_eq!(
            std::mem::size_of_val(&snap.fpu),
            std::mem::size_of::<kvm_fpu>()
        );
    }

    #[test]
    fn cpu_snapshot_round_trip() {
        let (_kvm, _vm, vcpu) = create_kvm_vcpu();

        // Set distinctive register values.
        let mut regs = vcpu.get_regs().expect("get_regs");
        regs.rax = 0xDEAD_BEEF;
        regs.rbx = 0xCAFE_BABE;
        regs.rip = 0x1000;
        vcpu.set_regs(&regs).expect("set_regs");

        // Save.
        let snap = save_cpu(&vcpu).expect("save_cpu");
        assert_eq!(snap.regs.rax, 0xDEAD_BEEF);
        assert_eq!(snap.regs.rbx, 0xCAFE_BABE);
        assert_eq!(snap.regs.rip, 0x1000);

        // Zero out registers.
        let mut zero_regs = vcpu.get_regs().expect("get_regs");
        zero_regs.rax = 0;
        zero_regs.rbx = 0;
        vcpu.set_regs(&zero_regs).expect("set_regs");

        // Restore.
        restore_cpu(&vcpu, &snap).expect("restore_cpu");

        // Verify.
        let restored = vcpu.get_regs().expect("get_regs");
        assert_eq!(restored.rax, 0xDEAD_BEEF);
        assert_eq!(restored.rbx, 0xCAFE_BABE);
        assert_eq!(restored.rip, 0x1000);
    }

    #[test]
    fn snapshot_bundle_save_restore() {
        let (_kvm, _vm, vcpu) = create_kvm_vcpu();

        // Set up some register state.
        let mut regs = vcpu.get_regs().expect("get_regs");
        regs.rax = 42;
        vcpu.set_regs(&regs).expect("set_regs");

        // Set up memory with a pattern.
        let mem = crate::memory::GuestMemory::new(8192, 0).expect("alloc memory");
        mem.write_bytes(0, &[0x42; 256]).expect("write");

        let dir = crate::testutil::tempdir("visor-vmm-snapshot-").expect("tmpdir");
        let device_state = vec![1, 2, 3, 4];

        let bundle =
            save_bundle(&vcpu, &mem, dir.path(), device_state.clone()).expect("save_bundle");
        assert_eq!(bundle.memory_size, 8192);
        assert_eq!(bundle.device_state, vec![1, 2, 3, 4]);
        assert!(bundle.memory_path.exists());
        assert!(dir.path().join("cpu_state.json").exists());
        assert!(dir.path().join("device_state.bin").exists());

        // Zero registers.
        let mut zero = vcpu.get_regs().expect("get_regs");
        zero.rax = 0;
        vcpu.set_regs(&zero).expect("set_regs");

        // Restore.
        let (restored_mem, restored_device) =
            restore_bundle(&vcpu, dir.path(), 8192, 0).expect("restore_bundle");

        // Verify CPU state restored.
        let restored_regs = vcpu.get_regs().expect("get_regs");
        assert_eq!(restored_regs.rax, 42);

        // Verify memory restored.
        let data = restored_mem.read_bytes(0, 256).expect("read");
        assert_eq!(&data, &[0x42; 256]);

        // Verify device state restored.
        assert_eq!(restored_device, vec![1, 2, 3, 4]);
    }

    #[test]
    fn cpu_state_serialization_round_trip() {
        let (_kvm, _vm, vcpu) = create_kvm_vcpu();

        let mut regs = vcpu.get_regs().expect("get_regs");
        regs.rax = 0x1234_5678_9ABC_DEF0;
        regs.rsp = 0xFFFF_FFFF_FFFF_0000;
        vcpu.set_regs(&regs).expect("set_regs");

        let snap = save_cpu(&vcpu).expect("save_cpu");
        let json = serialize_cpu_state(&snap);
        let deserialized = deserialize_cpu_state(&json).expect("deserialize");

        assert_eq!(deserialized.regs.rax, 0x1234_5678_9ABC_DEF0);
        assert_eq!(deserialized.regs.rsp, 0xFFFF_FFFF_FFFF_0000);
    }
}

// ── Cross-platform memory tests ────────────────────────────────────

#[test]
fn memory_save_creates_correct_file() {
    let mem = crate::memory::GuestMemory::new(4096, 0).expect("alloc memory");

    // Write a known pattern.
    mem.write_bytes(0, &[0xAA; 64]).expect("write");

    let dir = crate::testutil::tempdir("visor-vmm-snapshot-").expect("tmpdir");
    let path = dir.path().join("memory.bin");

    save_memory(&mem, &path).expect("save_memory");

    let metadata = std::fs::metadata(&path).expect("stat");
    assert_eq!(metadata.len(), 4096);

    // Verify the pattern is in the file.
    let data = std::fs::read(&path).expect("read");
    assert_eq!(&data[..64], &[0xAA; 64]);
}

#[test]
fn memory_restore_round_trip() {
    let mem = crate::memory::GuestMemory::new(4096, 0).expect("alloc memory");
    mem.write_bytes(0, &[0xBB; 128]).expect("write pattern");
    mem.write_bytes(3000, &[0xCC; 100])
        .expect("write pattern 2");

    let dir = crate::testutil::tempdir("visor-vmm-snapshot-").expect("tmpdir");
    let path = dir.path().join("memory.bin");

    save_memory(&mem, &path).expect("save");
    let restored = restore_memory(&path, 4096, 0).expect("restore");

    // Read back and verify.
    let data1 = restored.read_bytes(0, 128).expect("read");
    assert_eq!(&data1, &[0xBB; 128]);

    let data2 = restored.read_bytes(3000, 100).expect("read");
    assert_eq!(&data2, &[0xCC; 100]);
}

#[test]
fn memory_restore_is_cow() {
    let mem = crate::memory::GuestMemory::new(4096, 0).expect("alloc memory");
    mem.write_bytes(0, &[0xDD; 64]).expect("write");

    let dir = crate::testutil::tempdir("visor-vmm-snapshot-").expect("tmpdir");
    let path = dir.path().join("memory.bin");

    save_memory(&mem, &path).expect("save");
    let restored = restore_memory(&path, 4096, 0).expect("restore");

    // Write to restored memory (should be CoW — doesn't modify the file).
    restored
        .write_bytes(0, &[0xEE; 64])
        .expect("write to restored");

    // Verify the file is unchanged.
    let file_data = std::fs::read(&path).expect("read file");
    assert_eq!(
        &file_data[..64],
        &[0xDD; 64],
        "original file should be unchanged (MAP_PRIVATE CoW)"
    );

    // Verify the restored memory has the new value.
    let read_back = restored.read_bytes(0, 64).expect("read restored");
    assert_eq!(&read_back, &[0xEE; 64]);
}

#[test]
fn memory_restore_size_mismatch_fails() {
    let mem = crate::memory::GuestMemory::new(4096, 0).expect("alloc memory");

    let dir = crate::testutil::tempdir("visor-vmm-snapshot-").expect("tmpdir");
    let path = dir.path().join("memory.bin");

    save_memory(&mem, &path).expect("save");

    let err = restore_memory(&path, 8192, 0).expect_err("should fail on size mismatch");
    match err {
        SnapshotError::MemoryMismatch { expected, actual } => {
            assert_eq!(expected, 8192);
            assert_eq!(actual, 4096);
        }
        other => panic!("expected MemoryMismatch, got: {other}"),
    }
}

#[test]
fn restore_nonexistent_memory_file_fails() {
    let err = restore_memory(Path::new("/nonexistent/memory.bin"), 4096, 0);
    assert!(err.is_err());
}

// ── macOS ARM64 tests ──────────────────────────────────────────────

#[cfg(target_os = "macos")]
mod macos_tests {
    use super::*;
    use crate::platform::regs::{SpecialRegs, StandardRegs};

    fn sample_regs() -> StandardRegs {
        let mut regs = StandardRegs::default();
        regs.x[0] = 0xDEAD_BEEF;
        regs.x[1] = 0xCAFE_BABE;
        regs.x[30] = 0x1234_5678;
        regs.sp = 0xFFFF_0000;
        regs.pc = 0x8000_0000;
        regs.cpsr = 0x3C5;
        regs.fpcr = 0x100;
        regs.fpsr = 0x200;
        regs
    }

    fn sample_sregs() -> SpecialRegs {
        let mut sregs = SpecialRegs::default();
        sregs.sctlr_el1 = 0x3050_5085;
        sregs.ttbr0_el1 = 0x4000_0000;
        sregs.ttbr1_el1 = 0x4000_1000;
        sregs.tcr_el1 = 0x19_B520_0011;
        sregs.mair_el1 = 0xFF_440C_0400;
        sregs.vbar_el1 = 0xFFFF_0000_0000_0000;
        sregs.spsr_el1 = 0x3C5;
        sregs.elr_el1 = 0x8000_1000;
        sregs.midr_el1 = 0x611F_0221;
        sregs.mpidr_el1 = 0x8000_0000;
        sregs
    }

    #[test]
    fn cpu_snapshot_serialization_round_trip() {
        let cpu = CpuSnapshot {
            regs: sample_regs(),
            sregs: sample_sregs(),
        };
        let json = serialize_cpu_state(&cpu);
        let restored = deserialize_cpu_state(&json).expect("deserialize");
        assert_eq!(restored.regs, cpu.regs);
        assert_eq!(restored.sregs, cpu.sregs);
    }

    #[test]
    fn serialization_preserves_all_gp_registers() {
        let mut regs = StandardRegs::default();
        for i in 0..31 {
            regs.x[i] = (i as u64 + 1) * 0x1111_1111;
        }
        regs.sp = u64::MAX;
        regs.pc = u64::MAX - 1;
        regs.cpsr = 0xFFFF_FFFF;
        regs.fpcr = 0xABCD;
        regs.fpsr = 0xEF01;

        let cpu = CpuSnapshot {
            regs,
            sregs: SpecialRegs::default(),
        };
        let json = serialize_cpu_state(&cpu);
        let restored = deserialize_cpu_state(&json).expect("deserialize");
        assert_eq!(restored.regs, cpu.regs);
    }

    #[test]
    fn serialization_preserves_all_system_registers() {
        let mut sregs = SpecialRegs::default();
        sregs.sctlr_el1 = 1;
        sregs.ttbr0_el1 = 2;
        sregs.ttbr1_el1 = 3;
        sregs.tcr_el1 = 4;
        sregs.mair_el1 = 5;
        sregs.vbar_el1 = 6;
        sregs.spsr_el1 = 7;
        sregs.elr_el1 = 8;
        sregs.sp_el0 = 9;
        sregs.sp_el1 = 10;
        sregs.esr_el1 = 11;
        sregs.far_el1 = 12;
        sregs.par_el1 = 13;
        sregs.cpacr_el1 = 14;
        sregs.cntkctl_el1 = 15;
        sregs.cntv_ctl_el0 = 16;
        sregs.cntv_cval_el0 = 17;
        sregs.tpidr_el0 = 18;
        sregs.tpidrro_el0 = 19;
        sregs.tpidr_el1 = 20;
        sregs.contextidr_el1 = 21;
        sregs.amair_el1 = 22;
        sregs.afsr0_el1 = 23;
        sregs.afsr1_el1 = 24;
        sregs.midr_el1 = 25;
        sregs.mpidr_el1 = 26;

        let cpu = CpuSnapshot {
            regs: StandardRegs::default(),
            sregs,
        };
        let json = serialize_cpu_state(&cpu);
        let restored = deserialize_cpu_state(&json).expect("deserialize");
        assert_eq!(restored.sregs, cpu.sregs);
    }

    #[test]
    fn serialization_handles_large_values() {
        let mut regs = StandardRegs::default();
        regs.x[0] = u64::MAX;
        regs.sp = u64::MAX;
        regs.pc = u64::MAX;

        let cpu = CpuSnapshot {
            regs,
            sregs: SpecialRegs::default(),
        };
        let json = serialize_cpu_state(&cpu);
        let restored = deserialize_cpu_state(&json).expect("deserialize");
        assert_eq!(restored.regs.x[0], u64::MAX);
        assert_eq!(restored.regs.sp, u64::MAX);
        assert_eq!(restored.regs.pc, u64::MAX);
    }

    #[test]
    fn serialization_handles_zero_values() {
        let cpu = CpuSnapshot {
            regs: StandardRegs::default(),
            sregs: SpecialRegs::default(),
        };
        let json = serialize_cpu_state(&cpu);
        let restored = deserialize_cpu_state(&json).expect("deserialize");
        assert_eq!(restored.regs, cpu.regs);
        assert_eq!(restored.sregs, cpu.sregs);
    }

    #[test]
    fn deserialize_rejects_missing_field() {
        let json = r#"{"regs":{"x":[0],"sp":0},"sregs":{}}"#;
        let err = deserialize_cpu_state(json);
        assert!(err.is_err());
    }

    #[test]
    fn deserialize_rejects_wrong_array_length() {
        // Build JSON with only 5 elements in x array instead of 31
        let json =
            r#"{"regs":{"x":[0,0,0,0,0],"sp":0,"pc":0,"cpsr":0,"fpcr":0,"fpsr":0},"sregs":{}}"#;
        let err = deserialize_cpu_state(json);
        assert!(err.is_err());
    }

    #[test]
    fn memory_save_creates_correct_file() {
        let mem = crate::memory::GuestMemory::new(4096, 0).expect("alloc memory");
        mem.write_bytes(0, &[0xAA; 64]).expect("write");

        let dir = crate::testutil::tempdir("visor-vmm-snapshot-").expect("tmpdir");
        let path = dir.path().join("memory.bin");

        save_memory(&mem, &path).expect("save_memory");

        let metadata = std::fs::metadata(&path).expect("stat");
        assert_eq!(metadata.len(), 4096);

        let data = std::fs::read(&path).expect("read");
        assert_eq!(&data[..64], &[0xAA; 64]);
    }

    #[test]
    fn memory_restore_round_trip() {
        let mem = crate::memory::GuestMemory::new(4096, 0).expect("alloc memory");
        mem.write_bytes(0, &[0xBB; 128]).expect("write pattern");
        mem.write_bytes(3000, &[0xCC; 100])
            .expect("write pattern 2");

        let dir = crate::testutil::tempdir("visor-vmm-snapshot-").expect("tmpdir");
        let path = dir.path().join("memory.bin");

        save_memory(&mem, &path).expect("save");
        let restored = restore_memory(&path, 4096, 0).expect("restore");

        let data1 = restored.read_bytes(0, 128).expect("read");
        assert_eq!(&data1, &[0xBB; 128]);
        let data2 = restored.read_bytes(3000, 100).expect("read");
        assert_eq!(&data2, &[0xCC; 100]);
    }

    #[test]
    fn memory_restore_is_cow() {
        let mem = crate::memory::GuestMemory::new(4096, 0).expect("alloc memory");
        mem.write_bytes(0, &[0xDD; 64]).expect("write");

        let dir = crate::testutil::tempdir("visor-vmm-snapshot-").expect("tmpdir");
        let path = dir.path().join("memory.bin");

        save_memory(&mem, &path).expect("save");
        let restored = restore_memory(&path, 4096, 0).expect("restore");

        restored
            .write_bytes(0, &[0xEE; 64])
            .expect("write to restored");

        let file_data = std::fs::read(&path).expect("read file");
        assert_eq!(
            &file_data[..64],
            &[0xDD; 64],
            "original file should be unchanged (MAP_PRIVATE CoW)"
        );

        let read_back = restored.read_bytes(0, 64).expect("read restored");
        assert_eq!(&read_back, &[0xEE; 64]);
    }

    #[test]
    fn memory_restore_size_mismatch_fails() {
        let mem = crate::memory::GuestMemory::new(4096, 0).expect("alloc memory");
        let dir = crate::testutil::tempdir("visor-vmm-snapshot-").expect("tmpdir");
        let path = dir.path().join("memory.bin");

        save_memory(&mem, &path).expect("save");

        let err = restore_memory(&path, 8192, 0).expect_err("should fail on size mismatch");
        match err {
            SnapshotError::MemoryMismatch { expected, actual } => {
                assert_eq!(expected, 8192);
                assert_eq!(actual, 4096);
            }
            other => panic!("expected MemoryMismatch, got: {other}"),
        }
    }

    #[test]
    fn restore_nonexistent_memory_file_fails() {
        let err = restore_memory(std::path::Path::new("/nonexistent/memory.bin"), 4096, 0);
        assert!(err.is_err());
    }
}
