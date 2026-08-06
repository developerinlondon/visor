//! Tests for CPU template parsing, CPUID filtering, and built-in templates.

use super::*;

// ── Template Parsing Tests (portable) ────────────────────────────────────────

#[test]
fn test_parse_template_from_toml() {
    let toml_str = r#"
[template]
name = "test-template"
description = "A test template"

[[template.filters]]
leaf = "0x07"
subleaf = "0x00"
ebx_mask = "0xFFFFFFDF"
"#;
    let template = CpuTemplate::from_toml(toml_str).unwrap();
    assert_eq!(template.name, "test-template");
    assert_eq!(template.filters.len(), 1);
    assert_eq!(template.filters[0].leaf, 0x07);
    assert_eq!(template.filters[0].subleaf, 0x00);
    assert_eq!(template.filters[0].ebx_mask, 0xFFFF_FFDF);
    // Unset masks default to all-ones (preserve all bits).
    assert_eq!(template.filters[0].eax_mask, 0xFFFF_FFFF);
    assert_eq!(template.filters[0].ecx_mask, 0xFFFF_FFFF);
    assert_eq!(template.filters[0].edx_mask, 0xFFFF_FFFF);
}

#[test]
fn test_parse_template_name_and_description() {
    let toml_str = r#"
[template]
name = "zen2-baseline"
description = "Zen 2 lowest common denominator for live migration"
"#;
    let template = CpuTemplate::from_toml(toml_str).unwrap();
    assert_eq!(template.name, "zen2-baseline");
    assert_eq!(
        template.description,
        "Zen 2 lowest common denominator for live migration"
    );
}

#[test]
fn test_parse_template_empty_filters() {
    let toml_str = r#"
[template]
name = "empty"
description = "No filters at all"
"#;
    let template = CpuTemplate::from_toml(toml_str).unwrap();
    assert!(template.filters.is_empty());
}

#[test]
fn test_parse_template_invalid_toml() {
    let result = CpuTemplate::from_toml("not valid toml {{{");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        err.to_string().contains("failed to parse CPU template"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_parse_template_hex_values() {
    let toml_str = r#"
[template]
name = "hex-test"
description = "Hex values for leaf and masks"

[[template.filters]]
leaf = "0x80000001"
subleaf = "0x00"
eax_mask = "0x00000000"
ebx_mask = "0xFFFFFFFF"
ecx_mask = "0xFFFFFFFE"
edx_mask = "0xABCD1234"
"#;
    let template = CpuTemplate::from_toml(toml_str).unwrap();
    assert_eq!(template.filters[0].leaf, 0x8000_0001);
    assert_eq!(template.filters[0].eax_mask, 0x0000_0000);
    assert_eq!(template.filters[0].ebx_mask, 0xFFFF_FFFF);
    assert_eq!(template.filters[0].ecx_mask, 0xFFFF_FFFE);
    assert_eq!(template.filters[0].edx_mask, 0xABCD_1234);
}

// ── Built-in Template Existence Tests (portable) ─────────────────────────────

#[test]
fn test_builtin_common_exists() {
    let template = CpuTemplate::common();
    assert_eq!(template.name, "common");
    assert!(!template.filters.is_empty());
}

#[test]
fn test_builtin_zen2_exists() {
    let template = CpuTemplate::zen2();
    assert_eq!(template.name, "zen2-baseline");
    assert!(!template.filters.is_empty());
}

#[test]
fn test_builtin_icelake_exists() {
    let template = CpuTemplate::icelake();
    assert_eq!(template.name, "icelake-baseline");
    assert!(!template.filters.is_empty());
}

// ── KVM-dependent tests (Linux only) ─────────────────────────────────────────

#[cfg(target_os = "linux")]
mod linux {
    use kvm_bindings::kvm_cpuid_entry2;

    use super::*;

    /// Creates a `kvm_cpuid_entry2` with the given leaf, subleaf, and register values.
    fn make_entry(
        function: u32,
        index: u32,
        eax: u32,
        ebx: u32,
        ecx: u32,
        edx: u32,
    ) -> kvm_cpuid_entry2 {
        kvm_cpuid_entry2 {
            function,
            index,
            flags: 0,
            eax,
            ebx,
            ecx,
            edx,
            padding: [0; 3],
        }
    }

    // ── CPUID Filtering Tests ─────────────────────────────────────────────────

    #[test]
    fn test_filter_masks_single_leaf() {
        // Template masks bit 5 of leaf 0x07 EBX.
        let template = CpuTemplate {
            name: String::from("test"),
            description: String::new(),
            filters: vec![CpuIdFilter {
                leaf: 0x07,
                subleaf: 0x00,
                eax_mask: 0xFFFF_FFFF,
                ebx_mask: 0xFFFF_FFDF, // clear bit 5
                ecx_mask: 0xFFFF_FFFF,
                edx_mask: 0xFFFF_FFFF,
            }],
        };

        let mut entries = vec![make_entry(
            0x07,
            0x00,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
        )];
        template.apply(&mut entries);

        assert_eq!(entries[0].ebx, 0xFFFF_FFDF);
        // Other registers should be unchanged.
        assert_eq!(entries[0].eax, 0xFFFF_FFFF);
        assert_eq!(entries[0].ecx, 0xFFFF_FFFF);
        assert_eq!(entries[0].edx, 0xFFFF_FFFF);
    }

    #[test]
    fn test_filter_preserves_unmasked_leaves() {
        // Template only touches leaf 0x07 — leaf 0x01 should pass through unchanged.
        let template = CpuTemplate {
            name: String::from("test"),
            description: String::new(),
            filters: vec![CpuIdFilter {
                leaf: 0x07,
                subleaf: 0x00,
                eax_mask: 0x0000_0000,
                ebx_mask: 0x0000_0000,
                ecx_mask: 0x0000_0000,
                edx_mask: 0x0000_0000,
            }],
        };

        let mut entries = vec![
            make_entry(
                0x01,
                0x00,
                0xDEAD_BEEF,
                0xCAFE_BABE,
                0x1234_5678,
                0xABCD_EF01,
            ),
            make_entry(
                0x07,
                0x00,
                0xFFFF_FFFF,
                0xFFFF_FFFF,
                0xFFFF_FFFF,
                0xFFFF_FFFF,
            ),
        ];
        template.apply(&mut entries);

        // Leaf 0x01 untouched.
        assert_eq!(entries[0].eax, 0xDEAD_BEEF);
        assert_eq!(entries[0].ebx, 0xCAFE_BABE);
        assert_eq!(entries[0].ecx, 0x1234_5678);
        assert_eq!(entries[0].edx, 0xABCD_EF01);

        // Leaf 0x07 fully zeroed.
        assert_eq!(entries[1].eax, 0x0000_0000);
        assert_eq!(entries[1].ebx, 0x0000_0000);
        assert_eq!(entries[1].ecx, 0x0000_0000);
        assert_eq!(entries[1].edx, 0x0000_0000);
    }

    #[test]
    fn test_filter_multiple_leaves() {
        let template = CpuTemplate {
            name: String::from("multi"),
            description: String::new(),
            filters: vec![
                CpuIdFilter {
                    leaf: 0x01,
                    subleaf: 0x00,
                    eax_mask: 0xFFFF_0000,
                    ebx_mask: 0xFFFF_FFFF,
                    ecx_mask: 0xFFFF_FFFF,
                    edx_mask: 0xFFFF_FFFF,
                },
                CpuIdFilter {
                    leaf: 0x07,
                    subleaf: 0x00,
                    eax_mask: 0xFFFF_FFFF,
                    ebx_mask: 0x0000_FFFF,
                    ecx_mask: 0xFFFF_FFFF,
                    edx_mask: 0xFFFF_FFFF,
                },
                CpuIdFilter {
                    leaf: 0x8000_0001,
                    subleaf: 0x00,
                    eax_mask: 0xFFFF_FFFF,
                    ebx_mask: 0xFFFF_FFFF,
                    ecx_mask: 0xFFFF_FFFF,
                    edx_mask: 0x0000_0000,
                },
            ],
        };

        let mut entries = vec![
            make_entry(
                0x01,
                0x00,
                0x1234_5678,
                0xAAAA_BBBB,
                0xCCCC_DDDD,
                0xEEEE_FFFF,
            ),
            make_entry(
                0x07,
                0x00,
                0x1111_2222,
                0x3333_4444,
                0x5555_6666,
                0x7777_8888,
            ),
            make_entry(
                0x8000_0001,
                0x00,
                0xAAAA_AAAA,
                0xBBBB_BBBB,
                0xCCCC_CCCC,
                0xDDDD_DDDD,
            ),
        ];
        template.apply(&mut entries);

        // Leaf 0x01: EAX upper 16 bits preserved, lower 16 cleared.
        assert_eq!(entries[0].eax, 0x1234_0000);
        // Leaf 0x07: EBX lower 16 bits preserved, upper 16 cleared.
        assert_eq!(entries[1].ebx, 0x0000_4444);
        // Leaf 0x80000001: EDX fully cleared.
        assert_eq!(entries[2].edx, 0x0000_0000);
    }

    #[test]
    fn test_filter_subleaf_specific() {
        // Filter targets subleaf 1 of leaf 0x07 — subleaf 0 should be untouched.
        let template = CpuTemplate {
            name: String::from("subleaf"),
            description: String::new(),
            filters: vec![CpuIdFilter {
                leaf: 0x07,
                subleaf: 1,
                eax_mask: 0x0000_0000,
                ebx_mask: 0x0000_0000,
                ecx_mask: 0x0000_0000,
                edx_mask: 0x0000_0000,
            }],
        };

        let mut entries = vec![
            make_entry(0x07, 0, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF),
            make_entry(0x07, 1, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF, 0xFFFF_FFFF),
        ];
        template.apply(&mut entries);

        // Subleaf 0 unchanged.
        assert_eq!(entries[0].eax, 0xFFFF_FFFF);
        assert_eq!(entries[0].ebx, 0xFFFF_FFFF);

        // Subleaf 1 zeroed.
        assert_eq!(entries[1].eax, 0x0000_0000);
        assert_eq!(entries[1].ebx, 0x0000_0000);
        assert_eq!(entries[1].ecx, 0x0000_0000);
        assert_eq!(entries[1].edx, 0x0000_0000);
    }

    #[test]
    fn test_filter_all_registers() {
        // Each register gets a different mask to verify independence.
        let template = CpuTemplate {
            name: String::from("all-regs"),
            description: String::new(),
            filters: vec![CpuIdFilter {
                leaf: 0x01,
                subleaf: 0x00,
                eax_mask: 0xFF00_FF00,
                ebx_mask: 0x00FF_00FF,
                ecx_mask: 0xF0F0_F0F0,
                edx_mask: 0x0F0F_0F0F,
            }],
        };

        let mut entries = vec![make_entry(
            0x01,
            0x00,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
        )];
        template.apply(&mut entries);

        assert_eq!(entries[0].eax, 0xFF00_FF00);
        assert_eq!(entries[0].ebx, 0x00FF_00FF);
        assert_eq!(entries[0].ecx, 0xF0F0_F0F0);
        assert_eq!(entries[0].edx, 0x0F0F_0F0F);
    }

    #[test]
    fn test_builtin_common_masks_hypervisor_bit() {
        let template = CpuTemplate::common();

        // Leaf 0x01, ECX bit 31 is the hypervisor present bit.
        let mut entries = vec![make_entry(
            0x01,
            0x00,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
        )];
        template.apply(&mut entries);

        // Bit 31 of ECX should be cleared.
        assert_eq!(
            entries[0].ecx & (1 << 31),
            0,
            "hypervisor bit should be masked"
        );
    }

    // ── Application Tests ─────────────────────────────────────────────────────

    #[test]
    fn test_apply_to_cpuid_entries() {
        let toml_str = r#"
[template]
name = "apply-test"
description = "Apply test"

[[template.filters]]
leaf = "0x01"
ecx_mask = "0x7FFFFFFF"
"#;
        let template = CpuTemplate::from_toml(toml_str).unwrap();

        let mut entries = vec![
            make_entry(0x00, 0x00, 0x0000_0010, 0x0, 0x0, 0x0),
            make_entry(
                0x01,
                0x00,
                0x0006_06E3,
                0x0010_0800,
                0xFFFA_F3FF,
                0xBFEB_FBFF,
            ),
            make_entry(0x02, 0x00, 0x7606_1301, 0x0, 0x0, 0x0),
        ];
        template.apply(&mut entries);

        // Leaf 0x00 and 0x02 untouched.
        assert_eq!(entries[0].eax, 0x0000_0010);
        assert_eq!(entries[2].eax, 0x7606_1301);

        // Leaf 0x01 ECX bit 31 cleared.
        assert_eq!(entries[1].ecx, 0x7FFA_F3FF);
    }

    #[test]
    fn test_two_templates_produce_same_output() {
        // Same template applied to two different host CPUID sets should produce
        // the same masked feature bits for the filtered leaves.
        let template = CpuTemplate {
            name: String::from("normalize"),
            description: String::new(),
            filters: vec![CpuIdFilter {
                leaf: 0x01,
                subleaf: 0x00,
                eax_mask: 0x0FFF_0FFF,
                ebx_mask: 0xFFFF_FFFF,
                ecx_mask: 0x7FFF_FFFF,
                edx_mask: 0xFFFF_FFFF,
            }],
        };

        // Host A has all bits set, Host B has a subset.
        let mut host_a = vec![make_entry(
            0x01,
            0x00,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
            0xFFFF_FFFF,
        )];
        let mut host_b = vec![make_entry(
            0x01,
            0x00,
            0x0FFF_0FFF,
            0xFFFF_FFFF,
            0x7FFF_FFFF,
            0xFFFF_FFFF,
        )];

        template.apply(&mut host_a);
        template.apply(&mut host_b);

        // After masking, the features exposed to guests are identical.
        assert_eq!(host_a[0].eax, host_b[0].eax);
        assert_eq!(host_a[0].ebx, host_b[0].ebx);
        assert_eq!(host_a[0].ecx, host_b[0].ecx);
        assert_eq!(host_a[0].edx, host_b[0].edx);
    }
}
