//! KVM platform implementation for Linux.
//!
//! Contains the [`KvmPlatform`], [`KvmVm`], and [`KvmVcpu`] types that
//! implement the platform traits, plus `From` conversions between
//! portable register types and KVM-specific structs.

use kvm_bindings::{
    kvm_dtable, kvm_pit_config, kvm_regs, kvm_segment, kvm_sregs, kvm_userspace_memory_region,
};
use kvm_ioctls::{Kvm, VcpuFd, VmFd};

use super::regs::{DescriptorTable, SegmentReg, SpecialRegs, StandardRegs};
use super::{Platform, PlatformError, VcpuOps, VmExit, VmOps};
use std::fs::OpenOptions;
use std::os::fd::FromRawFd;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::fs::OpenOptionsExt;

/// Expected KVM API version (stable since Linux 2.6.36).
const KVM_API_VERSION: i32 = 12;

// ── KVM Platform ────────────────────────────────────────────────────

/// KVM-based hypervisor platform.
///
/// Wraps the `/dev/kvm` device fd and provides VM creation.
pub struct KvmPlatform {
    kvm: Kvm,
}

impl Platform for KvmPlatform {
    type Vm = KvmVm;

    fn new() -> Result<Self, PlatformError> {
        let kvm = Kvm::new().map_err(kvm_err)?;
        let version = kvm.get_api_version();
        if version != KVM_API_VERSION {
            return Err(PlatformError::ApiVersionMismatch {
                expected: KVM_API_VERSION,
                actual: version,
            });
        }
        Ok(Self { kvm })
    }

    fn create_vm(&self) -> Result<Self::Vm, PlatformError> {
        let fd = self.kvm.create_vm().map_err(kvm_err)?;
        Ok(KvmVm { fd })
    }
}

impl KvmPlatform {
    /// Returns a reference to the underlying KVM handle.
    /// Bridge method for backward compatibility during migration.
    #[must_use]
    pub fn kvm(&self) -> &Kvm {
        &self.kvm
    }

    /// Returns the hypervisor API version.
    /// Bridge method for backward compatibility during migration.
    #[must_use]
    pub fn api_version(&self) -> i32 {
        self.kvm.get_api_version()
    }
}

// ── KVM VM ──────────────────────────────────────────────────────────

/// KVM virtual machine handle.
///
/// Wraps a [`VmFd`] and provides methods for IRQ chip, PIT, memory,
/// and vCPU creation.
pub struct KvmVm {
    pub(crate) fd: VmFd,
}

impl VmOps for KvmVm {
    type Vcpu = KvmVcpu;

    fn create_irq_chip(&self) -> Result<(), PlatformError> {
        self.fd.create_irq_chip().map_err(kvm_err)
    }

    fn create_pit(&self) -> Result<(), PlatformError> {
        let pit_config = kvm_pit_config::default();
        self.fd.create_pit2(pit_config).map_err(kvm_err)
    }

    fn register_memory(
        &self,
        slot: u32,
        guest_addr: u64,
        size: u64,
        host_addr: *mut u8,
    ) -> Result<(), PlatformError> {
        let region = kvm_userspace_memory_region {
            slot,
            guest_phys_addr: guest_addr,
            memory_size: size,
            userspace_addr: host_addr as u64,
            flags: 0,
        };
        // SAFETY: The caller guarantees that host_addr points to a valid
        // memory region of at least `size` bytes that remains valid for
        // the lifetime of the VM.
        #[allow(unsafe_code)]
        unsafe {
            self.fd.set_user_memory_region(region).map_err(kvm_err)
        }
    }

    fn register_irqfd(&self, event: &dyn InterruptEvent, gsi: u32) -> Result<(), PlatformError> {
        let fd = event.as_raw();
        // SAFETY: The caller guarantees that the InterruptEvent is a valid eventfd.
        #[allow(unsafe_code)]
        let eventfd = unsafe { vmm_sys_util::eventfd::EventFd::from_raw_fd(fd) };
        self.fd.register_irqfd(&eventfd, gsi).map_err(kvm_err)?;
        // Prevent EventFd from closing the fd — the caller owns it.
        std::mem::forget(eventfd);
        Ok(())
    }

    fn create_vcpu(&self, index: u64) -> Result<Self::Vcpu, PlatformError> {
        let fd = self.fd.create_vcpu(index).map_err(kvm_err)?;
        Ok(KvmVcpu { fd })
    }
}

// ── KVM vCPU ────────────────────────────────────────────────────────

/// KVM virtual CPU handle.
///
/// Wraps a [`VcpuFd`] and implements register access and execution.
pub struct KvmVcpu {
    fd: VcpuFd,
}

impl VcpuOps for KvmVcpu {
    fn set_regs(&self, regs: &StandardRegs) -> Result<(), PlatformError> {
        let kvm_regs: kvm_regs = regs.clone().into();
        self.fd.set_regs(&kvm_regs).map_err(kvm_err)
    }

    fn get_regs(&self) -> Result<StandardRegs, PlatformError> {
        let kvm_regs = self.fd.get_regs().map_err(kvm_err)?;
        Ok(kvm_regs.into())
    }

    fn set_sregs(&self, sregs: &SpecialRegs) -> Result<(), PlatformError> {
        let kvm_sregs: kvm_sregs = sregs.clone().into();
        self.fd.set_sregs(&kvm_sregs).map_err(kvm_err)
    }

    fn get_sregs(&self) -> Result<SpecialRegs, PlatformError> {
        let kvm_sregs = self.fd.get_sregs().map_err(kvm_err)?;
        Ok(kvm_sregs.into())
    }

    fn run(&mut self) -> Result<VmExit, PlatformError> {
        use super::ExitData;
        use kvm_ioctls::VcpuExit;

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
                if event_type == 2 {
                    Ok(VmExit::Reboot)
                } else {
                    Ok(VmExit::Shutdown)
                }
            }
            Ok(VcpuExit::Hlt | _) => Ok(VmExit::Halt),
            Err(e) => {
                let errno = e.errno();
                if errno == libc::EAGAIN || errno == libc::EINTR {
                    Ok(VmExit::Halt)
                } else {
                    Err(PlatformError::System(std::io::Error::from_raw_os_error(
                        errno,
                    )))
                }
            }
        }
    }
}

/// Converts a KVM ioctl error to a [`PlatformError`].
fn kvm_err(e: kvm_ioctls::Error) -> PlatformError {
    PlatformError::System(std::io::Error::from_raw_os_error(e.errno()))
}

pub(crate) fn open_tap_interface(name: &str) -> Result<OwnedFd, std::io::Error> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .custom_flags(libc::O_NONBLOCK)
        .open("/dev/net/tun")?;
    let fd: OwnedFd = file.into();
    let mut ifr = tap_ifreq(name)?;
    // SAFETY: `fd` is a valid `/dev/net/tun` descriptor and `ifr` is an
    // initialized `ifreq` naming the TAP interface plus `IFF_TAP|IFF_NO_PI`.
    #[allow(unsafe_code)]
    let ret = unsafe { libc::ioctl(fd.as_raw_fd(), libc::TUNSETIFF, &mut ifr) };
    if ret >= 0 {
        Ok(fd)
    } else {
        Err(std::io::Error::last_os_error())
    }
}

fn tap_ifreq(name: &str) -> Result<libc::ifreq, std::io::Error> {
    if !name.is_ascii() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("tap interface name must not contain non-ASCII characters: {name}"),
        ));
    }
    if name.len() >= libc::IFNAMSIZ {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("tap interface name too long: {name}"),
        ));
    }

    // SAFETY: `ifreq` is a plain C struct; zero-initialization is a valid
    // starting state before filling the active union fields below.
    #[allow(unsafe_code)]
    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    for (index, byte) in name.bytes().enumerate() {
        ifr.ifr_name[index] = libc::c_char::try_from(byte).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("tap interface name must not contain non-ASCII characters: {name}"),
            )
        })?;
    }
    let flags = libc::IFF_TAP | libc::IFF_NO_PI;
    ifr.ifr_ifru.ifru_flags = libc::c_short::try_from(flags).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("tap interface flags out of range: {flags}"),
        )
    })?;
    Ok(ifr)
}

// ── LinuxEventFd ─────────────────────────────────────────────────────

use super::event::{InterruptEvent, RawEventHandle};

/// Linux interrupt event backed by `eventfd2`.
///
/// Wraps a [`vmm_sys_util::eventfd::EventFd`] and implements
/// [`InterruptEvent`] for cross-platform interrupt signaling.
pub struct LinuxEventFd {
    inner: vmm_sys_util::eventfd::EventFd,
}

impl LinuxEventFd {
    /// Creates a new non-blocking event fd.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the `eventfd2` syscall fails.
    pub fn new() -> Result<Self, std::io::Error> {
        let inner = vmm_sys_util::eventfd::EventFd::new(libc::EFD_NONBLOCK)?;
        Ok(Self { inner })
    }
}

impl InterruptEvent for LinuxEventFd {
    fn trigger(&self) -> Result<(), std::io::Error> {
        self.inner.write(1)?;
        Ok(())
    }

    fn as_raw(&self) -> RawEventHandle {
        use std::os::fd::AsRawFd;
        self.inner.as_raw_fd()
    }
}

// ── StandardRegs ↔ kvm_regs ─────────────────────────────────────────

impl From<kvm_regs> for StandardRegs {
    fn from(k: kvm_regs) -> Self {
        Self {
            rax: k.rax,
            rbx: k.rbx,
            rcx: k.rcx,
            rdx: k.rdx,
            rsi: k.rsi,
            rdi: k.rdi,
            rsp: k.rsp,
            rbp: k.rbp,
            r8: k.r8,
            r9: k.r9,
            r10: k.r10,
            r11: k.r11,
            r12: k.r12,
            r13: k.r13,
            r14: k.r14,
            r15: k.r15,
            rip: k.rip,
            rflags: k.rflags,
        }
    }
}

impl From<StandardRegs> for kvm_regs {
    fn from(s: StandardRegs) -> Self {
        Self {
            rax: s.rax,
            rbx: s.rbx,
            rcx: s.rcx,
            rdx: s.rdx,
            rsi: s.rsi,
            rdi: s.rdi,
            rsp: s.rsp,
            rbp: s.rbp,
            r8: s.r8,
            r9: s.r9,
            r10: s.r10,
            r11: s.r11,
            r12: s.r12,
            r13: s.r13,
            r14: s.r14,
            r15: s.r15,
            rip: s.rip,
            rflags: s.rflags,
        }
    }
}

// ── SegmentReg ↔ kvm_segment ────────────────────────────────────────

impl From<kvm_segment> for SegmentReg {
    fn from(k: kvm_segment) -> Self {
        Self {
            base: k.base,
            limit: k.limit,
            selector: k.selector,
            type_: k.type_,
            present: k.present,
            dpl: k.dpl,
            db: k.db,
            s: k.s,
            l: k.l,
            g: k.g,
            avl: k.avl,
        }
    }
}

impl From<SegmentReg> for kvm_segment {
    fn from(s: SegmentReg) -> Self {
        Self {
            base: s.base,
            limit: s.limit,
            selector: s.selector,
            type_: s.type_,
            present: s.present,
            dpl: s.dpl,
            db: s.db,
            s: s.s,
            l: s.l,
            g: s.g,
            avl: s.avl,
            unusable: 0,
            padding: 0,
        }
    }
}

// ── DescriptorTable ↔ kvm_dtable ────────────────────────────────────

impl From<kvm_dtable> for DescriptorTable {
    fn from(k: kvm_dtable) -> Self {
        Self {
            base: k.base,
            limit: k.limit,
        }
    }
}

impl From<DescriptorTable> for kvm_dtable {
    fn from(d: DescriptorTable) -> Self {
        Self {
            base: d.base,
            limit: d.limit,
            padding: [0; 3],
        }
    }
}

// ── SpecialRegs ↔ kvm_sregs ────────────────────────────────────────

impl From<kvm_sregs> for SpecialRegs {
    fn from(k: kvm_sregs) -> Self {
        Self {
            cs: k.cs.into(),
            ds: k.ds.into(),
            es: k.es.into(),
            fs: k.fs.into(),
            gs: k.gs.into(),
            ss: k.ss.into(),
            tr: k.tr.into(),
            ldt: k.ldt.into(),
            gdt: k.gdt.into(),
            idt: k.idt.into(),
            cr0: k.cr0,
            cr2: k.cr2,
            cr3: k.cr3,
            cr4: k.cr4,
            cr8: k.cr8,
            efer: k.efer,
            apic_base: k.apic_base,
            interrupt_bitmap: k.interrupt_bitmap,
        }
    }
}

impl From<SpecialRegs> for kvm_sregs {
    fn from(s: SpecialRegs) -> Self {
        Self {
            cs: s.cs.into(),
            ds: s.ds.into(),
            es: s.es.into(),
            fs: s.fs.into(),
            gs: s.gs.into(),
            ss: s.ss.into(),
            tr: s.tr.into(),
            ldt: s.ldt.into(),
            gdt: s.gdt.into(),
            idt: s.idt.into(),
            cr0: s.cr0,
            cr2: s.cr2,
            cr3: s.cr3,
            cr4: s.cr4,
            cr8: s.cr8,
            efer: s.efer,
            apic_base: s.apic_base,
            interrupt_bitmap: s.interrupt_bitmap,
        }
    }
}

#[cfg(test)]
#[path = "linux_test.rs"]
mod tests;
