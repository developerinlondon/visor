use super::*;

#[test]
fn default_filter_builds_without_error() {
    let filter = SyscallFilter::default_vmm_filter();
    assert!(filter.is_ok());
}

#[test]
fn default_filter_compiles_to_bpf() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let bpf = filter.compile();
    assert!(bpf.is_ok());
}

#[test]
fn compiled_bpf_is_non_empty() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let bpf = filter.compile().unwrap();
    assert!(!bpf.is_empty(), "BPF program should have instructions");
}

#[test]
fn allowlist_contains_read_write_close() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let syscalls = filter.allowed_syscalls();
    assert!(
        syscalls.contains(&libc::SYS_read),
        "allowlist must include read"
    );
    assert!(
        syscalls.contains(&libc::SYS_write),
        "allowlist must include write"
    );
    assert!(
        syscalls.contains(&libc::SYS_close),
        "allowlist must include close"
    );
}

#[test]
fn allowlist_contains_kvm_ioctl() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let syscalls = filter.allowed_syscalls();
    assert!(
        syscalls.contains(&libc::SYS_ioctl),
        "allowlist must include ioctl for KVM"
    );
}

#[test]
fn allowlist_contains_memory_syscalls() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let syscalls = filter.allowed_syscalls();
    for &nr in &[
        libc::SYS_mmap,
        libc::SYS_mprotect,
        libc::SYS_munmap,
        libc::SYS_brk,
        libc::SYS_madvise,
        libc::SYS_mremap,
    ] {
        assert!(
            syscalls.contains(&nr),
            "allowlist must include syscall {nr}"
        );
    }
}

#[test]
fn allowlist_contains_epoll_syscalls() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let syscalls = filter.allowed_syscalls();
    for &nr in &[
        libc::SYS_epoll_create1,
        libc::SYS_epoll_ctl,
        libc::SYS_epoll_wait,
    ] {
        assert!(
            syscalls.contains(&nr),
            "allowlist must include epoll syscall {nr}"
        );
    }
}

#[test]
fn allowlist_contains_networking_syscalls() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let syscalls = filter.allowed_syscalls();
    for &nr in &[
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
    ] {
        assert!(
            syscalls.contains(&nr),
            "allowlist must include networking syscall {nr}"
        );
    }
}

#[test]
fn allowlist_contains_signal_syscalls() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let syscalls = filter.allowed_syscalls();
    for &nr in &[
        libc::SYS_sigaltstack,
        libc::SYS_rt_sigaction,
        libc::SYS_rt_sigprocmask,
    ] {
        assert!(
            syscalls.contains(&nr),
            "allowlist must include signal syscall {nr}"
        );
    }
}

#[test]
fn allowlist_contains_exit_syscalls() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let syscalls = filter.allowed_syscalls();
    assert!(
        syscalls.contains(&libc::SYS_exit),
        "allowlist must include exit"
    );
    assert!(
        syscalls.contains(&libc::SYS_exit_group),
        "allowlist must include exit_group"
    );
}

#[test]
fn allowlist_contains_misc_required_syscalls() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let syscalls = filter.allowed_syscalls();
    for &nr in &[
        libc::SYS_openat,
        libc::SYS_fcntl,
        libc::SYS_getrandom,
        libc::SYS_clock_gettime,
        libc::SYS_nanosleep,
        libc::SYS_newfstatat,
        libc::SYS_sched_yield,
        libc::SYS_futex,
        libc::SYS_clone3,
        libc::SYS_ppoll,
        libc::SYS_statx,
        libc::SYS_rseq,
        libc::SYS_set_robust_list,
        libc::SYS_prlimit64,
    ] {
        assert!(
            syscalls.contains(&nr),
            "allowlist must include syscall {nr}"
        );
    }
}

#[test]
fn allowlist_has_at_least_30_syscalls() {
    let filter = SyscallFilter::default_vmm_filter().unwrap();
    let count = filter.allowed_syscalls().len();
    assert!(
        count >= 30,
        "expected at least 30 allowed syscalls, got {count}"
    );
}

#[test]
fn custom_filter_with_subset_builds() {
    let filter = SyscallFilter::new(&[libc::SYS_read, libc::SYS_write, libc::SYS_exit_group]);
    assert!(filter.is_ok());
}

#[test]
fn custom_filter_compiles_to_bpf() {
    let filter =
        SyscallFilter::new(&[libc::SYS_read, libc::SYS_write, libc::SYS_exit_group]).unwrap();
    let bpf = filter.compile();
    assert!(bpf.is_ok());
}

#[test]
fn custom_filter_allowlist_matches_input() {
    let input = [libc::SYS_read, libc::SYS_write, libc::SYS_exit_group];
    let filter = SyscallFilter::new(&input).unwrap();
    let syscalls = filter.allowed_syscalls();
    assert_eq!(syscalls.len(), input.len());
    for &nr in &input {
        assert!(syscalls.contains(&nr));
    }
}

#[test]
fn empty_allowlist_builds() {
    let filter = SyscallFilter::new(&[]);
    assert!(filter.is_ok());
}

#[test]
fn empty_allowlist_compiles_to_bpf() {
    let filter = SyscallFilter::new(&[]).unwrap();
    let bpf = filter.compile();
    assert!(bpf.is_ok());
}

#[test]
fn duplicate_syscalls_are_deduplicated() {
    let filter = SyscallFilter::new(&[libc::SYS_read, libc::SYS_read, libc::SYS_read]).unwrap();
    let syscalls = filter.allowed_syscalls();
    assert_eq!(syscalls.len(), 1, "duplicates should be deduplicated");
}

#[test]
fn error_display_is_human_readable() {
    let err = SeccompError::Compile("test error".to_string());
    let msg = err.to_string();
    assert!(
        msg.contains("test error"),
        "error message should contain the source: {msg}"
    );
}
