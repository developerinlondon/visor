//! CPU templates for CPUID leaf filtering and standardization.
//!
//! Templates ensure guests see identical CPU features regardless of host
//! hardware, enabling safe live migration between heterogeneous hosts.
//!
//! Each template contains a set of [`CpuIdFilter`]s that AND-mask specific
//! CPUID leaf registers. This hides host-specific features from the guest
//! so that migrating to a host with fewer features does not cause crashes.
//!
//! # Example
//!
//! ```rust
//! use visor_vmm::cpu_template::CpuTemplate;
//!
//! let toml = r#"
//! [template]
//! name = "my-baseline"
//! description = "Mask hypervisor bit"
//!
//! [[template.filters]]
//! leaf = "0x01"
//! ecx_mask = "0x7FFFFFFF"
//! "#;
//!
//! let template = CpuTemplate::from_toml(toml).unwrap();
//! assert_eq!(template.name, "my-baseline");
//! ```

#[cfg(target_os = "linux")]
use kvm_bindings::kvm_cpuid_entry2;
use serde::{Deserialize, Deserializer, Serialize};

/// Errors from CPU template operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CpuTemplateError {
    /// Failed to parse TOML template.
    #[error("failed to parse CPU template: {0}")]
    Parse(String),

    /// Template references an invalid CPUID leaf.
    #[error("invalid CPUID leaf {leaf:#x} in template '{name}'")]
    InvalidLeaf {
        /// Template name.
        name: String,
        /// The invalid leaf number.
        leaf: u32,
    },
}

/// A CPU template that filters CPUID leaves via AND masks.
///
/// Templates ensure guests see identical CPU features regardless of
/// host hardware, enabling safe live migration between heterogeneous hosts.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CpuTemplate {
    /// Template name (e.g., `"zen2-baseline"`).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// CPUID leaf filters to apply.
    #[serde(default)]
    pub filters: Vec<CpuIdFilter>,
}

/// A filter for a single CPUID leaf/subleaf.
///
/// Each register mask is AND-ed with the host CPUID value.
/// A mask of `0xFFFF_FFFF` preserves all bits; `0x0000_0000` clears all.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CpuIdFilter {
    /// CPUID leaf number (e.g., `0x07`).
    #[serde(deserialize_with = "deserialize_hex_u32")]
    pub leaf: u32,
    /// CPUID subleaf (`0` if not applicable).
    #[serde(default, deserialize_with = "deserialize_hex_u32_or_default")]
    pub subleaf: u32,
    /// AND mask for EAX (default: all bits preserved).
    #[serde(
        default = "mask_all",
        deserialize_with = "deserialize_hex_u32_or_default_all"
    )]
    pub eax_mask: u32,
    /// AND mask for EBX (default: all bits preserved).
    #[serde(
        default = "mask_all",
        deserialize_with = "deserialize_hex_u32_or_default_all"
    )]
    pub ebx_mask: u32,
    /// AND mask for ECX (default: all bits preserved).
    #[serde(
        default = "mask_all",
        deserialize_with = "deserialize_hex_u32_or_default_all"
    )]
    pub ecx_mask: u32,
    /// AND mask for EDX (default: all bits preserved).
    #[serde(
        default = "mask_all",
        deserialize_with = "deserialize_hex_u32_or_default_all"
    )]
    pub edx_mask: u32,
}

/// Default mask value: preserve all bits.
const fn mask_all() -> u32 {
    0xFFFF_FFFF
}

impl CpuTemplate {
    /// Parses a CPU template from a TOML string.
    ///
    /// The TOML must contain a `[template]` table with `name`, `description`,
    /// and an optional `[[template.filters]]` array. Leaf and mask values are
    /// parsed from hex strings (e.g., `"0x07"`, `"0xFFFFFFDF"`).
    ///
    /// # Errors
    ///
    /// Returns [`CpuTemplateError::Parse`] if the TOML is invalid or missing
    /// required fields.
    pub fn from_toml(toml_str: &str) -> Result<Self, CpuTemplateError> {
        let wrapper: TomlWrapper =
            toml::from_str(toml_str).map_err(|e| CpuTemplateError::Parse(e.to_string()))?;
        Ok(wrapper.template)
    }

    /// Applies this template's filters to a slice of CPUID entries.
    ///
    /// For each filter, finds matching entries by `(leaf, subleaf)` and
    /// ANDs each register with the corresponding mask. Entries not matched
    /// by any filter pass through unchanged.
    #[cfg(target_os = "linux")]
    pub fn apply(&self, entries: &mut [kvm_cpuid_entry2]) {
        for filter in &self.filters {
            for entry in entries.iter_mut() {
                if entry.function == filter.leaf && entry.index == filter.subleaf {
                    entry.eax &= filter.eax_mask;
                    entry.ebx &= filter.ebx_mask;
                    entry.ecx &= filter.ecx_mask;
                    entry.edx &= filter.edx_mask;
                }
            }
        }
    }

    /// Returns the built-in "common" template.
    ///
    /// This template masks features that vary between host CPUs and are
    /// problematic for live migration:
    ///
    /// - **Leaf 0x01 ECX bit 31**: Hypervisor present bit — cleared so
    ///   the guest does not assume a specific hypervisor ABI.
    /// - **Leaf 0x01 ECX bit 24**: TSC deadline timer — cleared because
    ///   not all hosts expose it uniformly.
    #[must_use]
    pub fn common() -> Self {
        Self {
            name: String::from("common"),
            description: String::from(
                "Common baseline: masks hypervisor bit and TSC deadline timer",
            ),
            filters: vec![CpuIdFilter {
                leaf: 0x01,
                subleaf: 0x00,
                eax_mask: mask_all(),
                ebx_mask: mask_all(),
                // Clear bit 31 (hypervisor) and bit 24 (TSC deadline).
                ecx_mask: 0xFEFF_FFFF & 0x7FFF_FFFF,
                edx_mask: mask_all(),
            }],
        }
    }

    /// Returns the built-in "Zen 2 baseline" template.
    ///
    /// Masks features not universally present across Zen 2 steppings,
    /// ensuring safe migration within a Zen 2 fleet.
    #[must_use]
    pub fn zen2() -> Self {
        Self {
            name: String::from("zen2-baseline"),
            description: String::from("AMD Zen 2 lowest common denominator for live migration"),
            filters: vec![
                // Leaf 0x01: mask hypervisor bit + TSC deadline.
                CpuIdFilter {
                    leaf: 0x01,
                    subleaf: 0x00,
                    eax_mask: mask_all(),
                    ebx_mask: mask_all(),
                    ecx_mask: 0xFEFF_FFFF & 0x7FFF_FFFF,
                    edx_mask: mask_all(),
                },
                // Leaf 0x07 subleaf 0: mask AVX-512 bits (not on Zen 2).
                // Bits 16 (AVX512F), 17 (AVX512DQ), 21 (AVX512IFMA),
                // 26 (AVX512PF), 27 (AVX512ER), 28 (AVX512CD),
                // 30 (AVX512BW), 31 (AVX512VL).
                CpuIdFilter {
                    leaf: 0x07,
                    subleaf: 0x00,
                    eax_mask: mask_all(),
                    ebx_mask: 0x11DC_FFFF,
                    ecx_mask: mask_all(),
                    edx_mask: mask_all(),
                },
                // Leaf 0x80000001: mask bit 0 of ECX (LAHF/SAHF availability
                // varies; keep conservative).
                CpuIdFilter {
                    leaf: 0x8000_0001,
                    subleaf: 0x00,
                    eax_mask: mask_all(),
                    ebx_mask: mask_all(),
                    ecx_mask: 0xFFFF_FFFE,
                    edx_mask: mask_all(),
                },
            ],
        }
    }

    /// Returns the built-in "Ice Lake baseline" template.
    ///
    /// Masks features not universally present across Ice Lake steppings,
    /// ensuring safe migration within an Ice Lake fleet.
    #[must_use]
    pub fn icelake() -> Self {
        Self {
            name: String::from("icelake-baseline"),
            description: String::from(
                "Intel Ice Lake lowest common denominator for live migration",
            ),
            filters: vec![
                // Leaf 0x01: mask hypervisor bit + TSC deadline.
                CpuIdFilter {
                    leaf: 0x01,
                    subleaf: 0x00,
                    eax_mask: mask_all(),
                    ebx_mask: mask_all(),
                    ecx_mask: 0xFEFF_FFFF & 0x7FFF_FFFF,
                    edx_mask: mask_all(),
                },
                // Leaf 0x07 subleaf 0: mask SGX (bit 2) — not all Ice Lake
                // SKUs have SGX enabled.
                CpuIdFilter {
                    leaf: 0x07,
                    subleaf: 0x00,
                    eax_mask: mask_all(),
                    ebx_mask: 0xFFFF_FFFB,
                    ecx_mask: mask_all(),
                    edx_mask: mask_all(),
                },
                // Leaf 0x80000001: mask bit 0 of ECX (LAHF/SAHF).
                CpuIdFilter {
                    leaf: 0x8000_0001,
                    subleaf: 0x00,
                    eax_mask: mask_all(),
                    ebx_mask: mask_all(),
                    ecx_mask: 0xFFFF_FFFE,
                    edx_mask: mask_all(),
                },
            ],
        }
    }
}

// ── TOML Deserialization Helpers ──────────────────────────────────────────

/// Wrapper for the TOML `[template]` table.
#[derive(Deserialize)]
struct TomlWrapper {
    template: CpuTemplate,
}

/// Parses a `u32` from a hex string like `"0x07"` or `"0xFFFFFFDF"`.
///
/// Returns an error if the string is not valid hex.
fn parse_hex_u32(s: &str) -> Result<u32, String> {
    let stripped = s
        .strip_prefix("0x")
        .or_else(|| s.strip_prefix("0X"))
        .unwrap_or(s);
    u32::from_str_radix(stripped, 16).map_err(|e| format!("invalid hex value '{s}': {e}"))
}

/// Deserializes a `u32` from a hex string.
fn deserialize_hex_u32<'de, D: Deserializer<'de>>(deserializer: D) -> Result<u32, D::Error> {
    let s = String::deserialize(deserializer)?;
    parse_hex_u32(&s).map_err(serde::de::Error::custom)
}

/// Deserializes an optional `u32` from a hex string, defaulting to `0`.
fn deserialize_hex_u32_or_default<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<u32, D::Error> {
    let s = String::deserialize(deserializer)?;
    if s.is_empty() {
        return Ok(0);
    }
    parse_hex_u32(&s).map_err(serde::de::Error::custom)
}

/// Deserializes an optional `u32` mask from a hex string, defaulting to all bits set.
fn deserialize_hex_u32_or_default_all<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<u32, D::Error> {
    let s = String::deserialize(deserializer)?;
    if s.is_empty() {
        return Ok(mask_all());
    }
    parse_hex_u32(&s).map_err(serde::de::Error::custom)
}

#[cfg(test)]
#[path = "cpu_template_test.rs"]
mod tests;
