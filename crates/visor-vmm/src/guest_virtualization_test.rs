use super::*;

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use kvm_bindings::kvm_cpuid_entry2;

#[test]
fn guest_virtualization_mode_display_is_stable() {
    assert_eq!(GuestVirtualizationMode::Standard.to_string(), "standard");
    assert_eq!(GuestVirtualizationMode::Nested.to_string(), "nested");
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn cpuid_entry(function: u32, index: u32, ecx: u32) -> kvm_cpuid_entry2 {
    kvm_cpuid_entry2 {
        function,
        index,
        flags: 0,
        eax: 0,
        ebx: 0,
        ecx,
        edx: 0,
        padding: [0; 3],
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn standard_mode_masks_nested_virtualization_cpuid_bits() {
    let mut entries = vec![
        cpuid_entry(0x0000_0001, 0, 1 << 5),
        cpuid_entry(0x8000_0001, 0, 1 << 2),
    ];

    apply_supported_cpuid(GuestVirtualizationMode::Standard, &mut entries);

    assert_eq!(entries[0].ecx & (1 << 5), 0);
    assert_eq!(entries[1].ecx & (1 << 2), 0);
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[test]
fn nested_mode_preserves_nested_virtualization_cpuid_bits() {
    let mut entries = vec![
        cpuid_entry(0x0000_0001, 0, 1 << 5),
        cpuid_entry(0x8000_0001, 0, 1 << 2),
    ];

    apply_supported_cpuid(GuestVirtualizationMode::Nested, &mut entries);

    assert_ne!(entries[0].ecx & (1 << 5), 0);
    assert_ne!(entries[1].ecx & (1 << 2), 0);
}
