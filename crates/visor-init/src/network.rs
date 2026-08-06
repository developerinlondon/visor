//! Guest network configuration.
//!
//! Configures the guest network interface with IP address, netmask,
//! and default gateway using `nix` ioctl wrappers and `libc` structs.

use std::net::Ipv4Addr;
use std::os::fd::AsRawFd;

use anyhow::{Context, Result};
use nix::sys::socket::{AddressFamily, SockFlag, SockType, socket};

use crate::config::{HostEntry, NetworkConfig};

/// `AF_INET` as `sa_family_t` for sockaddr construction.
///
/// POSIX guarantees `AF_INET` is 2, which always fits in `u16`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "AF_INET (2) fits in sa_family_t (u16) by POSIX guarantee"
)]
const INET_FAMILY: libc::sa_family_t = libc::AF_INET as libc::sa_family_t;

/// `IFF_UP` as `c_short` for ifreq flag manipulation.
///
/// `IFF_UP` is 0x1, which always fits in `i16`.
#[expect(
    clippy::cast_possible_truncation,
    reason = "IFF_UP (0x1) fits in c_short (i16)"
)]
const IFF_UP_FLAG: libc::c_short = libc::IFF_UP as libc::c_short;

/// Converts an ioctl request constant to the platform-appropriate type.
///
/// On glibc, `libc::ioctl` expects `c_ulong` (u64). On musl, it expects `c_int` (i32).
/// The SIOC* constants are always small enough to fit in both types.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
const fn ioctl_request(req: u64) -> libc::Ioctl {
    req as libc::Ioctl
}

/// Parsed network configuration ready for applying to a guest interface.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct NetworkSetup {
    /// Network interface name (defaults to `"eth0"`).
    pub interface: String,
    /// IPv4 address to assign to the interface.
    pub address: Ipv4Addr,
    /// Subnet mask for the interface.
    pub netmask: Ipv4Addr,
    /// Default gateway address.
    pub gateway: Ipv4Addr,
    /// Whether this attachment should own the guest default route.
    pub default_route: bool,
}

impl NetworkSetup {
    /// Creates a [`NetworkSetup`] from a [`NetworkConfig`], parsing string IPs
    /// to [`Ipv4Addr`].
    ///
    /// The interface defaults to `"eth0"`.
    ///
    /// # Errors
    ///
    /// Returns an error if any IP address string is not a valid IPv4 address.
    pub fn from_config(config: &NetworkConfig, index: usize) -> Result<Self> {
        Ok(Self {
            interface: config
                .interface
                .clone()
                .unwrap_or_else(|| format!("eth{index}")),
            address: parse_ipv4(&config.address).context("invalid network address")?,
            netmask: parse_ipv4(&config.netmask).context("invalid netmask")?,
            gateway: parse_ipv4(&config.gateway).context("invalid gateway")?,
            default_route: config.default_route,
        })
    }
}

/// Parses a string as an IPv4 address.
///
/// # Errors
///
/// Returns an error if `s` is not a valid IPv4 address.
pub fn parse_ipv4(s: &str) -> Result<Ipv4Addr> {
    s.parse::<Ipv4Addr>()
        .with_context(|| format!("invalid IPv4 address: {s}"))
}

/// Builds a zeroed `libc::ifreq` with the interface name set.
///
/// Interface names longer than `IFNAMSIZ - 1` (typically 15) bytes are
/// truncated to fit, preserving the NUL terminator.
#[must_use]
#[allow(unsafe_code)]
pub fn build_ifreq(interface: &str) -> libc::ifreq {
    // SAFETY: ifreq is a C struct with union fields; zeroing all bytes
    // produces a valid representation (all-zeros is valid for every field).
    let mut ifr: libc::ifreq = unsafe { std::mem::zeroed() };
    let bytes = interface.as_bytes();
    let len = bytes.len().min(libc::IFNAMSIZ - 1);

    // SAFETY: We write at most IFNAMSIZ-1 bytes into ifr_name (which is
    // [c_char; IFNAMSIZ]), leaving the last byte as the NUL terminator
    // from the zeroing above.
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), ifr.ifr_name.as_mut_ptr().cast::<u8>(), len);
    }
    ifr
}

/// Builds a `libc::sockaddr_in` from an [`Ipv4Addr`].
#[must_use]
pub fn sockaddr_in_from_ip(ip: Ipv4Addr) -> libc::sockaddr_in {
    libc::sockaddr_in {
        sin_family: INET_FAMILY,
        sin_port: 0,
        sin_addr: libc::in_addr {
            s_addr: u32::from_ne_bytes(ip.octets()),
        },
        sin_zero: [0; 8],
    }
}

/// Converts a `sockaddr_in` to a `sockaddr` via byte copy.
#[allow(unsafe_code)]
fn sockaddr_from_in(sin: libc::sockaddr_in) -> libc::sockaddr {
    // SAFETY: sockaddr_in (16 bytes) and sockaddr (16 bytes) are
    // layout-compatible for AF_INET per POSIX.
    let mut sa: libc::sockaddr = unsafe { std::mem::zeroed() };
    // SAFETY: Both structs are 16 bytes. We copy the full sockaddr_in
    // representation into the sockaddr, which is the standard POSIX
    // reinterpretation for AF_INET addresses.
    unsafe {
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(&sin).cast::<u8>(),
            std::ptr::from_mut(&mut sa).cast::<u8>(),
            std::mem::size_of::<libc::sockaddr_in>(),
        );
    }
    sa
}

/// Copies a `sockaddr_in` into an ifreq's address union field.
#[allow(unsafe_code)]
fn set_ifreq_addr(ifr: &mut libc::ifreq, addr: libc::sockaddr_in) {
    // SAFETY: sockaddr_in and sockaddr are both 16 bytes and
    // layout-compatible for AF_INET per POSIX. We write into the
    // ifru_addr union field via raw pointer to avoid union access issues.
    unsafe {
        std::ptr::copy_nonoverlapping(
            std::ptr::from_ref(&addr).cast::<u8>(),
            (&raw mut ifr.ifr_ifru).cast::<u8>(),
            std::mem::size_of::<libc::sockaddr_in>(),
        );
    }
}

/// Configures a guest network interface with the given settings.
///
/// Performs the following steps:
/// 1. Sets the interface IP address (`SIOCSIFADDR`)
/// 2. Sets the subnet mask (`SIOCSIFNETMASK`)
/// 3. Brings the interface up (`SIOCSIFFLAGS` with `IFF_UP`)
/// 4. Adds a default route through the gateway (`SIOCADDRT`) when requested
///
/// # Errors
///
/// Returns an error if any network ioctl fails.
#[allow(unsafe_code)]
pub fn configure_network(setup: &NetworkSetup) -> Result<()> {
    let sock = socket(
        AddressFamily::Inet,
        SockType::Datagram,
        SockFlag::empty(),
        None,
    )
    .context("failed to open network socket")?;
    let fd = sock.as_raw_fd();

    // Set interface address
    let mut ifr = build_ifreq(&setup.interface);
    set_ifreq_addr(&mut ifr, sockaddr_in_from_ip(setup.address));
    // SAFETY: fd is a valid socket from nix::sys::socket::socket().
    // ifr is a properly initialized ifreq with interface name and
    // AF_INET address. SIOCSIFADDR sets the interface address.
    let ret = unsafe { libc::ioctl(fd, ioctl_request(libc::SIOCSIFADDR), &ifr) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error())
            .context(format!("SIOCSIFADDR failed for {}", setup.interface));
    }

    // Set netmask
    let mut ifr = build_ifreq(&setup.interface);
    set_ifreq_addr(&mut ifr, sockaddr_in_from_ip(setup.netmask));
    // SAFETY: fd is a valid socket, ifr has interface name and AF_INET
    // netmask. SIOCSIFNETMASK sets the subnet mask.
    let ret = unsafe { libc::ioctl(fd, ioctl_request(libc::SIOCSIFNETMASK), &ifr) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error())
            .context(format!("SIOCSIFNETMASK failed for {}", setup.interface));
    }

    // Bring interface up
    bring_interface_up(fd, &setup.interface)
        .context(format!("failed to bring up {}", setup.interface))?;

    if setup.default_route {
        add_default_route(fd, setup.gateway).context("failed to add default route")?;
    }

    Ok(())
}

fn resolv_conf_contents(configs: &[NetworkConfig]) -> String {
    let mut servers = Vec::new();

    for config in configs {
        let config_servers = if config.dns_servers.is_empty() {
            std::slice::from_ref(&config.gateway)
        } else {
            config.dns_servers.as_slice()
        };
        for server in config_servers {
            if !servers.contains(server) {
                servers.push(server.clone());
            }
        }
    }

    let mut resolv_conf = String::new();
    for server in servers {
        use std::fmt::Write as _;
        let _ = writeln!(&mut resolv_conf, "nameserver {server}");
    }
    resolv_conf
}

fn hosts_file_contents(extra_hosts: &[HostEntry]) -> String {
    let mut hosts = String::from(
        "127.0.0.1 localhost\n\
         ::1 localhost ip6-localhost ip6-loopback\n\
         fe00::0 ip6-localnet\n\
         ff00::0 ip6-mcastprefix\n\
         ff02::1 ip6-allnodes\n\
         ff02::2 ip6-allrouters\n",
    );
    for entry in extra_hosts {
        use std::fmt::Write as _;
        let _ = writeln!(&mut hosts, "{} {}", entry.address, entry.hostname);
    }
    hosts
}

/// Writes `/etc/resolv.conf` for the guest.
///
/// Uses the configured DNS servers, or falls back to the guest gateway
/// address when no explicit DNS servers were provided.
///
/// # Errors
///
/// Returns an error if `/etc` cannot be created or the file cannot be written.
pub fn write_resolv_conf(configs: &[NetworkConfig]) -> Result<()> {
    let resolv_path = std::path::Path::new("/etc/resolv.conf");
    if let Some(parent) = resolv_path.parent() {
        std::fs::create_dir_all(parent).context("create /etc for resolv.conf")?;
    }
    std::fs::write(resolv_path, resolv_conf_contents(configs))
        .context("write guest resolv.conf")?;
    Ok(())
}

/// Writes `/etc/hosts` for the guest.
///
/// Always includes localhost defaults, then appends any extra static entries.
///
/// # Errors
///
/// Returns an error if `/etc` cannot be created or the file cannot be written.
pub fn write_hosts_file(extra_hosts: &[HostEntry]) -> Result<()> {
    let hosts_path = std::path::Path::new("/etc/hosts");
    if let Some(parent) = hosts_path.parent() {
        std::fs::create_dir_all(parent).context("create /etc for hosts file")?;
    }
    std::fs::write(hosts_path, hosts_file_contents(extra_hosts))
        .context("write guest hosts file")?;
    Ok(())
}

/// Brings a network interface up by setting the `IFF_UP` flag.
#[allow(unsafe_code)]
fn bring_interface_up(fd: i32, interface: &str) -> Result<()> {
    let mut ifr = build_ifreq(interface);

    // SAFETY: fd is a valid socket, ifr has a valid interface name.
    // SIOCGIFFLAGS reads the current interface flags into ifru_flags.
    let ret = unsafe { libc::ioctl(fd, ioctl_request(libc::SIOCGIFFLAGS), &mut ifr) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error()).context("SIOCGIFFLAGS failed");
    }

    // SAFETY: After a successful SIOCGIFFLAGS, ifru_flags is the active
    // union variant and contains the current interface flags. We OR in
    // IFF_UP to bring the interface up.
    unsafe {
        ifr.ifr_ifru.ifru_flags |= IFF_UP_FLAG;
    }

    // SAFETY: fd is a valid socket, ifr has a valid interface name and
    // updated flags. SIOCSIFFLAGS sets the interface flags.
    let ret = unsafe { libc::ioctl(fd, ioctl_request(libc::SIOCSIFFLAGS), &ifr) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error()).context("SIOCSIFFLAGS failed");
    }

    Ok(())
}

/// Adds a default route (0.0.0.0/0) via the given gateway.
#[allow(unsafe_code)]
fn add_default_route(fd: i32, gateway: Ipv4Addr) -> Result<()> {
    // SAFETY: rtentry is a C struct; zeroing all bytes produces a valid
    // representation (all-zeros means "default" for dst/genmask).
    let mut rt: libc::rtentry = unsafe { std::mem::zeroed() };

    rt.rt_dst = sockaddr_from_in(sockaddr_in_from_ip(Ipv4Addr::UNSPECIFIED));
    rt.rt_gateway = sockaddr_from_in(sockaddr_in_from_ip(gateway));
    rt.rt_genmask = sockaddr_from_in(sockaddr_in_from_ip(Ipv4Addr::UNSPECIFIED));
    rt.rt_flags = libc::RTF_UP | libc::RTF_GATEWAY;

    // SAFETY: fd is a valid socket, rt is a properly initialized rtentry
    // with default destination (0.0.0.0), the given gateway, and zero
    // genmask. SIOCADDRT adds the route to the kernel routing table.
    let ret = unsafe { libc::ioctl(fd, ioctl_request(libc::SIOCADDRT), &rt) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error()).context("SIOCADDRT failed");
    }

    Ok(())
}

/// Configures the loopback interface (`lo`) with address `127.0.0.1`
/// and brings it up.
///
/// # Errors
///
/// Returns an error if any network ioctl fails.
#[allow(unsafe_code)]
pub fn configure_loopback() -> Result<()> {
    let sock = socket(
        AddressFamily::Inet,
        SockType::Datagram,
        SockFlag::empty(),
        None,
    )
    .context("failed to open network socket for loopback")?;
    let fd = sock.as_raw_fd();

    // Set loopback address to 127.0.0.1
    let mut ifr = build_ifreq("lo");
    set_ifreq_addr(&mut ifr, sockaddr_in_from_ip(Ipv4Addr::LOCALHOST));
    // SAFETY: fd is a valid socket, ifr has interface name "lo" and
    // address 127.0.0.1. SIOCSIFADDR sets the interface address.
    let ret = unsafe { libc::ioctl(fd, ioctl_request(libc::SIOCSIFADDR), &ifr) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error()).context("SIOCSIFADDR failed for lo");
    }

    // Set loopback netmask to 255.0.0.0
    let mut ifr = build_ifreq("lo");
    set_ifreq_addr(&mut ifr, sockaddr_in_from_ip(Ipv4Addr::new(255, 0, 0, 0)));
    // SAFETY: fd is a valid socket, ifr has interface name "lo" and
    // netmask 255.0.0.0. SIOCSIFNETMASK sets the subnet mask.
    let ret = unsafe { libc::ioctl(fd, ioctl_request(libc::SIOCSIFNETMASK), &ifr) };
    if ret < 0 {
        return Err(std::io::Error::last_os_error()).context("SIOCSIFNETMASK failed for lo");
    }

    bring_interface_up(fd, "lo").context("failed to bring up lo")?;

    Ok(())
}

#[cfg(test)]
#[path = "network_test.rs"]
mod tests;
