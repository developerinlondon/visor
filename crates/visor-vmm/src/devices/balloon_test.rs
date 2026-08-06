use super::*;
use crate::transport::{DeviceType, VirtioDevice};

// ── Construction tests ─────────────────────────────────────────────────

#[test]
fn new_balloon_has_correct_device_type() {
    let balloon = BalloonDevice::new(0);
    assert_eq!(balloon.device_type(), DeviceType::Balloon);
}

#[test]
fn new_balloon_starts_with_zero_actual_pages() {
    let balloon = BalloonDevice::new(100);
    assert_eq!(balloon.actual_pages(), 0);
}

#[test]
fn new_balloon_stores_initial_target() {
    let balloon = BalloonDevice::new(256);
    assert_eq!(balloon.target_pages(), 256);
}

#[test]
fn new_balloon_has_two_queues() {
    let balloon = BalloonDevice::new(0);
    assert_eq!(
        balloon.queues().len(),
        2,
        "balloon needs inflate + deflate queues"
    );
}

#[test]
fn new_balloon_starts_not_activated() {
    let balloon = BalloonDevice::new(0);
    assert!(!balloon.is_activated());
}

#[test]
fn new_balloon_has_version_1_feature() {
    let balloon = BalloonDevice::new(0);
    assert_ne!(
        balloon.avail_features() & VIRTIO_F_VERSION_1,
        0,
        "should offer VIRTIO_F_VERSION_1"
    );
}

// ── Target pages tests ─────────────────────────────────────────────────

#[test]
fn set_target_pages_updates_value() {
    let balloon = BalloonDevice::new(0);
    balloon.set_target_pages(512);
    assert_eq!(balloon.target_pages(), 512);
}

#[test]
fn set_target_pages_overwrites_previous() {
    let balloon = BalloonDevice::new(100);
    balloon.set_target_pages(200);
    assert_eq!(balloon.target_pages(), 200);
}

// ── Reclaimed bytes test ───────────────────────────────────────────────

#[test]
fn reclaimed_bytes_is_zero_initially() {
    let balloon = BalloonDevice::new(0);
    assert_eq!(balloon.reclaimed_bytes(), 0);
}

// ── Lifetime counters ──────────────────────────────────────────────────

#[test]
fn lifetime_counters_start_at_zero() {
    let balloon = BalloonDevice::new(0);
    assert_eq!(balloon.total_inflated(), 0);
    assert_eq!(balloon.total_deflated(), 0);
}

// ── Feature negotiation tests ──────────────────────────────────────────

#[test]
fn set_acked_features_masks_to_available() {
    let mut balloon = BalloonDevice::new(0);
    // Try to ack features we don't offer
    balloon.set_acked_features(0xFFFF_FFFF_FFFF_FFFF);
    // Should only have the features we actually offer
    assert_eq!(balloon.acked_features(), balloon.avail_features());
}

#[test]
fn set_acked_features_with_subset() {
    let mut balloon = BalloonDevice::new(0);
    balloon.set_acked_features(VIRTIO_F_VERSION_1);
    assert_eq!(balloon.acked_features(), VIRTIO_F_VERSION_1);
}

// ── Config space tests ─────────────────────────────────────────────────

#[test]
fn read_config_returns_target_at_offset_0() {
    let balloon = BalloonDevice::new(42);
    let mut data = [0u8; 4];
    balloon.read_config(0, &mut data);
    let target = u32::from_le_bytes(data);
    assert_eq!(target, 42);
}

#[test]
fn read_config_returns_actual_at_offset_4() {
    let balloon = BalloonDevice::new(0);
    let mut data = [0u8; 4];
    balloon.read_config(4, &mut data);
    let actual = u32::from_le_bytes(data);
    assert_eq!(actual, 0);
}

#[test]
fn read_full_config_space() {
    let balloon = BalloonDevice::new(100);
    let mut data = [0u8; 8];
    balloon.read_config(0, &mut data);
    let target = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    let actual = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    assert_eq!(target, 100);
    assert_eq!(actual, 0);
}

#[test]
fn write_config_updates_actual_at_offset_4() {
    let mut balloon = BalloonDevice::new(0);
    let actual_bytes = 50_u32.to_le_bytes();
    balloon.write_config(4, &actual_bytes);
    assert_eq!(balloon.actual_pages(), 50);
}

#[test]
fn write_config_at_offset_0_is_ignored() {
    let mut balloon = BalloonDevice::new(42);
    let bogus = 999_u32.to_le_bytes();
    balloon.write_config(0, &bogus);
    // target should be unchanged (host-controlled, not guest-writable)
    assert_eq!(balloon.target_pages(), 42);
}

#[test]
fn config_reflects_set_target() {
    let balloon = BalloonDevice::new(0);
    balloon.set_target_pages(1024);
    let mut data = [0u8; 4];
    balloon.read_config(0, &mut data);
    assert_eq!(u32::from_le_bytes(data), 1024);
}

// ── Activation tests ───────────────────────────────────────────────────

#[test]
fn activate_succeeds() {
    let mut balloon = BalloonDevice::new(0);
    assert!(balloon.activate().is_ok());
    assert!(balloon.is_activated());
}

#[test]
fn reset_deactivates_device() {
    let mut balloon = BalloonDevice::new(0);
    balloon.activate().unwrap();
    balloon.reset();
    assert!(!balloon.is_activated());
}

#[test]
fn reset_clears_acked_features() {
    let mut balloon = BalloonDevice::new(0);
    balloon.set_acked_features(VIRTIO_F_VERSION_1);
    balloon.reset();
    assert_eq!(balloon.acked_features(), 0);
}

#[test]
fn reset_resets_queues() {
    let mut balloon = BalloonDevice::new(0);
    balloon.queues_mut()[0].ready = true;
    balloon.queues_mut()[0].size = 64;
    balloon.reset();
    assert!(!balloon.queues()[0].ready);
    assert_eq!(balloon.queues()[0].size, 0);
}

// ── Queue configuration tests ──────────────────────────────────────────

#[test]
fn queue_max_size_is_128() {
    let balloon = BalloonDevice::new(0);
    for queue in balloon.queues() {
        assert_eq!(queue.max_size, 128);
    }
}

#[test]
fn queues_mut_allows_configuration() {
    let mut balloon = BalloonDevice::new(0);
    balloon.queues_mut()[0].size = 64;
    balloon.queues_mut()[0].ready = true;
    assert_eq!(balloon.queues()[0].size, 64);
    assert!(balloon.queues()[0].ready);
}

// ── Process queue with no ready queues ─────────────────────────────────

#[test]
fn process_queue_on_unready_returns_false() {
    let mut balloon = BalloonDevice::new(0);
    let memory = GuestMemory::new(1024 * 1024, 0).unwrap();
    let result = balloon.process_queue(0, &memory);
    assert!(result.is_ok());
    assert!(!result.unwrap());
}

#[test]
fn process_queue_invalid_index_returns_false() {
    let mut balloon = BalloonDevice::new(0);
    let memory = GuestMemory::new(1024 * 1024, 0).unwrap();
    let result = balloon.process_queue(99, &memory);
    assert!(result.is_ok());
    assert!(!result.unwrap());
}

// ── Error display tests ────────────────────────────────────────────────

#[test]
fn balloon_error_display_is_readable() {
    let err = BalloonError::Memory(crate::memory::MemoryError::OutOfBounds {
        addr: 0x1000,
        size: 0x100,
    });
    let msg = format!("{err}");
    assert!(
        msg.contains("memory access error"),
        "should mention memory: {msg}"
    );
}

// ── Config space boundary tests ────────────────────────────────────────

#[test]
fn read_config_past_end_returns_zeros() {
    let balloon = BalloonDevice::new(42);
    let mut data = [0xFFu8; 4];
    balloon.read_config(8, &mut data);
    assert_eq!(data, [0, 0, 0, 0]);
}

#[test]
fn read_config_partial_overlap() {
    let balloon = BalloonDevice::new(42);
    let mut data = [0xFFu8; 4];
    // Read starting at offset 6 — overlaps last 2 bytes of config
    balloon.read_config(6, &mut data);
    // Bytes 0-1 come from config[6..8] (actual field bytes 2-3 = 0,0)
    // Bytes 2-3 are past config end = 0
    assert_eq!(data, [0, 0, 0, 0]);
}
