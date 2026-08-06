//! Virtio vsock device backend (`virtio-vsock`).
//!
//! Provides socket communication between the guest and host via the
//! virtio vsock protocol. Includes:
//!
//! - **Device model** ([`VsockDevice`]) — feature negotiation, config space, virtqueues.
//! - **Connection state machine** ([`VsockConnection`]) — per-connection protocol state,
//!   credit flow control, handshake, and graceful shutdown.
//! - **TX ring buffer** ([`TxBuf`]) — bounded 64 KiB buffer for guest→host data.
//!
//! # Config space layout (virtio-vsock spec)
//!
//! - Offset `0..7`: guest CID (`u64`, little-endian).

use std::collections::HashMap;
use std::collections::VecDeque;
use std::num::Wrapping;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::memory::GuestMemory;
use crate::transport::{
    DeviceType, VIRTQ_DESC_F_NEXT, VirtQueue, VirtioDevice, VirtioError, VirtqDesc,
};
use tokio::sync::Notify;

// ── Feature flags ────────────────────────────────────────────────────

/// Virtio feature: modern virtio (version 1).
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

// ── Constants ────────────────────────────────────────────────────────

/// Maximum virtqueue size for the vsock device.
const QUEUE_MAX_SIZE: u16 = 256;

/// Number of virtqueues (vsock has 3: rx, tx, event).
const NUM_QUEUES: usize = 3;

/// RX virtqueue index (host → guest).
const RXQ: usize = 0;

/// TX virtqueue index (guest → host).
const TXQ: usize = 1;

/// Event virtqueue index (used in Phase 3).
const _EVQ: usize = 2;
/// Size of the vsock packet header in bytes.
pub const VSOCK_PKT_HDR_SIZE: usize = 44;
/// Size of the vsock packet header as `u32` (for descriptor length arithmetic).
const VSOCK_PKT_HDR_SIZE_U32: u32 = 44;

// ── VsockOp ──────────────────────────────────────────────────────────

/// Vsock operation types per the virtio-vsock specification.
#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum VsockOp {
    /// Connection request.
    Request = 1,
    /// Connection response.
    Response = 2,
    /// Connection reset.
    Rst = 3,
    /// Connection shutdown.
    Shutdown = 4,
    /// Data read/write.
    Rw = 5,
    /// Credit update.
    CreditUpdate = 6,
    /// Credit request.
    CreditRequest = 7,
}

impl VsockOp {
    /// Converts a raw `u16` operation code to the typed enum.
    #[must_use]
    pub fn from_u16(v: u16) -> Option<Self> {
        match v {
            1 => Some(Self::Request),
            2 => Some(Self::Response),
            3 => Some(Self::Rst),
            4 => Some(Self::Shutdown),
            5 => Some(Self::Rw),
            6 => Some(Self::CreditUpdate),
            7 => Some(Self::CreditRequest),
            _ => None,
        }
    }
}

// ── VsockPacket ──────────────────────────────────────────────────────

/// Vsock packet header for virtio-vsock communication.
///
/// Represents the 44-byte header that precedes all vsock data packets.
/// Used for serialization/deserialization of vsock protocol messages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct VsockPacket {
    /// Source context ID.
    pub src_cid: u64,
    /// Destination context ID.
    pub dst_cid: u64,
    /// Source port.
    pub src_port: u32,
    /// Destination port.
    pub dst_port: u32,
    /// Payload length in bytes.
    pub len: u32,
    /// Packet type (1 = stream).
    pub pkt_type: u16,
    /// Operation code (see [`VsockOp`]).
    pub op: u16,
    /// Flags.
    pub flags: u32,
    /// Peer buffer allocation.
    pub buf_alloc: u32,
    /// Forwarded bytes count.
    pub fwd_cnt: u32,
}

impl VsockPacket {
    /// Parses a vsock packet header from a byte slice.
    ///
    /// Returns `None` if the buffer is shorter than [`VSOCK_PKT_HDR_SIZE`] (44 bytes).
    #[must_use]
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < VSOCK_PKT_HDR_SIZE {
            return None;
        }
        Some(Self {
            src_cid: u64::from_le_bytes(buf[0..8].try_into().ok()?),
            dst_cid: u64::from_le_bytes(buf[8..16].try_into().ok()?),
            src_port: u32::from_le_bytes(buf[16..20].try_into().ok()?),
            dst_port: u32::from_le_bytes(buf[20..24].try_into().ok()?),
            len: u32::from_le_bytes(buf[24..28].try_into().ok()?),
            pkt_type: u16::from_le_bytes(buf[28..30].try_into().ok()?),
            op: u16::from_le_bytes(buf[30..32].try_into().ok()?),
            flags: u32::from_le_bytes(buf[32..36].try_into().ok()?),
            buf_alloc: u32::from_le_bytes(buf[36..40].try_into().ok()?),
            fwd_cnt: u32::from_le_bytes(buf[40..44].try_into().ok()?),
        })
    }

    /// Serializes the packet header to a 44-byte array in little-endian format.
    #[must_use]
    pub fn to_bytes(&self) -> [u8; VSOCK_PKT_HDR_SIZE] {
        let mut buf = [0u8; VSOCK_PKT_HDR_SIZE];
        buf[0..8].copy_from_slice(&self.src_cid.to_le_bytes());
        buf[8..16].copy_from_slice(&self.dst_cid.to_le_bytes());
        buf[16..20].copy_from_slice(&self.src_port.to_le_bytes());
        buf[20..24].copy_from_slice(&self.dst_port.to_le_bytes());
        buf[24..28].copy_from_slice(&self.len.to_le_bytes());
        buf[28..30].copy_from_slice(&self.pkt_type.to_le_bytes());
        buf[30..32].copy_from_slice(&self.op.to_le_bytes());
        buf[32..36].copy_from_slice(&self.flags.to_le_bytes());
        buf[36..40].copy_from_slice(&self.buf_alloc.to_le_bytes());
        buf[40..44].copy_from_slice(&self.fwd_cnt.to_le_bytes());
        buf
    }
}

// ── ConnMapKey ───────────────────────────────────────────────────────

/// Key for the vsock connection map: identifies a connection by port pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ConnMapKey {
    /// Local (host) port.
    pub local_port: u32,
    /// Peer (guest) port.
    pub peer_port: u32,
}

// ── VsockDevice ──────────────────────────────────────────────────────

/// Virtio vsock device for guest-host socket communication.
///
/// Implements [`VirtioDevice`] for use with the MMIO transport.
/// Provides three virtqueues: rx (index 0), tx (index 1), and event (index 2).
#[derive(Debug)]
#[non_exhaustive]
pub struct VsockDevice {
    /// Guest context ID.
    guest_cid: u64,
    /// Feature bits offered by the device.
    avail_features: u64,
    /// Feature bits acknowledged by the driver.
    acked_features: u64,
    /// Virtqueues (vsock has 3: rx, tx, event).
    queues: Vec<VirtQueue>,
    /// Whether the device has been activated by the driver.
    activated: bool,
    /// Active vsock connections, keyed by `(local_port, peer_port)`.
    connections: HashMap<ConnMapKey, VsockConnection>,
    /// Notification channel for the muxer — poked when guest TX data is available.
    tx_notify: Arc<Notify>,
}

impl VsockDevice {
    /// Creates a new vsock device with the given guest context ID.
    #[must_use]
    pub fn new(guest_cid: u64) -> Self {
        let queues = (0..NUM_QUEUES)
            .map(|_| VirtQueue::new(QUEUE_MAX_SIZE))
            .collect();

        Self {
            guest_cid,
            avail_features: VIRTIO_F_VERSION_1,
            acked_features: 0,
            queues,
            activated: false,
            connections: HashMap::new(),
            tx_notify: Arc::new(Notify::new()),
        }
    }

    /// Returns a clone of the TX notification handle.
    ///
    /// The muxer awaits this `Notify` to wake when the guest produces TX data.
    #[must_use]
    pub fn tx_notify(&self) -> Arc<Notify> {
        Arc::clone(&self.tx_notify)
    }

    /// Returns the guest context ID.
    #[must_use]
    pub fn guest_cid(&self) -> u64 {
        self.guest_cid
    }

    /// Returns a reference to the active connection map.
    #[must_use]
    pub fn connections(&self) -> &HashMap<ConnMapKey, VsockConnection> {
        &self.connections
    }

    /// Returns a mutable reference to the active connection map.
    pub fn connections_mut(&mut self) -> &mut HashMap<ConnMapKey, VsockConnection> {
        &mut self.connections
    }

    /// Inserts a connection into the device's connection map.
    ///
    /// Used by the vsock muxer to add host-initiated connections.
    pub fn add_connection(&mut self, key: ConnMapKey, conn: VsockConnection) {
        self.connections.insert(key, conn);
    }

    /// Removes a connection from the device's connection map.
    ///
    /// Returns the removed connection if it existed.
    pub fn remove_connection(&mut self, key: &ConnMapKey) -> Option<VsockConnection> {
        self.connections.remove(key)
    }

    /// Removes expired connections (kill timer elapsed) from the map.
    pub fn expire_connections(&mut self) {
        let expired: Vec<ConnMapKey> = self
            .connections
            .iter()
            .filter(|(_, conn)| conn.has_expired())
            .map(|(key, _)| *key)
            .collect();
        for key in expired {
            self.connections.remove(&key);
        }
    }
}

// ── Queue processing ─────────────────────────────────────────────────

impl VsockDevice {
    /// Reads a single descriptor from the descriptor table in guest memory.
    fn read_desc(
        memory: &GuestMemory,
        queue: &VirtQueue,
        idx: u16,
    ) -> Result<VirtqDesc, VsockError> {
        if idx >= queue.size {
            return Err(VsockError::InvalidDescriptor(format!(
                "descriptor index {idx} >= queue size {}",
                queue.size
            )));
        }
        let addr = queue.desc_table_addr + u64::from(idx) * 16;
        let bytes = memory.read_bytes(addr, 16).map_err(VsockError::Memory)?;
        Ok(VirtqDesc {
            addr: u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ]),
            len: u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]),
            flags: u16::from_le_bytes([bytes[12], bytes[13]]),
            next: u16::from_le_bytes([bytes[14], bytes[15]]),
        })
    }

    /// Reads the current avail ring index from guest memory.
    fn read_avail_idx(memory: &GuestMemory, queue: &VirtQueue) -> Result<u16, VsockError> {
        let bytes = memory
            .read_bytes(queue.avail_ring_addr + 2, 2)
            .map_err(VsockError::Memory)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Writes a used ring entry (id + len) at the current `last_used_idx` position.
    fn write_used_entry(
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
        written: u32,
    ) -> Result<(), VsockError> {
        let used_offset = 4 + u64::from(queue.last_used_idx % queue.size) * 8;
        let used_addr = queue.used_ring_addr + used_offset;
        memory
            .write_bytes(used_addr, &u32::from(head_idx).to_le_bytes())
            .map_err(VsockError::Memory)?;
        memory
            .write_bytes(used_addr + 4, &written.to_le_bytes())
            .map_err(VsockError::Memory)?;
        Ok(())
    }

    /// Updates the used ring index so the guest sees new completed entries.
    fn write_used_idx(memory: &GuestMemory, queue: &VirtQueue) -> Result<(), VsockError> {
        memory
            .write_bytes(queue.used_ring_addr + 2, &queue.last_used_idx.to_le_bytes())
            .map_err(VsockError::Memory)?;
        Ok(())
    }

    // ── TX queue ─────────────────────────────────────────────────────

    /// Processes all pending TX descriptor chains (guest → host).
    ///
    /// Reads packets from the TX avail ring, parses headers, and dispatches
    /// to the connection state machine.
    fn process_tx_queue_inner(
        &mut self,
        memory: &GuestMemory,
        queue: &mut VirtQueue,
    ) -> Result<bool, VsockError> {
        if !queue.ready || queue.size == 0 {
            return Ok(false);
        }

        let avail_idx = Self::read_avail_idx(memory, queue)?;
        let mut processed = false;

        while queue.last_avail_idx != avail_idx {
            let avail_offset = 4 + u64::from(queue.last_avail_idx % queue.size) * 2;
            let desc_idx_bytes = memory
                .read_bytes(queue.avail_ring_addr + avail_offset, 2)
                .map_err(VsockError::Memory)?;
            let head_idx = u16::from_le_bytes([desc_idx_bytes[0], desc_idx_bytes[1]]);

            // Parse and dispatch — errors from individual packets are non-fatal.
            let _ = self.handle_tx_chain(memory, queue, head_idx);

            // Write used ring entry (0 bytes written — host consumed the buffer).
            Self::write_used_entry(memory, queue, head_idx, 0)?;
            queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
            queue.last_used_idx = queue.last_used_idx.wrapping_add(1);
            processed = true;
        }

        if processed {
            Self::write_used_idx(memory, queue)?;
        }

        Ok(processed)
    }

    /// Parses a single TX descriptor chain and dispatches to the connection map.
    fn handle_tx_chain(
        &mut self,
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
    ) -> Result<(), VsockError> {
        let header_desc = Self::read_desc(memory, queue, head_idx)?;

        if (header_desc.len as usize) < VSOCK_PKT_HDR_SIZE {
            return Ok(()); // Silently drop malformed packet.
        }

        let hdr_bytes = memory
            .read_bytes(header_desc.addr, VSOCK_PKT_HDR_SIZE)
            .map_err(VsockError::Memory)?;
        let Some(pkt) = VsockPacket::from_bytes(&hdr_bytes) else {
            return Ok(());
        };

        // Read payload from second descriptor if present.
        let payload = if header_desc.flags & VIRTQ_DESC_F_NEXT != 0 {
            let data_desc = Self::read_desc(memory, queue, header_desc.next)?;
            let len = std::cmp::min(data_desc.len as usize, pkt.len as usize);
            if len > 0 {
                memory
                    .read_bytes(data_desc.addr, len)
                    .map_err(VsockError::Memory)?
            } else {
                Vec::new()
            }
        } else {
            Vec::new()
        };

        let key = ConnMapKey {
            local_port: pkt.dst_port,
            peer_port: pkt.src_port,
        };

        match VsockOp::from_u16(pkt.op) {
            Some(VsockOp::Request) => {
                // Guest-initiated connection — create if not already tracked.
                if !self.connections.contains_key(&key) {
                    let conn = VsockConnection::new_peer_init(
                        HOST_CID,
                        self.guest_cid,
                        pkt.dst_port,
                        pkt.src_port,
                        pkt.buf_alloc,
                    );
                    self.connections.insert(key, conn);
                }
            }
            Some(VsockOp::Rst) => {
                self.connections.remove(&key);
            }
            _ => {
                if let Some(conn) = self.connections.get_mut(&key) {
                    // Non-fatal: TxBufFull etc. don't crash the device.
                    let _ = conn.send_pkt(&pkt, &payload);
                }
            }
        }

        Ok(())
    }

    // ── RX queue ─────────────────────────────────────────────────────

    /// Fills guest-provided RX buffers from connections with pending data.
    ///
    /// Iterates available RX descriptor chains and matches them with
    /// connections that have pending packets (responses, data, RSTs).
    fn process_rx_queue_inner(
        &mut self,
        memory: &GuestMemory,
        queue: &mut VirtQueue,
    ) -> Result<bool, VsockError> {
        if !queue.ready || queue.size == 0 {
            return Ok(false);
        }

        let avail_idx = Self::read_avail_idx(memory, queue)?;

        // Log when connections have pending data but no descriptors available.
        if queue.last_avail_idx == avail_idx {
            let pending_count = self
                .connections
                .values()
                .filter(|c| c.has_pending_rx())
                .count();
            if pending_count > 0 {
                tracing::warn!(
                    avail_idx,
                    last_avail = queue.last_avail_idx,
                    pending_connections = pending_count,
                    total_connections = self.connections.len(),
                    "vsock RX: no available descriptors but connections have pending data"
                );
            }
        }

        let avail_idx = Self::read_avail_idx(memory, queue)?;
        let mut processed = false;

        while queue.last_avail_idx != avail_idx {
            // Find a connection with pending RX data.
            let pending_keys: Vec<ConnMapKey> = self
                .connections
                .iter()
                .filter(|(_, conn)| conn.has_pending_rx())
                .map(|(key, _)| *key)
                .collect();

            if pending_keys.is_empty() {
                break;
            }

            // Read the guest's RX descriptor chain to determine buffer sizes.
            let avail_offset = 4 + u64::from(queue.last_avail_idx % queue.size) * 2;
            let desc_idx_bytes = memory
                .read_bytes(queue.avail_ring_addr + avail_offset, 2)
                .map_err(VsockError::Memory)?;
            let head_idx = u16::from_le_bytes([desc_idx_bytes[0], desc_idx_bytes[1]]);
            let header_desc = Self::read_desc(memory, queue, head_idx)?;

            let (payload_addr, max_data_len) = if header_desc.flags & VIRTQ_DESC_F_NEXT != 0 {
                let data_desc = Self::read_desc(memory, queue, header_desc.next)?;
                (Some(data_desc.addr), data_desc.len as usize)
            } else {
                let inline_capacity = (header_desc.len as usize).saturating_sub(VSOCK_PKT_HDR_SIZE);
                (
                    (inline_capacity > 0)
                        .then_some(header_desc.addr + u64::from(VSOCK_PKT_HDR_SIZE_U32)),
                    inline_capacity,
                )
            };

            // Try each pending connection until one produces data.
            let mut filled = false;
            let mut rst_key: Option<ConnMapKey> = None;

            for key in &pending_keys {
                let (pkt, payload) = {
                    let Some(conn) = self.connections.get_mut(key) else {
                        continue;
                    };
                    match conn.recv_pkt(max_data_len) {
                        Ok(result) => result,
                        Err(_) => continue,
                    }
                };

                // Write packet header to the first descriptor.
                let hdr_bytes = pkt.to_bytes();
                if header_desc.len as usize >= VSOCK_PKT_HDR_SIZE {
                    memory
                        .write_bytes(header_desc.addr, &hdr_bytes)
                        .map_err(VsockError::Memory)?;
                }

                // Write payload to the data descriptor if present.
                let mut total_written = VSOCK_PKT_HDR_SIZE_U32;
                if !payload.is_empty() {
                    let write_len = std::cmp::min(payload.len(), max_data_len);
                    if let Some(addr) = payload_addr {
                        memory
                            .write_bytes(addr, &payload[..write_len])
                            .map_err(VsockError::Memory)?;
                    }
                    total_written =
                        total_written.saturating_add(u32::try_from(write_len).unwrap_or(u32::MAX));
                }

                Self::write_used_entry(memory, queue, head_idx, total_written)?;
                queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
                queue.last_used_idx = queue.last_used_idx.wrapping_add(1);
                filled = true;
                processed = true;

                if pkt.op == VsockOp::Rst as u16 {
                    rst_key = Some(*key);
                }
                break;
            }

            // Clean up RST'd connections after releasing the borrow.
            if let Some(key) = rst_key {
                self.connections.remove(&key);
            }

            if !filled {
                break; // No connection could produce data.
            }
        }

        if processed {
            Self::write_used_idx(memory, queue)?;
        }

        Ok(processed)
    }
}

impl VirtioDevice for VsockDevice {
    fn device_type(&self) -> DeviceType {
        DeviceType::Vsock
    }

    fn avail_features(&self) -> u64 {
        self.avail_features
    }

    fn acked_features(&self) -> u64 {
        self.acked_features
    }

    fn set_acked_features(&mut self, features: u64) {
        self.acked_features = features;
    }

    fn queues(&self) -> &[VirtQueue] {
        &self.queues
    }

    fn queues_mut(&mut self) -> &mut [VirtQueue] {
        &mut self.queues
    }

    fn read_config(&self, offset: u64, data: &mut [u8]) {
        let config_bytes = self.guest_cid.to_le_bytes();
        for (i, byte) in data.iter_mut().enumerate() {
            let Some(idx) = usize::try_from(offset).ok().and_then(|o| o.checked_add(i)) else {
                *byte = 0;
                continue;
            };
            if let Some(&val) = config_bytes.get(idx) {
                *byte = val;
            } else {
                *byte = 0;
            }
        }
    }

    fn write_config(&mut self, _offset: u64, _data: &[u8]) {
        // Config is read-only for vsock devices — no-op.
    }

    fn activate(&mut self) -> Result<(), VirtioError> {
        self.activated = true;
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.activated
    }

    fn reset(&mut self) {
        let was_activated = self.activated;
        self.activated = false;
        // During initial guest-driver bring-up, Linux writes STATUS=0 before
        // queue setup. If host-side exec traffic arrived slightly earlier, we
        // must keep those pending host-initiated connections so the request can
        // be delivered after DRIVER_OK.
        if was_activated {
            self.connections.clear();
        }
        for queue in &mut self.queues {
            queue.reset();
        }
    }

    fn process_queue(
        &mut self,
        queue_idx: usize,
        memory: &GuestMemory,
    ) -> Result<bool, VirtioError> {
        match queue_idx {
            TXQ => {
                // Process guest TX packets, then try to fill RX queue.
                let mut tx_queue = self.queues[TXQ].clone();
                let tx_ok = self
                    .process_tx_queue_inner(memory, &mut tx_queue)
                    .unwrap_or(false);
                self.queues[TXQ].last_avail_idx = tx_queue.last_avail_idx;
                self.queues[TXQ].last_used_idx = tx_queue.last_used_idx;

                // TX may have created pending RX items (e.g., REQUEST → RESPONSE).
                let mut rx_queue = self.queues[RXQ].clone();
                let rx_ok = self
                    .process_rx_queue_inner(memory, &mut rx_queue)
                    .unwrap_or(false);
                self.queues[RXQ].last_avail_idx = rx_queue.last_avail_idx;
                self.queues[RXQ].last_used_idx = rx_queue.last_used_idx;

                // Notify the muxer that TX data is available for draining.
                if tx_ok {
                    self.tx_notify.notify_one();
                }

                Ok(tx_ok || rx_ok)
            }
            RXQ => {
                let mut rx_queue = self.queues[RXQ].clone();
                let ok = self
                    .process_rx_queue_inner(memory, &mut rx_queue)
                    .unwrap_or(false);
                self.queues[RXQ].last_avail_idx = rx_queue.last_avail_idx;
                self.queues[RXQ].last_used_idx = rx_queue.last_used_idx;
                Ok(ok)
            }
            _ => Ok(false),
        }
    }
}

// ── Vsock protocol constants ──────────────────────────────────────────

/// Host context ID (always 2 per the vsock specification).
pub const HOST_CID: u64 = 2;

/// Stream socket type (the only type defined by the vsock spec).
pub const VSOCK_TYPE_STREAM: u16 = 1;

/// Shutdown flag: sender will receive no more data.
pub const VSOCK_FLAGS_SHUTDOWN_RCV: u32 = 1;

/// Shutdown flag: sender will send no more data.
pub const VSOCK_FLAGS_SHUTDOWN_SEND: u32 = 2;

/// Connection TX buffer capacity in bytes.
pub(crate) const CONN_TX_BUF_SIZE: u32 = 64 * 1024;

/// Proactive credit update threshold: when the peer thinks we have less
/// than this many bytes free, we send a `CREDIT_UPDATE` packet.
const CONN_CREDIT_UPDATE_THRESHOLD: u32 = 4 * 1024;

/// Timeout for unanswered connection requests (milliseconds).
const CONN_REQUEST_TIMEOUT_MS: u64 = 2000;

/// Timeout for graceful shutdown drain (milliseconds).
const CONN_SHUTDOWN_TIMEOUT_MS: u64 = 2000;

// ── VsockError ────────────────────────────────────────────────────────

/// Errors from vsock connection and device operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VsockError {
    /// TX buffer is full — peer should have honoured credit flow control.
    #[error("TX buffer is full")]
    TxBufFull,

    /// TX buffer flush I/O error.
    #[error("TX buffer flush I/O error: {0}")]
    TxBufFlush(std::io::Error),

    /// No pending RX data available.
    #[error("no pending RX data")]
    NoData,

    /// Guest memory access error.
    #[error("guest memory error: {0}")]
    Memory(crate::memory::MemoryError),

    /// Invalid virtqueue descriptor.
    #[error("invalid descriptor: {0}")]
    InvalidDescriptor(String),
}

// ── ConnState ─────────────────────────────────────────────────────────

/// Vsock connection state, following Firecracker's battle-tested model.
///
/// # State diagram
///
/// ```text
///              Guest REQUEST
///   ────────────────────────────► PeerInit
///                                    │
///    Host connect                    │ send RESPONSE
///    ▼                               ▼
/// LocalInit ──────────────────► Established
///              Guest RESPONSE        │
///                              ┌─────┴─────┐
///                 SHUTDOWN     │           │ SHUTDOWN
///                 (guest)      ▼           ▼ (host)
///                         PeerClosed   LocalClosed
///                              │           │
///                              └─────┬─────┘
///                                    │ both sides closed
///                                    ▼
///  Killed ◄───────────────────── send RST
/// ```
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConnState {
    /// Host-initiated connection, awaiting guest response.
    LocalInit,
    /// Guest-initiated connection, awaiting host confirmation.
    PeerInit,
    /// Connection established, data exchange allowed.
    Established,
    /// Host-side stream was closed.
    LocalClosed,
    /// Guest sent SHUTDOWN. Tuple is `(no_recv, no_send)`:
    /// - `no_recv`: guest will not receive more data.
    /// - `no_send`: guest will not send more data.
    PeerClosed(bool, bool),
    /// Connection scheduled for forceful termination.
    Killed,
}

// ── PendingRx ─────────────────────────────────────────────────────────

/// An RX indication used by [`VsockConnection`] to schedule future
/// [`recv_pkt`](VsockConnection::recv_pkt) responses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingRx {
    /// Connection request (`VSOCK_OP_REQUEST`).
    Request = 0,
    /// Connection response (`VSOCK_OP_RESPONSE`).
    Response = 1,
    /// Forceful termination (`VSOCK_OP_RST`).
    Rst = 2,
    /// Data packet (`VSOCK_OP_RW`).
    Rw = 3,
    /// Credit update (`VSOCK_OP_CREDIT_UPDATE`).
    CreditUpdate = 4,
}

impl PendingRx {
    fn into_mask(self) -> u16 {
        1u16 << (self as u16)
    }
}

/// Bitmask set of [`PendingRx`] indications.
#[derive(Debug)]
struct PendingRxSet {
    data: u16,
}

impl PendingRxSet {
    #[cfg(test)]
    fn empty() -> Self {
        Self { data: 0 }
    }

    fn insert(&mut self, it: PendingRx) {
        self.data |= it.into_mask();
    }

    fn remove(&mut self, it: PendingRx) -> bool {
        let present = self.contains(it);
        self.data &= !it.into_mask();
        present
    }

    fn contains(&self, it: PendingRx) -> bool {
        self.data & it.into_mask() != 0
    }

    fn is_empty(&self) -> bool {
        self.data == 0
    }
}

impl From<PendingRx> for PendingRxSet {
    fn from(it: PendingRx) -> Self {
        Self {
            data: it.into_mask(),
        }
    }
}

// ── TxBuf ─────────────────────────────────────────────────────────────

/// Ring buffer for vsock TX data (guest → host).
///
/// Memory is allocated lazily on first push, since most connections may
/// not need buffering (data goes directly to the host stream).
#[derive(Debug)]
pub(crate) struct TxBuf {
    /// Backing storage — allocated on first push.
    data: Option<Box<[u8]>>,
    /// Ring-buffer write position.
    head: Wrapping<u32>,
    /// Ring-buffer read position.
    tail: Wrapping<u32>,
}

impl TxBuf {
    const SIZE: usize = CONN_TX_BUF_SIZE as usize;

    /// Creates an empty TX buffer (memory allocated on first push).
    pub(crate) fn new() -> Self {
        Self {
            data: None,
            head: Wrapping(0),
            tail: Wrapping(0),
        }
    }

    /// Number of bytes currently stored in the buffer.
    #[must_use]
    pub(crate) fn len(&self) -> usize {
        (self.head - self.tail).0 as usize
    }

    /// Returns `true` if the buffer holds no data.
    #[must_use]
    pub(crate) fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Push a byte slice onto the ring buffer.
    ///
    /// Atomic: the entire slice is pushed or nothing.
    ///
    /// # Errors
    ///
    /// Returns [`VsockError::TxBufFull`] if the buffer cannot fit `src`.
    pub(crate) fn push(&mut self, src: &[u8]) -> Result<(), VsockError> {
        if self.len() + src.len() > Self::SIZE {
            return Err(VsockError::TxBufFull);
        }

        let data = self
            .data
            .get_or_insert_with(|| vec![0u8; Self::SIZE].into_boxed_slice());

        let head_ofs = self.head.0 as usize % Self::SIZE;
        let first_len = std::cmp::min(Self::SIZE - head_ofs, src.len());
        data[head_ofs..head_ofs + first_len].copy_from_slice(&src[..first_len]);

        if first_len < src.len() {
            data[..src.len() - first_len].copy_from_slice(&src[first_len..]);
        }

        self.head += Wrapping(u32::try_from(src.len()).unwrap_or(u32::MAX));
        Ok(())
    }

    /// Flush buffered data to a writer.
    ///
    /// Returns the number of bytes successfully flushed. May flush fewer
    /// bytes than stored if the writer cannot accept all data.
    ///
    /// # Errors
    ///
    /// Returns [`VsockError::TxBufFlush`] on I/O error.
    pub(crate) fn flush_to<W: std::io::Write>(
        &mut self,
        sink: &mut W,
    ) -> Result<usize, VsockError> {
        if self.is_empty() {
            return Ok(0);
        }

        let Some(data) = self.data.as_ref() else {
            return Ok(0);
        };

        let tail_ofs = self.tail.0 as usize % Self::SIZE;
        let first_len = std::cmp::min(Self::SIZE - tail_ofs, self.len());

        let written = sink
            .write(&data[tail_ofs..tail_ofs + first_len])
            .map_err(VsockError::TxBufFlush)?;

        self.tail += Wrapping(u32::try_from(written).unwrap_or(u32::MAX));

        if written < first_len {
            return Ok(written);
        }

        // Try second write for wrapped data.
        Ok(written + self.flush_to(sink).unwrap_or(0))
    }
}

// ── VsockConnection ───────────────────────────────────────────────────

/// A vsock connection state machine.
///
/// Manages the protocol state for a single guest↔host vsock connection,
/// including handshake, data transfer, credit flow control, and teardown.
///
/// The connection does not hold a host-side stream directly — data is
/// buffered internally and made available to the caller for forwarding.
/// Host data is pushed in via [`push_host_data`](Self::push_host_data)
/// and TX data is drained via [`flush_tx_buf`](Self::flush_tx_buf).
#[derive(Debug)]
pub struct VsockConnection {
    /// Current connection state.
    state: ConnState,
    /// Local (host) CID.
    local_cid: u64,
    /// Peer (guest) CID.
    peer_cid: u64,
    /// Local (host) port.
    local_port: u32,
    /// Peer (guest) port.
    peer_port: u32,
    /// TX ring buffer for guest → host data.
    tx_buf: TxBuf,
    /// Bytes we have forwarded to the host stream.
    fwd_cnt: Wrapping<u32>,
    /// Guest's advertised RX buffer size.
    peer_buf_alloc: u32,
    /// Bytes the guest has forwarded from its buffer.
    peer_fwd_cnt: Wrapping<u32>,
    /// Total bytes sent to guest.
    rx_cnt: Wrapping<u32>,
    /// Our `fwd_cnt` as last reported to the guest.
    last_fwd_cnt_to_peer: Wrapping<u32>,
    /// Pending RX packet indications.
    pending_rx: PendingRxSet,
    /// Kill timer for timeouts.
    expiry: Option<Instant>,
    /// Data from host awaiting delivery to guest via RX queue.
    rx_buf: VecDeque<u8>,
    /// Whether the host-side stream has been closed.
    host_closed: bool,
}

impl VsockConnection {
    /// Create a guest-initiated connection (peer sent REQUEST).
    ///
    /// The connection starts in [`ConnState::PeerInit`] with a pending
    /// RESPONSE indication.
    #[must_use]
    pub fn new_peer_init(
        local_cid: u64,
        peer_cid: u64,
        local_port: u32,
        peer_port: u32,
        peer_buf_alloc: u32,
    ) -> Self {
        Self {
            state: ConnState::PeerInit,
            local_cid,
            peer_cid,
            local_port,
            peer_port,
            tx_buf: TxBuf::new(),
            fwd_cnt: Wrapping(0),
            peer_buf_alloc,
            peer_fwd_cnt: Wrapping(0),
            rx_cnt: Wrapping(0),
            last_fwd_cnt_to_peer: Wrapping(0),
            pending_rx: PendingRxSet::from(PendingRx::Response),
            expiry: None,
            rx_buf: VecDeque::new(),
            host_closed: false,
        }
    }

    /// Create a host-initiated connection (host wants to connect to guest).
    ///
    /// The connection starts in [`ConnState::LocalInit`] with a pending
    /// REQUEST indication.
    #[must_use]
    pub fn new_local_init(local_cid: u64, peer_cid: u64, local_port: u32, peer_port: u32) -> Self {
        Self {
            state: ConnState::LocalInit,
            local_cid,
            peer_cid,
            local_port,
            peer_port,
            tx_buf: TxBuf::new(),
            fwd_cnt: Wrapping(0),
            peer_buf_alloc: 0,
            peer_fwd_cnt: Wrapping(0),
            rx_cnt: Wrapping(0),
            last_fwd_cnt_to_peer: Wrapping(0),
            pending_rx: PendingRxSet::from(PendingRx::Request),
            expiry: None,
            rx_buf: VecDeque::new(),
            host_closed: false,
        }
    }

    /// Returns the current connection state.
    #[must_use]
    pub fn state(&self) -> ConnState {
        self.state
    }

    /// Returns the local (host) port.
    #[must_use]
    pub fn local_port(&self) -> u32 {
        self.local_port
    }

    /// Returns the peer (guest) port.
    #[must_use]
    pub fn peer_port(&self) -> u32 {
        self.peer_port
    }

    /// Returns `true` if there is a pending RX packet to deliver to the guest.
    #[must_use]
    pub fn has_pending_rx(&self) -> bool {
        !self.pending_rx.is_empty()
    }

    /// Returns `true` if this connection has a kill timer that has expired.
    #[must_use]
    pub fn has_expired(&self) -> bool {
        match self.expiry {
            None => false,
            Some(t) => t <= Instant::now(),
        }
    }

    /// Returns `true` if this connection has a kill timer set in the future.
    #[must_use]
    pub fn will_expire(&self) -> bool {
        match self.expiry {
            None => false,
            Some(t) => t > Instant::now(),
        }
    }

    /// Schedule immediate forceful termination (RST on next `recv_pkt`).
    pub fn kill(&mut self) {
        self.state = ConnState::Killed;
        self.pending_rx.insert(PendingRx::Rst);
    }

    /// Returns a reference to the TX buffer (test-only).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn tx_buf(&self) -> &TxBuf {
        &self.tx_buf
    }

    // ── Host data interface ──────────────────────────────────────────

    /// Push data received from the host stream into the RX buffer.
    ///
    /// This makes the data available for delivery to the guest via
    /// [`recv_pkt`](Self::recv_pkt).
    pub fn push_host_data(&mut self, data: &[u8]) {
        tracing::debug!(
            local_port = self.local_port,
            peer_port = self.peer_port,
            state = ?self.state,
            bytes = data.len(),
            "vsock conn: host data pushed"
        );
        self.rx_buf.extend(data);
        self.pending_rx.insert(PendingRx::Rw);
    }

    /// Notify the connection that the host-side stream has been closed.
    ///
    /// The next [`recv_pkt`](Self::recv_pkt) call will produce a SHUTDOWN
    /// packet once all buffered data has been delivered.
    pub fn notify_host_closed(&mut self) {
        self.host_closed = true;
        self.pending_rx.insert(PendingRx::Rw);
    }

    /// Flush buffered TX data (guest → host) to the given writer.
    ///
    /// Returns the number of bytes flushed. Also updates credit counters
    /// and may trigger pending RX indications (RST if shutdown draining
    /// is complete, or `CREDIT_UPDATE` if peer's view is stale).
    ///
    /// # Errors
    ///
    /// Returns [`VsockError::TxBufFlush`] on I/O error.
    pub fn flush_tx_buf<W: std::io::Write>(&mut self, sink: &mut W) -> Result<usize, VsockError> {
        let flushed = self.tx_buf.flush_to(sink)?;
        self.fwd_cnt += Wrapping(u32::try_from(flushed).unwrap_or(u32::MAX));

        // If shutdown was waiting for TX drain, check if we can now send RST.
        if self.state == ConnState::PeerClosed(true, true) && self.tx_buf.is_empty() {
            self.pending_rx.insert(PendingRx::Rst);
        } else if self.peer_needs_credit_update() {
            self.pending_rx.insert(PendingRx::CreditUpdate);
        }

        Ok(flushed)
    }

    // ── Packet interface ─────────────────────────────────────────────

    /// Process a guest-sent TX packet, updating connection state.
    ///
    /// Called when the TX virtqueue yields a packet from the guest.
    /// The `payload` is the data following the 44-byte header (empty for
    /// control packets).
    ///
    /// # Errors
    ///
    /// Returns [`VsockError::TxBufFull`] if an RW packet's payload cannot
    /// be buffered (should not happen if credit flow control is working).
    pub fn send_pkt(&mut self, pkt: &VsockPacket, payload: &[u8]) -> Result<(), VsockError> {
        // Always update peer credit from the packet header.
        self.peer_buf_alloc = pkt.buf_alloc;
        self.peer_fwd_cnt = Wrapping(pkt.fwd_cnt);

        // If credit just became available and we have buffered host data,
        // schedule an Rw indication so the RX queue delivers it.
        if self.peer_avail_credit() > 0 && !self.rx_buf.is_empty() {
            self.pending_rx.insert(PendingRx::Rw);
        }

        match self.state {
            // Data transfer in established or half-closed-for-send connections.
            ConnState::Established | ConnState::PeerClosed(_, false)
                if pkt.op == VsockOp::Rw as u16 =>
            {
                if payload.is_empty() {
                    return Ok(());
                }
                self.tx_buf.push(payload)?;
                if self.peer_needs_credit_update() {
                    self.pending_rx.insert(PendingRx::CreditUpdate);
                }
            }

            // Guest confirms host-initiated connection.
            ConnState::LocalInit if pkt.op == VsockOp::Response as u16 => {
                self.expiry = None;
                self.state = ConnState::Established;
                tracing::debug!(
                    local_port = self.local_port,
                    peer_port = self.peer_port,
                    pending_rx_data = !self.rx_buf.is_empty(),
                    "vsock conn: guest RESPONSE received, connection established"
                );
            }

            // Guest initiates graceful shutdown on established connection.
            ConnState::Established if pkt.op == VsockOp::Shutdown as u16 => {
                let no_recv = pkt.flags & VSOCK_FLAGS_SHUTDOWN_RCV != 0;
                let no_send = pkt.flags & VSOCK_FLAGS_SHUTDOWN_SEND != 0;
                self.state = ConnState::PeerClosed(no_recv, no_send);
                if no_recv && no_send {
                    if self.tx_buf.is_empty() {
                        self.pending_rx.insert(PendingRx::Rst);
                    } else {
                        self.expiry =
                            Some(Instant::now() + Duration::from_millis(CONN_SHUTDOWN_TIMEOUT_MS));
                    }
                }
            }

            // Guest updates shutdown flags (flags are sticky — can only be set).
            ConnState::PeerClosed(ref mut no_recv, ref mut no_send)
                if pkt.op == VsockOp::Shutdown as u16 =>
            {
                *no_recv = *no_recv || (pkt.flags & VSOCK_FLAGS_SHUTDOWN_RCV != 0);
                *no_send = *no_send || (pkt.flags & VSOCK_FLAGS_SHUTDOWN_SEND != 0);
                if *no_recv && *no_send && self.tx_buf.is_empty() {
                    self.pending_rx.insert(PendingRx::Rst);
                }
            }

            // Credit update from peer (valid in data-receiving states).
            ConnState::Established | ConnState::PeerInit | ConnState::PeerClosed(false, _)
                if pkt.op == VsockOp::CreditUpdate as u16 =>
            {
                // Already updated peer credit above.
            }

            // Credit request from peer (valid in data-sending states).
            ConnState::Established | ConnState::PeerInit | ConnState::PeerClosed(_, false)
                if pkt.op == VsockOp::CreditRequest as u16 =>
            {
                self.pending_rx.insert(PendingRx::CreditUpdate);
            }

            _ => {
                // Invalid packet for current state — silently drop.
            }
        }

        Ok(())
    }

    /// Get the next packet to deliver to the guest via the RX virtqueue.
    ///
    /// Returns the packet header and payload. Control packets have an empty
    /// payload; data packets (`OP_RW`) include the data bytes.
    ///
    /// `max_data_len` limits the data payload to the available guest buffer.
    ///
    /// # Errors
    ///
    /// Returns [`VsockError::NoData`] if there are no pending RX indications.
    pub fn recv_pkt(&mut self, max_data_len: usize) -> Result<(VsockPacket, Vec<u8>), VsockError> {
        let mut pkt = self.init_pkt();

        // Priority 1: Forceful termination.
        if self.pending_rx.remove(PendingRx::Rst) {
            pkt.op = VsockOp::Rst as u16;
            return Ok((pkt, Vec::new()));
        }

        // Priority 2: Connection response (guest-initiated handshake).
        if self.pending_rx.remove(PendingRx::Response) {
            self.state = ConnState::Established;
            pkt.op = VsockOp::Response as u16;
            return Ok((pkt, Vec::new()));
        }

        // Priority 3: Connection request (host-initiated handshake).
        if self.pending_rx.remove(PendingRx::Request) {
            self.expiry = Some(Instant::now() + Duration::from_millis(CONN_REQUEST_TIMEOUT_MS));
            pkt.op = VsockOp::Request as u16;
            tracing::debug!(
                local_port = self.local_port,
                peer_port = self.peer_port,
                peer_cid = self.peer_cid,
                "vsock conn: REQUEST packet produced for guest"
            );
            return Ok((pkt, Vec::new()));
        }

        // Priority 4: Data packet.
        if self.pending_rx.remove(PendingRx::Rw) {
            return self.recv_data_pkt(pkt, max_data_len);
        }

        // Priority 5: Credit update (lowest priority — only if nothing else).
        if self.pending_rx.remove(PendingRx::CreditUpdate) && !self.has_pending_rx() {
            pkt.op = VsockOp::CreditUpdate as u16;
            self.last_fwd_cnt_to_peer = self.fwd_cnt;
            return Ok((pkt, Vec::new()));
        }

        Err(VsockError::NoData)
    }

    /// Handle the data-packet branch of `recv_pkt`.
    fn recv_data_pkt(
        &mut self,
        mut pkt: VsockPacket,
        max_data_len: usize,
    ) -> Result<(VsockPacket, Vec<u8>), VsockError> {
        match self.state {
            ConnState::Established | ConnState::PeerClosed(false, _) => {}
            ConnState::LocalInit | ConnState::PeerInit => {
                // Connection still in handshake — defer data delivery.
                // Host data arrived before the guest processed the REQUEST/RESPONSE.
                // Re-insert PendingRx::Rw so data is retried after Established.
                self.pending_rx.insert(PendingRx::Rw);
                return Err(VsockError::NoData);
            }
            _ => {
                // Invalid state for data — send RST.
                pkt.op = VsockOp::Rst as u16;
                return Ok((pkt, Vec::new()));
            }
        }

        // Check if peer has buffer space.
        if self.need_credit_update_from_peer() {
            self.last_fwd_cnt_to_peer = self.fwd_cnt;
            pkt.op = VsockOp::CreditRequest as u16;
            // Re-insert Rw so data delivery is retried once credit arrives.
            self.pending_rx.insert(PendingRx::Rw);
            return Ok((pkt, Vec::new()));
        }
        if self.rx_buf.is_empty() {
            if self.host_closed {
                // Host stream closed — tell guest to shut down.
                self.state = ConnState::LocalClosed;
                self.expiry =
                    Some(Instant::now() + Duration::from_millis(CONN_SHUTDOWN_TIMEOUT_MS));
                pkt.op = VsockOp::Shutdown as u16;
                pkt.flags = VSOCK_FLAGS_SHUTDOWN_RCV | VSOCK_FLAGS_SHUTDOWN_SEND;
                return Ok((pkt, Vec::new()));
            }
            // No data available yet — nothing to produce.
            return Err(VsockError::NoData);
        }

        if max_data_len == 0 {
            // The guest provided a header-only RX buffer. Keep the data pending
            // until a descriptor chain arrives with payload capacity.
            self.pending_rx.insert(PendingRx::Rw);
            return Err(VsockError::NoData);
        }

        // Drain up to min(max_data_len, peer_avail_credit, rx_buf.len()).
        let max_len = std::cmp::min(max_data_len, self.peer_avail_credit() as usize);
        let drain_len = std::cmp::min(max_len, self.rx_buf.len());
        let payload: Vec<u8> = self.rx_buf.drain(..drain_len).collect();

        pkt.op = VsockOp::Rw as u16;
        pkt.len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
        self.rx_cnt += Wrapping(pkt.len);
        self.last_fwd_cnt_to_peer = self.fwd_cnt;

        tracing::debug!(
            local_port = self.local_port,
            peer_port = self.peer_port,
            data_len = payload.len(),
            remaining = self.rx_buf.len(),
            "vsock conn: Rw data delivered to guest"
        );

        Ok((pkt, payload))
    }

    // ── Credit flow control ──────────────────────────────────────────

    /// How many bytes we can send to the guest without overflowing
    /// its advertised buffer.
    #[must_use]
    pub fn peer_avail_credit(&self) -> u32 {
        (Wrapping(self.peer_buf_alloc) - (self.rx_cnt - self.peer_fwd_cnt)).0
    }

    /// Returns `true` if the peer has no available buffer space.
    fn need_credit_update_from_peer(&self) -> bool {
        self.peer_avail_credit() == 0
    }

    /// Returns `true` if the peer's view of our free buffer space is stale
    /// enough that we should send a proactive `CREDIT_UPDATE`.
    fn peer_needs_credit_update(&self) -> bool {
        let peer_seen_free =
            Wrapping(CONN_TX_BUF_SIZE) - (self.fwd_cnt - self.last_fwd_cnt_to_peer);
        peer_seen_free < Wrapping(CONN_CREDIT_UPDATE_THRESHOLD)
    }

    /// Build a packet with common header fields set.
    fn init_pkt(&self) -> VsockPacket {
        VsockPacket {
            src_cid: self.local_cid,
            dst_cid: self.peer_cid,
            src_port: self.local_port,
            dst_port: self.peer_port,
            len: 0,
            pkt_type: VSOCK_TYPE_STREAM,
            op: 0,
            flags: 0,
            buf_alloc: CONN_TX_BUF_SIZE,
            fwd_cnt: self.fwd_cnt.0,
        }
    }
}

#[cfg(test)]
#[path = "vsock_test.rs"]
mod tests;
