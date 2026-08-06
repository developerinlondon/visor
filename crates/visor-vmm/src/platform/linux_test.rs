use super::*;
use crate::platform::regs::StandardRegs;
use crate::platform::{Platform, PlatformError, VcpuOps, VmOps};

#[test]
fn kvm_platform_new_succeeds() {
    let platform = KvmPlatform::new().expect("failed to open /dev/kvm");
    // Just verifying it doesn't panic or error
    drop(platform);
}

#[test]
fn kvm_create_vm_returns_valid_vm() {
    let platform = KvmPlatform::new().expect("failed to open /dev/kvm");
    let _vm = platform.create_vm().expect("failed to create VM");
}

#[test]
fn kvm_vm_create_irq_chip_succeeds() {
    let platform = KvmPlatform::new().expect("failed to open /dev/kvm");
    let vm = platform.create_vm().expect("failed to create VM");
    vm.create_irq_chip().expect("failed to create IRQ chip");
}

#[test]
fn kvm_vm_create_pit_succeeds() {
    let platform = KvmPlatform::new().expect("failed to open /dev/kvm");
    let vm = platform.create_vm().expect("failed to create VM");
    vm.create_irq_chip().expect("failed to create IRQ chip");
    vm.create_pit().expect("failed to create PIT");
}

#[test]
fn kvm_vm_create_vcpu_succeeds() {
    let platform = KvmPlatform::new().expect("failed to open /dev/kvm");
    let vm = platform.create_vm().expect("failed to create VM");
    let _vcpu = vm.create_vcpu(0).expect("failed to create vCPU");
}

#[test]
fn kvm_vcpu_set_get_regs_roundtrip() {
    let platform = KvmPlatform::new().expect("failed to open /dev/kvm");
    let vm = platform.create_vm().expect("failed to create VM");
    let vcpu = vm.create_vcpu(0).expect("failed to create vCPU");

    let regs = StandardRegs {
        rax: 0x1234,
        rip: 0xDEAD_BEEF,
        rsp: 0x7000,
        rflags: 0x0000_0002,
        ..Default::default()
    };
    vcpu.set_regs(&regs).expect("failed to set regs");
    let got = vcpu.get_regs().expect("failed to get regs");

    assert_eq!(got.rax, regs.rax);
    assert_eq!(got.rip, regs.rip);
    assert_eq!(got.rsp, regs.rsp);
    assert_eq!(got.rflags, regs.rflags);
}

#[test]
fn kvm_platform_implements_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<KvmPlatform>();
}

#[test]
fn kvm_vm_implements_send() {
    fn assert_send<T: Send>() {}
    assert_send::<KvmVm>();
}

#[test]
fn kvm_vcpu_implements_send() {
    fn assert_send<T: Send>() {}
    assert_send::<KvmVcpu>();
}

#[test]
fn kvm_platform_error_on_unsupported_shows_message() {
    let err = PlatformError::Unsupported;
    assert!(err.to_string().contains("unsupported"));
}

#[test]
fn kvm_platform_kvm_returns_valid_reference() {
    let platform = KvmPlatform::new().expect("failed to open /dev/kvm");
    let kvm = platform.kvm();
    // Verify the Kvm handle is valid by checking the API version.
    assert_eq!(kvm.get_api_version(), 12);
}

#[test]
fn tap_ifreq_rejects_non_ascii_interface_names() {
    let err = tap_ifreq("tap-uberascii-\u{00E4}")
        .expect_err("non-ASCII interface names should be rejected");

    assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
    assert!(err.to_string().contains("non-ASCII"));
}

// ── LinuxEventFd tests ────────────────────────────────────────────

use crate::platform::event::InterruptEvent;

#[test]
fn linux_eventfd_new_succeeds() {
    let eventfd = LinuxEventFd::new().expect("failed to create eventfd");
    drop(eventfd);
}

#[test]
fn linux_eventfd_trigger_succeeds() {
    let eventfd = LinuxEventFd::new().expect("failed to create eventfd");
    eventfd.trigger().expect("trigger should succeed");
}

#[test]
fn linux_eventfd_as_raw_returns_valid_fd() {
    let eventfd = LinuxEventFd::new().expect("failed to create eventfd");
    let raw = eventfd.as_raw();
    // A valid fd is >= 0.
    assert!(raw >= 0, "raw fd should be non-negative, got {raw}");
}

#[test]
fn linux_eventfd_implements_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<LinuxEventFd>();
}

#[test]
fn linux_eventfd_usable_as_trait_object() {
    let eventfd = LinuxEventFd::new().expect("failed to create eventfd");
    let event: std::sync::Arc<dyn InterruptEvent> = std::sync::Arc::new(eventfd);
    event
        .trigger()
        .expect("trigger via trait object should succeed");
}
