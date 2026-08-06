//! Virtual switch for network membership and MAC-based forwarding.
//!
//! Manages which VMs are connected to a network and provides lookup tables
//! for routing packets by MAC address, IP address, or VM identifier.
//!
//! Actual packet forwarding (memory copies between virtqueues) is handled by
//! `visor-vmm`'s virtio-net device — this module only manages the routing tables.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::net::Ipv4Addr;

use anyhow::bail;

/// A 6-byte MAC address.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MacAddr {
    /// The 6 octets of the MAC address.
    octets: [u8; 6],
}

impl MacAddr {
    /// Create a MAC address from raw octets.
    #[must_use]
    pub fn new(octets: [u8; 6]) -> Self {
        Self { octets }
    }

    /// Returns the raw octets.
    #[must_use]
    pub fn octets(&self) -> [u8; 6] {
        self.octets
    }

    /// Generate a deterministic, locally-administered MAC address.
    ///
    /// Uses the network name and VM index to produce a repeatable MAC.
    /// The result has the locally-administered bit set and the multicast bit clear.
    #[must_use]
    pub fn generate(network_name: &str, vm_index: u32) -> Self {
        use std::hash::Hasher;
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        network_name.hash(&mut hasher);
        vm_index.hash(&mut hasher);
        let hash = hasher.finish();

        let bytes = hash.to_le_bytes();
        let mut octets = [bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]];
        // Set locally-administered bit, clear multicast bit
        octets[0] = (octets[0] | 0x02) & 0xFE;
        Self { octets }
    }
}

impl fmt::Display for MacAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self.octets[0],
            self.octets[1],
            self.octets[2],
            self.octets[3],
            self.octets[4],
            self.octets[5],
        )
    }
}

/// Information about a VM's network port on the virtual switch.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct VmNetPort {
    /// VM identifier.
    vm_id: String,
    /// MAC address assigned to this port.
    mac: MacAddr,
    /// IP address assigned to this port.
    ip: Ipv4Addr,
}

impl VmNetPort {
    /// Returns the VM identifier.
    #[must_use]
    pub fn vm_id(&self) -> &str {
        &self.vm_id
    }

    /// Returns the MAC address.
    #[must_use]
    pub fn mac(&self) -> MacAddr {
        self.mac
    }

    /// Returns the IP address.
    #[must_use]
    pub fn ip(&self) -> Ipv4Addr {
        self.ip
    }
}

/// Virtual switch managing network membership and routing tables.
///
/// Routes packets between VM network ports based on MAC addresses.
/// Manages port registration/unregistration and provides lookup by MAC, IP, or VM ID.
#[non_exhaustive]
pub struct VirtualSwitch {
    /// Network name (e.g., "visor0").
    name: String,
    /// Subnet base address.
    subnet_base: Ipv4Addr,
    /// Subnet prefix length.
    subnet_prefix: u8,
    /// Gateway address.
    gateway: Ipv4Addr,
    /// MAC → port mapping (forwarding table).
    mac_table: HashMap<MacAddr, VmNetPort>,
    /// IP → MAC reverse lookup.
    ip_table: HashMap<Ipv4Addr, MacAddr>,
    /// VM ID → MAC reverse lookup.
    vm_table: HashMap<String, MacAddr>,
}

impl VirtualSwitch {
    /// Create a new virtual switch for the given network.
    #[must_use]
    pub fn new(name: &str, subnet_base: Ipv4Addr, subnet_prefix: u8, gateway: Ipv4Addr) -> Self {
        Self {
            name: name.to_owned(),
            subnet_base,
            subnet_prefix,
            gateway,
            mac_table: HashMap::new(),
            ip_table: HashMap::new(),
            vm_table: HashMap::new(),
        }
    }

    /// Create a virtual switch for the default visor network.
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
        self.mac_table.len()
    }

    /// Check whether a port with the given MAC is registered.
    #[must_use]
    pub fn has_port(&self, mac: &MacAddr) -> bool {
        self.mac_table.contains_key(mac)
    }

    /// Returns a snapshot of all registered ports.
    #[must_use]
    pub fn ports(&self) -> Vec<&VmNetPort> {
        self.mac_table.values().collect()
    }

    /// Register a new VM port on the switch.
    ///
    /// # Errors
    ///
    /// Returns an error if the MAC or IP is already registered.
    pub fn register_port(&mut self, vm_id: &str, mac: MacAddr, ip: Ipv4Addr) -> anyhow::Result<()> {
        if self.mac_table.contains_key(&mac) {
            bail!("MAC {mac} is already registered on switch '{}'", self.name);
        }
        if self.ip_table.contains_key(&ip) {
            bail!("IP {ip} is already registered on switch '{}'", self.name);
        }

        let port = VmNetPort {
            vm_id: vm_id.to_owned(),
            mac,
            ip,
        };

        self.mac_table.insert(mac, port);
        self.ip_table.insert(ip, mac);
        self.vm_table.insert(vm_id.to_owned(), mac);

        Ok(())
    }

    /// Unregister a VM port from the switch.
    ///
    /// # Errors
    ///
    /// Returns an error if the MAC is not registered.
    pub fn unregister_port(&mut self, mac: &MacAddr) -> anyhow::Result<()> {
        let port = self.mac_table.remove(mac).ok_or_else(|| {
            anyhow::anyhow!("MAC {mac} is not registered on switch '{}'", self.name)
        })?;

        self.ip_table.remove(&port.ip);
        self.vm_table.remove(&port.vm_id);

        Ok(())
    }

    /// Look up a port by MAC address.
    #[must_use]
    pub fn lookup_mac(&self, mac: &MacAddr) -> Option<&VmNetPort> {
        self.mac_table.get(mac)
    }

    /// Look up a port by IP address.
    #[must_use]
    pub fn lookup_ip(&self, ip: Ipv4Addr) -> Option<&VmNetPort> {
        self.ip_table
            .get(&ip)
            .and_then(|mac| self.mac_table.get(mac))
    }

    /// Look up a port by VM identifier.
    #[must_use]
    pub fn lookup_vm(&self, vm_id: &str) -> Option<&VmNetPort> {
        self.vm_table
            .get(vm_id)
            .and_then(|mac| self.mac_table.get(mac))
    }
}

#[cfg(test)]
#[path = "switch_test.rs"]
mod tests;
