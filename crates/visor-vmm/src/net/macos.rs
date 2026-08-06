//! macOS network backend using Apple's vmnet.framework and `pfctl`.
//!
//! Implements [`NetworkBackend`] for macOS by creating shared-mode vmnet
//! interfaces (which provide built-in NAT), and using `pfctl` for port
//! forwarding rules.
//!
//! # Requirements
//!
//! - macOS 15.0+ (Sequoia)
//! - `com.apple.vm.networking` entitlement OR root privileges
//! - For port forwarding: `pfctl` (ships with macOS)
//!
//! All resources use RAII — interfaces are finalized and pfctl rules are
//! removed when their handles are dropped.

use std::fmt;
use std::process::Command;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use vmnet::mode::Shared;
use vmnet::{Events, Interface, Options};

use crate::devices::net::PacketIo;

use super::backend::{
    InterfaceConfig, NatConfig, NatHandle, NetError, NetworkBackend, NetworkInterface,
    PortForwardHandle, PortMapping,
};

// ── macOS network backend ────────────────────────────────────────────

/// macOS network backend using vmnet.framework and `pfctl`.
///
/// This backend requires the `com.apple.vm.networking` entitlement or
/// root privileges. Shared-mode vmnet interfaces provide built-in NAT,
/// so [`setup_nat`](NetworkBackend::setup_nat) is a no-op that returns
/// a zero-rule handle. Port forwarding uses `pfctl` anchor rules.
pub struct MacosNetworkBackend;

impl MacosNetworkBackend {
    /// Create a new macOS network backend.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

impl Default for MacosNetworkBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBackend for MacosNetworkBackend {
    type Interface = MacosNetworkInterface;
    type Nat = MacosNatHandle;
    type PortForward = MacosPortForwardHandle;

    fn create_interface(&self, config: &InterfaceConfig) -> Result<Self::Interface, NetError> {
        config.validate()?;

        let shared_mode = Shared {
            subnet_options: None,
            ..Default::default()
        };

        let iface = Interface::new(vmnet::mode::Mode::Shared(shared_mode), Options::default())
            .map_err(|e| NetError::Interface(format!("vmnet interface creation failed: {e}")))?;

        Ok(MacosNetworkInterface {
            name: config.name().to_owned(),
            interface: Some(iface),
        })
    }

    fn setup_nat(&self, _config: &NatConfig) -> Result<Self::Nat, NetError> {
        // vmnet shared mode provides built-in NAT — no explicit setup needed.
        Ok(MacosNatHandle { _private: () })
    }

    fn setup_port_forward(&self, mappings: &[PortMapping]) -> Result<Self::PortForward, NetError> {
        let rules = generate_pf_rules(mappings);
        let mut applied: Vec<PfRule> = Vec::new();

        for rule in rules {
            if let Err(e) = rule.apply() {
                // Roll back previously applied rules on failure.
                for applied_rule in applied.into_iter().rev() {
                    let _ignore = applied_rule.remove();
                }
                return Err(NetError::PortForward(format!(
                    "failed to apply pfctl rule {rule}: {e}"
                )));
            }
            applied.push(rule);
        }

        Ok(MacosPortForwardHandle {
            mapping_count: mappings.len(),
            applied_rules: applied,
        })
    }
}

// ── Network interface handle ─────────────────────────────────────────

/// Handle to a macOS vmnet network interface.
///
/// The interface is automatically finalized when this handle is dropped.
impl fmt::Debug for MacosNetworkInterface {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MacosNetworkInterface")
            .field("name", &self.name)
            .finish_non_exhaustive()
    }
}

pub struct MacosNetworkInterface {
    name: String,
    interface: Option<Interface>,
}

impl NetworkInterface for MacosNetworkInterface {
    fn name(&self) -> &str {
        &self.name
    }
}

impl MacosNetworkInterface {
    /// Returns a reference to the underlying vmnet interface.
    #[must_use]
    pub fn vmnet_interface(&self) -> Option<&Interface> {
        self.interface.as_ref()
    }

    /// Takes ownership of the underlying vmnet interface.
    ///
    /// After calling this, the `MacosNetworkInterface` no longer owns the interface
    /// and will not finalize it on drop.
    pub fn take_interface(&mut self) -> Option<Interface> {
        self.interface.take()
    }
}

impl Drop for MacosNetworkInterface {
    fn drop(&mut self) {
        // Interface::finalize() takes ownership, but we only have &mut self.
        // vmnet::Interface's own Drop impl handles cleanup, so we don't need
        // to call finalize() explicitly here.
        if self.interface.is_some() {
            tracing::debug!(name = %self.name, "dropping macOS network interface");
        }
    }
}

// ── SendableInterface ─────────────────────────────────────────────────

/// Wrapper around `vmnet::Interface` that implements `Send`.
///
/// `vmnet::Interface` is `!Send` because it contains `*mut c_void` raw
/// pointers (auto-derived by the compiler). There are **no explicit
/// `!Send`/`!Sync` impls** in the vmnet crate — confirmed by source
/// inspection. Production VMMs (QEMU, Google Alioth, Cirrus Softnet)
/// all call `vmnet_read`/`vmnet_write` across threads safely.
pub(crate) struct SendableInterface(Interface);

// SAFETY: vmnet_read/vmnet_write are thread-safe — called cross-thread by
// QEMU, Google Alioth, and Cirrus Softnet in production. The !Send is
// auto-derived from *mut c_void raw pointers, not an intentional safety marker.
#[allow(unsafe_code)]
unsafe impl Send for SendableInterface {}

impl SendableInterface {
    /// Creates a new sendable wrapper around a vmnet interface.
    pub(crate) fn new(iface: Interface) -> Self {
        Self(iface)
    }

    /// Returns a mutable reference to the underlying interface.
    fn inner_mut(&mut self) -> &mut Interface {
        &mut self.0
    }
}

// ── VmnetPacketIo ─────────────────────────────────────────────────────

/// [`PacketIo`] implementation backed by vmnet.framework.
///
/// Wraps a [`SendableInterface`] for packet send/recv and an
/// [`Arc<AtomicBool>`] flag that is set by the vmnet event callback
/// when packets are available.
pub struct VmnetPacketIo {
    iface: SendableInterface,
    /// Flag set by vmnet's event callback when packets arrive.
    has_pending: Arc<AtomicBool>,
}

impl VmnetPacketIo {
    /// Creates a new `VmnetPacketIo` from a vmnet interface.
    ///
    /// Installs the vmnet event callback to signal `has_pending` when
    /// packets are available for reading.
    ///
    /// # Errors
    ///
    /// Returns a `NetError::Interface` if the event callback registration fails.
    pub(crate) fn new(mut iface: SendableInterface) -> Result<(Self, Arc<AtomicBool>), NetError> {
        let has_pending = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&has_pending);

        iface
            .inner_mut()
            .set_event_callback(Events::PACKETS_AVAILABLE, move |_, _| {
                flag.store(true, Ordering::Release);
            })
            .map_err(|e| NetError::Interface(format!("vmnet set_event_callback failed: {e}")))?;

        Ok((
            Self {
                iface,
                has_pending: Arc::clone(&has_pending),
            },
            has_pending,
        ))
    }

    /// Returns the pending-packets flag.
    ///
    /// The run loop checks this to decide whether to call `process_external_queue`.
    #[must_use]
    pub fn has_pending_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.has_pending)
    }
}

impl PacketIo for VmnetPacketIo {
    fn send(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        self.iface
            .inner_mut()
            .write(buf)
            .map_err(|e| std::io::Error::other(format!("vmnet write: {e}")))
    }

    fn try_recv(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        match self.iface.inner_mut().read(buf) {
            Ok(n) => Ok(n),
            Err(vmnet::Error::VmnetReadNothing) => Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "no packets available",
            )),
            Err(e) => Err(std::io::Error::other(format!("vmnet read: {e}"))),
        }
    }
}

// ── NAT handle ───────────────────────────────────────────────────────

/// Handle for macOS NAT (no-op since vmnet shared mode has built-in NAT).
///
/// vmnet shared mode automatically provides NAT for all traffic from the
/// virtual interface. No explicit iptables-like rules are needed.
pub struct MacosNatHandle {
    _private: (),
}

impl NatHandle for MacosNatHandle {
    fn rule_count(&self) -> usize {
        // vmnet shared mode handles NAT internally — no explicit rules.
        0
    }

    fn teardown(&mut self) -> Result<(), NetError> {
        // Nothing to tear down — NAT is managed by vmnet.
        Ok(())
    }
}

// ── Port-forward handle ──────────────────────────────────────────────

/// Handle to applied macOS port-forwarding `pfctl` rules.
///
/// Rules are automatically removed when this handle is dropped.
pub struct MacosPortForwardHandle {
    mapping_count: usize,
    applied_rules: Vec<PfRule>,
}

impl PortForwardHandle for MacosPortForwardHandle {
    fn mapping_count(&self) -> usize {
        self.mapping_count
    }

    fn teardown(&mut self) -> Result<(), NetError> {
        let mut errors = Vec::new();
        for rule in self.applied_rules.drain(..).rev() {
            if let Err(e) = rule.remove() {
                errors.push(format!("{rule}: {e}"));
            }
        }
        self.mapping_count = 0;
        if errors.is_empty() {
            Ok(())
        } else {
            Err(NetError::PortForward(format!(
                "failed to remove some pfctl rules: {}",
                errors.join("; ")
            )))
        }
    }
}

impl Drop for MacosPortForwardHandle {
    fn drop(&mut self) {
        if !self.applied_rules.is_empty() {
            if let Err(e) = self.teardown() {
                tracing::warn!(error = %e, "failed to clean up pfctl port-forward rules on drop");
            }
        }
    }
}

// ── PF rule helper ───────────────────────────────────────────────────

/// Anchor name prefix for visor pfctl rules.
const PF_ANCHOR: &str = "com.visor";

/// A single `pfctl` rule stored as raw rule text within a named anchor.
///
/// Rules are applied to a per-mapping anchor under [`PF_ANCHOR`] and
/// removed by flushing that anchor.
#[derive(Debug, Clone)]
pub(crate) struct PfRule {
    /// Anchor name (e.g., "com.visor/portfwd-8080-tcp").
    pub anchor: String,
    /// The pf rule text (e.g., "rdr pass on lo0 proto tcp ...").
    pub rule_text: String,
}

impl PfRule {
    /// Apply this rule by loading it into the pfctl anchor.
    ///
    /// # Errors
    ///
    /// Returns an error if the pfctl command fails.
    pub fn apply(&self) -> Result<(), NetError> {
        let output = Command::new("pfctl")
            .args(["-a", &self.anchor, "-f", "-"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .and_then(|mut child| {
                use std::io::Write;
                if let Some(ref mut stdin) = child.stdin {
                    stdin.write_all(self.rule_text.as_bytes())?;
                }
                child.wait_with_output()
            })?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NetError::PortForward(format!(
                "pfctl anchor load failed: {stderr}"
            )));
        }
        Ok(())
    }

    /// Remove this rule by flushing the pfctl anchor.
    ///
    /// # Errors
    ///
    /// Returns an error if the pfctl command fails.
    pub fn remove(&self) -> Result<(), NetError> {
        let output = Command::new("pfctl")
            .args(["-a", &self.anchor, "-F", "all"])
            .output()?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // Ignore "anchor does not exist" during cleanup.
            if !stderr.contains("does not exist") && !stderr.contains("No such") {
                return Err(NetError::PortForward(format!(
                    "pfctl anchor flush failed: {stderr}"
                )));
            }
        }
        Ok(())
    }
}

impl fmt::Display for PfRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "pfctl -a {} [{}]", self.anchor, self.rule_text)
    }
}

// ── Rule generators ──────────────────────────────────────────────────

/// Generate pfctl redirect rules for port forwarding.
///
/// Each mapping produces an `rdr` rule in a dedicated anchor under
/// [`PF_ANCHOR`]. Using per-mapping anchors allows independent
/// add/remove of individual port forwards.
pub(crate) fn generate_pf_rules(mappings: &[PortMapping]) -> Vec<PfRule> {
    mappings
        .iter()
        .map(|m| {
            let anchor = format!("{PF_ANCHOR}/portfwd-{}-{}", m.host_port(), m.protocol());
            let rule_text = format!(
                "rdr pass on lo0 proto {} from any to any port {} -> {} port {}",
                m.protocol(),
                m.host_port(),
                m.guest_ip(),
                m.guest_port()
            );
            PfRule { anchor, rule_text }
        })
        .collect()
}

// ── vmnet-helper (macOS 26+ rootless networking) ───────────────────

/// Default path to the vmnet-helper binary.
pub(crate) const VMNET_HELPER_PATH: &str = "/opt/vmnet-helper/bin/vmnet-helper";

/// Returns true if the current macOS version is 26.0 or later.
///
/// Uses `kern.osproductversion` sysctl to detect OS version at runtime.
/// Returns false on any detection failure (safe default = use sudo path).
///
/// # Errors
///
/// Returns `NetError::Interface` if sysctl cannot be read.
pub fn is_macos_26_or_later() -> Result<bool, NetError> {
    let output = Command::new("sysctl")
        .args(["-n", "kern.osproductversion"])
        .output()
        .map_err(|e| NetError::Interface(format!("sysctl failed: {e}")))?;

    if !output.status.success() {
        return Err(NetError::Interface("sysctl returned non-zero".into()));
    }

    let version_str = String::from_utf8_lossy(&output.stdout);
    let version_str = version_str.trim();

    let major: u32 = version_str
        .split('.')
        .next()
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| NetError::Interface(format!("cannot parse OS version: {version_str}")))?;

    Ok(major >= 26)
}

/// Interface information returned by vmnet-helper's JSON handshake.
#[derive(Debug, Clone, serde::Deserialize)]
pub(crate) struct VmnetHelperInfo {
    /// MAC address assigned by vmnet (e.g. "aa:bb:cc:dd:ee:ff").
    #[serde(default, rename = "vmnet_mac_address")]
    pub mac_address: String,
    /// MTU for the vmnet interface.
    #[serde(default, rename = "vmnet_mtu")]
    pub mtu: u32,
    /// Maximum packet size for the vmnet interface.
    #[serde(default, rename = "vmnet_max_packet_size")]
    pub max_packet_size: u32,
}

/// [`PacketIo`] implementation using vmnet-helper subprocess.
///
/// Instead of calling vmnet APIs directly (which requires root on macOS <26),
/// this spawns `vmnet-helper` as a subprocess and communicates via a
/// `AF_UNIX SOCK_DGRAM` socketpair. On macOS 26+, the helper runs without
/// sudo. On older macOS, sudo is used.
///
/// Protocol:
/// 1. Create `socketpair(AF_UNIX, SOCK_DGRAM)`
/// 2. Spawn vmnet-helper with `--fd=3` (helper reads/writes packets via fd 3)
/// 3. Read JSON interface info from helper stdout
/// 4. Send/recv Ethernet frames through our end of the socketpair
pub struct VmnetHelperPacketIo {
    /// Our end of the socketpair for frame I/O.
    socket: std::os::unix::net::UnixDatagram,
    /// The helper child process (killed on drop).
    helper: std::process::Child,
}

impl VmnetHelperPacketIo {
    /// Spawns the vmnet-helper subprocess and returns the I/O handle plus interface info.
    ///
    /// Creates a `SOCK_DGRAM` socketpair, spawns vmnet-helper with fd 3 pointing
    /// to the helper’s end, reads one line of JSON from stdout, and returns the
    /// ready-to-use `PacketIo` handle.
    ///
    /// # Errors
    ///
    /// Returns `NetError::Interface` if the subprocess cannot be spawned,
    /// the socketpair cannot be created, or the JSON handshake fails.
    #[allow(unsafe_code)]
    pub(crate) fn spawn() -> Result<(Self, VmnetHelperInfo), NetError> {
        use std::io::BufRead;
        use std::os::unix::io::AsRawFd;
        use std::os::unix::net::UnixDatagram;
        use std::os::unix::process::CommandExt;

        // 1. Create socketpair.
        let (our_sock, helper_sock) = UnixDatagram::pair()
            .map_err(|e| NetError::Interface(format!("socketpair creation failed: {e}")))?;

        // 2. Set socket buffer sizes.
        // SAFETY: libc::setsockopt operates on valid fds from UnixDatagram::pair().
        // The fds are guaranteed to be open and valid at this point.
        let our_fd = our_sock.as_raw_fd();
        let send_buf: libc::c_int = 65 * 1024; // 65 KB
        let recv_buf: libc::c_int = 4 * 1024 * 1024; // 4 MB
        // c_int is 4 bytes on all macOS platforms (32-bit and 64-bit).
        let optlen: libc::socklen_t = 4;
        unsafe {
            libc::setsockopt(
                our_fd,
                libc::SOL_SOCKET,
                libc::SO_SNDBUF,
                std::ptr::from_ref(&send_buf).cast(),
                optlen,
            );
            libc::setsockopt(
                our_fd,
                libc::SOL_SOCKET,
                libc::SO_RCVBUF,
                std::ptr::from_ref(&recv_buf).cast(),
                optlen,
            );
        }

        // 3. Set our socket to non-blocking for try_recv.
        our_sock
            .set_nonblocking(true)
            .map_err(|e| NetError::Interface(format!("set_nonblocking failed: {e}")))?;

        // 4. Build command: rootless on macOS 26+, sudo on older.
        let is_26_plus = is_macos_26_or_later().unwrap_or(false);
        let helper_fd = helper_sock.as_raw_fd();

        let mut cmd = if is_26_plus {
            let mut c = Command::new(VMNET_HELPER_PATH);
            c.args(["--fd=3", "--operation-mode", "shared"]);
            c
        } else {
            let mut c = Command::new("sudo");
            c.args([
                "--non-interactive",
                "--close-from=4",
                VMNET_HELPER_PATH,
                "--fd=3",
                "--operation-mode",
                "shared",
            ]);
            c
        };

        cmd.stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // SAFETY: pre_exec runs in the forked child process between fork and exec.
        // We dup2 the helper socket fd to fd 3 so vmnet-helper inherits it.
        // This is safe because:
        // - helper_fd is valid (from UnixDatagram::pair above)
        // - dup2 and close are async-signal-safe per POSIX
        unsafe {
            cmd.pre_exec(move || {
                if helper_fd != 3 {
                    if libc::dup2(helper_fd, 3) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    libc::close(helper_fd);
                }
                Ok(())
            });
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| NetError::Interface(format!("failed to spawn vmnet-helper: {e}")))?;

        // Close helper's end of the socketpair in our process.
        drop(helper_sock);

        // 5. Read JSON handshake from helper's stdout.
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| NetError::Interface("vmnet-helper stdout not captured".into()))?;
        let mut reader = std::io::BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).map_err(|e| {
            NetError::Interface(format!("failed to read vmnet-helper handshake: {e}"))
        })?;

        let info: VmnetHelperInfo = serde_json::from_str(line.trim())
            .map_err(|e| NetError::Interface(format!("failed to parse vmnet-helper JSON: {e}")))?;

        tracing::info!(
            mac = %info.mac_address,
            mtu = info.mtu,
            max_packet = info.max_packet_size,
            rootless = is_26_plus,
            "vmnet-helper started"
        );

        Ok((
            Self {
                socket: our_sock,
                helper: child,
            },
            info,
        ))
    }
}

impl PacketIo for VmnetHelperPacketIo {
    fn send(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        self.socket.send(buf)
    }

    fn try_recv(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        self.socket.recv(buf)
    }
}

impl Drop for VmnetHelperPacketIo {
    fn drop(&mut self) {
        if let Err(e) = self.helper.kill() {
            tracing::warn!(error = %e, "failed to kill vmnet-helper subprocess");
        }
    }
}

#[cfg(test)]
#[path = "macos_test.rs"]
mod tests;
