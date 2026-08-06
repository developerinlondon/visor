use super::*;

// -- DirtyBitmap: construction ------------------------------------------------

#[test]
fn test_bitmap_new_all_clean() {
    let bm = DirtyBitmap::new(4096 * 8); // 8 pages
    assert_eq!(bm.dirty_count(), 0);
    assert_eq!(bm.page_count(), 8);
}

#[test]
fn test_bitmap_set_page_dirty() {
    let mut bm = DirtyBitmap::new(4096 * 8);
    bm.set_dirty(3);
    assert!(bm.is_dirty(3));
    assert!(!bm.is_dirty(0));
    assert_eq!(bm.dirty_count(), 1);
}

#[test]
fn test_bitmap_clear_resets_all() {
    let mut bm = DirtyBitmap::new(4096 * 8);
    bm.set_dirty(0);
    bm.set_dirty(5);
    bm.set_dirty(7);
    assert_eq!(bm.dirty_count(), 3);
    bm.clear();
    assert_eq!(bm.dirty_count(), 0);
    assert!(!bm.is_dirty(0));
    assert!(!bm.is_dirty(5));
    assert!(!bm.is_dirty(7));
}

#[test]
fn test_bitmap_dirty_count() {
    let mut bm = DirtyBitmap::new(4096 * 64); // 64 pages = 8 bytes
    bm.set_dirty(0);
    bm.set_dirty(1);
    bm.set_dirty(63);
    assert_eq!(bm.dirty_count(), 3);

    // Setting the same page again should not change the count.
    bm.set_dirty(0);
    assert_eq!(bm.dirty_count(), 3);
}

#[test]
fn test_bitmap_page_granularity_4k() {
    // 4 KiB page granularity: 2 pages = 8192 bytes of memory.
    let bm = DirtyBitmap::new(8192);
    assert_eq!(bm.page_count(), 2);
    assert_eq!(bm.bitmap_size(), 1); // 2 bits → 1 byte
}

#[test]
fn test_bitmap_size_for_memory() {
    // 256 MiB = 256 * 1024 * 1024 bytes = 65,536 pages.
    // Bitmap: 65,536 bits = 8,192 bytes.
    let mem_size = 256 * 1024 * 1024;
    let bm = DirtyBitmap::new(mem_size);
    assert_eq!(bm.page_count(), 65_536);
    assert_eq!(bm.bitmap_size(), 8_192);
}

// -- DirtyBitmap: iteration ---------------------------------------------------

#[test]
fn test_bitmap_iter_dirty_pages() {
    let mut bm = DirtyBitmap::new(4096 * 16); // 16 pages
    bm.set_dirty(2);
    bm.set_dirty(7);
    bm.set_dirty(15);
    let dirty: Vec<usize> = bm.dirty_pages().collect();
    assert_eq!(dirty, vec![2, 7, 15]);
}

#[test]
fn test_bitmap_iter_empty() {
    let bm = DirtyBitmap::new(4096 * 16);
    let dirty: Vec<usize> = bm.dirty_pages().collect();
    assert!(dirty.is_empty());
}

// -- DirtyBitmap: merge -------------------------------------------------------

#[test]
fn test_bitmap_merge() {
    let mut a = DirtyBitmap::new(4096 * 16);
    a.set_dirty(0);
    a.set_dirty(4);

    let mut b = DirtyBitmap::new(4096 * 16);
    b.set_dirty(4);
    b.set_dirty(10);

    a.merge(&b);
    let dirty: Vec<usize> = a.dirty_pages().collect();
    assert_eq!(dirty, vec![0, 4, 10]);
    assert_eq!(a.dirty_count(), 3);
}

// -- DirtyBitmap: from_raw ----------------------------------------------------

#[test]
fn test_bitmap_from_raw_valid() {
    // 2 bytes = 16 pages → memory = 16 * 4096 = 65536
    let raw = vec![0b0000_0101, 0b1000_0000]; // pages 0, 2, 15
    let bm = DirtyBitmap::from_raw(raw, 4096 * 16).unwrap();
    assert_eq!(bm.dirty_count(), 3);
    assert!(bm.is_dirty(0));
    assert!(bm.is_dirty(2));
    assert!(bm.is_dirty(15));
}

#[test]
fn test_bitmap_from_raw_size_mismatch() {
    // 1 byte bitmap but 256 pages of memory → mismatch.
    let raw = vec![0xFF];
    let result = DirtyBitmap::from_raw(raw, 4096 * 256);
    assert!(result.is_err());
}

// -- DirtyRateEstimator -------------------------------------------------------

#[test]
fn test_rate_estimator_initial() {
    let mut est = DirtyRateEstimator::new();
    // First sample: no previous data → returns None.
    assert!(est.sample(1000, 100).is_none());
}

#[test]
fn test_rate_estimator_constant_rate() {
    let mut est = DirtyRateEstimator::new();
    est.sample(0, 0); // baseline
    // 1000 dirty pages in 1 second → 1000 pages/sec.
    let rate = est.sample(1000, 1000).unwrap();
    assert!((rate - 1000.0).abs() < f64::EPSILON);
}

#[test]
fn test_rate_estimator_decreasing_rate() {
    let mut est = DirtyRateEstimator::new();
    est.sample(0, 0);
    let rate1 = est.sample(1000, 500).unwrap(); // 500/sec
    let rate2 = est.sample(2000, 100).unwrap(); // 100/sec
    assert!(rate2 < rate1);
    assert!((rate1 - 500.0).abs() < f64::EPSILON);
    assert!((rate2 - 100.0).abs() < f64::EPSILON);
}

#[test]
fn test_rate_estimator_zero_rate() {
    let mut est = DirtyRateEstimator::new();
    est.sample(0, 0);
    let rate = est.sample(1000, 0).unwrap();
    assert!((rate - 0.0).abs() < f64::EPSILON);
}

#[test]
fn test_rate_estimator_reset() {
    let mut est = DirtyRateEstimator::new();
    est.sample(0, 0);
    est.sample(1000, 500);
    est.reset();
    // After reset, next sample is first again → None.
    assert!(est.sample(2000, 200).is_none());
}

// -- DirtyTracker -------------------------------------------------------------

#[test]
fn test_tracker_new() {
    let tracker = DirtyTracker::new(256 * 1024 * 1024, 0);
    assert_eq!(tracker.slot(), 0);
    assert!(tracker.current_rate().is_none());
}

#[test]
fn test_tracker_collect_returns_bitmap() {
    let mut tracker = DirtyTracker::new(4096 * 16, 0);
    let mut bm = DirtyBitmap::new(4096 * 16);
    bm.set_dirty(3);
    bm.set_dirty(10);

    let snap = tracker.collect_from_bitmap(bm, 1000).unwrap();
    assert_eq!(snap.dirty_count, 2);
    assert!(snap.bitmap.is_dirty(3));
    assert!(snap.bitmap.is_dirty(10));
    // First collect: no rate yet.
    assert!(snap.rate.is_none());
}

#[test]
fn test_tracker_collect_computes_rate() {
    let mut tracker = DirtyTracker::new(4096 * 16, 0);

    // First collect.
    let bm1 = DirtyBitmap::new(4096 * 16);
    tracker.collect_from_bitmap(bm1, 0).unwrap();

    // Second collect: 5 dirty pages over 1 second.
    let mut bm2 = DirtyBitmap::new(4096 * 16);
    for i in 0..5 {
        bm2.set_dirty(i);
    }
    let snap = tracker.collect_from_bitmap(bm2, 1000).unwrap();
    assert_eq!(snap.dirty_count, 5);
    let rate = snap.rate.unwrap();
    assert!((rate - 5.0).abs() < f64::EPSILON);
}

// -- Edge cases ---------------------------------------------------------------

#[test]
fn test_bitmap_single_page_memory() {
    let bm = DirtyBitmap::new(4096); // 1 page
    assert_eq!(bm.page_count(), 1);
    assert_eq!(bm.bitmap_size(), 1);
}

#[test]
fn test_bitmap_non_page_aligned_rounds_up() {
    // 5000 bytes → ceil(5000/4096) = 2 pages → 1 byte bitmap.
    let bm = DirtyBitmap::new(5000);
    assert_eq!(bm.page_count(), 2);
    assert_eq!(bm.bitmap_size(), 1);
}
