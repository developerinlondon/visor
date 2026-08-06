use super::*;

#[test]
fn broadcaster_new_with_capacity() {
    let bc = EventBroadcaster::new(64);
    assert_eq!(bc.capacity(), 64);
}

#[test]
fn broadcaster_default_capacity() {
    let bc = EventBroadcaster::new(1024);
    assert_eq!(bc.capacity(), 1024);
}

#[test]
fn broadcaster_send_without_receivers_does_not_panic() {
    let bc = EventBroadcaster::new(16);
    bc.send(VmEvent::new("vm.created", "test-id"));
    // No panic = success — no receivers connected.
}

#[tokio::test]
async fn broadcaster_send_and_receive() {
    let bc = EventBroadcaster::new(16);
    let mut rx = bc.subscribe();

    bc.send(VmEvent::new("vm.created", "vm-123"));

    let event = rx.recv().await.expect("should receive event");
    assert_eq!(event.event_type, "vm.created");
    assert_eq!(event.vm_id, "vm-123");
}

#[tokio::test]
async fn broadcaster_multiple_receivers() {
    let bc = EventBroadcaster::new(16);
    let mut rx1 = bc.subscribe();
    let mut rx2 = bc.subscribe();

    bc.send(VmEvent::new("vm.stopped", "vm-456"));

    let e1 = rx1.recv().await.expect("rx1 should receive");
    let e2 = rx2.recv().await.expect("rx2 should receive");
    assert_eq!(e1.event_type, "vm.stopped");
    assert_eq!(e2.event_type, "vm.stopped");
}

#[test]
fn vm_event_new_sets_fields() {
    let event = VmEvent::new("vm.created", "vm-abc");
    assert_eq!(event.event_type, "vm.created");
    assert_eq!(event.vm_id, "vm-abc");
    assert_eq!(event.timestamp, "1970-01-01T00:00:00Z");
    assert_eq!(event.data, serde_json::Value::Null);
}

#[test]
fn vm_event_with_data() {
    let event =
        VmEvent::new("vm.created", "vm-1").with_data(serde_json::json!({"image": "alpine"}));
    assert_eq!(event.data["image"], "alpine");
}

#[test]
fn vm_event_serialization_round_trip() {
    let event = VmEvent::new("vm.destroyed", "vm-999")
        .with_data(serde_json::json!({"reason": "user request"}));

    let json = serde_json::to_string(&event).expect("serialize");
    let decoded: VmEvent = serde_json::from_str(&json).expect("deserialize");

    assert_eq!(decoded.event_type, "vm.destroyed");
    assert_eq!(decoded.vm_id, "vm-999");
    assert_eq!(decoded.data["reason"], "user request");
}

#[test]
fn vm_event_clone() {
    let event = VmEvent::new("vm.created", "vm-1");
    let cloned = event.clone();
    assert_eq!(cloned.vm_id, event.vm_id);
    assert_eq!(cloned.event_type, event.event_type);
}
