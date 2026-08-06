//! Embedded DNS resolver for VM name resolution.
//!
//! Provides an in-process DNS registry that resolves VM names within a network
//! to their assigned IP addresses. Unknown queries are forwarded to upstream
//! DNS servers (from host `/etc/resolv.conf`).
//!
//! Uses hickory-server for DNS wire protocol handling.

use std::collections::HashMap;
use std::net::Ipv4Addr;

/// Registry of VM name → IP address mappings for DNS resolution.
///
/// Names are stored in lowercase for case-insensitive lookup.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DnsRegistry {
    /// Forward lookup: name → IP.
    forward: HashMap<String, Ipv4Addr>,
    /// Reverse lookup: IP → name.
    reverse: HashMap<Ipv4Addr, String>,
}

impl DnsRegistry {
    /// Create an empty DNS registry.
    #[must_use]
    pub fn new() -> Self {
        Self {
            forward: HashMap::new(),
            reverse: HashMap::new(),
        }
    }

    /// Register a VM name with its IP address.
    ///
    /// If the name already exists, the IP is updated.
    pub fn register(&mut self, name: &str, ip: Ipv4Addr) {
        let lower = name.to_lowercase();
        // Remove old reverse entry if overwriting
        if let Some(old_ip) = self.forward.get(&lower) {
            self.reverse.remove(old_ip);
        }
        self.forward.insert(lower.clone(), ip);
        self.reverse.insert(ip, lower);
    }

    /// Remove a VM name from the registry.
    pub fn unregister(&mut self, name: &str) {
        let lower = name.to_lowercase();
        if let Some(ip) = self.forward.remove(&lower) {
            self.reverse.remove(&ip);
        }
    }

    /// Resolve a VM name to an IP address.
    ///
    /// Lookup is case-insensitive.
    #[must_use]
    pub fn resolve(&self, name: &str) -> Option<Ipv4Addr> {
        self.forward.get(&name.to_lowercase()).copied()
    }

    /// Reverse-resolve an IP address to a VM name.
    #[must_use]
    pub fn reverse_lookup(&self, ip: Ipv4Addr) -> Option<&str> {
        self.reverse.get(&ip).map(String::as_str)
    }

    /// Returns the number of registered entries.
    #[must_use]
    pub fn count(&self) -> usize {
        self.forward.len()
    }

    /// Returns a snapshot of all entries as (name, ip) pairs.
    #[must_use]
    pub fn all_entries(&self) -> Vec<(&str, Ipv4Addr)> {
        self.forward
            .iter()
            .map(|(name, ip)| (name.as_str(), *ip))
            .collect()
    }
}

impl Default for DnsRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Configuration for the embedded DNS resolver.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DnsResolverConfig {
    /// IP address to listen on (gateway IP).
    listen_ip: Ipv4Addr,
    /// Port to listen on (default: 53).
    listen_port: u16,
    /// Upstream DNS servers for forwarding unknown queries.
    upstream_servers: Vec<Ipv4Addr>,
}

impl DnsResolverConfig {
    /// Create a new DNS resolver configuration.
    ///
    /// Defaults to port 53 with Google's DNS (8.8.8.8) as upstream.
    #[must_use]
    pub fn new(listen_ip: Ipv4Addr) -> Self {
        Self {
            listen_ip,
            listen_port: 53,
            upstream_servers: vec![Ipv4Addr::new(8, 8, 8, 8)],
        }
    }

    /// Set the listen port.
    #[must_use]
    pub fn with_port(mut self, port: u16) -> Self {
        self.listen_port = port;
        self
    }

    /// Add an upstream DNS server.
    #[must_use]
    pub fn with_upstream(mut self, server: Ipv4Addr) -> Self {
        self.upstream_servers.push(server);
        self
    }

    /// Returns the listen IP address.
    #[must_use]
    pub fn listen_ip(&self) -> Ipv4Addr {
        self.listen_ip
    }

    /// Returns the listen port.
    #[must_use]
    pub fn listen_port(&self) -> u16 {
        self.listen_port
    }

    /// Returns the upstream DNS servers.
    #[must_use]
    pub fn upstream_servers(&self) -> &[Ipv4Addr] {
        &self.upstream_servers
    }
}

#[cfg(test)]
#[path = "dns_test.rs"]
mod tests;
