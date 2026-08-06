use super::*;
use std::sync::Arc;
use std::sync::atomic::Ordering;

#[test]
fn mock_trigger_increments_count() {
    let mock = MockInterruptEvent::new();
    assert_eq!(mock.trigger_count.load(Ordering::SeqCst), 0);
    mock.trigger().expect("mock trigger should succeed");
    assert_eq!(mock.trigger_count.load(Ordering::SeqCst), 1);
    mock.trigger().expect("mock trigger should succeed");
    assert_eq!(mock.trigger_count.load(Ordering::SeqCst), 2);
}

#[test]
fn mock_as_raw_returns_sentinel() {
    let mock = MockInterruptEvent::new();
    // Mock should return a sentinel value (e.g. -1) since there's no real OS resource.
    assert_eq!(mock.as_raw(), -1);
}

#[test]
fn mock_implements_send_sync() {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<MockInterruptEvent>();
}

#[test]
fn mock_usable_as_trait_object() {
    let mock = Arc::new(MockInterruptEvent::new());
    let event: Arc<dyn InterruptEvent> = mock.clone();
    event
        .trigger()
        .expect("trait object trigger should succeed");
    assert_eq!(mock.trigger_count.load(Ordering::SeqCst), 1);
}

#[test]
fn raw_event_handle_is_i32_on_unix() {
    // On Linux/macOS, RawEventHandle should be RawFd (i32).
    let handle: RawEventHandle = -1;
    assert_eq!(handle, -1_i32);
}
