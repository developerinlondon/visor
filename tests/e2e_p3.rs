//! P3 integration tests: CPU templates, dirty page tracking, and live migration.
//!
//! These tests validate the P3 non-hardware layers work correctly together.
//! They exercise public APIs across crate boundaries. Since most P3 types
//! are `#[non_exhaustive]`, we use public constructors and methods only.

use visor_vmm::cpu_template::CpuTemplate;
use visor_vmm::dirty_tracking::{DirtyBitmap, DirtyRateEstimator, DirtyTracker};
use visor_vmm::migration::{ConvergenceDetector, PreCopyAction, PreCopyState};

// ── CPU template tests ───────────────────────────────────────────────

#[test]
fn cpu_template_common_preset_exists() {
    let template = CpuTemplate::common();
    assert_eq!(template.name, "common");
    assert!(
        !template.filters.is_empty(),
        "common template should have at least one filter"
    );
}

#[test]
fn cpu_template_zen2_preset_exists() {
    let template = CpuTemplate::zen2();
    assert_eq!(template.name, "zen2-baseline");
    assert!(!template.filters.is_empty());
}

#[test]
fn cpu_template_icelake_preset_exists() {
    let template = CpuTemplate::icelake();
    assert_eq!(template.name, "icelake-baseline");
    assert!(!template.filters.is_empty());
}

#[cfg(target_os = "linux")]
#[test]
fn cpu_template_apply_masks_cpuid() {
    // AVX-512F is CPUID leaf 7, subleaf 0, EBX bit 16
    let toml_str = r#"
[template]
name = "test-no-avx512"
description = "Masks AVX-512F"

[[template.filters]]
leaf = "0x07"
subleaf = "0x00"
eax_mask = "0xFFFFFFFF"
ebx_mask = "0xFFFEFFFF"
ecx_mask = "0xFFFFFFFF"
edx_mask = "0xFFFFFFFF"
"#;
    let template = CpuTemplate::from_toml(toml_str).unwrap();

    let mut entries = [kvm_bindings::kvm_cpuid_entry2 {
        function: 0x07,
        index: 0x00,
        eax: 0xFFFF_FFFF,
        ebx: 0xFFFF_FFFF,
        ecx: 0xFFFF_FFFF,
        edx: 0xFFFF_FFFF,
        ..Default::default()
    }];

    template.apply(&mut entries);

    assert_eq!(
        entries[0].ebx & (1 << 16),
        0,
        "AVX-512F bit should be cleared"
    );
    assert_eq!(entries[0].eax, 0xFFFF_FFFF, "EAX should be untouched");
}

#[cfg(target_os = "linux")]
#[test]
fn cpu_template_apply_ignores_unmatched_leaves() {
    let toml_str = r#"
[template]
name = "narrow"
description = "Only filters leaf 0x07"

[[template.filters]]
leaf = "0x07"
subleaf = "0x00"
eax_mask = "0x00000000"
ebx_mask = "0x00000000"
ecx_mask = "0x00000000"
edx_mask = "0x00000000"
"#;
    let template = CpuTemplate::from_toml(toml_str).unwrap();

    let mut entries = [kvm_bindings::kvm_cpuid_entry2 {
        function: 0x01,
        index: 0x00,
        eax: 0xDEAD_BEEF,
        ebx: 0xCAFE_BABE,
        ecx: 0x1234_5678,
        edx: 0x9ABC_DEF0,
        ..Default::default()
    }];

    template.apply(&mut entries);

    assert_eq!(entries[0].eax, 0xDEAD_BEEF, "unmatched leaf passes through");
    assert_eq!(entries[0].ebx, 0xCAFE_BABE);
}

#[test]
fn cpu_template_from_toml_parses() {
    let toml_str = r#"
[template]
name = "test"
description = "Test template"

[[template.filters]]
leaf = "0x01"
eax_mask = "0xFFFFFFFF"
ebx_mask = "0xFFFFFFFF"
ecx_mask = "0x7EFFFFFF"
edx_mask = "0xFFFFFFFF"
"#;

    let template = CpuTemplate::from_toml(toml_str).unwrap();
    assert_eq!(template.name, "test");
    assert_eq!(template.filters.len(), 1);
    assert_eq!(template.filters[0].ecx_mask, 0x7EFF_FFFF);
}

#[test]
fn cpu_template_from_toml_invalid_returns_error() {
    assert!(CpuTemplate::from_toml("not valid {{{").is_err());
}

// ── Dirty page tracking tests ────────────────────────────────────────

#[test]
fn dirty_bitmap_new_is_clean() {
    let bitmap = DirtyBitmap::new(1024 * 4096);
    assert_eq!(bitmap.dirty_count(), 0);
    assert_eq!(bitmap.page_count(), 1024);
}

#[test]
fn dirty_bitmap_set_and_query() {
    let mut bitmap = DirtyBitmap::new(256 * 4096);
    bitmap.set_dirty(0);
    bitmap.set_dirty(100);
    bitmap.set_dirty(255);

    assert!(bitmap.is_dirty(0));
    assert!(bitmap.is_dirty(100));
    assert!(bitmap.is_dirty(255));
    assert!(!bitmap.is_dirty(50));
    assert_eq!(bitmap.dirty_count(), 3);
}

#[test]
fn dirty_bitmap_clear_resets() {
    let mut bitmap = DirtyBitmap::new(128 * 4096);
    for i in 0..128 {
        bitmap.set_dirty(i);
    }
    assert_eq!(bitmap.dirty_count(), 128);
    bitmap.clear();
    assert_eq!(bitmap.dirty_count(), 0);
}

#[test]
fn dirty_bitmap_merge() {
    let mut a = DirtyBitmap::new(64 * 4096);
    let mut b = DirtyBitmap::new(64 * 4096);

    a.set_dirty(0);
    a.set_dirty(10);
    b.set_dirty(10);
    b.set_dirty(50);

    a.merge(&b);

    assert!(a.is_dirty(0));
    assert!(a.is_dirty(10));
    assert!(a.is_dirty(50));
    assert_eq!(a.dirty_count(), 3); // 0, 10, 50 (10 is shared)
}

#[test]
fn dirty_bitmap_pages_iterator() {
    let mut bitmap = DirtyBitmap::new(32 * 4096);
    bitmap.set_dirty(3);
    bitmap.set_dirty(7);
    bitmap.set_dirty(31);

    let pages: Vec<usize> = bitmap.dirty_pages().collect();
    assert_eq!(pages, vec![3, 7, 31]);
}

#[test]
fn dirty_bitmap_from_raw() {
    let raw = vec![0xFF, 0x00]; // First 8 pages dirty, next 8 clean
    let bitmap = DirtyBitmap::from_raw(raw, 16 * 4096).unwrap();
    assert_eq!(bitmap.dirty_count(), 8);
}

#[test]
fn dirty_bitmap_from_raw_size_mismatch() {
    let raw = vec![0xFF, 0xFF]; // 2 bytes
    assert!(DirtyBitmap::from_raw(raw, 32 * 4096).is_err());
}

#[test]
fn dirty_rate_estimator_two_samples() {
    let mut est = DirtyRateEstimator::new();
    // First sample: no rate
    assert!(est.sample(0, 100).is_none());
    // Second sample: 200 dirty pages in 1000ms = 200 pages/sec
    let rate = est.sample(1000, 200).unwrap();
    assert!(
        (rate - 200.0).abs() < 1.0,
        "expected ~200 pages/sec, got {rate}"
    );
}

#[test]
fn dirty_tracker_creates_with_slot() {
    let tracker = DirtyTracker::new(256 * 1024 * 1024, 0);
    assert_eq!(tracker.slot(), 0);
    assert!(tracker.current_rate().is_none());
}

#[test]
fn dirty_tracker_collect_produces_snapshot() {
    let mut tracker = DirtyTracker::new(64 * 4096, 0); // 64 pages = 256 KiB
    let mut bitmap = DirtyBitmap::new(64 * 4096);
    bitmap.set_dirty(0);
    bitmap.set_dirty(63);

    let snapshot = tracker.collect_from_bitmap(bitmap, 1000).unwrap();
    assert_eq!(snapshot.dirty_count, 2);
    assert!(snapshot.bitmap.is_dirty(0));
    assert!(snapshot.bitmap.is_dirty(63));
}

// ── Migration convergence tests ──────────────────────────────────────

#[test]
fn convergence_detector_low_rate_triggers() {
    let detector = ConvergenceDetector::new();
    assert!(detector.should_stop_and_copy(Some(10.0), 1));
}

#[test]
fn convergence_detector_high_rate_continues() {
    let detector = ConvergenceDetector::new();
    assert!(!detector.should_stop_and_copy(Some(1000.0), 1));
}

#[test]
fn convergence_detector_max_rounds_forces() {
    let detector = ConvergenceDetector::with_config(50.0, 5);
    assert!(detector.should_stop_and_copy(Some(1000.0), 5));
}

#[test]
fn convergence_detector_no_data_waits() {
    let detector = ConvergenceDetector::new();
    assert!(!detector.should_stop_and_copy(None, 1));
}

#[test]
fn pre_copy_state_empty_bitmap_converges() {
    let mut state = PreCopyState::new();
    let mut tracker = DirtyTracker::new(1024 * 4096, 0);
    let bitmap = DirtyBitmap::new(1024 * 4096); // All clean

    let snapshot = tracker.collect_from_bitmap(bitmap, 1000).unwrap();
    let action = state.process_snapshot(&snapshot);
    assert!(
        matches!(action, PreCopyAction::StopAndCopy { .. }),
        "zero dirty pages should converge immediately"
    );
}

#[test]
fn pre_copy_state_high_rate_sends_pages() {
    let mut state = PreCopyState::new();
    let mut tracker = DirtyTracker::new(1024 * 4096, 0);

    // First sample to establish baseline
    let bitmap1 = DirtyBitmap::new(1024 * 4096);
    let _snap1 = tracker.collect_from_bitmap(bitmap1, 0).unwrap();

    // Second sample with lots of dirty pages + high rate
    let mut bitmap2 = DirtyBitmap::new(1024 * 4096);
    for i in 0..500 {
        bitmap2.set_dirty(i);
    }
    let snapshot = tracker.collect_from_bitmap(bitmap2, 100).unwrap();
    // Rate should be very high (500 pages in 100ms = 5000 pages/sec)
    let action = state.process_snapshot(&snapshot);
    match action {
        PreCopyAction::SendPages {
            round,
            dirty_page_indices,
        } => {
            assert_eq!(round, 1);
            assert_eq!(dirty_page_indices.len(), 500);
        }
        PreCopyAction::StopAndCopy { .. } => {
            panic!("high dirty rate should NOT trigger stop-and-copy yet");
        }
        _ => panic!("unexpected PreCopyAction variant"),
    }
}

// ── Cross-layer integration ──────────────────────────────────────────

#[cfg(target_os = "linux")]
#[test]
fn cpu_template_and_dirty_tracking_workflow() {
    // Step 1: Create and apply a CPU template
    let template = CpuTemplate::common();
    let mut entries = [kvm_bindings::kvm_cpuid_entry2 {
        function: 0x01,
        index: 0x00,
        eax: 0xFFFF_FFFF,
        ebx: 0xFFFF_FFFF,
        ecx: 0xFFFF_FFFF,
        edx: 0xFFFF_FFFF,
        ..Default::default()
    }];
    template.apply(&mut entries);
    // Common template clears hypervisor bit (31) and TSC deadline (24)
    assert_eq!(entries[0].ecx & (1 << 31), 0, "hypervisor bit cleared");
    assert_eq!(entries[0].ecx & (1 << 24), 0, "TSC deadline cleared");

    // Step 2: Track dirty pages
    let mut tracker = DirtyTracker::new(64 * 4096, 0);
    let mut bitmap = DirtyBitmap::new(64 * 4096);
    bitmap.set_dirty(0);
    bitmap.set_dirty(1);

    let snapshot = tracker.collect_from_bitmap(bitmap, 1000).unwrap();
    assert_eq!(snapshot.dirty_count, 2);

    // Step 3: Check convergence via pre-copy state
    let mut pre_copy = PreCopyState::new();
    let action = pre_copy.process_snapshot(&snapshot);
    // Either SendPages or StopAndCopy is valid for first sample
    match &action {
        PreCopyAction::SendPages {
            dirty_page_indices, ..
        } => {
            assert_eq!(dirty_page_indices.len(), 2);
        }
        PreCopyAction::StopAndCopy { .. } => {
            // Also valid
        }
        _ => panic!("unexpected PreCopyAction variant"),
    }
}
