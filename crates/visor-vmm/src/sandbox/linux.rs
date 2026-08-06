//! Linux sandbox backend using seccomp BPF syscall filtering.
//!
//! Implements [`SandboxBackend`] for Linux by building and installing a
//! seccomp BPF allowlist filter. Syscalls not in the allowlist are denied
//! with `EPERM`.
//!
//! This wraps the lower-level [`seccompiler`] crate to provide a
//! trait-based interface compatible with visor-vmm's platform abstraction.

use std::collections::BTreeMap;

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};

use super::backend::{SandboxBackend, SandboxError};

// ── Linux sandbox ────────────────────────────────────────────────────

/// Linux sandbox using seccomp BPF syscall filtering.
///
/// Wraps a compiled allowlist of syscall numbers. Syscalls not in the
/// allowlist are denied with `EPERM`. The filter is compiled at construction
/// time and applied via [`SandboxBackend::apply`].
#[derive(Debug)]
#[non_exhaustive]
pub struct LinuxSandbox {
    /// The compiled BPF program ready for installation.
    bpf: BpfProgram,
    /// The syscall numbers in the allowlist (for introspection/testing).
    allowed: Vec<i64>,
}

impl LinuxSandbox {
    /// Creates a sandbox allowing only the specified syscalls.
    ///
    /// Duplicates in `syscalls` are automatically deduplicated. Any syscall
    /// not in the list will return `EPERM` when invoked after the filter is
    /// installed.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Compile`] if the filter cannot be built for
    /// the current architecture.
    pub fn new(syscalls: &[i64]) -> Result<Self, SandboxError> {
        let mut rule_map: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for &nr in syscalls {
            rule_map.entry(nr).or_default();
        }

        let allowed: Vec<i64> = rule_map.keys().copied().collect();

        let arch: TargetArch = std::env::consts::ARCH
            .try_into()
            .map_err(|e: seccompiler::BackendError| SandboxError::Compile(e.to_string()))?;

        let filter = SeccompFilter::new(
            rule_map,
            SeccompAction::Errno(libc::EPERM as u32),
            SeccompAction::Allow,
            arch,
        )
        .map_err(|e| SandboxError::Compile(e.to_string()))?;

        let bpf: BpfProgram = filter
            .try_into()
            .map_err(|e: seccompiler::BackendError| SandboxError::Compile(e.to_string()))?;

        Ok(Self { bpf, allowed })
    }

    /// Creates the default sandbox for a visor VMM daemon process.
    ///
    /// The allowlist includes the ~43 syscalls needed for KVM VMM operation,
    /// async I/O, networking, signal handling, and essential memory/process
    /// management. See [`crate::seccomp::SyscallFilter::default_vmm_filter`]
    /// for the canonical syscall list.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::Compile`] if the filter cannot be compiled.
    pub fn default_vmm_sandbox() -> Result<Self, SandboxError> {
        Self::new(&DEFAULT_VMM_ALLOWLIST)
    }

    /// Returns the syscall numbers in this sandbox's allowlist.
    ///
    /// The returned slice is sorted and deduplicated.
    #[must_use]
    pub fn allowed_syscalls(&self) -> &[i64] {
        &self.allowed
    }
}

impl SandboxBackend for LinuxSandbox {
    fn apply(&self) -> Result<(), SandboxError> {
        seccompiler::apply_filter_all_threads(&self.bpf)
            .map_err(|e| SandboxError::Install(e.to_string()))
    }
}

/// Default allowlist of syscalls for a visor VMM daemon after initialization.
///
/// This covers KVM operation, async I/O, networking, signal handling, and
/// essential memory/process management. Anything not listed is denied.
const DEFAULT_VMM_ALLOWLIST: [i64; 43] = [
    // File I/O
    libc::SYS_read,
    libc::SYS_write,
    libc::SYS_close,
    libc::SYS_openat,
    libc::SYS_fcntl,
    libc::SYS_newfstatat,
    libc::SYS_statx,
    // Memory management
    libc::SYS_mmap,
    libc::SYS_mprotect,
    libc::SYS_munmap,
    libc::SYS_brk,
    libc::SYS_madvise,
    libc::SYS_mremap,
    // KVM / device ioctls
    libc::SYS_ioctl,
    // Async event loop
    libc::SYS_epoll_create1,
    libc::SYS_epoll_ctl,
    libc::SYS_epoll_wait,
    libc::SYS_ppoll,
    // Networking
    libc::SYS_socket,
    libc::SYS_bind,
    libc::SYS_listen,
    libc::SYS_accept4,
    libc::SYS_sendto,
    libc::SYS_recvfrom,
    libc::SYS_recvmsg,
    libc::SYS_sendmsg,
    libc::SYS_setsockopt,
    libc::SYS_getsockopt,
    // Signals
    libc::SYS_sigaltstack,
    libc::SYS_rt_sigaction,
    libc::SYS_rt_sigprocmask,
    // Threading / synchronization
    libc::SYS_clone3,
    libc::SYS_futex,
    libc::SYS_set_robust_list,
    libc::SYS_rseq,
    libc::SYS_sched_yield,
    // Process lifecycle
    libc::SYS_exit,
    libc::SYS_exit_group,
    libc::SYS_prlimit64,
    // Entropy
    libc::SYS_getrandom,
    // Time
    libc::SYS_clock_gettime,
    libc::SYS_nanosleep,
    // Filesystem metadata (used by tokio/mio runtime)
    libc::SYS_fstat,
];

#[cfg(test)]
#[path = "linux_test.rs"]
mod tests;
