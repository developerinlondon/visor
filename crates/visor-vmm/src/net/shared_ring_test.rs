use super::*;
use crate::shared_memory::{unlink_shared_memory, SharedMemoryRegion};
use std::thread;

/// Create a shared memory region, unlinking any stale segment first.
fn create_test_shm(name: &str, size: usize) -> SharedMemoryRegion {
    let _ = unlink_shared_memory(name);
    SharedMemoryRegion::create(name, size).expect("failed to create shm")
}

#[test]
fn header_size_is_192_bytes() {
    // 3 cache lines (64 bytes each) = 192 bytes
    assert_eq!(HEADER_SIZE, 192);
}

#[test]
fn send_recv_single_packet() {
    let shm = create_test_shm("/visor-test-single", DEFAULT_CAPACITY);

    let (producer, consumer) = create_pair(&shm).expect("failed to create pair");

    let packet = vec![0x42u8; 64];
    assert!(producer.try_send(&packet), "send should succeed");

    let mut buf = vec![0u8; 64];
    let len = consumer.try_recv(&mut buf).expect("recv should succeed");
    assert_eq!(len, 64);
    assert_eq!(&buf[..len], &packet[..]);

    shm.unlink().ok();
}

#[test]
fn send_recv_multiple() {
    let shm = create_test_shm("/visor-test-multiple", DEFAULT_CAPACITY);

    let (producer, consumer) = create_pair(&shm).expect("failed to create pair");

    // Send 10 packets of varying sizes
    let packets: Vec<Vec<u8>> = (0..10).map(|i| vec![i as u8; 100 + i * 10]).collect();

    for packet in &packets {
        assert!(producer.try_send(packet), "send should succeed for packet");
    }

    // Receive all packets
    let mut buf = vec![0u8; 2048];
    for (i, expected) in packets.iter().enumerate() {
        let len = consumer.try_recv(&mut buf).expect("recv should succeed");
        assert_eq!(len, expected.len(), "packet {} size mismatch", i);
        assert_eq!(&buf[..len], &expected[..], "packet {} content mismatch", i);
    }

    // Ring should be empty now
    assert!(
        consumer.try_recv(&mut buf).is_none(),
        "ring should be empty"
    );

    shm.unlink().ok();
}

#[test]
fn ring_full_returns_false() {
    let shm = create_test_shm("/visor-test-full", 128 * 1024);

    let (producer, _consumer) = create_pair(&shm).expect("failed to create pair");

    // Fill the ring
    let packet = vec![0x42u8; 1024];
    let mut count = 0;
    while producer.try_send(&packet) {
        count += 1;
    }

    assert!(count > 0, "should have sent at least one packet");
    assert!(
        !producer.try_send(&packet),
        "send should fail when ring is full"
    );

    shm.unlink().ok();
}

#[test]
fn empty_ring_returns_none() {
    let shm = create_test_shm("/visor-test-empty", DEFAULT_CAPACITY);

    let (_producer, consumer) = create_pair(&shm).expect("failed to create pair");

    let mut buf = vec![0u8; 1024];
    assert!(
        consumer.try_recv(&mut buf).is_none(),
        "recv on empty ring should return None"
    );

    shm.unlink().ok();
}

#[test]
fn wraparound() {
    let shm = create_test_shm("/visor-test-wraparound", 128 * 1024);

    let (producer, consumer) = create_pair(&shm).expect("failed to create pair");

    let packet = vec![0x42u8; 512];

    // Fill, drain, repeat 3 times to test wraparound
    for round in 0..3 {
        let mut count = 0;
        while producer.try_send(&packet) {
            count += 1;
        }

        assert!(count > 0, "round {}: should have sent packets", round);

        let mut buf = vec![0u8; 512];
        let mut recv_count = 0;
        while let Some(len) = consumer.try_recv(&mut buf) {
            assert_eq!(len, 512, "round {}: packet size mismatch", round);
            recv_count += 1;
        }

        assert_eq!(
            count, recv_count,
            "round {}: sent and received counts should match",
            round
        );
    }

    shm.unlink().ok();
}

#[test]
fn variable_size_packets() {
    let shm = create_test_shm("/visor-test-variable", DEFAULT_CAPACITY);

    let (producer, consumer) = create_pair(&shm).expect("failed to create pair");

    // Mix small (14 bytes) and large (1514 bytes) packets
    let small = vec![0x01u8; 14];
    let large = vec![0x02u8; 1514];

    assert!(producer.try_send(&small), "send small packet");
    assert!(producer.try_send(&large), "send large packet");
    assert!(producer.try_send(&small), "send small packet");

    let mut buf = vec![0u8; 2048];

    let len = consumer.try_recv(&mut buf).expect("recv 1");
    assert_eq!(len, 14);
    assert_eq!(&buf[..len], &small[..]);

    let len = consumer.try_recv(&mut buf).expect("recv 2");
    assert_eq!(len, 1514);
    assert_eq!(&buf[..len], &large[..]);

    let len = consumer.try_recv(&mut buf).expect("recv 3");
    assert_eq!(len, 14);
    assert_eq!(&buf[..len], &small[..]);

    shm.unlink().ok();
}

#[test]
fn max_packet_size() {
    let shm = create_test_shm("/visor-test-max", DEFAULT_CAPACITY);

    let (producer, consumer) = create_pair(&shm).expect("failed to create pair");

    let jumbo = vec![0x42u8; MAX_PACKET_SIZE];
    assert!(producer.try_send(&jumbo), "send jumbo frame");

    let mut buf = vec![0u8; MAX_PACKET_SIZE + 100];
    let len = consumer.try_recv(&mut buf).expect("recv jumbo");
    assert_eq!(len, MAX_PACKET_SIZE);
    assert_eq!(&buf[..len], &jumbo[..]);

    shm.unlink().ok();
}

#[test]
fn concurrent_producer_consumer() {
    let shm = create_test_shm("/visor-test-concurrent", DEFAULT_CAPACITY);

    let (producer, consumer) = create_pair(&shm).expect("failed to create pair");

    let producer_thread = thread::spawn(move || {
        for i in 0..10000 {
            let packet = vec![(i % 256) as u8; 64];
            while !producer.try_send(&packet) {
                thread::yield_now();
            }
        }
    });

    let consumer_thread = thread::spawn(move || {
        let mut buf = vec![0u8; 1024];
        let mut count = 0;
        while count < 10000 {
            if let Some(len) = consumer.try_recv(&mut buf) {
                assert_eq!(len, 64, "packet size mismatch");
                count += 1;
            } else {
                thread::yield_now();
            }
        }
        count
    });

    producer_thread.join().expect("producer thread panicked");
    let recv_count = consumer_thread.join().expect("consumer thread panicked");

    assert_eq!(recv_count, 10000, "should have received all packets");

    shm.unlink().ok();
}

#[test]
fn zero_length_packet() {
    let shm = create_test_shm("/visor-test-zero", DEFAULT_CAPACITY);

    let (producer, consumer) = create_pair(&shm).expect("failed to create pair");

    let empty = vec![];
    assert!(producer.try_send(&empty), "send zero-length packet");

    let mut buf = vec![0u8; 1024];
    let len = consumer.try_recv(&mut buf).expect("recv zero-length");
    assert_eq!(len, 0);

    shm.unlink().ok();
}
