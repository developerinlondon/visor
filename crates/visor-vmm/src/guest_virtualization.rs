//! Guest virtualization policy for portable builder-guest support.
//!
//! This keeps the control surface platform-agnostic while allowing each
//! hypervisor backend to decide what "nested virtualization" means. Today the
//! Linux `x86_64` KVM backend can expose nested CPU features to selected
//! guests. macOS keeps the same API shape but reports nested mode as
//! unsupported until an HVF-backed implementation exists.

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
use kvm_bindings::kvm_cpuid_entry2;

/// How much hardware virtualization support the guest should see.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum GuestVirtualizationMode {
    /// Default guest profile with nested virtualization disabled.
    #[default]
    Standard,
    /// Expose nested virtualization support to the guest where available.
    Nested,
}

impl std::fmt::Display for GuestVirtualizationMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Standard => write!(f, "standard"),
            Self::Nested => write!(f, "nested"),
        }
    }
}

/// Errors from platform-specific guest virtualization handling.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GuestVirtualizationError {
    /// The requested guest virtualization mode is not available on this build.
    #[error("guest virtualization mode '{mode}' is not supported on {platform}")]
    Unsupported {
        /// Requested mode.
        mode: GuestVirtualizationMode,
        /// Human-readable platform name.
        platform: &'static str,
    },
}

/// Platform hook for guest virtualization exposure.
pub trait GuestVirtualizationBackend: Send + Sync {
    /// Validates whether this platform/backend supports the requested mode.
    fn validate(&self, mode: GuestVirtualizationMode) -> Result<(), GuestVirtualizationError>;

    /// Applies the platform's CPU-exposure policy to the supported CPUID set.
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn apply_supported_cpuid(
        &self,
        mode: GuestVirtualizationMode,
        entries: &mut [kvm_cpuid_entry2],
    );
}

#[cfg(target_os = "linux")]
struct LinuxGuestVirtualizationBackend;
#[cfg(target_os = "macos")]
struct MacosGuestVirtualizationBackend;
#[cfg(target_os = "windows")]
struct WindowsGuestVirtualizationBackend;

#[cfg(target_os = "linux")]
static LINUX_BACKEND: LinuxGuestVirtualizationBackend = LinuxGuestVirtualizationBackend;
#[cfg(target_os = "macos")]
static MACOS_BACKEND: MacosGuestVirtualizationBackend = MacosGuestVirtualizationBackend;
#[cfg(target_os = "windows")]
static WINDOWS_BACKEND: WindowsGuestVirtualizationBackend = WindowsGuestVirtualizationBackend;

#[cfg(target_os = "linux")]
fn platform_backend() -> &'static dyn GuestVirtualizationBackend {
    &LINUX_BACKEND
}

#[cfg(target_os = "macos")]
fn platform_backend() -> &'static dyn GuestVirtualizationBackend {
    &MACOS_BACKEND
}

#[cfg(target_os = "windows")]
fn platform_backend() -> &'static dyn GuestVirtualizationBackend {
    &WINDOWS_BACKEND
}

/// Validates the requested guest virtualization mode for the current platform.
pub fn validate_guest_virtualization(
    mode: GuestVirtualizationMode,
) -> Result<(), GuestVirtualizationError> {
    platform_backend().validate(mode)
}

/// Applies platform-specific nested-virtualization policy to supported CPUID.
#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub fn apply_supported_cpuid(mode: GuestVirtualizationMode, entries: &mut [kvm_cpuid_entry2]) {
    platform_backend().apply_supported_cpuid(mode, entries);
}

#[cfg(target_os = "linux")]
impl GuestVirtualizationBackend for LinuxGuestVirtualizationBackend {
    fn validate(&self, mode: GuestVirtualizationMode) -> Result<(), GuestVirtualizationError> {
        #[cfg(target_arch = "x86_64")]
        {
            let _ = mode;
            Ok(())
        }

        #[cfg(not(target_arch = "x86_64"))]
        {
            match mode {
                GuestVirtualizationMode::Standard => Ok(()),
                GuestVirtualizationMode::Nested => Err(GuestVirtualizationError::Unsupported {
                    mode,
                    platform: "linux non-x86_64",
                }),
            }
        }
    }

    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    fn apply_supported_cpuid(
        &self,
        mode: GuestVirtualizationMode,
        entries: &mut [kvm_cpuid_entry2],
    ) {
        if mode == GuestVirtualizationMode::Nested {
            return;
        }

        clear_cpuid_feature(entries, 0x0000_0001, 0, Register::Ecx, 1 << 5);
        clear_cpuid_feature(entries, 0x8000_0001, 0, Register::Ecx, 1 << 2);
    }
}

#[cfg(target_os = "macos")]
impl GuestVirtualizationBackend for MacosGuestVirtualizationBackend {
    fn validate(&self, mode: GuestVirtualizationMode) -> Result<(), GuestVirtualizationError> {
        match mode {
            GuestVirtualizationMode::Standard => Ok(()),
            GuestVirtualizationMode::Nested => Err(GuestVirtualizationError::Unsupported {
                mode,
                platform: "macOS",
            }),
        }
    }
}

#[cfg(target_os = "windows")]
impl GuestVirtualizationBackend for WindowsGuestVirtualizationBackend {
    fn validate(&self, mode: GuestVirtualizationMode) -> Result<(), GuestVirtualizationError> {
        match mode {
            GuestVirtualizationMode::Standard => Ok(()),
            GuestVirtualizationMode::Nested => Err(GuestVirtualizationError::Unsupported {
                mode,
                platform: "windows",
            }),
        }
    }
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
#[derive(Clone, Copy)]
enum Register {
    Ecx,
}

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
fn clear_cpuid_feature(
    entries: &mut [kvm_cpuid_entry2],
    leaf: u32,
    subleaf: u32,
    register: Register,
    bit: u32,
) {
    for entry in entries {
        if entry.function != leaf || entry.index != subleaf {
            continue;
        }
        match register {
            Register::Ecx => entry.ecx &= !bit,
        }
    }
}

#[cfg(test)]
#[path = "guest_virtualization_test.rs"]
mod tests;
