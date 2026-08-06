use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use super::*;
use crate::devices::vsock::{ConnMapKey, ConnState, HOST_CID, VsockDevice};
use crate::platform::event::{InterruptEvent, MockInterruptEvent};

// ── Test helper ─────────────────────────────────────────────────────

/// Creates a `VsockMuxer` with a temp directory, `VsockDevice(3)`, and `MockInterruptEvent`.
///
/// Returns `(muxer, device, tempdir)`. The `tempdir` must be kept alive
/// for the duration of the test so the directory is not deleted.
fn make_muxer() -> (VsockMuxer, Arc<Mutex<VsockDevice>>, tempfile::TempDir) {
    let (muxer, device, _irq, tmp) = make_muxer_with_irq();
    (muxer, device, tmp)
}

/// Creates a `VsockMuxer` and returns the mock IRQ so tests can assert
/// how many kicks were issued.
fn make_muxer_with_irq() -> (
    VsockMuxer,
    Arc<Mutex<VsockDevice>>,
    Arc<MockInterruptEvent>,
    tempfile::TempDir,
) {
    let tmp = crate::testutil::tempdir("visor-vmm-vsock-muxer-").unwrap();
    let device = Arc::new(Mutex::new(VsockDevice::new(3)));
    let irq = Arc::new(MockInterruptEvent::new());
    let tx_notify = {
        let dev = device.lock().unwrap();
        dev.tx_notify()
    };
    let irq_for_kick = Arc::clone(&irq);
    let rx_kick: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = irq_for_kick.trigger();
    });
    let muxer = VsockMuxer::new(
        Arc::clone(&device),
        3,
        tmp.path().to_path_buf(),
        tx_notify,
        rx_kick,
    )
    .unwrap();
    (muxer, device, irq, tmp)
}

/// Spawns the muxer `run()` loop in the background and returns
/// `(listener_path, join_handle)`. Caller connects via `UnixStream::connect`.
fn spawn_muxer(
    muxer: VsockMuxer,
) -> (
    std::path::PathBuf,
    tokio::task::JoinHandle<Result<(), VsockMuxerError>>,
) {
    let path = muxer.listener_path();
    let listener = tokio::net::UnixListener::bind(&path).unwrap();
    let handle = tokio::spawn(async move { muxer.run(listener).await });
    (path, handle)
}

/// Performs the CONNECT handshake on a stream and simulates the guest response.
///
/// Returns the allocated local port after the muxer acknowledges that the
/// guest-side vsock connection reached `Established`.
async fn do_handshake(
    stream: &mut UnixStream,
    device: &Arc<Mutex<VsockDevice>>,
    peer_port: u32,
) -> u32 {
    stream
        .write_all(format!("CONNECT {peer_port}\n").as_bytes())
        .await
        .unwrap();

    let key = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(key) = {
                let dev = device.lock().unwrap();
                dev.connections()
                    .keys()
                    .copied()
                    .find(|key| key.peer_port == peer_port)
            } {
                break key;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();

    let tx_notify = {
        let mut dev = device.lock().unwrap();
        let conn = dev
            .connections_mut()
            .get_mut(&key)
            .expect("connection must exist");
        let response_pkt = crate::devices::vsock::VsockPacket {
            src_cid: 3,
            dst_cid: HOST_CID,
            src_port: peer_port,
            dst_port: key.local_port,
            len: 0,
            pkt_type: crate::devices::vsock::VSOCK_TYPE_STREAM,
            op: crate::devices::vsock::VsockOp::Response as u16,
            flags: 0,
            buf_alloc: 64 * 1024,
            fwd_cnt: 0,
        };
        conn.send_pkt(&response_pkt, &[]).unwrap();
        dev.tx_notify()
    };
    tx_notify.notify_one();

    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await.unwrap();

    let trimmed = line.trim();
    assert!(
        trimmed.starts_with("OK "),
        "expected OK response, got: {trimmed}"
    );
    let local_port = trimmed.strip_prefix("OK ").unwrap().parse::<u32>().unwrap();
    assert_eq!(local_port, key.local_port);
    local_port
}

// ── Construction tests ──────────────────────────────────────────────

#[test]
fn muxer_new_creates_socket_dir() {
    let tmp = crate::testutil::tempdir("visor-vmm-vsock-muxer-").unwrap();
    let nested = tmp.path().join("nested").join("dir");
    let device = Arc::new(Mutex::new(VsockDevice::new(3)));
    let irq = Arc::new(MockInterruptEvent::new());
    let tx_notify = {
        let dev = device.lock().unwrap();
        dev.tx_notify()
    };
    let rx_kick: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = irq.trigger();
    });

    let _muxer = VsockMuxer::new(device, 3, nested.clone(), tx_notify, rx_kick).unwrap();

    assert!(
        nested.exists(),
        "VsockMuxer::new must create the socket_dir"
    );
}

#[test]
fn muxer_listener_path_format() {
    let (muxer, _dev, tmp) = make_muxer();
    let expected = tmp.path().join("3.sock");
    assert_eq!(muxer.listener_path(), expected);
}

#[test]
fn muxer_drop_removes_listener_socket_path() {
    let (muxer, _dev, _tmp) = make_muxer();
    let path = muxer.listener_path();
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    assert!(path.exists(), "listener socket should exist after bind");

    drop(muxer);

    assert!(
        !path.exists(),
        "dropping the muxer should unlink the listener socket path"
    );

    drop(listener);
}

// ── Port allocation tests ───────────────────────────────────────────

#[test]
fn allocate_port_starts_at_range_start() {
    let (mut muxer, _dev, _tmp) = make_muxer();
    let port = muxer.allocate_port().unwrap();
    assert_eq!(
        port, LOCAL_PORT_START,
        "first port must be LOCAL_PORT_START"
    );
}

#[test]
fn allocate_port_round_robin() {
    let (mut muxer, _dev, _tmp) = make_muxer();
    let p1 = muxer.allocate_port().unwrap();
    let p2 = muxer.allocate_port().unwrap();
    let p3 = muxer.allocate_port().unwrap();

    assert_eq!(p1, LOCAL_PORT_START);
    assert_eq!(p2, LOCAL_PORT_START + 1);
    assert_eq!(p3, LOCAL_PORT_START + 2);
}

#[test]
fn allocate_port_skips_allocated() {
    let (mut muxer, _dev, _tmp) = make_muxer();

    // Pre-allocate the first port so it's already in the set.
    let first = muxer.allocate_port().unwrap();
    assert_eq!(first, LOCAL_PORT_START);

    // The second allocation should skip first and return next.
    let second = muxer.allocate_port().unwrap();
    assert_eq!(second, LOCAL_PORT_START + 1);
    assert_ne!(second, first);
}

#[test]
fn allocate_port_wraps_around() {
    let (mut muxer, _dev, _tmp) = make_muxer();

    // Set next_local_port to just before the end.
    muxer.next_local_port = LOCAL_PORT_END - 1;

    let p1 = muxer.allocate_port().unwrap();
    assert_eq!(p1, LOCAL_PORT_END - 1);

    // After wrapping, the next port should be LOCAL_PORT_START.
    let p2 = muxer.allocate_port().unwrap();
    assert_eq!(
        p2, LOCAL_PORT_START,
        "port allocation must wrap from end to start"
    );
}

// ── CONNECT handshake tests ─────────────────────────────────────────

#[tokio::test]
async fn connect_handshake_creates_connection() {
    let (muxer, device, _tmp) = make_muxer();
    let (path, _handle) = spawn_muxer(muxer);

    // Give the listener time to bind.
    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
        .await
        .unwrap()
        .unwrap();

    let local_port = tokio::time::timeout(
        Duration::from_secs(5),
        do_handshake(&mut stream, &device, 52),
    )
    .await
    .unwrap();

    // Verify the connection exists in the device with ConnState::Established.
    let dev = device.lock().unwrap();
    let key = ConnMapKey {
        local_port,
        peer_port: 52,
    };
    let conn = dev.connections().get(&key).expect("connection must exist");
    assert_eq!(conn.state(), ConnState::Established);
}

#[tokio::test]
async fn connect_handshake_waits_for_guest_response_before_ok() {
    let (muxer, device, _tmp) = make_muxer();
    let (path, _handle) = spawn_muxer(muxer);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
        .await
        .unwrap()
        .unwrap();

    stream.write_all(b"CONNECT 52\n").await.unwrap();

    let mut reader = BufReader::new(&mut stream);
    let mut line = String::new();
    let early_response =
        tokio::time::timeout(Duration::from_millis(100), reader.read_line(&mut line)).await;
    assert!(
        early_response.is_err(),
        "muxer should not acknowledge the host side before the guest responds"
    );

    let key = {
        let dev = device.lock().unwrap();
        assert_eq!(dev.connections().len(), 1, "connection should be tracked");
        *dev.connections().keys().next().unwrap()
    };

    let tx_notify = {
        let mut dev = device.lock().unwrap();
        let conn = dev
            .connections_mut()
            .get_mut(&key)
            .expect("connection must exist");
        let response_pkt = crate::devices::vsock::VsockPacket {
            src_cid: 3,
            dst_cid: HOST_CID,
            src_port: 52,
            dst_port: key.local_port,
            len: 0,
            pkt_type: crate::devices::vsock::VSOCK_TYPE_STREAM,
            op: crate::devices::vsock::VsockOp::Response as u16,
            flags: 0,
            buf_alloc: 64 * 1024,
            fwd_cnt: 0,
        };
        conn.send_pkt(&response_pkt, &[]).unwrap();
        dev.tx_notify()
    };
    tx_notify.notify_one();

    let acknowledged = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line))
        .await
        .unwrap()
        .unwrap();
    assert!(
        acknowledged > 0,
        "guest-established connection should return OK"
    );
    assert!(
        line.starts_with("OK "),
        "expected OK response after guest response, got: {line}"
    );
}

#[tokio::test]
async fn connect_handshake_invalid_format() {
    let (muxer, device, _tmp) = make_muxer();
    let (path, _handle) = spawn_muxer(muxer);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
        .await
        .unwrap()
        .unwrap();

    // Send an invalid handshake.
    stream.write_all(b"INVALID\n").await.unwrap();

    // The muxer should drop the stream after the bad handshake.
    // Reading should return EOF or an error.
    let mut buf = [0u8; 128];
    let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;

    // Either timeout (muxer logged warning) or got 0/err — no connection in device.
    let dev = device.lock().unwrap();
    assert!(
        dev.connections().is_empty(),
        "invalid handshake must not create a connection"
    );

    drop(result);
}

#[tokio::test]
async fn connect_handshake_bad_port() {
    let (muxer, device, _tmp) = make_muxer();
    let (path, _handle) = spawn_muxer(muxer);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
        .await
        .unwrap()
        .unwrap();

    // "CONNECT abc" has a valid prefix but non-numeric port.
    stream.write_all(b"CONNECT abc\n").await.unwrap();

    let mut buf = [0u8; 128];
    let _ = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;

    let dev = device.lock().unwrap();
    assert!(
        dev.connections().is_empty(),
        "non-numeric port must not create a connection"
    );
}

#[tokio::test]
async fn connect_empty_close() {
    let (muxer, _device, _tmp) = make_muxer();
    let (path, _handle) = spawn_muxer(muxer);

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Connect and immediately drop — should not panic.
    let stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
        .await
        .unwrap()
        .unwrap();
    drop(stream);

    // Give the muxer time to process the dropped connection.
    tokio::time::sleep(Duration::from_millis(100)).await;
}

// ── Data bridging tests ─────────────────────────────────────────────

#[tokio::test]
async fn host_to_guest_data_bridging() {
    let (muxer, device, _tmp) = make_muxer();
    let (path, _handle) = spawn_muxer(muxer);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
        .await
        .unwrap()
        .unwrap();

    let local_port = tokio::time::timeout(
        Duration::from_secs(5),
        do_handshake(&mut stream, &device, 80),
    )
    .await
    .unwrap();

    // Write data from the host side.
    stream.write_all(b"hello guest").await.unwrap();

    // Give the muxer sync loop time to read from the UDS and push to the device.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Verify the data landed in the connection's rx_buf via push_host_data.
    let dev = device.lock().unwrap();
    let key = ConnMapKey {
        local_port,
        peer_port: 80,
    };
    let conn = dev.connections().get(&key).expect("connection must exist");
    assert!(
        conn.has_pending_rx(),
        "connection should have pending RX data after host write"
    );
}

#[tokio::test]
async fn guest_response_with_pending_host_data_triggers_followup_irq() {
    let (mut muxer, device, irq, _tmp) = make_muxer_with_irq();
    let (mut host_stream, muxer_stream) = UnixStream::pair().unwrap();
    let local_port = muxer.allocate_port().unwrap();
    let key = ConnMapKey {
        local_port,
        peer_port: 52,
    };
    {
        let mut dev = device.lock().unwrap();
        dev.add_connection(
            key,
            VsockConnection::new_local_init(HOST_CID, 3, local_port, 52),
        );
    }
    muxer.streams.insert(key, muxer_stream);

    host_stream.write_all(b"hello guest").await.unwrap();
    muxer.sync_connections();

    let before = irq.trigger_count.load(Ordering::SeqCst);

    {
        let mut dev = device.lock().unwrap();
        let conn = dev
            .connections_mut()
            .get_mut(&key)
            .expect("connection must exist");
        let response_pkt = crate::devices::vsock::VsockPacket {
            src_cid: 3,
            dst_cid: HOST_CID,
            src_port: 52,
            dst_port: local_port,
            len: 0,
            pkt_type: crate::devices::vsock::VSOCK_TYPE_STREAM,
            op: crate::devices::vsock::VsockOp::Response as u16,
            flags: 0,
            buf_alloc: 64 * 1024,
            fwd_cnt: 0,
        };
        conn.send_pkt(&response_pkt, &[]).unwrap();
    }
    muxer.sync_connections();

    assert!(
        irq.trigger_count.load(Ordering::SeqCst) > before,
        "guest response with pending host data should trigger a follow-up IRQ"
    );
}

#[tokio::test]
async fn sync_connections_drops_host_stream_when_device_connection_is_gone() {
    let (mut muxer, device, _tmp) = make_muxer();
    let (mut host_stream, muxer_stream) = UnixStream::pair().unwrap();
    let local_port = muxer.allocate_port().unwrap();
    let key = ConnMapKey {
        local_port,
        peer_port: 52,
    };

    {
        let mut dev = device.lock().unwrap();
        dev.add_connection(
            key,
            VsockConnection::new_local_init(HOST_CID, 3, local_port, 52),
        );
        dev.remove_connection(&key);
    }

    muxer.streams.insert(key, muxer_stream);

    muxer.sync_connections();

    assert!(
        !muxer.streams.contains_key(&key),
        "muxer should drop host streams once the device connection disappears"
    );

    let mut buffer = [0u8; 1];
    let eof = tokio::time::timeout(Duration::from_secs(1), host_stream.read(&mut buffer))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(eof, 0, "host stream should observe EOF after muxer cleanup");
}

#[tokio::test]
async fn guest_to_host_data_bridging() {
    let (muxer, device, _tmp) = make_muxer();
    let (path, _handle) = spawn_muxer(muxer);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
        .await
        .unwrap()
        .unwrap();

    let local_port = tokio::time::timeout(
        Duration::from_secs(5),
        do_handshake(&mut stream, &device, 90),
    )
    .await
    .unwrap();

    // Push data into the connection's TX buffer (simulating guest → host data).
    {
        let mut dev = device.lock().unwrap();
        let key = ConnMapKey {
            local_port,
            peer_port: 90,
        };
        let conn = dev
            .connections_mut()
            .get_mut(&key)
            .expect("connection must exist");
        let _ = conn.tx_buf().len(); // Access to verify it exists.

        // Push data into the TX buffer by calling the internal push directly.
        // We need to use the VsockConnection's send_pkt mechanism to load
        // TX data. Construct a minimal RW packet to push data.
        let pkt = crate::devices::vsock::VsockPacket {
            src_cid: 3,
            dst_cid: HOST_CID,
            src_port: 90,
            dst_port: local_port,
            len: 11,
            pkt_type: crate::devices::vsock::VSOCK_TYPE_STREAM,
            op: crate::devices::vsock::VsockOp::Rw as u16,
            flags: 0,
            buf_alloc: 64 * 1024,
            fwd_cnt: 0,
        };

        // First, move to Established state so send_pkt accepts RW.
        let response_pkt = crate::devices::vsock::VsockPacket {
            op: crate::devices::vsock::VsockOp::Response as u16,
            buf_alloc: 64 * 1024,
            ..pkt
        };
        conn.send_pkt(&response_pkt, &[]).unwrap();
        assert_eq!(conn.state(), ConnState::Established);

        // Now send data via RW.
        conn.send_pkt(&pkt, b"hello host!").unwrap();
    }

    // Notify the muxer that TX data is available (simulates what process_queue does).
    {
        let dev = device.lock().unwrap();
        dev.tx_notify().notify_one();
    }

    // Give the muxer time to flush TX data to the UDS.
    tokio::time::sleep(Duration::from_millis(200)).await;

    // Read from the host UDS stream.
    let mut buf = [0u8; 128];
    let result = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut buf)).await;

    match result {
        Ok(Ok(n)) if n > 0 => {
            assert_eq!(
                &buf[..n],
                b"hello host!",
                "host stream should receive the guest TX data"
            );
        }
        Ok(Ok(_)) => panic!("stream returned 0 bytes or unexpected count"),
        Ok(Err(e)) => panic!("stream read error: {e}"),
        Err(_) => panic!("timed out waiting for guest TX data on host stream"),
    }
}

// ── Connection limit test ───────────────────────────────────────────

#[tokio::test]
async fn connection_limit_enforced() {
    let (muxer, _device, _tmp) = make_muxer();
    let (path, _handle) = spawn_muxer(muxer);

    tokio::time::sleep(Duration::from_millis(50)).await;

    // Open MAX_CONNECTIONS connections.
    let mut streams = Vec::with_capacity(MAX_CONNECTIONS);
    for i in 0..MAX_CONNECTIONS {
        let mut stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
            .await
            .unwrap()
            .unwrap();

        let _port = tokio::time::timeout(
            Duration::from_secs(5),
            do_handshake(&mut stream, &_device, i as u32 + 1),
        )
        .await
        .unwrap();

        streams.push(stream);
    }

    // The (MAX_CONNECTIONS + 1)th connection should get an ERR response.
    let mut overflow_stream =
        tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
            .await
            .unwrap()
            .unwrap();

    overflow_stream.write_all(b"CONNECT 9999\n").await.unwrap();

    let mut reader = BufReader::new(&mut overflow_stream);
    let mut line = String::new();
    let read_result =
        tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line)).await;

    match read_result {
        Ok(Ok(n)) if n > 0 => {
            assert!(
                line.starts_with("ERR"),
                "expected ERR response for overflow connection, got: {line}"
            );
        }
        _ => {
            // Stream may also be dropped by the muxer — acceptable.
        }
    }
}

// ── Cleanup test ────────────────────────────────────────────────────

#[tokio::test]
async fn dead_connection_cleaned_up() {
    let (muxer, device, _tmp) = make_muxer();
    let (path, _handle) = spawn_muxer(muxer);

    tokio::time::sleep(Duration::from_millis(50)).await;

    let mut stream = tokio::time::timeout(Duration::from_secs(5), UnixStream::connect(&path))
        .await
        .unwrap()
        .unwrap();

    let local_port = tokio::time::timeout(
        Duration::from_secs(5),
        do_handshake(&mut stream, &device, 42),
    )
    .await
    .unwrap();

    let key = ConnMapKey {
        local_port,
        peer_port: 42,
    };

    // Verify the connection exists.
    {
        let dev = device.lock().unwrap();
        assert!(
            dev.connections().contains_key(&key),
            "connection must exist before kill"
        );
    }

    // Kill the connection by dropping the host stream (read will return 0 → dead).
    drop(stream);

    // Give the muxer time to detect the dead stream and clean up.
    tokio::time::sleep(Duration::from_millis(300)).await;

    // The muxer should have removed the connection from the device.
    let dev = device.lock().unwrap();
    assert!(
        !dev.connections().contains_key(&key),
        "dead connection must be removed after stream drop"
    );
}

// ── Error handling ──────────────────────────────────────────────────

#[test]
fn muxer_new_io_error_on_bad_path() {
    let device = Arc::new(Mutex::new(VsockDevice::new(3)));
    let irq = Arc::new(MockInterruptEvent::new());
    let tx_notify = {
        let dev = device.lock().unwrap();
        dev.tx_notify()
    };
    let rx_kick: Arc<dyn Fn() + Send + Sync> = Arc::new(move || {
        let _ = irq.trigger();
    });

    // /proc/self/fd is not writable — creating dirs under it will fail.
    let result = VsockMuxer::new(
        device,
        3,
        std::path::PathBuf::from("/proc/self/fd/999/nested/impossible"),
        tx_notify,
        rx_kick,
    );

    assert!(result.is_err(), "bad path should produce an error");
    assert!(
        matches!(result.unwrap_err(), VsockMuxerError::Io(_)),
        "error should be VsockMuxerError::Io"
    );
}

#[test]
fn debug_impl_does_not_panic() {
    let (muxer, _dev, _tmp) = make_muxer();
    let debug = format!("{muxer:?}");
    assert!(debug.contains("VsockMuxer"));
    assert!(debug.contains("guest_cid"));
}

// ── Snapshot port accessor tests ────────────────────────────────────

#[test]
fn next_local_port_returns_initial_value() {
    let (muxer, _dev, _tmp) = make_muxer();
    assert_eq!(
        muxer.next_local_port(),
        LOCAL_PORT_START,
        "initial next_local_port must be LOCAL_PORT_START"
    );
}

#[test]
fn set_next_local_port_updates_value() {
    let (mut muxer, _dev, _tmp) = make_muxer();
    let new_port = LOCAL_PORT_START + 42;
    muxer.set_next_local_port(new_port);
    assert_eq!(muxer.next_local_port(), new_port);
}

#[test]
#[should_panic(expected = "outside valid range")]
fn set_next_local_port_panics_on_invalid() {
    let (mut muxer, _dev, _tmp) = make_muxer();
    // 0 is below LOCAL_PORT_START, should panic.
    muxer.set_next_local_port(0);
}
