//! Seccomp BPF syscall filtering for the visor daemon process.
//!
//! After initialization, the visor daemon installs a seccomp filter that restricts
//! the process to only the syscalls required for KVM VMM operation. This reduces
//! the kernel attack surface by denying access to ~300+ unnecessary syscalls.
//!
//! # Usage
//!
//! ```no_run
//! use visor_vmm::seccomp::SyscallFilter;
//!
//! // Build and apply the default VMM filter after initialization:
//! let filter = SyscallFilter::default_vmm_filter().expect("build filter");
//! filter.apply().expect("install seccomp filter");
//! ```
//!
//! # Design
//!
//! The filter uses an allowlist model: only explicitly permitted syscalls succeed.
//! Any syscall not in the allowlist returns `EPERM` to the caller. This is safer
//! than a denylist because new (unknown) syscalls are blocked by default.

use std::collections::BTreeMap;

use seccompiler::{BpfProgram, SeccompAction, SeccompFilter, SeccompRule, TargetArch};

/// Errors from seccomp filter construction or installation.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum SeccompError {
    /// Filter compilation to BPF bytecode failed.
    #[error("seccomp filter compilation failed: {0}")]
    Compile(String),

    /// Installing the filter via `prctl(PR_SET_SECCOMP)` failed.
    #[error("seccomp filter installation failed: {0}")]
    Install(String),
}

/// A seccomp BPF syscall filter for the visor VMM process.
///
/// Wraps a compiled allowlist of syscall numbers. Syscalls not in the list
/// are denied with `EPERM`. The filter is built once and can be applied to
/// the current thread (or all threads via `apply()`).
#[derive(Debug)]
#[non_exhaustive]
pub struct SyscallFilter {
    /// The compiled BPF program ready for installation.
    bpf: BpfProgram,
    /// The syscall numbers in the allowlist (for introspection/testing).
    allowed: Vec<i64>,
}

impl SyscallFilter {
    /// Creates a filter allowing only the specified syscalls.
    ///
    /// Duplicates in `syscalls` are automatically deduplicated. Any syscall
    /// not in the list will return `EPERM` when invoked after the filter is
    /// installed.
    ///
    /// # Errors
    ///
    /// Returns [`SeccompError::Compile`] if the filter cannot be built for
    /// the current architecture.
    pub fn new(syscalls: &[i64]) -> Result<Self, SeccompError> {
        let mut rule_map: BTreeMap<i64, Vec<SeccompRule>> = BTreeMap::new();
        for &nr in syscalls {
            rule_map.entry(nr).or_default();
        }

        let allowed: Vec<i64> = rule_map.keys().copied().collect();

        let arch: TargetArch = std::env::consts::ARCH
            .try_into()
            .map_err(|e: seccompiler::BackendError| SeccompError::Compile(e.to_string()))?;

        let filter = SeccompFilter::new(
            rule_map,
            SeccompAction::Errno(libc::EPERM as u32),
            SeccompAction::Allow,
            arch,
        )
        .map_err(|e| SeccompError::Compile(e.to_string()))?;

        let bpf: BpfProgram = filter
            .try_into()
            .map_err(|e: seccompiler::BackendError| SeccompError::Compile(e.to_string()))?;

        Ok(Self { bpf, allowed })
    }

    /// Creates the default filter for a visor VMM daemon process.
    ///
    /// The allowlist includes ~43 syscalls needed for:
    /// - KVM ioctls (VM creation, vCPU run, memory mapping)
    /// - Memory management (mmap, mprotect, munmap, brk, madvise, mremap)
    /// - I/O (read, write, close, openat, fcntl)
    /// - Async event loops (`epoll_create1`, `epoll_ctl`, `epoll_wait`, ppoll)
    /// - Networking (socket, bind, listen, accept4, send/recv variants)
    /// - Signals (sigaltstack, `rt_sigaction`, `rt_sigprocmask`)
    /// - Threading (clone3, futex, `set_robust_list`, rseq, `sched_yield`)
    /// - Misc (getrandom, `clock_gettime`, nanosleep, statx, newfstatat, prlimit64)
    ///
    /// # Errors
    ///
    /// Returns [`SeccompError::Compile`] if the filter cannot be compiled.
    pub fn default_vmm_filter() -> Result<Self, SeccompError> {
        Self::new(&DEFAULT_VMM_ALLOWLIST)
    }

    /// Compiles this filter to a BPF program.
    ///
    /// The returned `Vec` contains the raw BPF instructions. This is useful
    /// for inspecting the compiled output or serializing it for later use.
    ///
    /// # Errors
    ///
    /// This method currently cannot fail (the BPF is compiled at construction
    /// time), but returns `Result` for forward compatibility.
    pub fn compile(&self) -> Result<Vec<seccompiler::sock_filter>, SeccompError> {
        Ok(self.bpf.clone())
    }

    /// Returns the syscall numbers in this filter's allowlist.
    ///
    /// The returned slice is sorted and deduplicated.
    #[must_use]
    pub fn allowed_syscalls(&self) -> &[i64] {
        &self.allowed
    }

    /// Installs this seccomp filter on all threads in the current process.
    ///
    /// After this call, any syscall not in the allowlist will fail with
    /// `EPERM`. This operation is **irreversible** — once installed, the
    /// filter cannot be removed or relaxed.
    ///
    /// # Errors
    ///
    /// Returns [`SeccompError::Install`] if the `prctl(PR_SET_SECCOMP)` call fails.
    pub fn apply(&self) -> Result<(), SeccompError> {
        seccompiler::apply_filter_all_threads(&self.bpf)
            .map_err(|e| SeccompError::Install(e.to_string()))
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
#[path = "seccomp_test.rs"]
mod tests;
