//! Ethernet frame parsing and in-process packet forwarding.
//!
//! Provides [`EthernetFrame`] for zero-copy frame parsing and [`PacketSwitch`]
//! for in-process packet forwarding between VM network ports. Frames are
//! delivered via tokio mpsc channels — no kernel TAP path needed.
//!
//! # Architecture
//!
//! ```text
//! VM-A virtio-net → PacketSwitch.forward_frame() → VM-B rx channel
//!                                                 → VM-C rx channel (broadcast)
//! ```

use std::collections::HashMap;
use std::net::Ipv4Addr;

use anyhow::{bail, Context};
use tokio::sync::mpsc;

use super::switch::MacAddr;

/// Ethernet frame ethertype for IPv4 (0x0800).
pub const ETHERTYPE_IPV4: u16 = 0x0800;

/// Ethernet frame ethertype for ARP (0x0806).
pub const ETHERTYPE_ARP: u16 = 0x0806;

/// Ethernet frame ethertype for IPv6 (0x86DD).
pub const ETHERTYPE_IPV6: u16 = 0x86DD;

/// Minimum Ethernet frame size (6 dst + 6 src + 2 ethertype = 14 bytes).
const MIN_FRAME_SIZE: usize = 14;

/// Broadcast MAC address (ff:ff:ff:ff:ff:ff).
const BROADCAST_MAC: [u8; 6] = [0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF];

/// Default channel buffer size for packet delivery.
const DEFAULT_CHANNEL_CAPACITY: usize = 256;

// ── EthernetFrame ─────────────────────────────────────────────────

/// A parsed Ethernet frame (header + payload reference).
///
/// Provides zero-copy access to frame fields. The frame data is borrowed
/// from the caller's buffer.
#[non_exhaustive]
pub struct EthernetFrame<'a> {
    /// Raw frame bytes (header + payload).
    data: &'a [u8],
}

impl<'a> EthernetFrame<'a> {
    /// Parse an Ethernet frame from raw bytes.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame is shorter than 14 bytes (minimum Ethernet header).
    pub fn parse(data: &'a [u8]) -> anyhow::Result<Self> {
        if data.len() < MIN_FRAME_SIZE {
            bail!(
                "frame too short: {} bytes (minimum {})",
                data.len(),
                MIN_FRAME_SIZE
            );
        }
        Ok(Self { data })
    }

    /// Returns the destination MAC address.
    #[must_use]
    pub fn dst_mac(&self) -> MacAddr {
        let mut octets = [0u8; 6];
        octets.copy_from_slice(&self.data[0..6]);
        MacAddr::new(octets)
    }

    /// Returns the source MAC address.
    #[must_use]
    pub fn src_mac(&self) -> MacAddr {
        let mut octets = [0u8; 6];
        octets.copy_from_slice(&self.data[6..12]);
        MacAddr::new(octets)
    }

    /// Returns the `EtherType` field (big-endian u16).
    #[must_use]
    pub fn ethertype(&self) -> u16 {
        u16::from_be_bytes([self.data[12], self.data[13]])
    }

    /// Returns the frame payload (everything after the 14-byte header).
    #[must_use]
    pub fn payload(&self) -> &'a [u8] {
        &self.data[MIN_FRAME_SIZE..]
    }

    /// Returns `true` if the destination is the broadcast address.
    #[must_use]
    pub fn is_broadcast(&self) -> bool {
        self.data[0..6] == BROADCAST_MAC
    }

    /// Returns `true` if the ethertype indicates an ARP frame.
    #[must_use]
    pub fn is_arp(&self) -> bool {
        self.ethertype() == ETHERTYPE_ARP
    }

    /// Returns `true` if the ethertype indicates an IPv4 frame.
    #[must_use]
    pub fn is_ipv4(&self) -> bool {
        self.ethertype() == ETHERTYPE_IPV4
    }

    /// Returns the raw frame bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &'a [u8] {
        self.data
    }

    /// Build an Ethernet frame from components.
    ///
    /// Returns a `Vec<u8>` containing the full frame (header + payload).
    #[must_use]
    pub fn build(dst: MacAddr, src: MacAddr, ethertype: u16, payload: &[u8]) -> Vec<u8> {
        let mut frame = Vec::with_capacity(MIN_FRAME_SIZE + payload.len());
        frame.extend_from_slice(&dst.octets());
        frame.extend_from_slice(&src.octets());
        frame.extend_from_slice(&ethertype.to_be_bytes());
        frame.extend_from_slice(payload);
        frame
    }
}

// ── Forwarding metrics ────────────────────────────────────────────

/// Packet forwarding metrics for a [`PacketSwitch`].
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct SwitchMetrics {
    /// Total unicast frames successfully forwarded.
    pub frames_forwarded: u64,
    /// Frames dropped (unknown destination, channel full, etc.).
    pub frames_dropped: u64,
    /// Broadcast frames sent.
    pub frames_broadcast: u64,
    /// Total bytes forwarded (including headers).
    pub bytes_forwarded: u64,
}

// ── Port entry ────────────────────────────────────────────────────

/// A registered port on the packet switch with its delivery channel.
struct SwitchPort {
    /// VM identifier.
    vm_id: String,
    /// MAC address of this port.
    mac: MacAddr,
    /// IP address assigned to this port.
    ip: Ipv4Addr,
    /// Sender half of the packet delivery channel.
    tx: mpsc::Sender<Vec<u8>>,
}

// ── PacketSwitch ──────────────────────────────────────────────────

/// In-process Ethernet frame switch with per-port delivery channels.
///
/// Each registered port gets a tokio mpsc channel. Incoming frames are
/// parsed, looked up by destination MAC, and delivered to the target
/// port's channel. Broadcast frames are delivered to all ports except
/// the sender.
///
/// # Thread Safety
///
/// `PacketSwitch` is `Send` but not `Sync` (it holds `mpsc::Sender`).
/// Wrap in `Arc<Mutex<...>>` for shared access across async tasks.
#[non_exhaustive]
pub struct PacketSwitch {
    /// Network name.
    name: String,
    /// Subnet base address.
    subnet_base: Ipv4Addr,
    /// Subnet prefix length.
    subnet_prefix: u8,
    /// Gateway address.
    gateway: Ipv4Addr,
    /// MAC → port mapping (forwarding table + channels).
    ports: HashMap<MacAddr, SwitchPort>,
    /// VM ID → MAC reverse lookup.
    vm_table: HashMap<String, MacAddr>,
    /// Forwarding metrics.
    metrics: SwitchMetrics,
}

impl PacketSwitch {
    /// Create a new packet switch for the given network.
    #[must_use]
    pub fn new(name: &str, subnet_base: Ipv4Addr, subnet_prefix: u8, gateway: Ipv4Addr) -> Self {
        Self {
            name: name.to_owned(),
            subnet_base,
            subnet_prefix,
            gateway,
            ports: HashMap::new(),
            vm_table: HashMap::new(),
            metrics: SwitchMetrics::default(),
        }
    }

    /// Create a packet switch for the default visor network.
    #[must_use]
    pub fn default_network() -> Self {
        Self::new(
            super::DEFAULT_NETWORK_NAME,
            super::DEFAULT_SUBNET_BASE,
            super::DEFAULT_SUBNET_PREFIX,
            super::DEFAULT_GATEWAY,
        )
    }

    /// Returns the network name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the subnet base address.
    #[must_use]
    pub fn subnet_base(&self) -> Ipv4Addr {
        self.subnet_base
    }

    /// Returns the subnet prefix length.
    #[must_use]
    pub fn subnet_prefix(&self) -> u8 {
        self.subnet_prefix
    }

    /// Returns the gateway address.
    #[must_use]
    pub fn gateway(&self) -> Ipv4Addr {
        self.gateway
    }

    /// Returns the number of registered ports.
    #[must_use]
    pub fn port_count(&self) -> usize {
        self.ports.len()
    }

    /// Check whether a port with the given MAC is registered.
    #[must_use]
    pub fn has_port(&self, mac: &MacAddr) -> bool {
        self.ports.contains_key(mac)
    }

    /// Returns current forwarding metrics.
    #[must_use]
    pub fn metrics(&self) -> &SwitchMetrics {
        &self.metrics
    }

    /// Register a VM port and return its packet receive channel.
    ///
    /// The returned `mpsc::Receiver<Vec<u8>>` delivers raw Ethernet frames
    /// forwarded to this port. Each frame is a complete Ethernet frame including
    /// the 14-byte header.
    ///
    /// # Errors
    ///
    /// Returns an error if the MAC or IP is already registered.
    pub fn register_port(
        &mut self,
        vm_id: &str,
        mac: MacAddr,
        ip: Ipv4Addr,
    ) -> anyhow::Result<mpsc::Receiver<Vec<u8>>> {
        if self.ports.contains_key(&mac) {
            bail!("MAC {mac} is already registered on switch '{}'", self.name);
        }
        // Check IP uniqueness across all ports
        for port in self.ports.values() {
            if port.ip == ip {
                bail!("IP {ip} is already registered on switch '{}'", self.name);
            }
        }

        let (tx, rx) = mpsc::channel(DEFAULT_CHANNEL_CAPACITY);
        let port = SwitchPort {
            vm_id: vm_id.to_owned(),
            mac,
            ip,
            tx,
        };

        self.ports.insert(mac, port);
        self.vm_table.insert(vm_id.to_owned(), mac);

        Ok(rx)
    }

    /// Unregister a VM port from the switch.
    ///
    /// Drops the port's delivery channel sender, causing any pending
    /// receives to return `None`.
    ///
    /// # Errors
    ///
    /// Returns an error if the MAC is not registered.
    pub fn unregister_port(&mut self, mac: &MacAddr) -> anyhow::Result<()> {
        let port = self
            .ports
            .remove(mac)
            .context(format!("MAC {mac} is not registered on switch '{}'", self.name))?;

        self.vm_table.remove(&port.vm_id);
        Ok(())
    }

    /// Forward an Ethernet frame to the appropriate port(s).
    ///
    /// - **Unicast**: Looks up destination MAC and delivers to that port.
    /// - **Broadcast**: Delivers to all ports except the sender.
    /// - **Unknown destination**: Frame is dropped (returns 0).
    ///
    /// Returns the number of ports the frame was delivered to.
    ///
    /// # Errors
    ///
    /// Returns an error if the frame cannot be parsed.
    pub fn forward_frame(&mut self, frame: &EthernetFrame<'_>) -> anyhow::Result<usize> {
        let src_mac = frame.src_mac();
        let dst_mac = frame.dst_mac();
        let frame_bytes = frame.as_bytes();

        if frame.is_broadcast() {
            // Broadcast: deliver to all ports except sender
            let mut delivered = 0;
            let senders: Vec<_> = self
                .ports
                .iter()
                .filter(|(mac, _)| **mac != src_mac)
                .map(|(_, port)| port.tx.clone())
                .collect();

            for tx in senders {
                if tx.try_send(frame_bytes.to_vec()).is_ok() {
                    delivered += 1;
                }
            }

            self.metrics.frames_broadcast += 1;
            self.metrics.frames_forwarded += delivered as u64;
            self.metrics.bytes_forwarded += (frame_bytes.len() as u64) * delivered as u64;
            Ok(delivered)
        } else {
            // Unicast: look up destination port
            match self.ports.get(&dst_mac) {
                Some(port) if port.mac != src_mac => {
                    // Don't loopback to sender
                    if port.tx.try_send(frame_bytes.to_vec()).is_ok() {
                        self.metrics.frames_forwarded += 1;
                        self.metrics.bytes_forwarded += frame_bytes.len() as u64;
                        Ok(1)
                    } else {
                        self.metrics.frames_dropped += 1;
                        Ok(0)
                    }
                }
                _ => {
                    // Unknown destination or loopback — drop
                    self.metrics.frames_dropped += 1;
                    Ok(0)
                }
            }
        }
    }

    /// Look up a port's MAC by VM identifier.
    #[must_use]
    pub fn lookup_vm(&self, vm_id: &str) -> Option<MacAddr> {
        self.vm_table.get(vm_id).copied()
    }

    /// Look up a port's VM ID by MAC address.
    #[must_use]
    pub fn lookup_mac(&self, mac: &MacAddr) -> Option<&str> {
        self.ports.get(mac).map(|p| p.vm_id.as_str())
    }
}

#[cfg(test)]
#[path = "packet_test.rs"]
mod tests;
