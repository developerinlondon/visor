//! Windows Hypervisor Platform (WHP) stub.
//!
//! This is a placeholder implementation. All methods return
//! [`PlatformError::Unsupported`] until WHP support is implemented.

use super::regs::{SpecialRegs, StandardRegs};
use super::{Platform, PlatformError, VcpuOps, VmExit, VmOps, event::InterruptEvent};

/// Windows Hypervisor Platform (stub).
pub struct WhpPlatform;

/// WHP virtual machine (stub).
pub struct WhpVm;

/// WHP virtual CPU (stub).
pub struct WhpVcpu;

impl Platform for WhpPlatform {
    type Vm = WhpVm;

    fn new() -> Result<Self, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn create_vm(&self) -> Result<Self::Vm, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

impl VmOps for WhpVm {
    type Vcpu = WhpVcpu;

    fn create_irq_chip(&self) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn create_pit(&self) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn register_memory(
        &self,
        _slot: u32,
        _guest_addr: u64,
        _size: u64,
        _host_addr: *mut u8,
    ) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn register_irqfd(&self, _event: &dyn InterruptEvent, _gsi: u32) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn create_vcpu(&self, _index: u64) -> Result<Self::Vcpu, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

impl VcpuOps for WhpVcpu {
    fn set_regs(&self, _regs: &StandardRegs) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn get_regs(&self) -> Result<StandardRegs, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn set_sregs(&self, _sregs: &SpecialRegs) -> Result<(), PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn get_sregs(&self) -> Result<SpecialRegs, PlatformError> {
        Err(PlatformError::Unsupported)
    }

    fn run(&mut self) -> Result<VmExit, PlatformError> {
        Err(PlatformError::Unsupported)
    }
}

#[cfg(test)]
#[path = "windows_test.rs"]
mod tests;
