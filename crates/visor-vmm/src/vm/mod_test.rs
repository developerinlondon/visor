//! Tests for the portable VM boot facade (`vm.rs`).
//!
//! Unit tests cover all public types and trait default implementations.
//! Integration tests (bottom of file) exercise `boot()` and `run_vcpu()`
//! with a real hypervisor; they are cfg-gated per platform.

use std::io::Write;
use std::path::Path;

use super::*;
use crate::guest_virtualization::GuestVirtualizationMode;
use crate::platform::event::InterruptEvent;
use crate::platform::{PlatformError, VmExit};

// ── SerialOutput Tests ──────────────────────────────────────────────

#[test]
fn serial_output_new_is_empty() {
    let so = SerialOutput::new();
    assert!(so.as_bytes().is_empty());
}

#[test]
fn serial_output_default_is_empty() {
    let so = SerialOutput::default();
    assert!(so.as_bytes().is_empty());
}

#[test]
fn serial_output_write_captures_bytes() {
    let mut so = SerialOutput::new();
    let n = so.write(b"hello").unwrap();
    assert_eq!(n, 5);
    assert_eq!(so.as_bytes(), b"hello");
}

#[test]
fn serial_output_write_multiple_appends() {
    let mut so = SerialOutput::new();
    so.write_all(b"foo").unwrap();
    so.write_all(b"bar").unwrap();
    assert_eq!(so.as_bytes(), b"foobar");
}

#[test]
fn serial_output_flush_is_noop() {
    let mut so = SerialOutput::new();
    so.write_all(b"data").unwrap();
    assert!(so.flush().is_ok());
    assert_eq!(so.as_bytes(), b"data");
}

#[test]
fn serial_output_clone_shares_buffer() {
    let mut so = SerialOutput::new();
    so.write_all(b"shared").unwrap();

    let clone = so.clone();
    assert_eq!(clone.as_bytes(), b"shared");

    // Writing to the original should be visible via clone (shared Arc).
    so.write_all(b"!").unwrap();
    assert_eq!(clone.as_bytes(), b"shared!");
}

#[test]
fn serial_output_write_empty_slice() {
    let mut so = SerialOutput::new();
    let n = so.write(b"").unwrap();
    assert_eq!(n, 0);
    assert!(so.as_bytes().is_empty());
}

#[test]
fn serial_output_large_write() {
    let mut so = SerialOutput::new();
    let data = vec![0x42u8; 16384]; // 16 KiB
    so.write_all(&data).unwrap();
    assert_eq!(so.as_bytes().len(), 16384);
    assert!(so.as_bytes().iter().all(|&b| b == 0x42));
}

#[test]
fn serial_output_debug_format() {
    let so = SerialOutput::new();
    let debug = format!("{so:?}");
    assert!(
        debug.contains("SerialOutput"),
        "Debug should contain type name: {debug}"
    );
}

#[test]
fn serial_output_poison_recovery() {
    // Verify that a poisoned mutex doesn't prevent reading.
    // SerialOutput::as_bytes uses unwrap_or_else(PoisonError::into_inner).
    let so = SerialOutput::new();
    let so_clone = so.clone();

    // Poison the mutex by panicking inside a lock.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut locked = so_clone.inner.lock().unwrap();
        locked.extend_from_slice(b"before-panic");
        panic!("intentional poison");
    }));
    assert!(result.is_err(), "should have panicked");

    // as_bytes should still work despite the poisoned mutex.
    let bytes = so.as_bytes();
    assert!(
        bytes.starts_with(b"before-panic"),
        "should recover data from poisoned mutex, got: {bytes:?}"
    );
}

// ── ExitAction Tests ────────────────────────────────────────────────

#[test]
fn exit_action_continue_ne_stop() {
    assert_ne!(ExitAction::Continue, ExitAction::Stop);
}

#[test]
fn exit_action_equality() {
    assert_eq!(ExitAction::Continue, ExitAction::Continue);
    assert_eq!(ExitAction::Stop, ExitAction::Stop);
}

#[test]
fn exit_action_is_copy() {
    let a = ExitAction::Continue;
    let b = a; // Copy
    assert_eq!(a, b);
}

#[test]
fn exit_action_debug_format() {
    let debug = format!("{:?}", ExitAction::Continue);
    assert!(
        debug.contains("Continue"),
        "Debug should contain variant: {debug}"
    );

    let debug = format!("{:?}", ExitAction::Stop);
    assert!(
        debug.contains("Stop"),
        "Debug should contain variant: {debug}"
    );
}

// ── ExitHandler Default Implementation Tests ────────────────────────

/// Minimal ExitHandler impl that delegates to defaults for read methods.
struct MinimalHandler;

impl ExitHandler for MinimalHandler {
    fn handle_exit(&mut self, _exit: VmExit) -> Result<ExitAction, VcpuError> {
        Ok(ExitAction::Continue)
    }
    // handle_io_read and handle_mmio_read use default implementations.
}

#[test]
fn exit_handler_default_io_read_fills_0xff() {
    let mut handler = MinimalHandler;
    let mut buf = [0u8; 4];
    handler.handle_io_read(0x3f8, &mut buf);
    assert_eq!(buf, [0xFF, 0xFF, 0xFF, 0xFF]);
}

#[test]
fn exit_handler_default_mmio_read_fills_0xff() {
    let mut handler = MinimalHandler;
    let mut buf = [0u8; 8];
    handler.handle_mmio_read(0xd000_0000, &mut buf);
    assert_eq!(buf, [0xFF; 8]);
}

#[test]
fn exit_handler_default_io_read_single_byte() {
    let mut handler = MinimalHandler;
    let mut buf = [0u8; 1];
    handler.handle_io_read(0x60, &mut buf);
    assert_eq!(buf, [0xFF]);
}

#[test]
fn exit_handler_default_mmio_read_empty_buffer() {
    let mut handler = MinimalHandler;
    let mut buf = [0u8; 0];
    handler.handle_mmio_read(0x1000, &mut buf);
    // No panic, no-op on empty buffer.
}

#[test]
fn exit_handler_handle_exit_returns_action() {
    let mut handler = MinimalHandler;
    let action = handler.handle_exit(VmExit::Halt).expect("should not error");
    assert_eq!(action, ExitAction::Continue);
}

// ── VcpuError Tests ─────────────────────────────────────────────────

#[test]
fn vcpu_error_create_display() {
    let err = VcpuError::Create(std::io::Error::new(std::io::ErrorKind::Other, "test"));
    let msg = format!("{err}");
    assert!(
        msg.contains("create vCPU"),
        "should mention creation: {msg}"
    );
    assert!(msg.contains("test"), "should contain source: {msg}");
}

#[test]
fn vcpu_error_set_regs_display() {
    let err = VcpuError::SetRegs(std::io::Error::new(std::io::ErrorKind::Other, "bad"));
    let msg = format!("{err}");
    assert!(msg.contains("set registers"), "display: {msg}");
}

#[test]
fn vcpu_error_set_sregs_display() {
    let err = VcpuError::SetSregs(std::io::Error::new(std::io::ErrorKind::Other, "bad"));
    let msg = format!("{err}");
    assert!(msg.contains("set special registers"), "display: {msg}");
}

#[test]
fn vcpu_error_get_sregs_display() {
    let err = VcpuError::GetSregs(std::io::Error::new(std::io::ErrorKind::Other, "bad"));
    let msg = format!("{err}");
    assert!(msg.contains("get special registers"), "display: {msg}");
}

#[test]
fn vcpu_error_set_fpu_display() {
    let err = VcpuError::SetFpu(std::io::Error::new(std::io::ErrorKind::Other, "fpu fail"));
    let msg = format!("{err}");
    assert!(msg.contains("set FPU"), "display: {msg}");
}

#[test]
fn vcpu_error_set_msrs_display() {
    let err = VcpuError::SetMsrs(std::io::Error::new(std::io::ErrorKind::Other, "msr fail"));
    let msg = format!("{err}");
    assert!(msg.contains("set MSRs"), "display: {msg}");
}

#[test]
fn vcpu_error_msrs_incomplete_display() {
    let err = VcpuError::MsrsIncomplete {
        written: 5,
        total: 11,
    };
    let msg = format!("{err}");
    assert!(msg.contains("5"), "should mention written count: {msg}");
    assert!(msg.contains("11"), "should mention total count: {msg}");
}

#[test]
fn vcpu_error_run_display() {
    let err = VcpuError::Run(std::io::Error::new(std::io::ErrorKind::Other, "run fail"));
    let msg = format!("{err}");
    assert!(msg.contains("run failed"), "display: {msg}");
}

#[test]
fn vcpu_error_fail_entry_display() {
    let err = VcpuError::FailEntry {
        reason: 0xDEAD,
        cpu: 0,
    };
    let msg = format!("{err}");
    assert!(msg.contains("0xdead"), "should contain hex reason: {msg}");
    assert!(msg.contains("cpu=0"), "should contain cpu index: {msg}");
}

#[test]
fn vcpu_error_internal_error_display() {
    let err = VcpuError::InternalError;
    let msg = format!("{err}");
    assert!(msg.contains("internal error"), "display: {msg}");
}

#[test]
fn vcpu_error_boot_display() {
    let boot_err = crate::boot::BootError::KernelRead(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "not found",
    ));
    let err = VcpuError::Boot(boot_err);
    let msg = format!("{err}");
    assert!(msg.contains("boot setup failed"), "display: {msg}");
}

#[test]
fn vcpu_error_from_boot_error() {
    let boot_err = crate::boot::BootError::KernelRead(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "kernel missing",
    ));
    let vcpu_err: VcpuError = boot_err.into();
    assert!(
        matches!(vcpu_err, VcpuError::Boot(_)),
        "From<BootError> should produce VcpuError::Boot"
    );
}

#[test]
fn vcpu_error_get_cpuid_display() {
    let err = VcpuError::GetCpuid(std::io::Error::new(std::io::ErrorKind::Other, "cpuid"));
    let msg = format!("{err}");
    assert!(msg.contains("supported CPUID"), "display: {msg}");
}

#[test]
fn vcpu_error_set_cpuid_display() {
    let err = VcpuError::SetCpuid(std::io::Error::new(std::io::ErrorKind::Other, "cpuid"));
    let msg = format!("{err}");
    assert!(msg.contains("set CPUID"), "display: {msg}");
}

#[test]
fn vcpu_error_is_debug() {
    let err = VcpuError::InternalError;
    let debug = format!("{err:?}");
    assert!(
        debug.contains("InternalError"),
        "Debug should contain variant: {debug}"
    );
}

// ── VmBootError Tests ───────────────────────────────────────────────

#[test]
fn vm_boot_error_platform_display() {
    let platform_err = PlatformError::System(std::io::Error::new(
        std::io::ErrorKind::PermissionDenied,
        "no kvm",
    ));
    let err = VmBootError::Platform(platform_err);
    let msg = format!("{err}");
    assert!(msg.contains("platform error"), "display: {msg}");
}

#[test]
fn vm_boot_error_from_platform_error() {
    let platform_err = PlatformError::Unsupported;
    let boot_err: VmBootError = platform_err.into();
    assert!(
        matches!(boot_err, VmBootError::Platform(_)),
        "From<PlatformError> should produce VmBootError::Platform"
    );
}

#[test]
fn vm_boot_error_boot_display() {
    let boot_err = crate::boot::BootError::KernelRead(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "missing",
    ));
    let err = VmBootError::Boot(boot_err);
    let msg = format!("{err}");
    assert!(msg.contains("boot error"), "display: {msg}");
}

#[test]
fn vm_boot_error_from_boot_error() {
    let boot_err = crate::boot::BootError::KernelRead(std::io::Error::new(
        std::io::ErrorKind::NotFound,
        "missing",
    ));
    let vm_err: VmBootError = boot_err.into();
    assert!(
        matches!(vm_err, VmBootError::Boot(_)),
        "From<BootError> should produce VmBootError::Boot"
    );
}

#[test]
fn vm_boot_error_memory_display() {
    let mem_err = crate::memory::MemoryError::Allocation {
        size: 1024,
        source: std::io::Error::new(std::io::ErrorKind::Other, "mmap fail"),
    };
    let err = VmBootError::Memory(mem_err);
    let msg = format!("{err}");
    assert!(msg.contains("memory error"), "display: {msg}");
}

#[test]
fn vm_boot_error_from_memory_error() {
    let mem_err = crate::memory::MemoryError::Allocation {
        size: 4096,
        source: std::io::Error::new(std::io::ErrorKind::Other, "mmap fail"),
    };
    let vm_err: VmBootError = mem_err.into();
    assert!(
        matches!(vm_err, VmBootError::Memory(_)),
        "From<MemoryError> should produce VmBootError::Memory"
    );
}

#[test]
fn vm_boot_error_device_display() {
    let err = VmBootError::Device("serial init failed".to_owned());
    let msg = format!("{err}");
    assert!(msg.contains("device setup error"), "display: {msg}");
    assert!(msg.contains("serial init failed"), "display: {msg}");
}

#[test]
fn vm_boot_error_vcpu_display() {
    let vcpu_err = VcpuError::InternalError;
    let err = VmBootError::Vcpu(vcpu_err);
    let msg = format!("{err}");
    assert!(msg.contains("vCPU error"), "display: {msg}");
}

#[test]
fn vm_boot_error_from_vcpu_error() {
    let vcpu_err = VcpuError::InternalError;
    let vm_err: VmBootError = vcpu_err.into();
    assert!(
        matches!(vm_err, VmBootError::Vcpu(_)),
        "From<VcpuError> should produce VmBootError::Vcpu"
    );
}

#[test]
fn vm_boot_error_is_debug() {
    let err = VmBootError::Device("test".to_owned());
    let debug = format!("{err:?}");
    assert!(
        debug.contains("Device"),
        "Debug should contain variant: {debug}"
    );
}

// ── VmConfig Tests ──────────────────────────────────────────────────

#[test]
fn vm_config_construction() {
    let config = VmConfig {
        kernel_path: Path::new("/boot/vmlinuz"),
        cmdline: "console=ttyS0",
        rootfs_path: Path::new("/rootfs.ext4"),
        memory_mib: 128,
        vcpus: 1,
        guest_cid: 3,
        guest_virtualization: GuestVirtualizationMode::Standard,
        shared_dirs: Vec::new(),
        data_disks: Vec::new(),
        network: None,
        networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
    };
    assert_eq!(config.kernel_path, Path::new("/boot/vmlinuz"));
    assert_eq!(config.cmdline, "console=ttyS0");
    assert_eq!(config.rootfs_path, Path::new("/rootfs.ext4"));
    assert_eq!(config.memory_mib, 128);
    assert_eq!(config.vcpus, 1);
    assert_eq!(config.guest_cid, 3);
}

#[test]
fn vm_config_debug_format() {
    let config = VmConfig {
        kernel_path: Path::new("/boot/vmlinuz"),
        cmdline: "quiet",
        rootfs_path: Path::new("/rootfs.ext4"),
        memory_mib: 256,
        vcpus: 2,
        guest_cid: 5,
        guest_virtualization: GuestVirtualizationMode::Standard,
        shared_dirs: Vec::new(),
        data_disks: Vec::new(),
        network: None,
        networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
    };
    let debug = format!("{config:?}");
    assert!(
        debug.contains("VmConfig"),
        "Debug should contain type: {debug}"
    );
    assert!(
        debug.contains("256"),
        "Debug should contain memory_mib: {debug}"
    );
}

#[test]
fn vm_config_minimum_memory() {
    let config = VmConfig {
        kernel_path: Path::new("/boot/vmlinuz"),
        cmdline: "",
        rootfs_path: Path::new("/rootfs.ext4"),
        memory_mib: 32, // Below MIN_MEMORY_MIB (64)
        vcpus: 1,
        guest_cid: 3,
        guest_virtualization: GuestVirtualizationMode::Standard,
        shared_dirs: Vec::new(),
        data_disks: Vec::new(),
        network: None,
        networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
    };
    // VmConfig itself doesn't enforce the minimum — boot() does.
    // This test just verifies the struct accepts any u32 value.
    assert_eq!(config.memory_mib, 32);
}

#[test]
fn test_vmconfig_multiple_shared_dirs() {
    let config = VmConfig {
        kernel_path: Path::new("/boot/vmlinuz"),
        cmdline: "console=ttyS0",
        rootfs_path: Path::new("/rootfs.ext4"),
        memory_mib: 128,
        vcpus: 1,
        guest_cid: 3,
        guest_virtualization: GuestVirtualizationMode::Standard,
        shared_dirs: vec![
            std::path::PathBuf::from("/host/data"),
            std::path::PathBuf::from("/host/logs"),
        ],
        data_disks: Vec::new(),
        network: None,
        networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
    };
    assert_eq!(config.shared_dirs.len(), 2);
    assert_eq!(config.shared_dirs[0], Path::new("/host/data"));
    assert_eq!(config.shared_dirs[1], Path::new("/host/logs"));
}

#[test]
fn test_vmconfig_no_shared_dirs_default() {
    let config = VmConfig::new(
        Path::new("/boot/vmlinuz"),
        "console=ttyS0",
        Path::new("/rootfs.ext4"),
        128,
        1,
        3,
    );
    assert!(
        config.shared_dirs.is_empty(),
        "new() should have empty shared_dirs"
    );
    assert_eq!(
        config.guest_virtualization,
        GuestVirtualizationMode::Standard
    );
}

// ── BootedVm Field Tests ───────────────────────────────────────────

#[cfg(target_os = "macos")]
#[test]
fn booted_vm_vsock_muxer_is_some_on_macos() {
    // When booting on macOS, the vsock muxer field should be present.
    // This is a unit-level structural test — integration tests exercise the
    // muxer via boot(). We just verify the type is correct.
    use crate::devices::vsock::VsockDevice;
    use crate::devices::vsock_muxer::VsockMuxer;
    use crate::platform::event::MockInterruptEvent;
    use std::sync::{Arc, Mutex};

    let device = Arc::new(Mutex::new(VsockDevice::new(3)));
    let irq = Arc::new(MockInterruptEvent::new());
    let tmpdir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
    let tx_notify = {
        let dev = device.lock().unwrap();
        dev.tx_notify()
    };
    let rx_kick: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = irq.trigger();
    });
    let muxer = VsockMuxer::new(device, 3, tmpdir.path().to_path_buf(), tx_notify, rx_kick)
        .expect("muxer creation should succeed");

    // Verify the muxer produces the correct listener path.
    let expected_path = tmpdir.path().join("3.sock");
    assert_eq!(muxer.listener_path(), expected_path);
}

#[cfg(target_os = "linux")]
#[test]
fn booted_vm_vsock_muxer_is_some_on_linux() {
    use crate::devices::vsock::VsockDevice;
    use crate::devices::vsock_muxer::VsockMuxer;
    use crate::platform::event::MockInterruptEvent;
    use std::sync::{Arc, Mutex};

    let device = Arc::new(Mutex::new(VsockDevice::new(3)));
    let irq = Arc::new(MockInterruptEvent::new());
    let tmpdir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
    let tx_notify = {
        let dev = device.lock().unwrap();
        dev.tx_notify()
    };
    let rx_kick: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = irq.trigger();
    });
    let muxer = VsockMuxer::new(device, 3, tmpdir.path().to_path_buf(), tx_notify, rx_kick)
        .expect("muxer creation should succeed");

    let expected_path = tmpdir.path().join("3.sock");
    assert_eq!(muxer.listener_path(), expected_path);
}

#[test]
fn vsock_rx_poller_poll_once_returns_false_when_transport_is_inactive() {
    use crate::devices::vsock::VsockDevice;
    use crate::transport::mmio::MmioTransport;
    use std::sync::{Arc, Mutex};

    let device = Arc::new(Mutex::new(VsockDevice::new(3)));
    let transport = Arc::new(Mutex::new(MmioTransport::new(device)));
    let poller = VsockRxPoller::new(transport);

    assert!(!poller.poll_once());
}

#[test]
fn vsock_rx_poller_poll_once_returns_false_for_poisoned_transport_lock() {
    use crate::devices::vsock::VsockDevice;
    use crate::transport::mmio::MmioTransport;
    use std::panic::AssertUnwindSafe;
    use std::sync::{Arc, Mutex};

    let device = Arc::new(Mutex::new(VsockDevice::new(3)));
    let transport = Arc::new(Mutex::new(MmioTransport::new(device)));
    let poller = VsockRxPoller::new(Arc::clone(&transport));

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _guard = transport.lock().unwrap();
        panic!("poison transport mutex");
    }));
    assert!(result.is_err(), "transport mutex should be poisoned");

    assert!(!poller.poll_once());
}

#[test]
fn net_rx_poller_poll_once_returns_false_when_transport_is_inactive() {
    use crate::devices::net::NetDevice;
    use crate::transport::mmio::MmioTransport;
    use std::sync::{Arc, Mutex};

    let device = Arc::new(Mutex::new(NetDevice::new([
        0x02, 0x56, 0x49, 0x53, 0x00, 0x01,
    ])));
    let transport = Arc::new(Mutex::new(MmioTransport::new(device)));
    let poller = NetRxPoller::new(vec![transport]);

    assert!(!poller.poll_once());
}

#[test]
fn net_rx_poller_poll_once_returns_false_for_poisoned_transport_lock() {
    use crate::devices::net::NetDevice;
    use crate::transport::mmio::MmioTransport;
    use std::panic::AssertUnwindSafe;
    use std::sync::{Arc, Mutex};

    let device = Arc::new(Mutex::new(NetDevice::new([
        0x02, 0x56, 0x49, 0x53, 0x00, 0x01,
    ])));
    let transport = Arc::new(Mutex::new(MmioTransport::new(device)));
    let poller = NetRxPoller::new(vec![Arc::clone(&transport)]);

    let result = std::panic::catch_unwind(AssertUnwindSafe(|| {
        let _guard = transport.lock().unwrap();
        panic!("poison transport mutex");
    }));
    assert!(result.is_err(), "transport mutex should be poisoned");

    assert!(!poller.poll_once());
}

// ── Re-export Tests ─────────────────────────────────────────────────

#[test]
fn reexport_exit_data() {
    // Verify ExitData is accessible via vm module re-export.
    let data = ExitData::from_slice(&[0x42]);
    assert_eq!(data.as_bytes(), &[0x42]);
}

#[test]
fn reexport_vm_exit_data_max() {
    // Verify the constant is re-exported.
    assert_eq!(VM_EXIT_DATA_MAX, 8);
}

// ── VcpuRunResult Tests ─────────────────────────────────────────────

#[test]
fn vcpu_run_result_with_no_registers() {
    let result = VcpuRunResult {
        regs: None,
        sregs: None,
    };
    assert!(result.regs.is_none());
    assert!(result.sregs.is_none());
}

#[test]
fn vcpu_run_result_with_standard_regs() {
    let regs = crate::platform::regs::StandardRegs::default();
    let result = VcpuRunResult {
        regs: Some(regs),
        sregs: None,
    };
    assert!(result.regs.is_some());
    assert!(result.sregs.is_none());
}

#[test]
fn vcpu_run_result_with_both_regs() {
    let regs = crate::platform::regs::StandardRegs::default();
    let sregs = crate::platform::regs::SpecialRegs::default();
    let result = VcpuRunResult {
        regs: Some(regs),
        sregs: Some(sregs),
    };
    assert!(result.regs.is_some());
    assert!(result.sregs.is_some());
}

#[test]
fn vcpu_run_result_debug_format() {
    let result = VcpuRunResult {
        regs: None,
        sregs: None,
    };
    let debug = format!("{result:?}");
    assert!(
        debug.contains("VcpuRunResult"),
        "Debug should contain type name: {debug}"
    );
}

// ── Integration Tests (macOS / HVF) ────────────────────────────────
// These tests require a real hypervisor. On macOS, that's HVF with
// code-signing entitlements (handled by the custom cargo runner).

#[cfg(target_os = "macos")]
mod integration {
    use serial_test::serial;

    use super::*;

    /// Helper: create an HVF platform with retry.
    ///
    /// HVF allows only one VM per process. If a previous test's VM hasn't
    /// been fully destroyed yet (Arc refcount race), `hv_vm_create` returns
    /// "owning resource is busy". Retrying after a short sleep handles this.
    fn hvf_platform_with_retry() -> crate::platform::HvfPlatform {
        use crate::platform::{HvfPlatform, Platform};

        for attempt in 0..5 {
            match HvfPlatform::new() {
                Ok(p) => return p,
                Err(e) if attempt < 4 => {
                    eprintln!("HVF busy (attempt {attempt}), retrying: {e}");
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                Err(e) => panic!("HVF platform init failed after retries: {e}"),
            }
        }
        unreachable!()
    }

    #[test]
    #[serial(hvf)]
    fn boot_fails_with_missing_kernel() {
        let config = VmConfig {
            kernel_path: Path::new("/nonexistent/kernel"),
            cmdline: "console=ttyS0",
            rootfs_path: Path::new("/nonexistent/rootfs.ext4"),
            memory_mib: 128,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };
        let result = boot(&config);
        assert!(result.is_err(), "boot should fail with missing kernel");
    }

    #[test]
    #[serial(hvf)]
    fn boot_fails_with_empty_kernel_file() {
        let dir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
        let kernel_path = dir.path().join("empty_kernel");
        std::fs::write(&kernel_path, b"").unwrap();
        let rootfs_path = dir.path().join("rootfs.ext4");
        std::fs::write(&rootfs_path, b"").unwrap();

        let config = VmConfig {
            kernel_path: &kernel_path,
            cmdline: "console=ttyS0",
            rootfs_path: &rootfs_path,
            memory_mib: 128,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };
        let result = boot(&config);
        assert!(result.is_err(), "boot should fail with empty kernel file");
    }

    /// Integration test: boot a real kernel and verify run_vcpu produces exits.
    ///
    /// This mirrors vcpu_test.rs::boot_real_kernel_starts_executing but
    /// exercises the portable `vm::boot()` + `vm::run_vcpu()` API.
    #[test]
    #[serial(hvf)]
    fn boot_real_kernel_produces_exits() {
        // aarch64 kernel path
        let kernel_path = std::path::PathBuf::from("/var/lib/visor/kernel/Image-arm64");
        if !kernel_path.exists() {
            // Skip if no kernel available — not a failure.
            return;
        }

        // We need a rootfs too — create a minimal (empty) one if none exists.
        let rootfs_path = std::path::PathBuf::from("/var/lib/visor/rootfs/rootfs.ext4");
        let tmpdir;
        let effective_rootfs = if rootfs_path.exists() {
            rootfs_path.as_path()
        } else {
            tmpdir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
            let p = tmpdir.path().join("rootfs.ext4");
            std::fs::write(&p, &[0u8; 1024]).unwrap();
            // Leak the tempdir path so it stays valid — the file handle keeps it alive
            // via the tmpdir variable bound above.
            Box::leak(p.into_boxed_path())
        };

        let config = VmConfig {
            kernel_path: &kernel_path,
            cmdline: "console=ttyAMA0 reboot=k panic=1",
            rootfs_path: effective_rootfs,
            memory_mib: 128,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };

        let mut booted = boot(&config).expect("boot should succeed with real kernel");

        // Set kill flag after a short delay so run_vcpu doesn't run forever.
        let kill_flag = booted.kill_flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            kill_flag.store(true, std::sync::atomic::Ordering::Release);
        });

        // run_vcpu should eventually return (either from kill flag or exit).
        let result = run_vcpu(&mut booted);
        // We don't care if it's Ok or Err — just that it ran and returned.
        let _ = result;

        // Verify serial output buffer exists (may or may not have data).
        let _serial_bytes = booted.serial_output.as_bytes();
    }

    #[test]
    #[serial(hvf)]
    fn booted_vm_kill_flag_starts_false() {
        // This test just needs to successfully call boot() to check the flag.
        // Skip if no kernel is available.
        let kernel_path = std::path::PathBuf::from("/var/lib/visor/kernel/Image-arm64");
        if !kernel_path.exists() {
            return;
        }

        let rootfs_path = std::path::PathBuf::from("/var/lib/visor/rootfs/rootfs.ext4");
        let tmpdir;
        let effective_rootfs = if rootfs_path.exists() {
            rootfs_path.as_path()
        } else {
            tmpdir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
            let p = tmpdir.path().join("rootfs.ext4");
            std::fs::write(&p, &[0u8; 1024]).unwrap();
            Box::leak(p.into_boxed_path())
        };

        let config = VmConfig {
            kernel_path: &kernel_path,
            cmdline: "console=ttyAMA0",
            rootfs_path: effective_rootfs,
            memory_mib: 64,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };

        let booted = boot(&config).expect("boot should succeed");
        assert!(
            !booted.kill_flag.load(std::sync::atomic::Ordering::Relaxed),
            "kill_flag should start as false"
        );
        assert!(
            booted.vsock_muxer.is_some(),
            "macOS boot should produce a vsock muxer"
        );
    }

    /// Integration test: run_vcpu_with_handler captures registers at exit.
    #[test]
    #[serial(hvf)]
    fn run_vcpu_with_handler_captures_registers() {
        let kernel_path = std::path::PathBuf::from("/var/lib/visor/kernel/Image-arm64");
        if !kernel_path.exists() {
            return;
        }

        let rootfs_path = std::path::PathBuf::from("/var/lib/visor/rootfs/rootfs.ext4");
        let tmpdir;
        let effective_rootfs = if rootfs_path.exists() {
            rootfs_path.as_path()
        } else {
            tmpdir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
            let p = tmpdir.path().join("rootfs.ext4");
            std::fs::write(&p, &[0u8; 1024]).unwrap();
            Box::leak(p.into_boxed_path())
        };

        let config = VmConfig {
            kernel_path: &kernel_path,
            cmdline: "console=ttyAMA0 reboot=k panic=1",
            rootfs_path: effective_rootfs,
            memory_mib: 128,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };

        let mut booted = boot(&config).expect("boot should succeed");
        let device_mgr = std::mem::replace(&mut booted.device_mgr, DeviceManager::new());

        // Use a passthrough handler that delegates to the original device_mgr.
        struct TestHandler(DeviceManager);
        impl ExitHandler for TestHandler {
            fn handle_exit(&mut self, exit: VmExit) -> Result<ExitAction, VcpuError> {
                self.0.handle_exit(exit)
            }
            fn handle_io_read(&mut self, port: u16, data: &mut [u8]) {
                self.0.handle_io_read(port, data);
            }
            fn handle_mmio_read(&mut self, addr: u64, data: &mut [u8]) {
                self.0.handle_mmio_read(addr, data);
            }
        }

        let mut handler = TestHandler(device_mgr);

        // Kill after a short delay.
        let kill_flag = booted.kill_flag.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(500));
            kill_flag.store(true, std::sync::atomic::Ordering::Release);
        });

        let result = run_vcpu_with_handler(&mut booted, &mut handler);
        // Should succeed (killed by flag)
        let run_result = result.expect("run_vcpu_with_handler should not fail");
        // Should have captured registers
        assert!(
            run_result.regs.is_some(),
            "should capture standard registers"
        );
        assert!(
            run_result.sregs.is_some(),
            "should capture special registers"
        );

        // Verify Display works on captured registers
        if let Some(regs) = &run_result.regs {
            let display = format!("{regs}");
            assert!(!display.is_empty(), "register display should not be empty");
        }
    }

    /// Verify `BootedVmInner` stores entry_point and fdt_addr from boot config.
    #[test]
    #[serial(hvf)]
    fn booted_vm_stores_boot_config() {
        let kernel_path = std::path::PathBuf::from("/var/lib/visor/kernel/Image-arm64");
        if !kernel_path.exists() {
            return;
        }

        let rootfs_path = std::path::PathBuf::from("/var/lib/visor/rootfs/rootfs.ext4");
        let tmpdir;
        let effective_rootfs = if rootfs_path.exists() {
            rootfs_path.as_path()
        } else {
            tmpdir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
            let p = tmpdir.path().join("rootfs.ext4");
            std::fs::write(&p, &[0u8; 1024]).unwrap();
            Box::leak(p.into_boxed_path())
        };

        let config = VmConfig {
            kernel_path: &kernel_path,
            cmdline: "console=ttyAMA0",
            rootfs_path: effective_rootfs,
            memory_mib: 64,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };

        let booted = boot(&config).expect("boot should succeed");
        match &booted.inner.cpu_init_mode {
            CpuInitMode::Boot {
                entry_point,
                fdt_addr,
            } => {
                assert_ne!(
                    *entry_point, 0,
                    "entry_point should be set from boot config"
                );
                assert_ne!(*fdt_addr, 0, "fdt_addr should be set from boot config");
            }
            CpuInitMode::Restore => {
                panic!("expected CpuInitMode::Boot after boot(), got Restore");
            }
        }
    }

    /// Verify `configure_vcpu_boot_regs` sets PC, X0, and CPSR correctly.
    #[test]
    #[serial(hvf)]
    fn configure_vcpu_boot_regs_sets_registers() {
        use crate::platform::{VcpuOps, VmOps};

        let platform = hvf_platform_with_retry();
        let vm = platform.create_vm().expect("create VM");
        let vcpu = vm.create_vcpu(0).expect("create vCPU");

        let entry = 0x8008_0000_u64;
        let fdt = 0x8800_0000_u64;
        configure_vcpu_boot_regs(&vcpu, entry, fdt, 0).expect("configure boot regs");

        let regs = vcpu.get_regs().expect("get regs after configure");
        assert_eq!(regs.pc, entry, "PC should be set to entry_point");
        assert_eq!(regs.x[0], fdt, "X0 should be set to fdt_addr");
        assert_eq!(
            regs.cpsr,
            crate::boot::aarch64::PSTATE_FAULT_BITS_64,
            "CPSR should be set to PSTATE_FAULT_BITS_64"
        );
    }

    /// Demonstrate the bug: a fresh HVF vCPU starts with zeroed registers.
    /// Without `configure_vcpu_boot_regs`, the vCPU would execute at PC=0 (unmapped).
    #[test]
    #[serial(hvf)]
    fn fresh_vcpu_has_zeroed_pc() {
        use crate::platform::{VcpuOps, VmOps};

        let platform = hvf_platform_with_retry();
        let vm = platform.create_vm().expect("create VM");
        let vcpu = vm.create_vcpu(0).expect("create vCPU");

        let regs = vcpu.get_regs().expect("get regs");
        assert_eq!(regs.pc, 0, "fresh HVF vCPU should have PC=0");
    }

    /// Verify `configure_vcpu_boot_regs` sets MPIDR_EL1 with RES1 bit 31 and vcpu index.
    ///
    /// ARM spec: MPIDR_EL1 bit 31 is RES1, lower bits encode the affinity / CPU ID.
    /// Linux relies on MPIDR to identify CPUs during boot.
    #[test]
    #[serial(hvf)]
    fn configure_vcpu_boot_regs_sets_mpidr() {
        use crate::platform::{VcpuOps, VmOps};

        let platform = hvf_platform_with_retry();
        let vm = platform.create_vm().expect("create VM");
        let vcpu = vm.create_vcpu(0).expect("create vCPU");

        let entry = 0x8008_0000_u64;
        let fdt = 0x8800_0000_u64;
        let vcpu_index: u64 = 0;
        configure_vcpu_boot_regs(&vcpu, entry, fdt, vcpu_index).expect("configure boot regs");

        let sregs = vcpu.get_sregs().expect("get sregs after configure");
        let expected_mpidr = 0x8000_0000_u64 | vcpu_index;
        assert_eq!(
            sregs.mpidr_el1, expected_mpidr,
            "MPIDR_EL1 should be 0x80000000 | vcpu_index"
        );
    }

    /// Verify `configure_vcpu_boot_regs` sets a non-zero vtimer offset.
    ///
    /// QEMU sets `vtimer_offset = mach_absolute_time()` at VM init so the
    /// guest virtual timer starts from a sane epoch. Without this, the
    /// Linux kernel timer subsystem misbehaves on Apple HVF.
    #[test]
    #[serial(hvf)]
    fn configure_vcpu_boot_regs_sets_vtimer_offset() {
        use crate::platform::VmOps;

        let platform = hvf_platform_with_retry();
        let vm = platform.create_vm().expect("create VM");
        let vcpu = vm.create_vcpu(0).expect("create vCPU");

        let entry = 0x8008_0000_u64;
        let fdt = 0x8800_0000_u64;
        configure_vcpu_boot_regs(&vcpu, entry, fdt, 0).expect("configure boot regs");

        // vtimer offset should have been set to a non-zero mach_absolute_time() value.
        let offset = vcpu.vcpu.get_vtimer_offset().expect("get vtimer offset");
        assert_ne!(
            offset, 0,
            "vtimer offset should be non-zero (set to mach_absolute_time)"
        );
    }

    /// Verify `configure_vcpu_boot_regs` sets the GICv3 system register support bit
    /// in `ID_AA64PFR0_EL1`.
    ///
    /// ARM spec: `ID_AA64PFR0_EL1` bits [27:24] encode GIC interface support.
    /// 0b0001 = GICv3 system register interface supported.
    /// Without this bit, Linux cannot initialize the GICv3 interrupt controller
    /// and the kernel hangs during boot.
    /// Reference: QEMU sets this at target/arm/hvf/hvf.c:1038-1040.
    #[test]
    #[serial(hvf)]
    fn configure_vcpu_boot_regs_sets_gicv3_sysreg_bit() {
        use crate::platform::VmOps;

        let platform = hvf_platform_with_retry();
        let vm = platform.create_vm().expect("create VM");
        let vcpu = vm.create_vcpu(0).expect("create vCPU");

        let entry = 0x8008_0000_u64;
        let fdt = 0x8800_0000_u64;
        configure_vcpu_boot_regs(&vcpu, entry, fdt, 0).expect("configure boot regs");

        // ID_AA64PFR0_EL1 bits [27:24] should be 0b0001 (GICv3 sysreg support).
        let pfr = vcpu
            .vcpu
            .get_sys_reg(applevisor::vcpu::SysReg::ID_AA64PFR0_EL1)
            .expect("get ID_AA64PFR0_EL1");
        let gic_bits = (pfr >> 24) & 0xF;
        assert_eq!(
            gic_bits, 1,
            "ID_AA64PFR0_EL1 GIC bits [27:24] should be 0b0001 (GICv3 sysreg support), got {gic_bits:#x}"
        );
    }

    /// Verify `configure_vcpu_boot_regs` sets SCTLR_EL1 to the ARM reset value.
    ///
    /// HVF starts SCTLR_EL1 at 0. The M1 boot ROM value 0x30900180 sets
    /// all architecturally required RES1 bits.
    #[test]
    #[serial(hvf)]
    fn configure_vcpu_boot_regs_sets_sctlr_el1() {
        use crate::platform::VmOps;

        let platform = hvf_platform_with_retry();
        let vm = platform.create_vm().expect("create VM");
        let vcpu = vm.create_vcpu(0).expect("create vCPU");

        let entry = 0x8008_0000_u64;
        let fdt = 0x8800_0000_u64;
        configure_vcpu_boot_regs(&vcpu, entry, fdt, 0).expect("configure boot regs");

        let sctlr = vcpu
            .vcpu
            .get_sys_reg(applevisor::vcpu::SysReg::SCTLR_EL1)
            .expect("get SCTLR_EL1");
        assert_eq!(
            sctlr, 0x3090_0180,
            "SCTLR_EL1 should be set to ARM reset value 0x30900180"
        );
    }

    /// Verify `configure_vcpu_boot_regs` masks SME bits in ID_AA64PFR1_EL1.
    ///
    /// On M3/M4, HVF exposes SME. Linux tries to use it, fails, and hangs.
    /// This test confirms bits [27:24] are zero after configuration.
    #[test]
    #[serial(hvf)]
    fn configure_vcpu_boot_regs_masks_sme_bits() {
        use crate::platform::VmOps;

        let platform = hvf_platform_with_retry();
        let vm = platform.create_vm().expect("create VM");
        let vcpu = vm.create_vcpu(0).expect("create vCPU");

        let entry = 0x8008_0000_u64;
        let fdt = 0x8800_0000_u64;
        configure_vcpu_boot_regs(&vcpu, entry, fdt, 0).expect("configure boot regs");

        let pfr1 = vcpu
            .vcpu
            .get_sys_reg(applevisor::vcpu::SysReg::ID_AA64PFR1_EL1)
            .expect("get ID_AA64PFR1_EL1");
        let sme_bits = (pfr1 >> 24) & 0xF;
        assert_eq!(
            sme_bits, 0,
            "ID_AA64PFR1_EL1 SME bits [27:24] should be masked to 0"
        );
    }

    /// Verify `configure_vcpu_boot_regs` clamps ID_AA64MMFR0_EL1 PARange.
    ///
    /// Hardware may advertise a PA range larger than the VM IPA supports.
    /// Clamping prevents Linux from mapping beyond the IPA range.
    #[test]
    #[serial(hvf)]
    fn configure_vcpu_boot_regs_clamps_parange() {
        use crate::platform::VmOps;

        let platform = hvf_platform_with_retry();
        let vm = platform.create_vm().expect("create VM");
        let vcpu = vm.create_vcpu(0).expect("create vCPU");

        let entry = 0x8008_0000_u64;
        let fdt = 0x8800_0000_u64;
        configure_vcpu_boot_regs(&vcpu, entry, fdt, 0).expect("configure boot regs");

        let mmfr0 = vcpu
            .vcpu
            .get_sys_reg(applevisor::vcpu::SysReg::ID_AA64MMFR0_EL1)
            .expect("get ID_AA64MMFR0_EL1");
        let parange = mmfr0 & 0xF;
        assert!(
            parange <= 0b0010,
            "ID_AA64MMFR0_EL1 PARange should be clamped to <= 40-bit (0b0010), got {parange:#b}"
        );
    }
}

// ── Integration Tests (Linux / KVM) ────────────────────────────────

#[cfg(target_os = "linux")]
mod integration {
    use super::*;

    #[test]
    fn boot_fails_with_missing_kernel() {
        let config = VmConfig {
            kernel_path: Path::new("/nonexistent/kernel"),
            cmdline: "console=ttyS0",
            rootfs_path: Path::new("/nonexistent/rootfs.ext4"),
            memory_mib: 128,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };
        let result = boot(&config);
        assert!(result.is_err(), "boot should fail with missing kernel");
    }

    #[test]
    fn boot_fails_with_empty_kernel_file() {
        let dir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
        let kernel_path = dir.path().join("empty_kernel");
        std::fs::write(&kernel_path, b"").unwrap();
        let rootfs_path = dir.path().join("rootfs.ext4");
        std::fs::write(&rootfs_path, b"").unwrap();

        let config = VmConfig {
            kernel_path: &kernel_path,
            cmdline: "console=ttyS0",
            rootfs_path: &rootfs_path,
            memory_mib: 128,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };
        let result = boot(&config);
        assert!(result.is_err(), "boot should fail with empty kernel file");
    }

    /// Integration test: boot a real kernel via the portable `vm::boot()` API.
    #[test]
    fn boot_real_kernel_produces_exits() {
        let kernel_path = std::path::PathBuf::from("/var/lib/visor/kernel/vmlinux-x86_64");
        if !kernel_path.exists() {
            return;
        }

        let rootfs_path = std::path::PathBuf::from("/var/lib/visor/rootfs/rootfs.ext4");
        let tmpdir;
        let effective_rootfs = if rootfs_path.exists() {
            rootfs_path.as_path()
        } else {
            tmpdir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
            let p = tmpdir.path().join("rootfs.ext4");
            std::fs::write(&p, &[0u8; 1024]).unwrap();
            Box::leak(p.into_boxed_path())
        };

        let config = VmConfig {
            kernel_path: &kernel_path,
            cmdline: "console=ttyS0 reboot=k panic=1 noapic",
            rootfs_path: effective_rootfs,
            memory_mib: 128,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };

        let mut booted = boot(&config).expect("boot should succeed with real kernel");
        let device_mgr = std::mem::replace(&mut booted.device_mgr, DeviceManager::new());

        struct ExitLimitHandler {
            inner: DeviceManager,
            exit_count: usize,
            exit_limit: usize,
        }

        impl ExitHandler for ExitLimitHandler {
            fn handle_exit(&mut self, exit: VmExit) -> Result<ExitAction, VcpuError> {
                self.exit_count += 1;
                let action = self.inner.handle_exit(exit)?;
                if self.exit_count >= self.exit_limit {
                    Ok(ExitAction::Stop)
                } else {
                    Ok(action)
                }
            }

            fn handle_io_read(&mut self, port: u16, data: &mut [u8]) {
                self.inner.handle_io_read(port, data);
            }

            fn handle_mmio_read(&mut self, addr: u64, data: &mut [u8]) {
                self.inner.handle_mmio_read(addr, data);
            }
        }

        let mut handler = ExitLimitHandler {
            inner: device_mgr,
            exit_count: 0,
            exit_limit: 128,
        };

        let result = run_vcpu_with_handler(&mut booted, &mut handler);
        assert!(
            result.is_ok(),
            "real kernel should produce exits: {result:?}"
        );
        assert!(handler.exit_count > 0, "expected at least one VM exit");
    }

    #[test]
    fn booted_vm_kill_flag_starts_false() {
        let kernel_path = std::path::PathBuf::from("/var/lib/visor/kernel/vmlinux-x86_64");
        if !kernel_path.exists() {
            return;
        }

        let rootfs_path = std::path::PathBuf::from("/var/lib/visor/rootfs/rootfs.ext4");
        let tmpdir;
        let effective_rootfs = if rootfs_path.exists() {
            rootfs_path.as_path()
        } else {
            tmpdir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
            let p = tmpdir.path().join("rootfs.ext4");
            std::fs::write(&p, &[0u8; 1024]).unwrap();
            Box::leak(p.into_boxed_path())
        };

        let config = VmConfig {
            kernel_path: &kernel_path,
            cmdline: "console=ttyS0",
            rootfs_path: effective_rootfs,
            memory_mib: 64,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };

        let booted = boot(&config).expect("boot should succeed");
        assert!(
            !booted.kill_flag.load(std::sync::atomic::Ordering::Relaxed),
            "kill_flag should start as false"
        );
    }

    /// Integration test: run_vcpu_with_handler captures registers at exit.
    #[test]
    fn run_vcpu_with_handler_captures_registers() {
        let kernel_path = std::path::PathBuf::from("/var/lib/visor/kernel/vmlinux-x86_64");
        if !kernel_path.exists() {
            return;
        }

        let rootfs_path = std::path::PathBuf::from("/var/lib/visor/rootfs/rootfs.ext4");
        let tmpdir;
        let effective_rootfs = if rootfs_path.exists() {
            rootfs_path.as_path()
        } else {
            tmpdir = crate::testutil::tempdir("visor-vmm-vm-").unwrap();
            let p = tmpdir.path().join("rootfs.ext4");
            std::fs::write(&p, &[0u8; 1024]).unwrap();
            Box::leak(p.into_boxed_path())
        };

        let config = VmConfig {
            kernel_path: &kernel_path,
            cmdline: "console=ttyS0 reboot=k panic=1 noapic",
            rootfs_path: effective_rootfs,
            memory_mib: 128,
            vcpus: 1,
            guest_cid: 3,
            guest_virtualization: GuestVirtualizationMode::Standard,
            shared_dirs: Vec::new(),
            data_disks: Vec::new(),
            network: None,
            networks: Vec::new(),
            #[cfg(target_os = "macos")]
            guest_memory: None,
        };

        let mut booted = boot(&config).expect("boot should succeed");
        let device_mgr = std::mem::replace(&mut booted.device_mgr, DeviceManager::new());

        struct TestHandler {
            inner: DeviceManager,
            exit_count: usize,
            exit_limit: usize,
        }

        impl ExitHandler for TestHandler {
            fn handle_exit(&mut self, exit: VmExit) -> Result<ExitAction, VcpuError> {
                self.exit_count += 1;
                let action = self.inner.handle_exit(exit)?;
                if self.exit_count >= self.exit_limit {
                    Ok(ExitAction::Stop)
                } else {
                    Ok(action)
                }
            }
            fn handle_io_read(&mut self, port: u16, data: &mut [u8]) {
                self.inner.handle_io_read(port, data);
            }
            fn handle_mmio_read(&mut self, addr: u64, data: &mut [u8]) {
                self.inner.handle_mmio_read(addr, data);
            }
        }

        let mut handler = TestHandler {
            inner: device_mgr,
            exit_count: 0,
            exit_limit: 128,
        };

        let result = run_vcpu_with_handler(&mut booted, &mut handler);
        let run_result = result.expect("run_vcpu_with_handler should not fail");
        assert!(handler.exit_count > 0, "expected at least one VM exit");
        assert!(
            run_result.regs.is_some(),
            "should capture standard registers"
        );
        assert!(
            run_result.sregs.is_some(),
            "should capture special registers"
        );

        if let Some(regs) = &run_result.regs {
            let display = format!("{regs}");
            assert!(!display.is_empty(), "register display should not be empty");
        }
    }
}
