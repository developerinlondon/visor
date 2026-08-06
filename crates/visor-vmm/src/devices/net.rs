//! Virtio network device backend (`virtio-net`).
//!
//! Provides a virtual network interface to the guest. Handles device
//! configuration, feature negotiation, and packet I/O through virtqueue
//! descriptor chains and a pluggable [`PacketIo`] backend.
//!
//! # Config space layout (virtio-net spec)
//!
//! - Offset `0..5`: MAC address (6 bytes).

use crate::memory::GuestMemory;
use crate::transport::{
    DeviceType, VIRTQ_DESC_F_NEXT, VIRTQ_DESC_F_WRITE, VirtQueue, VirtioDevice, VirtioError,
    VirtqDesc,
};

// ── Feature flags ────────────────────────────────────────────────────

/// Virtio feature: device has given MAC address.
pub const VIRTIO_NET_F_MAC: u64 = 1 << 5;

/// Virtio feature: link status reporting.
pub const VIRTIO_NET_F_STATUS: u64 = 1 << 16;

/// Virtio feature: modern virtio (version 1).
pub const VIRTIO_F_VERSION_1: u64 = 1 << 32;

// ── Constants ────────────────────────────────────────────────────────

/// Maximum virtqueue size for the net device.
const QUEUE_MAX_SIZE: u16 = 256;

/// Number of virtqueues (net device has rx + tx).
const NUM_QUEUES: usize = 2;

/// Length of the MAC address in bytes.
const MAC_LEN: usize = 6;

/// Size of the modern `virtio_net_hdr_v1` header in bytes.
const VNET_HDR_SIZE: usize = 12;

/// RX virtqueue index (host → guest).
const RXQ: usize = 0;

/// TX virtqueue index (guest → host).
const TXQ: usize = 1;

/// Maximum ethernet frame size (standard MTU 1500 + ethernet header 14).
const MAX_FRAME_SIZE: usize = 1514;

// ── Errors ───────────────────────────────────────────────────────────

/// Errors from net device operations.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum NetError {
    /// Guest memory access error.
    #[error("guest memory error: {0}")]
    Memory(crate::memory::MemoryError),
    /// Invalid virtqueue descriptor.
    #[error("invalid descriptor: {0}")]
    InvalidDescriptor(String),
}

// ── PacketIo trait ───────────────────────────────────────────────────

/// Trait for sending and receiving raw ethernet frames.
///
/// Implemented by platform backends (vmnet on macOS, TAP on Linux)
/// and by [`MockPacketIo`] in tests.
pub trait PacketIo: Send {
    /// Sends a raw ethernet frame to the network.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the send fails.
    fn send(&mut self, buf: &[u8]) -> Result<usize, std::io::Error>;

    /// Attempts to receive a raw ethernet frame without blocking.
    ///
    /// Returns `Err(WouldBlock)` when no packets are available.
    ///
    /// # Errors
    ///
    /// Returns an I/O error if the receive fails.
    fn try_recv(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error>;
}

// ── NetDevice ────────────────────────────────────────────────────────

/// Virtio network device presenting a guest NIC.
///
/// Implements [`VirtioDevice`] for use with the MMIO transport.
/// The MAC address is exposed to the guest via the config space.
#[non_exhaustive]
pub struct NetDevice {
    /// MAC address for the guest NIC.
    mac_addr: [u8; MAC_LEN],
    /// Feature bits offered by the device.
    avail_features: u64,
    /// Feature bits acknowledged by the driver.
    acked_features: u64,
    /// Virtqueues (rx at index 0, tx at index 1).
    queues: Vec<VirtQueue>,
    /// Whether the device has been activated by the driver.
    activated: bool,
    /// Pluggable packet I/O backend for send/recv.
    packet_io: Option<Box<dyn PacketIo>>,
}

impl std::fmt::Debug for NetDevice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NetDevice")
            .field("mac_addr", &self.mac_addr)
            .field("avail_features", &self.avail_features)
            .field("acked_features", &self.acked_features)
            .field("queues", &self.queues)
            .field("activated", &self.activated)
            .field("packet_io", &self.packet_io.as_ref().map(|_| "<PacketIo>"))
            .finish_non_exhaustive()
    }
}

impl NetDevice {
    /// Creates a new net device with the given MAC address.
    #[must_use]
    pub fn new(mac_addr: [u8; MAC_LEN]) -> Self {
        let avail_features = VIRTIO_NET_F_MAC | VIRTIO_F_VERSION_1;

        let queues = (0..NUM_QUEUES)
            .map(|_| VirtQueue::new(QUEUE_MAX_SIZE))
            .collect();

        Self {
            mac_addr,
            avail_features,
            acked_features: 0,
            queues,
            activated: false,
            packet_io: None,
        }
    }

    /// Creates a new net device with the given MAC address and packet I/O backend.
    #[must_use]
    pub fn with_packet_io(mac_addr: [u8; MAC_LEN], io: Box<dyn PacketIo>) -> Self {
        let mut dev = Self::new(mac_addr);
        dev.packet_io = Some(io);
        dev
    }

    /// Generates a default locally-administered MAC address.
    ///
    /// Returns `[0x02, 0x56, 0x49, 0x53, 0x00, 0x01]` — the `0x02` prefix
    /// marks it as locally administered, and `0x56 0x49 0x53` spells "VIS"
    /// (visor) in ASCII.
    #[must_use]
    pub fn generate_mac() -> [u8; MAC_LEN] {
        [0x02, 0x56, 0x49, 0x53, 0x00, 0x01]
    }

    /// Returns the MAC address configured for this device.
    #[must_use]
    pub fn mac_addr(&self) -> &[u8; MAC_LEN] {
        &self.mac_addr
    }
}

impl VirtioDevice for NetDevice {
    fn device_type(&self) -> DeviceType {
        DeviceType::Net
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
        let config_bytes = self.mac_addr;
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
        // Config is read-only for net devices — no-op.
    }

    fn activate(&mut self) -> Result<(), VirtioError> {
        self.activated = true;
        Ok(())
    }

    fn is_activated(&self) -> bool {
        self.activated
    }

    fn reset(&mut self) {
        self.activated = false;
        for queue in &mut self.queues {
            queue.reset();
        }
    }

    fn process_queue(
        &mut self,
        queue_idx: usize,
        memory: &GuestMemory,
    ) -> Result<bool, VirtioError> {
        if self.packet_io.is_none() {
            return Ok(false);
        }
        match queue_idx {
            TXQ => {
                let mut queue_state = self.queues[TXQ].clone();
                let result = self.process_tx_queue(memory, &mut queue_state);
                self.queues[TXQ].last_avail_idx = queue_state.last_avail_idx;
                self.queues[TXQ].last_used_idx = queue_state.last_used_idx;
                match result {
                    Ok(processed) => Ok(processed),
                    Err(_) => Ok(false),
                }
            }
            RXQ => {
                let mut queue_state = self.queues[RXQ].clone();
                let result = self.process_rx_queue(memory, &mut queue_state);
                self.queues[RXQ].last_avail_idx = queue_state.last_avail_idx;
                self.queues[RXQ].last_used_idx = queue_state.last_used_idx;
                match result {
                    Ok(processed) => Ok(processed),
                    Err(_) => Ok(false),
                }
            }
            _ => Ok(false),
        }
    }
}

// ── I/O processing ────────────────────────────────────────────────────

impl NetDevice {
    /// Reads a single descriptor from the descriptor table in guest memory.
    fn read_desc(memory: &GuestMemory, queue: &VirtQueue, idx: u16) -> Result<VirtqDesc, NetError> {
        if idx >= queue.size {
            return Err(NetError::InvalidDescriptor(format!(
                "descriptor index {idx} >= queue size {}",
                queue.size
            )));
        }
        let addr = queue.desc_table_addr + u64::from(idx) * 16;
        let bytes = memory.read_bytes(addr, 16).map_err(NetError::Memory)?;
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
    fn read_avail_idx(memory: &GuestMemory, queue: &VirtQueue) -> Result<u16, NetError> {
        let bytes = memory
            .read_bytes(queue.avail_ring_addr + 2, 2)
            .map_err(NetError::Memory)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Writes a used ring entry (id + len) at the current `last_used_idx` position.
    fn write_used_entry(
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
        written: u32,
    ) -> Result<(), NetError> {
        let used_offset = 4 + u64::from(queue.last_used_idx % queue.size) * 8;
        let used_addr = queue.used_ring_addr + used_offset;
        memory
            .write_bytes(used_addr, &u32::from(head_idx).to_le_bytes())
            .map_err(NetError::Memory)?;
        memory
            .write_bytes(used_addr + 4, &written.to_le_bytes())
            .map_err(NetError::Memory)?;
        Ok(())
    }

    /// Updates the used ring index so the guest sees new completed entries.
    fn write_used_idx(memory: &GuestMemory, queue: &VirtQueue) -> Result<(), NetError> {
        memory
            .write_bytes(queue.used_ring_addr + 2, &queue.last_used_idx.to_le_bytes())
            .map_err(NetError::Memory)?;
        Ok(())
    }

    // ── TX queue (guest → host) ─────────────────────────────────────

    /// Processes all pending TX descriptor chains.
    ///
    /// Reads the virtio-net header + ethernet frame from each chain,
    /// strips the header, and sends the frame via the packet I/O backend.
    fn process_tx_queue(
        &mut self,
        memory: &GuestMemory,
        queue: &mut VirtQueue,
    ) -> Result<bool, NetError> {
        if !queue.ready || queue.size == 0 {
            return Ok(false);
        }

        let avail_idx = Self::read_avail_idx(memory, queue)?;
        let mut processed = false;

        while queue.last_avail_idx != avail_idx {
            let avail_offset = 4 + u64::from(queue.last_avail_idx % queue.size) * 2;
            let desc_idx_bytes = memory
                .read_bytes(queue.avail_ring_addr + avail_offset, 2)
                .map_err(NetError::Memory)?;
            let head_idx = u16::from_le_bytes([desc_idx_bytes[0], desc_idx_bytes[1]]);

            // Non-fatal: individual TX errors drop the packet silently.
            let _ = self.handle_tx_chain(memory, queue, head_idx);

            // TX used ring entry always has len=0 (we consumed, not wrote).
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

    /// Handles a single TX descriptor chain: reads vnet header + frame, sends frame.
    fn handle_tx_chain(
        &mut self,
        memory: &GuestMemory,
        queue: &VirtQueue,
        head_idx: u16,
    ) -> Result<(), NetError> {
        let header_desc = Self::read_desc(memory, queue, head_idx)?;

        // Read all bytes from the descriptor chain (header + potentially frame).
        let mut chain_data = Vec::new();
        let first_bytes = memory
            .read_bytes(header_desc.addr, header_desc.len as usize)
            .map_err(NetError::Memory)?;
        chain_data.extend_from_slice(&first_bytes);

        // Walk any chained descriptors.
        let mut current = header_desc;
        let mut visited = 1u32;
        while current.flags & VIRTQ_DESC_F_NEXT != 0 {
            visited += 1;
            if visited > u32::from(queue.size) {
                return Err(NetError::InvalidDescriptor(
                    "descriptor chain cycle detected".into(),
                ));
            }
            let next = Self::read_desc(memory, queue, current.next)?;
            let bytes = memory
                .read_bytes(next.addr, next.len as usize)
                .map_err(NetError::Memory)?;
            chain_data.extend_from_slice(&bytes);
            current = next;
        }

        // Must have at least the virtio-net header.
        if chain_data.len() < VNET_HDR_SIZE {
            return Ok(());
        }

        // Strip the virtio-net header, send only the ethernet frame.
        let frame = &chain_data[VNET_HDR_SIZE..];
        if !frame.is_empty() {
            if let Some(ref mut io) = self.packet_io {
                let _ = io.send(frame);
            }
        }

        Ok(())
    }

    // ── RX queue (host → guest) ─────────────────────────────────────

    /// Fills guest-provided RX buffers with frames from the packet I/O backend.
    ///
    /// Calls `try_recv()` in a loop until `WouldBlock`, writing a zeroed
    /// virtio-net header (all zeros = no offload) followed by the frame data
    /// into each available RX descriptor chain.
    fn process_rx_queue(
        &mut self,
        memory: &GuestMemory,
        queue: &mut VirtQueue,
    ) -> Result<bool, NetError> {
        if !queue.ready || queue.size == 0 {
            return Ok(false);
        }

        let avail_idx = Self::read_avail_idx(memory, queue)?;
        let mut processed = false;

        while queue.last_avail_idx != avail_idx {
            // Try to receive a packet from the backend.
            let mut frame_buf = [0u8; MAX_FRAME_SIZE];
            let frame_len = match self.packet_io {
                Some(ref mut io) => match io.try_recv(&mut frame_buf) {
                    Ok(n) => n,
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(_) => break,
                },
                None => break,
            };

            if frame_len == 0 {
                break;
            }

            // Read the RX descriptor chain.
            let avail_offset = 4 + u64::from(queue.last_avail_idx % queue.size) * 2;
            let desc_idx_bytes = memory
                .read_bytes(queue.avail_ring_addr + avail_offset, 2)
                .map_err(NetError::Memory)?;
            let head_idx = u16::from_le_bytes([desc_idx_bytes[0], desc_idx_bytes[1]]);
            let header_desc = Self::read_desc(memory, queue, head_idx)?;

            // RX descriptors must be device-writable.
            if header_desc.flags & VIRTQ_DESC_F_WRITE == 0 {
                break;
            }

            // Build the payload: zeroed virtio-net header + frame.
            let vnet_hdr = [0u8; VNET_HDR_SIZE];
            let total_payload_len = VNET_HDR_SIZE + frame_len;

            // Write into the descriptor chain.
            let total_written = Self::write_rx_chain(
                memory,
                queue,
                &header_desc,
                &vnet_hdr,
                &frame_buf[..frame_len],
            )?;

            // If we couldn't write anything, the descriptor was too small.
            if total_written == 0 && total_payload_len > 0 {
                break;
            }

            Self::write_used_entry(memory, queue, head_idx, total_written)?;
            queue.last_avail_idx = queue.last_avail_idx.wrapping_add(1);
            queue.last_used_idx = queue.last_used_idx.wrapping_add(1);
            processed = true;
        }

        if processed {
            Self::write_used_idx(memory, queue)?;
        }

        Ok(processed)
    }

    /// Writes the vnet header and frame data into an RX descriptor chain.
    ///
    /// Returns the total number of bytes written across all descriptors.
    fn write_rx_chain(
        memory: &GuestMemory,
        queue: &VirtQueue,
        first_desc: &VirtqDesc,
        vnet_hdr: &[u8],
        frame: &[u8],
    ) -> Result<u32, NetError> {
        // Concatenate header + frame for simpler writing logic.
        let mut payload = Vec::with_capacity(vnet_hdr.len() + frame.len());
        payload.extend_from_slice(vnet_hdr);
        payload.extend_from_slice(frame);

        let mut written: u32 = 0;
        let mut remaining = &payload[..];
        let mut desc = *first_desc;
        let mut visited = 0u32;

        loop {
            visited += 1;
            if visited > u32::from(queue.size) {
                return Err(NetError::InvalidDescriptor(
                    "descriptor chain cycle detected".into(),
                ));
            }

            let write_len = std::cmp::min(remaining.len(), desc.len as usize);
            if write_len > 0 {
                memory
                    .write_bytes(desc.addr, &remaining[..write_len])
                    .map_err(NetError::Memory)?;
                written = written.saturating_add(u32::try_from(write_len).unwrap_or(u32::MAX));
                remaining = &remaining[write_len..];
            }

            if remaining.is_empty() || desc.flags & VIRTQ_DESC_F_NEXT == 0 {
                break;
            }
            desc = Self::read_desc(memory, queue, desc.next)?;
        }

        Ok(written)
    }
}
#[cfg(test)]
#[path = "net_test.rs"]
mod tests;
