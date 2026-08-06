//! Vsock muxer — bridges `VsockDevice` connections to host-side Unix domain sockets.
//!
//! The muxer runs as a separate async task (not in the `process_queue` hot path).
//! Host processes connect via a Unix listener, perform a `CONNECT {port}\n` handshake,
//! and are then bridged to the guest vsock connection state machine.
//!
//! # Data flow
//!
//! ```text
//! Host UDS ──► VsockMuxer ──► VsockConnection.push_host_data() ──► RX queue ──► Guest
//! Guest   ──► TX queue    ──► VsockConnection.flush_tx_buf()   ──► VsockMuxer ──► Host UDS
//! ```
//!
//! # Lock discipline
//!
//! The `VsockDevice` mutex is held for the **shortest** possible time.
//! All Unix socket I/O happens outside the lock. The sync loop is:
//!
//! 1. **Read phase** — `try_read()` from each Unix stream (no lock).
//! 2. **Device lock phase** — push host data, drain TX buffers, expire connections.
//! 3. **Write phase** — `try_write()` to each Unix stream (no lock).
//! 4. **Cleanup phase** — remove dead streams, deallocate ports.
//! 5. **RX kick phase** — ask the transport to deliver pending host data.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Notify;

use crate::devices::vsock::{ConnMapKey, ConnState, HOST_CID, VsockConnection, VsockDevice};

// ── Constants ────────────────────────────────────────────────────────

/// Start of the local port allocation range (high range to avoid guest conflicts).
const LOCAL_PORT_START: u32 = 1 << 30;

/// End of the local port allocation range (exclusive).
const LOCAL_PORT_END: u32 = 1 << 31;

/// Maximum number of concurrent connections.
const MAX_CONNECTIONS: usize = 1024;

/// Timeout for the initial CONNECT handshake on a new host connection.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);

/// Timeout for the guest to accept a host-initiated vsock connection.
const CONNECT_ESTABLISH_TIMEOUT: Duration = Duration::from_secs(5);

/// Per-stream read buffer size.
const STREAM_BUF_SIZE: usize = 4096;

// ── VsockMuxerError ──────────────────────────────────────────────────

/// Errors from vsock muxer operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VsockMuxerError {
    /// I/O error from socket or filesystem operations.
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// The CONNECT handshake from a host client was malformed or invalid.
    #[error("invalid CONNECT handshake: {0}")]
    InvalidHandshake(String),

    /// All ports in the allocation range have been exhausted.
    #[error("port allocation exhausted")]
    PortExhausted,
}

// ── VsockMuxer ───────────────────────────────────────────────────────

/// Bridges [`VsockDevice`] connections to host-side Unix domain sockets.
///
/// Each host process that wants to communicate with the guest connects to
/// the muxer's Unix listener, performs a `CONNECT {port}\n` handshake, and
/// is then bridged to a [`VsockConnection`] inside the device.
///
/// The muxer owns the mapping between `ConnMapKey` and `tokio::net::UnixStream`,
/// shuttling data between the two in a non-blocking async loop.
pub struct VsockMuxer {
    /// Shared reference to the vsock device (locked only during state updates).
    device: Arc<Mutex<VsockDevice>>,

    /// Guest context ID for this VM.
    guest_cid: u64,

    /// Directory where the Unix listener socket is created.
    socket_dir: PathBuf,

    /// Active host-side Unix streams, keyed by connection port pair.
    streams: HashMap<ConnMapKey, tokio::net::UnixStream>,

    /// Next local port to try when allocating a new host-initiated connection.
    next_local_port: u32,

    /// Set of currently allocated local ports.
    allocated_ports: HashSet<u32>,

    /// Notification channel — poked by `VsockDevice::process_queue()` when guest
    /// produces TX data. Wakes the muxer's event loop to drain TX buffers.
    tx_notify: Arc<Notify>,

    /// Callback invoked when host-side data should be pushed into the guest RX queue.
    rx_kick: Arc<dyn Fn() + Send + Sync>,
}

impl VsockMuxer {
    /// Creates a new vsock muxer for the given device and guest CID.
    ///
    /// The `socket_dir` directory is created if it does not exist. The muxer's
    /// Unix listener socket will be placed at `{socket_dir}/{cid}.sock`.
    ///
    /// # Errors
    ///
    /// Returns [`VsockMuxerError::Io`] if `socket_dir` cannot be created.
    pub fn new(
        device: Arc<Mutex<VsockDevice>>,
        guest_cid: u64,
        socket_dir: PathBuf,
        tx_notify: Arc<Notify>,
        rx_kick: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<Self, VsockMuxerError> {
        std::fs::create_dir_all(&socket_dir)?;

        Ok(Self {
            device,
            guest_cid,
            socket_dir,
            streams: HashMap::new(),
            next_local_port: LOCAL_PORT_START,
            allocated_ports: HashSet::new(),
            tx_notify,
            rx_kick,
        })
    }

    /// Returns the path where the Unix listener socket should be bound.
    ///
    /// The path is `{socket_dir}/{guest_cid}.sock`.
    #[must_use]
    pub fn listener_path(&self) -> PathBuf {
        self.socket_dir.join(format!("{}.sock", self.guest_cid))
    }

    /// Returns the current next-local-port counter.
    ///
    /// Used by snapshot to persist allocator state and avoid port collision on restore.
    #[must_use]
    pub fn next_local_port(&self) -> u32 {
        self.next_local_port
    }

    /// Sets the next-local-port counter (for snapshot restore).
    ///
    /// # Panics
    ///
    /// Panics if `port` is outside the valid range `[LOCAL_PORT_START, LOCAL_PORT_END)`.
    pub fn set_next_local_port(&mut self, port: u32) {
        assert!(
            (LOCAL_PORT_START..LOCAL_PORT_END).contains(&port),
            "port {port} outside valid range [{LOCAL_PORT_START}, {LOCAL_PORT_END})"
        );
        self.next_local_port = port;
    }

    /// Runs the muxer's event-driven async loop until the listener is closed.
    ///
    /// The loop wakes on exactly three events:
    /// 1. **New connection** — `listener.accept()` completes.
    /// 2. **Host stream readable** — any active stream has data (or EOF).
    /// 3. **Guest TX notify** — `VsockDevice::process_queue()` produced TX data.
    ///
    /// This method consumes `self` and runs indefinitely. It is intended to be
    /// spawned as a `tokio::spawn` task.
    ///
    /// # Errors
    ///
    /// Returns [`VsockMuxerError`] only on fatal listener errors. Individual
    /// connection errors are logged and the connection is cleaned up.
    pub async fn run(mut self, listener: UnixListener) -> Result<(), VsockMuxerError> {
        loop {
            if self.streams.is_empty() {
                // No active streams — only wait for accept or TX notify.
                tokio::select! {
                    accept_result = listener.accept() => {
                        self.handle_accept(accept_result).await;
                    }
                    () = self.tx_notify.notified() => {
                        self.sync_connections();
                    }
                }
            } else {
                // Active streams present — also wake on stream readability.
                tokio::select! {
                    accept_result = listener.accept() => {
                        self.handle_accept(accept_result).await;
                    }
                    _key = Self::wait_any_stream_readable(&self.streams) => {
                        self.sync_connections();
                    }
                    () = self.tx_notify.notified() => {
                        self.sync_connections();
                    }
                }
            }
        }
    }

    /// Handles the result of a `listener.accept()` call.
    async fn handle_accept(
        &mut self,
        result: std::io::Result<(tokio::net::UnixStream, tokio::net::unix::SocketAddr)>,
    ) {
        match result {
            Ok((stream, _addr)) => {
                if let Err(e) = self.handle_new_connection(stream).await {
                    tracing::warn!(error = %e, "vsock muxer: failed to handle new connection");
                }
            }
            Err(e) => {
                tracing::warn!(error = %e, "vsock muxer: accept error");
            }
        }
    }

    /// Waits until any stream in the map becomes readable.
    ///
    /// Uses `poll_fn` to register wakers with the tokio reactor for each
    /// stream's read readiness. Zero CPU when all streams are idle —
    /// the OS (kqueue/epoll) does the multiplexing.
    ///
    /// Returns the `ConnMapKey` of the first readable stream.
    async fn wait_any_stream_readable(
        streams: &HashMap<ConnMapKey, tokio::net::UnixStream>,
    ) -> ConnMapKey {
        std::future::poll_fn(|cx| {
            for (key, stream) in streams {
                match stream.poll_read_ready(cx) {
                    std::task::Poll::Ready(Ok(()) | Err(_)) => {
                        return std::task::Poll::Ready(*key);
                    }
                    std::task::Poll::Pending => {}
                }
            }
            std::task::Poll::Pending
        })
        .await
    }

    /// Handles a new host connection: reads the CONNECT handshake, allocates
    /// a local port, creates the vsock connection, and stores the stream.
    ///
    /// # Protocol
    ///
    /// The host client must send `CONNECT {port}\n` within [`HANDSHAKE_TIMEOUT`].
    /// On success, the muxer responds with `OK {local_port}\n`.
    /// On failure, the muxer responds with an error message and drops the stream.
    ///
    /// # Errors
    ///
    /// Returns [`VsockMuxerError`] if the handshake fails, port allocation is
    /// exhausted, or the connection limit is reached.
    async fn handle_new_connection(
        &mut self,
        mut stream: tokio::net::UnixStream,
    ) -> Result<(), VsockMuxerError> {
        // Enforce connection limit.
        if self.streams.len() >= MAX_CONNECTIONS {
            let _ = stream.write_all(b"ERR too many connections\n").await;
            return Err(VsockMuxerError::InvalidHandshake(
                "connection limit reached".into(),
            ));
        }

        // Read the CONNECT line with a timeout.
        let peer_port = Self::read_connect_handshake(&mut stream, HANDSHAKE_TIMEOUT).await?;

        // Allocate a local port.
        let local_port = self.allocate_port()?;

        // Create the vsock connection inside the device.
        let key = ConnMapKey {
            local_port,
            peer_port,
        };
        let conn = VsockConnection::new_local_init(HOST_CID, self.guest_cid, local_port, peer_port);

        {
            let mut device = self
                .device
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            device.add_connection(key, conn);
        }

        (self.rx_kick)();

        let establish_result = tokio::time::timeout(
            CONNECT_ESTABLISH_TIMEOUT,
            self.wait_for_guest_established(key),
        )
        .await;

        match establish_result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                self.drop_pending_connection(key);
                let _ = stream.write_all(b"ERR guest connection failed\n").await;
                return Err(error);
            }
            Err(_) => {
                self.drop_pending_connection(key);
                let _ = stream.write_all(b"ERR guest connection timeout\n").await;
                return Err(VsockMuxerError::InvalidHandshake(
                    "guest connection timeout".into(),
                ));
            }
        }

        // Respond with OK once the guest has accepted the vsock connection.
        stream
            .write_all(format!("OK {local_port}\n").as_bytes())
            .await
            .map_err(VsockMuxerError::Io)?;

        self.streams.insert(key, stream);

        tracing::debug!(
            local_port,
            peer_port,
            guest_cid = self.guest_cid,
            "vsock muxer: new host connection established"
        );

        Ok(())
    }

    /// Waits until the guest side acknowledges a host-initiated connection.
    ///
    /// Returns once the tracked connection reaches [`ConnState::Established`].
    async fn wait_for_guest_established(&self, key: ConnMapKey) -> Result<(), VsockMuxerError> {
        loop {
            let state = {
                let device = self
                    .device
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                device.connections().get(&key).map(VsockConnection::state)
            };

            match state {
                Some(ConnState::Established) => return Ok(()),
                Some(ConnState::Killed) | None => {
                    return Err(VsockMuxerError::InvalidHandshake(
                        "guest rejected connection".into(),
                    ));
                }
                _ => self.tx_notify.notified().await,
            }
        }
    }

    /// Removes a pending host connection after guest-side connection setup fails.
    fn drop_pending_connection(&mut self, key: ConnMapKey) {
        self.allocated_ports.remove(&key.local_port);
        let mut device = self
            .device
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _ = device.remove_connection(&key);
    }

    /// Reads and parses the `CONNECT {port}\n` handshake from a host stream.
    ///
    /// # Errors
    ///
    /// Returns [`VsockMuxerError::InvalidHandshake`] if the line is malformed,
    /// or [`VsockMuxerError::Io`] on read/timeout errors.
    async fn read_connect_handshake(
        stream: &mut tokio::net::UnixStream,
        timeout: Duration,
    ) -> Result<u32, VsockMuxerError> {
        let mut reader = BufReader::new(stream);

        let mut line = String::new();
        let read_result = tokio::time::timeout(timeout, reader.read_line(&mut line)).await;

        match read_result {
            Ok(Ok(0)) => {
                return Err(VsockMuxerError::InvalidHandshake(
                    "connection closed before handshake".into(),
                ));
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => return Err(VsockMuxerError::Io(e)),
            Err(_elapsed) => {
                return Err(VsockMuxerError::InvalidHandshake(
                    "handshake timeout".into(),
                ));
            }
        }

        let trimmed = line.trim();
        let port_str = trimmed.strip_prefix("CONNECT ").ok_or_else(|| {
            VsockMuxerError::InvalidHandshake(format!("expected 'CONNECT <port>', got: {trimmed}"))
        })?;

        let port: u32 = port_str.parse().map_err(|_| {
            VsockMuxerError::InvalidHandshake(format!("invalid port number: {port_str}"))
        })?;

        Ok(port)
    }

    /// Allocates a local port from the `[1<<30, 1<<31)` range using round-robin.
    ///
    /// # Errors
    ///
    /// Returns [`VsockMuxerError::PortExhausted`] if all ports in the range
    /// are currently allocated.
    fn allocate_port(&mut self) -> Result<u32, VsockMuxerError> {
        let range_size = LOCAL_PORT_END - LOCAL_PORT_START;
        let start = self.next_local_port;
        let mut attempts: u32 = 0;

        loop {
            let port = self.next_local_port;

            // Advance for next call, wrapping within range.
            self.next_local_port = if self.next_local_port + 1 >= LOCAL_PORT_END {
                LOCAL_PORT_START
            } else {
                self.next_local_port + 1
            };

            if !self.allocated_ports.contains(&port) {
                self.allocated_ports.insert(port);
                return Ok(port);
            }

            attempts += 1;
            if attempts >= range_size {
                return Err(VsockMuxerError::PortExhausted);
            }

            // Safety net: we've wrapped all the way around.
            if self.next_local_port == start && attempts > 0 {
                return Err(VsockMuxerError::PortExhausted);
            }
        }
    }

    /// Synchronizes data between host Unix streams and vsock connections.
    ///
    /// This is the core data-bridging method, structured to minimize lock hold time:
    ///
    /// 1. **Read phase** — `try_read()` from each stream (no device lock).
    /// 2. **Device lock phase** — push data, drain TX, detect dead connections.
    /// 3. **Write phase** — `try_write()` to each stream (no device lock).
    /// 4. **Cleanup phase** — remove dead streams, deallocate ports.
    /// 5. **IRQ phase** — trigger interrupt if guest has new RX data.
    fn sync_connections(&mut self) {
        if self.streams.is_empty() {
            return;
        }

        // Phase 1 + 2: read from streams, then update device state.
        let (tx_data, mut dead_keys, any_host_data_pushed, pending_host_to_guest) =
            self.read_and_update_device();

        // Phase 3: Write TX data to host streams (no lock).
        self.write_tx_to_streams(&tx_data, &mut dead_keys);

        // Phase 4: Cleanup dead streams and ports.
        self.cleanup_dead_connections(&dead_keys);

        // Phase 5: Push pending host data into the guest RX queue.
        if any_host_data_pushed || pending_host_to_guest {
            (self.rx_kick)();
        }
    }

    /// Reads from all host streams and updates device state under lock.
    ///
    /// Returns `(tx_data, dead_keys, any_host_data_pushed)` where:
    /// - `tx_data`: guest TX data to write to host streams
    /// - `dead_keys`: connections to clean up
    /// - `any_host_data_pushed`: whether any host data was pushed to the device
    fn read_and_update_device(
        &self,
    ) -> (HashMap<ConnMapKey, Vec<u8>>, Vec<ConnMapKey>, bool, bool) {
        // ── Read phase (no lock) ──────────────────────────────────
        let mut read_data: HashMap<ConnMapKey, Vec<u8>> = HashMap::new();
        let mut read_errors: Vec<ConnMapKey> = Vec::new();

        for (key, stream) in &self.streams {
            let mut buf = [0u8; STREAM_BUF_SIZE];
            match stream.try_read(&mut buf) {
                Ok(0) => read_errors.push(*key),
                Ok(n) => {
                    read_data.insert(*key, buf[..n].to_vec());
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {}
                Err(_) => read_errors.push(*key),
            }
        }

        // ── Device lock phase ─────────────────────────────────────
        let mut tx_data: HashMap<ConnMapKey, Vec<u8>> = HashMap::new();
        let mut dead_keys: Vec<ConnMapKey> = Vec::new();
        let mut any_host_data_pushed = false;
        let pending_host_to_guest = {
            let mut device = self
                .device
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);

            // Push host data into connections.
            for (key, data) in &read_data {
                if let Some(conn) = device.connections_mut().get_mut(key) {
                    conn.push_host_data(data);
                    tracing::debug!(
                        local_port = key.local_port,
                        peer_port = key.peer_port,
                        bytes = data.len(),
                        "vsock muxer: pushed host data into connection"
                    );
                    any_host_data_pushed = true;
                }
            }

            // Notify closed host streams.
            for key in &read_errors {
                if let Some(conn) = device.connections_mut().get_mut(key) {
                    conn.notify_host_closed();
                }
            }

            // Drain TX buffers for all connections that have streams.
            let stream_keys: Vec<ConnMapKey> = self.streams.keys().copied().collect();
            for key in &stream_keys {
                if let Some(conn) = device.connections_mut().get_mut(key) {
                    let mut buf = Vec::new();
                    match conn.flush_tx_buf(&mut buf) {
                        Ok(n) if n > 0 => {
                            tx_data.insert(*key, buf);
                        }
                        Ok(_) => {}
                        Err(e) => {
                            tracing::debug!(
                                local_port = key.local_port,
                                peer_port = key.peer_port,
                                error = %e,
                                "vsock muxer: TX flush error"
                            );
                        }
                    }
                }
            }

            // Detect dead (Killed) connections.
            for key in &stream_keys {
                match device.connections().get(key) {
                    Some(conn) if conn.state() == ConnState::Killed => {
                        dead_keys.push(*key);
                    }
                    None => {
                        dead_keys.push(*key);
                    }
                    _ => {}
                }
            }

            // Also add read-error keys as dead.
            for key in &read_errors {
                if !dead_keys.contains(key) {
                    dead_keys.push(*key);
                }
            }

            // Expire timed-out connections.
            device.expire_connections();

            device
                .connections()
                .values()
                .any(VsockConnection::has_pending_rx)
        };

        (
            tx_data,
            dead_keys,
            any_host_data_pushed,
            pending_host_to_guest,
        )
    }

    /// Writes guest TX data to host streams, marking failed connections as dead.
    fn write_tx_to_streams(
        &self,
        tx_data: &HashMap<ConnMapKey, Vec<u8>>,
        dead_keys: &mut Vec<ConnMapKey>,
    ) {
        for (key, data) in tx_data {
            if let Some(stream) = self.streams.get(key) {
                match stream.try_write(data) {
                    Ok(_) => {}
                    Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        tracing::debug!(
                            local_port = key.local_port,
                            peer_port = key.peer_port,
                            "vsock muxer: stream write would block, data deferred"
                        );
                    }
                    Err(e) => {
                        tracing::debug!(
                            local_port = key.local_port,
                            peer_port = key.peer_port,
                            error = %e,
                            "vsock muxer: stream write error, marking dead"
                        );
                        if !dead_keys.contains(key) {
                            dead_keys.push(*key);
                        }
                    }
                }
            }
        }
    }

    /// Removes dead connections: drops streams, frees ports, removes from device.
    fn cleanup_dead_connections(&mut self, dead_keys: &[ConnMapKey]) {
        if dead_keys.is_empty() {
            return;
        }

        // Remove streams and deallocate ports (borrows self.streams/allocated_ports).
        for key in dead_keys {
            self.streams.remove(key);
            self.allocated_ports.remove(&key.local_port);
        }

        // Lock device to remove connections.
        let mut device = self
            .device
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in dead_keys {
            device.remove_connection(key);
            tracing::debug!(
                local_port = key.local_port,
                peer_port = key.peer_port,
                "vsock muxer: connection cleaned up"
            );
        }
    }
}

impl Drop for VsockMuxer {
    fn drop(&mut self) {
        let listener_path = self.listener_path();
        if let Err(error) = std::fs::remove_file(&listener_path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %listener_path.display(),
                    error = %error,
                    "failed to remove vsock muxer listener socket"
                );
            }
        }
    }
}

impl std::fmt::Debug for VsockMuxer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VsockMuxer")
            .field("guest_cid", &self.guest_cid)
            .field("socket_dir", &self.socket_dir)
            .field("active_streams", &self.streams.len())
            .field("allocated_ports", &self.allocated_ports.len())
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "vsock_muxer_test.rs"]
mod tests;
