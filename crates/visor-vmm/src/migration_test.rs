use super::*;

use crate::dirty_tracking::{DirtyBitmap, DirtySnapshot, PAGE_SIZE};

// ── State serialization ─────────────────────────────────────────────

#[test]
fn test_migration_state_serializes() {
    let init = MigrationInit {
        memory_size: 256 * 1024 * 1024,
        cpu_template: Some("zen2-baseline".to_owned()),
        vcpu_count: 4,
    };
    let msg = MigrationMessage::Init(init.clone());
    let json = serde_json::to_string(&msg).unwrap();
    let roundtripped: MigrationMessage = serde_json::from_str(&json).unwrap();

    match roundtripped {
        MigrationMessage::Init(rt) => {
            assert_eq!(rt.memory_size, init.memory_size);
            assert_eq!(rt.cpu_template, init.cpu_template);
            assert_eq!(rt.vcpu_count, init.vcpu_count);
        }
        other => panic!("expected Init, got {other:?}"),
    }
}

#[test]
fn test_page_batch_creation() {
    let mut bitmap = DirtyBitmap::new(8 * PAGE_SIZE);
    bitmap.set_dirty(1);
    bitmap.set_dirty(5);

    let dirty_indices: Vec<usize> = bitmap.dirty_pages().collect();
    let batch = PageBatch {
        round: 1,
        pages: dirty_indices
            .iter()
            .map(|&idx| (idx, vec![0u8; PAGE_SIZE]))
            .collect(),
    };

    assert_eq!(batch.pages.len(), 2);
    assert_eq!(batch.pages[0].0, 1);
    assert_eq!(batch.pages[1].0, 5);
    assert_eq!(batch.pages[0].1.len(), PAGE_SIZE);
}

// ── Pre-copy algorithm ──────────────────────────────────────────────

#[test]
fn test_precopy_round_produces_page_batch() {
    let mut state = PreCopyState::new();
    let snapshot = make_snapshot_with_dirty_pages(&[0, 3, 7], 16, Some(200.0));

    let action = state.process_snapshot(&snapshot);
    match action {
        PreCopyAction::SendPages {
            round,
            dirty_page_indices,
        } => {
            assert_eq!(round, 1);
            assert_eq!(dirty_page_indices, vec![0, 3, 7]);
        }
        PreCopyAction::StopAndCopy { .. } => panic!("expected SendPages, got StopAndCopy"),
    }
    assert_eq!(state.round, 1);
    assert_eq!(state.total_pages_sent, 3);
}

#[test]
fn test_precopy_convergence_detected() {
    let mut state = PreCopyState::new();

    // First round: high dirty rate — keep going.
    let snap1 = make_snapshot_with_dirty_pages(&[0, 1, 2], 16, Some(200.0));
    let action1 = state.process_snapshot(&snap1);
    assert!(matches!(action1, PreCopyAction::SendPages { .. }));

    // Second round: dirty rate below threshold (50 pages/sec) — converge.
    let snap2 = make_snapshot_with_dirty_pages(&[4], 16, Some(10.0));
    let action2 = state.process_snapshot(&snap2);
    assert!(matches!(action2, PreCopyAction::StopAndCopy { round: 2 }));
}

#[test]
fn test_precopy_max_rounds_limit() {
    let convergence = ConvergenceDetector::with_config(50.0, 3);
    let mut state = PreCopyState::with_convergence(convergence);

    // Rounds 1-3: always high dirty rate.
    for i in 0..3 {
        let snap = make_snapshot_with_dirty_pages(&[0, 1], 16, Some(200.0));
        let action = state.process_snapshot(&snap);

        if i < 2 {
            assert!(
                matches!(action, PreCopyAction::SendPages { .. }),
                "round {} should be SendPages",
                i + 1
            );
        } else {
            // Round 3 (max_rounds) should force stop-and-copy.
            assert!(
                matches!(action, PreCopyAction::StopAndCopy { round: 3 }),
                "round 3 should force StopAndCopy"
            );
        }
    }
}

#[test]
fn test_precopy_no_dirty_pages_converges_immediately() {
    let mut state = PreCopyState::new();
    let snapshot = make_snapshot_with_dirty_pages(&[], 16, Some(0.0));

    let action = state.process_snapshot(&snapshot);
    assert!(matches!(action, PreCopyAction::StopAndCopy { round: 1 }));
}

// ── Convergence detector ────────────────────────────────────────────

#[test]
fn test_convergence_above_threshold() {
    let detector = ConvergenceDetector::new();
    // 200 pages/sec is well above default threshold of 50.
    assert!(!detector.should_stop_and_copy(Some(200.0), 1));
}

#[test]
fn test_convergence_below_threshold() {
    let detector = ConvergenceDetector::new();
    // 10 pages/sec is below default threshold of 50.
    assert!(detector.should_stop_and_copy(Some(10.0), 1));
}

#[test]
fn test_convergence_default_threshold() {
    let detector = ConvergenceDetector::new();
    // Default threshold is 50 pages/sec.
    // At exactly 50, should NOT converge (strictly below).
    assert!(!detector.should_stop_and_copy(Some(50.0), 1));
    // At 49.9, should converge.
    assert!(detector.should_stop_and_copy(Some(49.9), 1));
}

// ── Migration protocol ──────────────────────────────────────────────

#[test]
fn test_migration_init_message() {
    let init = MigrationInit {
        memory_size: 512 * 1024 * 1024,
        cpu_template: Some("common".to_owned()),
        vcpu_count: 2,
    };
    assert_eq!(init.memory_size, 512 * 1024 * 1024);
    assert_eq!(init.cpu_template.as_deref(), Some("common"));
    assert_eq!(init.vcpu_count, 2);

    // Verify it serializes as part of MigrationMessage.
    let msg = MigrationMessage::Init(init);
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("536870912"));
}

#[test]
fn test_migration_ready_response() {
    let ready = MigrationReady {
        session_id: "mig-abc-123".to_owned(),
    };
    assert_eq!(ready.session_id, "mig-abc-123");

    let msg = MigrationMessage::Ready(ready);
    let json = serde_json::to_string(&msg).unwrap();
    let roundtripped: MigrationMessage = serde_json::from_str(&json).unwrap();
    match roundtripped {
        MigrationMessage::Ready(r) => assert_eq!(r.session_id, "mig-abc-123"),
        other => panic!("expected Ready, got {other:?}"),
    }
}

#[test]
fn test_final_state_includes_cpu_regs() {
    let cpu_state = vec![0xDE, 0xAD, 0xBE, 0xEF];
    let last_pages = vec![(42_usize, vec![0xAA; PAGE_SIZE])];

    let final_state = FinalState {
        cpu_state: cpu_state.clone(),
        last_pages: last_pages.clone(),
    };

    assert_eq!(final_state.cpu_state, cpu_state);
    assert_eq!(final_state.last_pages.len(), 1);
    assert_eq!(final_state.last_pages[0].0, 42);
    assert_eq!(final_state.last_pages[0].1.len(), PAGE_SIZE);

    // Verify round-trip serialization.
    let msg = MigrationMessage::FinalState(final_state);
    let json = serde_json::to_string(&msg).unwrap();
    let roundtripped: MigrationMessage = serde_json::from_str(&json).unwrap();
    match roundtripped {
        MigrationMessage::FinalState(fs) => {
            assert_eq!(fs.cpu_state, cpu_state);
            assert_eq!(fs.last_pages.len(), 1);
        }
        other => panic!("expected FinalState, got {other:?}"),
    }
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Creates a `DirtySnapshot` with specific dirty pages set.
fn make_snapshot_with_dirty_pages(
    dirty_indices: &[usize],
    total_pages: usize,
    rate: Option<f64>,
) -> DirtySnapshot {
    let mut bitmap = DirtyBitmap::new(total_pages * PAGE_SIZE);
    for &idx in dirty_indices {
        bitmap.set_dirty(idx);
    }
    DirtySnapshot {
        dirty_count: bitmap.dirty_count(),
        bitmap,
        rate,
    }
}
