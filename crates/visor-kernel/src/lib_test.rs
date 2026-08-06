use super::*;

fn kernel_config_file(path: &str) -> String {
    let config_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    std::fs::read_to_string(&config_path)
        .unwrap_or_else(|error| panic!("failed to read {}: {error}", config_path.display()))
}

#[test]
fn kernel_path_returns_existing_file() {
    let path = kernel_path();
    assert!(
        path.exists(),
        "kernel binary not found at {path:?} — build.rs should have resolved it"
    );
}

#[test]
fn kernel_file_has_correct_magic_bytes() {
    let path = kernel_path();
    let bytes = std::fs::read(&path).expect("failed to read kernel binary");

    #[cfg(target_arch = "x86_64")]
    {
        assert!(
            bytes.len() > 4,
            "kernel file too small: {} bytes",
            bytes.len()
        );
        // ELF magic: 0x7f 'E' 'L' 'F'
        assert_eq!(
            &bytes[..4],
            b"\x7fELF",
            "kernel file does not have ELF magic header"
        );
    }

    #[cfg(target_arch = "aarch64")]
    {
        // ARM64 Image header has magic number 0x644d5241 ("ARM\x64")
        // at offset 0x38 (56 bytes into the header).
        assert!(
            bytes.len() > 0x3C,
            "kernel file too small for ARM64 Image header: {} bytes",
            bytes.len()
        );
        assert_eq!(
            &bytes[0x38..0x3C],
            b"ARM\x64",
            "kernel file does not have ARM64 Image magic at offset 0x38"
        );
    }
}

#[test]
fn kernel_file_is_at_least_1mb() {
    let path = kernel_path();
    let metadata = std::fs::metadata(&path).expect("failed to stat kernel binary");
    assert!(
        metadata.len() >= 1_000_000,
        "kernel file suspiciously small: {} bytes (minimum 1MB)",
        metadata.len()
    );
}

#[test]
fn kernel_version_starts_with_linux_version() {
    let version = kernel_version();
    assert!(
        version.starts_with("Linux version "),
        "expected kernel version to start with 'Linux version ', got: {version}"
    );
}

#[test]
fn kernel_size_matches_file() {
    let path = kernel_path();
    let actual = std::fs::metadata(&path)
        .expect("failed to stat kernel")
        .len();
    assert_eq!(
        kernel_size(),
        actual,
        "kernel_size() doesn't match file metadata"
    );
}

#[test]
fn kernel_sha256_is_valid_hex() {
    let hash = kernel_sha256();
    assert_eq!(
        hash.len(),
        64,
        "SHA-256 hash should be 64 hex chars, got: {hash}"
    );
    assert!(
        hash.chars().all(|c| c.is_ascii_hexdigit()),
        "SHA-256 hash should be hex, got: {hash}"
    );
}

#[test]
fn x86_64_kernel_fragment_enables_posix_mqueue() {
    let config = kernel_config_file("config/fragments/x86_64/devices.config");
    assert!(
        config.contains("CONFIG_POSIX_MQUEUE=y"),
        "x86_64 devices fragment should enable POSIX mqueue support"
    );
}

#[test]
fn resolved_kernel_config_enables_posix_mqueue() {
    let config = kernel_config_file("config/visor-kernel.config");
    assert!(
        config.contains("CONFIG_POSIX_MQUEUE=y"),
        "resolved kernel config should enable POSIX mqueue support"
    );
}

#[test]
fn resolved_kernel_config_enables_bpf_syscall() {
    let config = kernel_config_file("config/visor-kernel.config");
    assert!(
        config.contains("CONFIG_BPF_SYSCALL=y"),
        "resolved kernel config should enable BPF syscalls for nested runc workloads"
    );
}

#[test]
fn resolved_kernel_config_enables_cgroup_device() {
    let config = kernel_config_file("config/visor-kernel.config");
    assert!(
        config.contains("CONFIG_CGROUP_DEVICE=y"),
        "resolved kernel config should enable cgroup device controls for nested runc workloads"
    );
}

#[test]
fn resolved_kernel_config_enables_cgroup_bpf() {
    let config = kernel_config_file("config/visor-kernel.config");
    assert!(
        config.contains("CONFIG_CGROUP_BPF=y"),
        "resolved kernel config should enable cgroup BPF hooks for nested runc workloads"
    );
}

#[test]
fn resolved_kernel_config_enables_kvm_for_nested_builder_guests() {
    let config = kernel_config_file("config/visor-kernel.config");
    assert!(
        config.contains("CONFIG_VIRTUALIZATION=y"),
        "resolved kernel config should enable generic virtualization support"
    );
    assert!(
        config.contains("CONFIG_KVM=y"),
        "resolved kernel config should enable KVM host support for nested builder guests"
    );
    assert!(
        config.contains("CONFIG_KVM_INTEL=y"),
        "resolved kernel config should enable Intel nested-KVM support"
    );
    assert!(
        config.contains("CONFIG_KVM_AMD=y"),
        "resolved kernel config should enable AMD nested-KVM support"
    );
}
