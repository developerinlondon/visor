//! Thread-safe IPv4 subnet allocator.
//!
//! Allocates and releases IP addresses from a /24 (or other prefix) subnet,
//! automatically skipping the network address (.0), gateway (.1), and
//! broadcast address (.255).
//!
//! # Thread Safety
//!
//! The allocator uses `Mutex` internally and can be shared across threads via `Arc`.

use std::collections::BTreeSet;
use std::net::Ipv4Addr;
use std::sync::Mutex;

use anyhow::{Context, bail};

/// Thread-safe IPv4 subnet allocator.
///
/// Manages IP address allocation within a subnet, skipping reserved addresses
/// (network, gateway, broadcast). Safe to share across threads via `Arc`.
#[non_exhaustive]
pub struct SubnetAllocator {
    /// Base address of the subnet (e.g., 172.20.0.0).
    base: Ipv4Addr,
    /// Subnet prefix length (e.g., 24 for /24).
    prefix: u8,
    /// Gateway address within the subnet.
    gateway: Ipv4Addr,
    /// Internal state protected by a mutex.
    state: Mutex<AllocState>,
}

/// Internal allocation state.
struct AllocState {
    /// Set of currently allocated IP addresses (as host offsets from base).
    allocated: BTreeSet<u32>,
    /// Next candidate offset to try.
    next_candidate: u32,
}

impl SubnetAllocator {
    /// Create a new allocator for the given subnet.
    ///
    /// # Errors
    ///
    /// Returns an error if the prefix length is not between 1 and 30.
    pub fn new(base: Ipv4Addr, prefix: u8, gateway: Ipv4Addr) -> anyhow::Result<Self> {
        if prefix == 0 || prefix > 30 {
            bail!("prefix length must be between 1 and 30, got {prefix}");
        }
        Ok(Self {
            base,
            prefix,
            gateway,
            state: Mutex::new(AllocState {
                allocated: BTreeSet::new(),
                // Start at offset 2 — skip .0 (network) and .1 (gateway)
                next_candidate: 2,
            }),
        })
    }

    /// Create an allocator for the default visor network (172.20.0.0/24, gw 172.20.0.1).
    ///
    /// # Errors
    ///
    /// Returns an error if allocation state cannot be initialized.
    pub fn default_network() -> anyhow::Result<Self> {
        Self::new(
            super::DEFAULT_SUBNET_BASE,
            super::DEFAULT_SUBNET_PREFIX,
            super::DEFAULT_GATEWAY,
        )
    }

    /// Returns the base address of the subnet.
    #[must_use]
    pub fn base(&self) -> Ipv4Addr {
        self.base
    }

    /// Returns the prefix length of the subnet.
    #[must_use]
    pub fn prefix(&self) -> u8 {
        self.prefix
    }

    /// Returns the gateway address.
    #[must_use]
    pub fn gateway(&self) -> Ipv4Addr {
        self.gateway
    }

    /// Allocate the next available IP address from the subnet.
    ///
    /// Skips the network address (.0), gateway (.1), and broadcast (.255 for /24).
    ///
    /// # Errors
    ///
    /// Returns an error if no addresses are available (subnet exhausted).
    pub fn allocate(&self) -> anyhow::Result<Ipv4Addr> {
        let mut state = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))
            .context("failed to acquire allocator lock")?;

        let host_count = self.host_count();
        let broadcast_offset = host_count - 1;
        let base_u32 = u32::from(self.base);

        let start = state.next_candidate;
        let mut offset = start;

        loop {
            if offset == 0 || offset == 1 || offset == broadcast_offset {
                offset = if offset + 1 >= host_count {
                    2
                } else {
                    offset + 1
                };
                if offset == start {
                    bail!(
                        "subnet exhausted: no available IPs in {}/{}",
                        self.base,
                        self.prefix
                    );
                }
                continue;
            }

            if !state.allocated.contains(&offset) {
                state.allocated.insert(offset);
                state.next_candidate = if offset + 1 >= host_count {
                    2
                } else {
                    offset + 1
                };
                return Ok(Ipv4Addr::from(base_u32 + offset));
            }

            offset = if offset + 1 >= host_count {
                2
            } else {
                offset + 1
            };
            if offset == start {
                bail!(
                    "subnet exhausted: no available IPs in {}/{}",
                    self.base,
                    self.prefix
                );
            }
        }
    }

    /// Release a previously allocated IP address back to the pool.
    ///
    /// # Errors
    ///
    /// Returns an error if:
    /// - The IP is outside the subnet
    /// - The IP is a reserved address (network, gateway, broadcast)
    /// - The IP was not previously allocated
    pub fn release(&self, ip: Ipv4Addr) -> anyhow::Result<()> {
        if !self.contains(ip) {
            bail!("IP {ip} is not in subnet {}/{}", self.base, self.prefix);
        }

        let offset = u32::from(ip) - u32::from(self.base);
        let broadcast_offset = self.host_count() - 1;

        if offset == 0 {
            bail!("cannot release network address {ip}");
        }
        if ip == self.gateway || offset == 1 {
            bail!("cannot release gateway address {ip}");
        }
        if offset == broadcast_offset {
            bail!("cannot release broadcast address {ip}");
        }

        let mut state = self
            .state
            .lock()
            .map_err(|e| anyhow::anyhow!("lock poisoned: {e}"))
            .context("failed to acquire allocator lock")?;

        if !state.allocated.remove(&offset) {
            bail!("IP {ip} was not allocated");
        }

        // Reset next_candidate to the released offset if it's lower,
        // so the next allocation can reuse it
        if offset < state.next_candidate {
            state.next_candidate = offset;
        }

        Ok(())
    }

    /// Returns the number of currently allocated addresses.
    #[must_use]
    pub fn allocated_count(&self) -> usize {
        self.state.lock().map_or(0, |s| s.allocated.len())
    }

    /// Returns the number of available (unallocated) addresses.
    ///
    /// Excludes network, gateway, and broadcast addresses.
    #[must_use]
    pub fn available_count(&self) -> usize {
        let total_usable = self.host_count() as usize - 3; // minus .0, .1, .255
        total_usable - self.allocated_count()
    }

    /// Check whether an IP address belongs to this subnet.
    #[must_use]
    pub fn contains(&self, ip: Ipv4Addr) -> bool {
        let mask = u32::MAX << (32 - self.prefix);
        let base = u32::from(self.base);
        let addr = u32::from(ip);
        (addr & mask) == (base & mask)
    }

    /// Total number of host addresses in this subnet (2^(32-prefix)).
    fn host_count(&self) -> u32 {
        1u32 << (32 - self.prefix)
    }
}

#[cfg(test)]
#[path = "ip_alloc_test.rs"]
mod tests;
