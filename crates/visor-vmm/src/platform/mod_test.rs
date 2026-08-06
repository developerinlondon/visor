use super::*;

// ── Mock implementations ────────────────────────────────────────────

struct MockVcpu;

impl VcpuOps for MockVcpu {
    fn set_regs(&self, _regs: &regs::StandardRegs) -> Result<(), PlatformError> {
        Ok(())
    }

    fn get_regs(&self) -> Result<regs::StandardRegs, PlatformError> {
        Ok(regs::StandardRegs::default())
    }

    fn set_sregs(&self, _sregs: &regs::SpecialRegs) -> Result<(), PlatformError> {
        Ok(())
    }

    fn get_sregs(&self) -> Result<regs::SpecialRegs, PlatformError> {
        Ok(regs::SpecialRegs::default())
    }

    fn run(&mut self) -> Result<VmExit, PlatformError> {
        Ok(VmExit::Halt)
    }
}

struct MockVm;

impl VmOps for MockVm {
    type Vcpu = MockVcpu;

    fn create_irq_chip(&self) -> Result<(), PlatformError> {
        Ok(())
    }

    fn create_pit(&self) -> Result<(), PlatformError> {
        Ok(())
    }

    fn register_memory(
        &self,
        _slot: u32,
        _guest_addr: u64,
        _size: u64,
        _host_addr: *mut u8,
    ) -> Result<(), PlatformError> {
        Ok(())
    }

    fn register_irqfd(
        &self,
        _event: &dyn event::InterruptEvent,
        _gsi: u32,
    ) -> Result<(), PlatformError> {
        Ok(())
    }

    fn create_vcpu(&self, _index: u64) -> Result<Self::Vcpu, PlatformError> {
        Ok(MockVcpu)
    }
}

struct MockPlatform;

impl Platform for MockPlatform {
    type Vm = MockVm;

    fn new() -> Result<Self, PlatformError> {
        Ok(MockPlatform)
    }

    fn create_vm(&self) -> Result<Self::Vm, PlatformError> {
        Ok(MockVm)
    }
}

// ── Tests ───────────────────────────────────────────────────────────

#[test]
fn mock_platform_can_be_instantiated() {
    let platform = MockPlatform::new().expect("mock platform creation should succeed");
    let _vm = platform
        .create_vm()
        .expect("mock VM creation should succeed");
}

#[test]
fn mock_vm_operations_succeed() {
    let vm = MockVm;
    vm.create_irq_chip()
        .expect("create_irq_chip should succeed");
    vm.create_pit().expect("create_pit should succeed");
    vm.register_memory(0, 0x1000, 0x1000, std::ptr::null_mut())
        .expect("register_memory should succeed");
    let mock_event = event::MockInterruptEvent::new();
    vm.register_irqfd(&mock_event, 0)
        .expect("register_irqfd should succeed");
}

#[test]
fn mock_vcpu_operations_succeed() {
    let vm = MockVm;
    let mut vcpu = vm.create_vcpu(0).expect("create_vcpu should succeed");

    let regs = regs::StandardRegs::default();
    vcpu.set_regs(&regs).expect("set_regs should succeed");
    let got = vcpu.get_regs().expect("get_regs should succeed");
    assert_eq!(got, regs::StandardRegs::default());

    let sregs = regs::SpecialRegs::default();
    vcpu.set_sregs(&sregs).expect("set_sregs should succeed");
    let _got = vcpu.get_sregs().expect("get_sregs should succeed");

    let exit = vcpu.run().expect("run should succeed");
    assert_eq!(exit, VmExit::Halt);
}

#[test]
fn vm_exit_variants_construct_correctly() {
    let io_in = VmExit::IoIn {
        port: 0x3f8,
        size: 1,
    };
    assert!(matches!(
        io_in,
        VmExit::IoIn {
            port: 0x3f8,
            size: 1
        }
    ));

    let data = ExitData::from_slice(&[0x42]);
    let io_out = VmExit::IoOut {
        port: 0x3f8,
        data: data.clone(),
    };
    assert!(matches!(io_out, VmExit::IoOut { port: 0x3f8, .. }));

    let mmio_read = VmExit::MmioRead {
        addr: 0xFEE0_0000,
        size: 4,
    };
    assert!(matches!(
        mmio_read,
        VmExit::MmioRead {
            addr: 0xFEE0_0000,
            size: 4
        }
    ));

    let mmio_write = VmExit::MmioWrite {
        addr: 0xFEE0_0000,
        data,
    };
    assert!(matches!(mmio_write, VmExit::MmioWrite { .. }));

    assert!(matches!(VmExit::Halt, VmExit::Halt));
    assert!(matches!(VmExit::Shutdown, VmExit::Shutdown));
    assert!(matches!(VmExit::Reboot, VmExit::Reboot));
}

#[test]
fn platform_error_unsupported_exists() {
    let err = PlatformError::Unsupported;
    let msg = err.to_string();
    assert!(msg.contains("unsupported"), "error message: {msg}");
}

#[test]
fn platform_error_system_wraps_io_error() {
    let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "test");
    let err = PlatformError::System(io_err);
    assert!(err.to_string().contains("test"));
}

#[test]
fn exit_data_from_slice_and_back() {
    let data = ExitData::from_slice(&[0x41, 0x42, 0x43]);
    assert_eq!(data.as_bytes(), &[0x41, 0x42, 0x43]);
    assert_eq!(data.len(), 3);
    assert!(!data.is_empty());
}

#[test]
fn exit_data_empty() {
    let data = ExitData::from_slice(&[]);
    assert!(data.is_empty());
    assert_eq!(data.len(), 0);
    assert_eq!(data.as_bytes(), &[] as &[u8]);
}
