use super::*;
use crate::sandbox::backend::SandboxBackend;

// ── LinuxSandbox construction ────────────────────────────────────────

#[test]
fn linux_sandbox_default_builds_successfully() {
    let result = LinuxSandbox::default_vmm_sandbox();
    assert!(
        result.is_ok(),
        "default VMM sandbox should compile: {:?}",
        result.err()
    );
}

#[test]
fn linux_sandbox_custom_syscalls_builds_successfully() {
    // A minimal allowlist — just exit and write.
    let result = LinuxSandbox::new(&[libc::SYS_exit, libc::SYS_write]);
    assert!(
        result.is_ok(),
        "custom sandbox should compile: {:?}",
        result.err()
    );
}

#[test]
fn linux_sandbox_empty_allowlist_builds_successfully() {
    // Even an empty allowlist should compile (deny everything).
    let result = LinuxSandbox::new(&[]);
    assert!(
        result.is_ok(),
        "empty allowlist sandbox should compile: {:?}",
        result.err()
    );
}

#[test]
fn linux_sandbox_deduplicates_syscalls() {
    let sandbox = LinuxSandbox::new(&[
        libc::SYS_read,
        libc::SYS_read,
        libc::SYS_write,
        libc::SYS_write,
        libc::SYS_write,
    ])
    .unwrap();
    assert_eq!(
        sandbox.allowed_syscalls().len(),
        2,
        "duplicates should be removed"
    );
}

#[test]
fn linux_sandbox_allowed_syscalls_are_sorted() {
    let sandbox = LinuxSandbox::new(&[libc::SYS_write, libc::SYS_read, libc::SYS_close]).unwrap();
    let allowed = sandbox.allowed_syscalls();
    let mut sorted = allowed.to_vec();
    sorted.sort();
    assert_eq!(allowed, &sorted, "allowed syscalls should be sorted");
}

// ── SandboxBackend trait ─────────────────────────────────────────────

#[test]
fn linux_sandbox_implements_sandbox_backend() {
    // Verify LinuxSandbox can be used as SandboxBackend.
    fn assert_backend<T: SandboxBackend>(_t: &T) {}
    let sandbox = LinuxSandbox::default_vmm_sandbox().unwrap();
    assert_backend(&sandbox);
}

// NOTE: We cannot test apply() in unit tests because it is irreversible
// (seccomp filters persist for the lifetime of the process). Testing apply()
// would restrict the test runner itself. This is tested in integration tests
// with a forked process.
