//! Linux network backend using TAP devices, iptables NAT, and iptables DNAT.
//!
//! Implements [`NetworkBackend`] for Linux by creating TAP interfaces via
//! `ip tuntap` commands, configuring NAT with iptables MASQUERADE, and
//! setting up port forwarding with iptables DNAT rules.
//!
//! All resources use RAII — interfaces are deleted and iptables rules are
//! removed when their handles are dropped.

use std::fmt;
use std::net::Ipv4Addr;
use std::os::fd::AsFd;
use std::process::Command;
use std::sync::Mutex;

use nix::unistd;

use crate::devices::net::PacketIo;

use super::backend::{
    InterfaceConfig, NatConfig, NatHandle, NetError, NetworkBackend, NetworkInterface,
    PortForwardHandle, PortMapping,
};

const VISOR_GUEST_SUPERNET: &str = "172.20.0.0/16";
const VISOR_IPTABLES_TAG_PREFIX: &str = "visor-";
const VISOR_IPTABLES_TABLES: &[&str] = &["nat", "filter"];
const DOCKER_USER_CHAIN: &str = "DOCKER-USER";
static IPTABLES_SETUP_LOCK: Mutex<()> = Mutex::new(());

// ── Linux network backend ────────────────────────────────────────────

/// Linux network backend using `ip` and `iptables` commands.
///
/// This backend requires root privileges (or appropriate `CAP_NET_ADMIN`).
pub struct LinuxNetworkBackend;

impl LinuxNetworkBackend {
    /// Create a new Linux network backend.
    #[must_use]
    pub fn new() -> Self {
        Self
    }
}

/// Removes stale Visor-tagged iptables rules left behind by an abrupt exit.
///
/// This is intended for daemon startup recovery on Linux hosts. Only rules with
/// Visor's comment tags are removed, and removal happens in reverse order to
/// respect dependencies between DNAT/filter/NAT rules.
///
/// # Errors
///
/// Returns [`NetError::Nat`] if rule discovery fails or one or more tagged
/// rules could not be removed.
pub fn cleanup_visor_iptables_rules() -> Result<usize, NetError> {
    let mut tagged_rules = Vec::new();
    for table in VISOR_IPTABLES_TABLES {
        tagged_rules.extend(list_visor_iptables_rules(table)?);
    }

    let removed = tagged_rules.len();
    let mut errors = Vec::new();
    for rule in tagged_rules.into_iter().rev() {
        if let Err(error) = rule.remove() {
            errors.push(format!("{rule}: {error}"));
        }
    }

    if errors.is_empty() {
        Ok(removed)
    } else {
        Err(NetError::Nat(format!(
            "failed to remove some stale Visor iptables rules: {}",
            errors.join("; ")
        )))
    }
}

impl Default for LinuxNetworkBackend {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkBackend for LinuxNetworkBackend {
    type Interface = LinuxNetworkInterface;
    type Nat = LinuxNatHandle;
    type PortForward = LinuxPortForwardHandle;

    fn create_interface(&self, config: &InterfaceConfig) -> Result<Self::Interface, NetError> {
        config.validate()?;

        create_tap_device(config.name())
            .map_err(|e| NetError::Interface(format!("failed to create TAP device: {e}")))?;

        if let Some(bridge_name) = config.bridge_name() {
            ensure_bridge_device(bridge_name, config.ip(), config.netmask()).map_err(|e| {
                NetError::Interface(format!("failed to prepare shared bridge device: {e}"))
            })?;
            set_route_localnet(bridge_name).map_err(|e| {
                NetError::Interface(format!("failed to enable route_localnet on bridge: {e}"))
            })?;
            attach_interface_to_bridge(config.name(), bridge_name).map_err(|e| {
                NetError::Interface(format!("failed to attach TAP device to bridge: {e}"))
            })?;
            bring_interface_up(config.name()).map_err(|e| {
                NetError::Interface(format!("failed to bring TAP interface up: {e}"))
            })?;
        } else {
            configure_interface_ip(config.name(), config.ip(), config.netmask()).map_err(|e| {
                NetError::Interface(format!("failed to configure TAP interface IP: {e}"))
            })?;
            bring_interface_up(config.name()).map_err(|e| {
                NetError::Interface(format!("failed to bring TAP interface up: {e}"))
            })?;
            set_route_localnet(config.name()).map_err(|e| {
                NetError::Interface(format!(
                    "failed to enable route_localnet on TAP interface: {e}"
                ))
            })?;
        }

        Ok(LinuxNetworkInterface {
            name: config.name().to_owned(),
            bridge_name: config.bridge_name().map(ToOwned::to_owned),
        })
    }

    fn setup_nat(&self, config: &NatConfig) -> Result<Self::Nat, NetError> {
        ensure_docker_user_chain()?;
        let rules = generate_nat_rules(config);
        let mut applied: Vec<IptablesRule> = Vec::new();

        for rule in rules {
            if let Err(e) = rule.apply() {
                // Roll back previously applied rules on failure.
                for applied_rule in applied.into_iter().rev() {
                    let _ignore = applied_rule.remove();
                }
                return Err(NetError::Nat(format!("failed to apply rule {rule}: {e}")));
            }
            applied.push(rule);
        }

        Ok(LinuxNatHandle {
            applied_rules: applied,
        })
    }

    fn setup_port_forward(&self, mappings: &[PortMapping]) -> Result<Self::PortForward, NetError> {
        let mut applied_rules: Vec<IptablesRule> = Vec::new();

        for mapping in mappings {
            let rules = generate_port_forward_rules(mapping);
            for rule in rules {
                if let Err(e) = rule.apply() {
                    // Roll back previously applied rules on failure.
                    for applied_rule in applied_rules.into_iter().rev() {
                        let _ignore = applied_rule.remove();
                    }
                    return Err(NetError::PortForward(format!(
                        "failed to apply port-forward rule {rule}: {e}"
                    )));
                }
                applied_rules.push(rule);
            }
        }

        Ok(LinuxPortForwardHandle {
            mapping_count: mappings.len(),
            applied_rules,
        })
    }
}

// ── Network interface handle ─────────────────────────────────────────

/// Handle to a Linux TAP network interface.
///
/// The interface is automatically deleted when this handle is dropped.
pub struct LinuxNetworkInterface {
    name: String,
    bridge_name: Option<String>,
}

impl NetworkInterface for LinuxNetworkInterface {
    fn name(&self) -> &str {
        &self.name
    }
}

impl Drop for LinuxNetworkInterface {
    fn drop(&mut self) {
        if let Err(e) = delete_interface(&self.name) {
            tracing::warn!(
                name = %self.name,
                error = %e,
                "failed to delete TAP interface on drop"
            );
        }
        if let Some(bridge_name) = self.bridge_name.as_deref()
            && let Err(error) = cleanup_bridge_if_unused(bridge_name)
        {
            tracing::warn!(
                bridge = %bridge_name,
                error = %error,
                "failed to clean up shared bridge on drop"
            );
        }
    }
}

// ── NAT handle ───────────────────────────────────────────────────────

/// Handle to applied Linux NAT iptables rules.
///
/// Rules are automatically removed when this handle is dropped.
pub struct LinuxNatHandle {
    applied_rules: Vec<IptablesRule>,
}

impl NatHandle for LinuxNatHandle {
    fn rule_count(&self) -> usize {
        self.applied_rules.len()
    }

    fn teardown(&mut self) -> Result<(), NetError> {
        let mut errors = Vec::new();
        for rule in self.applied_rules.drain(..).rev() {
            if let Err(e) = rule.remove() {
                errors.push(format!("{rule}: {e}"));
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(NetError::Nat(format!(
                "failed to remove some NAT rules: {}",
                errors.join("; ")
            )))
        }
    }
}

impl Drop for LinuxNatHandle {
    fn drop(&mut self) {
        if !self.applied_rules.is_empty() {
            if let Err(e) = self.teardown() {
                tracing::warn!(error = %e, "failed to clean up NAT rules on drop");
            }
        }
    }
}

// ── Port-forward handle ──────────────────────────────────────────────

/// Handle to applied Linux port-forwarding iptables rules.
///
/// Rules are automatically removed when this handle is dropped.
pub struct LinuxPortForwardHandle {
    mapping_count: usize,
    applied_rules: Vec<IptablesRule>,
}

impl PortForwardHandle for LinuxPortForwardHandle {
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
                "failed to remove some port-forward rules: {}",
                errors.join("; ")
            )))
        }
    }
}

impl Drop for LinuxPortForwardHandle {
    fn drop(&mut self) {
        if !self.applied_rules.is_empty() {
            if let Err(e) = self.teardown() {
                tracing::warn!(error = %e, "failed to clean up port-forward rules on drop");
            }
        }
    }
}

// ── TAP packet I/O ───────────────────────────────────────────────────

/// Raw packet I/O bound to a Linux TAP interface.
///
/// This is the host-side data path used by the Linux virtio-net device. Frames
/// are read from and written to a packet socket bound to the guest TAP device.
pub struct TapPacketIo {
    fd: std::os::fd::OwnedFd,
}

impl TapPacketIo {
    /// Opens a packet socket bound to the given TAP interface.
    ///
    /// # Errors
    ///
    /// Returns [`NetError::Interface`] if the socket cannot be created or the
    /// interface binding fails.
    pub fn open(name: &str) -> Result<Self, NetError> {
        let fd = crate::platform::open_tap_interface(name)
            .map_err(|e| NetError::Interface(format!("open TAP fd for '{name}': {e}")))?;
        Ok(Self { fd })
    }
}

impl PacketIo for TapPacketIo {
    fn send(&mut self, buf: &[u8]) -> Result<usize, std::io::Error> {
        unistd::write(self.fd.as_fd(), buf).map_err(nix_err_to_io)
    }

    fn try_recv(&mut self, buf: &mut [u8]) -> Result<usize, std::io::Error> {
        unistd::read(self.fd.as_fd(), buf).map_err(nix_err_to_io)
    }
}

// ── Iptables rule helper ─────────────────────────────────────────────

/// A single iptables rule with its table and arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IptablesRule {
    /// The iptables table (nat, filter, mangle).
    pub table: String,
    /// The full argument list (e.g., ["-A", "POSTROUTING", ...]).
    pub args: Vec<String>,
}

impl IptablesRule {
    /// Apply this rule using `iptables`.
    ///
    /// # Errors
    ///
    /// Returns an error if the iptables command fails.
    pub fn apply(&self) -> Result<(), NetError> {
        let mut cmd = Command::new("iptables");
        cmd.arg("-t").arg(&self.table);
        for arg in &self.args {
            cmd.arg(arg);
        }

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NetError::Nat(format!("iptables rule failed: {stderr}")));
        }
        Ok(())
    }

    #[must_use]
    pub(crate) fn check_args(&self) -> Vec<String> {
        let mut args = self.args.clone();
        if matches!(args.first().map(String::as_str), Some("-A" | "-I")) {
            let inserted_at_position = args.first().is_some_and(|operation| operation == "-I")
                && args.get(2).is_some_and(|position| position == "1");
            "-C".clone_into(&mut args[0]);
            if inserted_at_position {
                args.remove(2);
            }
        }
        args
    }

    fn exists(&self) -> Result<bool, NetError> {
        let output = Command::new("iptables")
            .arg("-t")
            .arg(&self.table)
            .args(self.check_args())
            .output()?;
        Ok(output.status.success())
    }

    /// Generate the delete (-D) version of this rule's arguments.
    ///
    /// Replaces `-A` with `-D` in the argument list.
    #[must_use]
    pub fn delete_args(&self) -> Vec<String> {
        self.args
            .iter()
            .map(|a| {
                if a == "-A" {
                    "-D".to_owned()
                } else {
                    a.clone()
                }
            })
            .collect()
    }

    /// Remove this rule using `iptables -D`.
    ///
    /// # Errors
    ///
    /// Returns an error if the iptables command fails.
    pub fn remove(&self) -> Result<(), NetError> {
        let delete_args = self.delete_args();
        let mut cmd = Command::new("iptables");
        cmd.arg("-t").arg(&self.table);
        for arg in &delete_args {
            cmd.arg(arg);
        }

        let output = cmd.output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NetError::Nat(format!(
                "iptables delete rule failed: {stderr}"
            )));
        }
        Ok(())
    }
}

fn docker_user_chain_rules() -> [IptablesRule; 2] {
    [
        IptablesRule {
            table: "filter".to_owned(),
            args: vec![
                "-I".to_owned(),
                "FORWARD".to_owned(),
                "1".to_owned(),
                "-j".to_owned(),
                DOCKER_USER_CHAIN.to_owned(),
            ],
        },
        IptablesRule {
            table: "filter".to_owned(),
            args: vec![
                "-A".to_owned(),
                DOCKER_USER_CHAIN.to_owned(),
                "-j".to_owned(),
                "RETURN".to_owned(),
            ],
        },
    ]
}

fn ensure_docker_user_chain() -> Result<(), NetError> {
    let guard = IPTABLES_SETUP_LOCK
        .lock()
        .map_err(|error| NetError::Nat(format!("lock iptables setup: {error}")))?;
    ensure_filter_chain(DOCKER_USER_CHAIN)?;

    let [hook, terminal_return] = docker_user_chain_rules();
    let forward_rules = list_filter_chain_rules("FORWARD")?;
    let repair = docker_user_hook_repair_plan(&forward_rules);
    let remove_hook = IptablesRule {
        table: "filter".to_owned(),
        args: vec![
            "-D".to_owned(),
            "FORWARD".to_owned(),
            "-j".to_owned(),
            DOCKER_USER_CHAIN.to_owned(),
        ],
    };
    for _ in 0..repair.remove_count {
        remove_hook.apply()?;
    }
    if repair.insert_at_head {
        hook.apply()?;
    }
    if !terminal_return.exists()? {
        terminal_return.apply()?;
    }
    drop(guard);
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct DockerUserHookRepairPlan {
    remove_count: usize,
    insert_at_head: bool,
}

fn docker_user_hook_repair_plan(forward_rules: &str) -> DockerUserHookRepairPlan {
    let rules = forward_rules
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("-A FORWARD "))
        .collect::<Vec<_>>();
    let hook_count = rules
        .iter()
        .filter(|line| **line == "-A FORWARD -j DOCKER-USER")
        .count();
    let already_normalized =
        hook_count == 1 && rules.first().copied() == Some("-A FORWARD -j DOCKER-USER");

    DockerUserHookRepairPlan {
        remove_count: if already_normalized { 0 } else { hook_count },
        insert_at_head: !already_normalized,
    }
}

fn list_filter_chain_rules(chain: &str) -> Result<String, NetError> {
    let output = Command::new("iptables")
        .args(["-t", "filter", "-S", chain])
        .output()?;
    if !output.status.success() {
        return Err(NetError::Nat(format!(
            "iptables -t filter -S {chain} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

fn ensure_filter_chain(chain: &str) -> Result<(), NetError> {
    if filter_chain_exists(chain)? {
        return Ok(());
    }

    let output = Command::new("iptables")
        .args(["-t", "filter", "-N", chain])
        .output()?;
    if output.status.success() || filter_chain_exists(chain)? {
        return Ok(());
    }

    Err(NetError::Nat(format!(
        "failed to create {chain} chain: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

fn filter_chain_exists(chain: &str) -> Result<bool, NetError> {
    let output = Command::new("iptables")
        .args(["-t", "filter", "-n", "-L", chain])
        .output()?;
    Ok(output.status.success())
}

impl fmt::Display for IptablesRule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "iptables -t {} {}", self.table, self.args.join(" "))
    }
}

fn list_visor_iptables_rules(table: &str) -> Result<Vec<IptablesRule>, NetError> {
    let output = Command::new("iptables")
        .args(["-t", table, "-S"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NetError::Nat(format!(
            "iptables -t {table} -S failed: {stderr}"
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(parse_visor_iptables_rules(table, &stdout))
}

fn list_visor_iptables_rules_with_comment(
    comment_prefix: &str,
) -> Result<Vec<IptablesRule>, NetError> {
    let mut rules = Vec::new();
    for table in VISOR_IPTABLES_TABLES {
        rules.extend(
            list_visor_iptables_rules(table)?
                .into_iter()
                .filter(|rule| rule.args.iter().any(|arg| arg.contains(comment_prefix))),
        );
    }
    Ok(rules)
}

fn parse_visor_iptables_rules(table: &str, output: &str) -> Vec<IptablesRule> {
    output
        .lines()
        .filter_map(|line| parse_visor_iptables_rule(table, line))
        .collect()
}

fn parse_visor_iptables_rule(table: &str, line: &str) -> Option<IptablesRule> {
    if !line.starts_with("-A ") || !is_visor_iptables_rule(line) {
        return None;
    }

    let args = line
        .split_whitespace()
        .map(strip_iptables_token_quotes)
        .collect::<Vec<_>>();
    Some(IptablesRule {
        table: table.to_owned(),
        args,
    })
}

fn is_visor_iptables_rule(line: &str) -> bool {
    line.contains("--comment") && line.contains(VISOR_IPTABLES_TAG_PREFIX)
}

fn strip_iptables_token_quotes(token: &str) -> String {
    token.trim_matches('"').to_owned()
}

// ── Rule generators ──────────────────────────────────────────────────

/// Generate iptables rules for NAT MASQUERADE.
fn generate_nat_rules(config: &NatConfig) -> Vec<IptablesRule> {
    let comment = format!("visor-nat-{}", config.interface());
    vec![
        // POSTROUTING MASQUERADE for outbound traffic
        IptablesRule {
            table: "nat".to_owned(),
            args: vec![
                "-A".to_owned(),
                "POSTROUTING".to_owned(),
                "-s".to_owned(),
                config.subnet().to_owned(),
                "!".to_owned(),
                "-o".to_owned(),
                config.interface().to_owned(),
                "-j".to_owned(),
                "MASQUERADE".to_owned(),
                "-m".to_owned(),
                "comment".to_owned(),
                "--comment".to_owned(),
                comment.clone(),
            ],
        },
        // FORWARD: allow traffic from the TAP interface
        IptablesRule {
            table: "filter".to_owned(),
            args: vec![
                "-A".to_owned(),
                "FORWARD".to_owned(),
                "-i".to_owned(),
                config.interface().to_owned(),
                "-j".to_owned(),
                "ACCEPT".to_owned(),
                "-m".to_owned(),
                "comment".to_owned(),
                "--comment".to_owned(),
                comment.clone(),
            ],
        },
        // FORWARD: allow guest-subnet traffic to other guest TAP interfaces
        IptablesRule {
            table: "filter".to_owned(),
            args: vec![
                "-A".to_owned(),
                "FORWARD".to_owned(),
                "-o".to_owned(),
                config.interface().to_owned(),
                "-s".to_owned(),
                VISOR_GUEST_SUPERNET.to_owned(),
                "-j".to_owned(),
                "ACCEPT".to_owned(),
                "-m".to_owned(),
                "comment".to_owned(),
                "--comment".to_owned(),
                comment.clone(),
            ],
        },
        // FORWARD: allow established/related return traffic
        IptablesRule {
            table: "filter".to_owned(),
            args: vec![
                "-A".to_owned(),
                "FORWARD".to_owned(),
                "-o".to_owned(),
                config.interface().to_owned(),
                "-m".to_owned(),
                "state".to_owned(),
                "--state".to_owned(),
                "RELATED,ESTABLISHED".to_owned(),
                "-j".to_owned(),
                "ACCEPT".to_owned(),
                "-m".to_owned(),
                "comment".to_owned(),
                "--comment".to_owned(),
                comment,
            ],
        },
    ]
}

fn generate_shared_nat_rules(
    interface: &str,
    subnet: &str,
    shared_supernet: &str,
) -> Vec<IptablesRule> {
    let comment = format!("visor-sharednat-{interface}");
    vec![
        IptablesRule {
            table: "nat".to_owned(),
            args: vec![
                "-A".to_owned(),
                "POSTROUTING".to_owned(),
                "-s".to_owned(),
                subnet.to_owned(),
                "!".to_owned(),
                "-o".to_owned(),
                interface.to_owned(),
                "-j".to_owned(),
                "MASQUERADE".to_owned(),
                "-m".to_owned(),
                "comment".to_owned(),
                "--comment".to_owned(),
                comment.clone(),
            ],
        },
        IptablesRule {
            table: "filter".to_owned(),
            args: vec![
                "-A".to_owned(),
                "FORWARD".to_owned(),
                "-i".to_owned(),
                interface.to_owned(),
                "-d".to_owned(),
                subnet.to_owned(),
                "-j".to_owned(),
                "ACCEPT".to_owned(),
                "-m".to_owned(),
                "comment".to_owned(),
                "--comment".to_owned(),
                comment.clone(),
            ],
        },
        IptablesRule {
            table: "filter".to_owned(),
            args: vec![
                "-A".to_owned(),
                "FORWARD".to_owned(),
                "-i".to_owned(),
                interface.to_owned(),
                "-d".to_owned(),
                shared_supernet.to_owned(),
                "-j".to_owned(),
                "DROP".to_owned(),
                "-m".to_owned(),
                "comment".to_owned(),
                "--comment".to_owned(),
                comment.clone(),
            ],
        },
        IptablesRule {
            table: "filter".to_owned(),
            args: vec![
                "-A".to_owned(),
                "FORWARD".to_owned(),
                "-i".to_owned(),
                interface.to_owned(),
                "-j".to_owned(),
                "ACCEPT".to_owned(),
                "-m".to_owned(),
                "comment".to_owned(),
                "--comment".to_owned(),
                comment.clone(),
            ],
        },
        IptablesRule {
            table: "filter".to_owned(),
            args: vec![
                "-A".to_owned(),
                "FORWARD".to_owned(),
                "-o".to_owned(),
                interface.to_owned(),
                "-m".to_owned(),
                "state".to_owned(),
                "--state".to_owned(),
                "RELATED,ESTABLISHED".to_owned(),
                "-j".to_owned(),
                "ACCEPT".to_owned(),
                "-m".to_owned(),
                "comment".to_owned(),
                "--comment".to_owned(),
                comment,
            ],
        },
    ]
}

/// Generate iptables rules for port forwarding via DNAT.
fn generate_port_forward_rules(mapping: &PortMapping) -> Vec<IptablesRule> {
    let comment = port_forward_comment(mapping);
    let host_ip = mapping.host_ip();
    let mut rules = vec![
        build_port_forward_dnat_rule("PREROUTING", mapping, host_ip, &comment),
        build_port_forward_dnat_rule("OUTPUT", mapping, host_ip, &comment),
        build_port_forward_loopback_nat_rule(mapping, &comment),
        build_port_forward_filter_rule(mapping, &comment),
    ];
    if host_ip.is_some() {
        rules.insert(3, build_port_forward_hairpin_nat_rule(mapping, &comment));
    }
    rules
}

fn port_forward_comment(mapping: &PortMapping) -> String {
    format!(
        "visor-portfwd-{}:{}-{}:{}",
        mapping.host_port(),
        mapping.protocol(),
        mapping.guest_ip(),
        mapping.guest_port()
    )
}

fn build_port_forward_dnat_rule(
    chain: &str,
    mapping: &PortMapping,
    host_ip: Option<Ipv4Addr>,
    comment: &str,
) -> IptablesRule {
    let mut args = vec![
        "-A".to_owned(),
        chain.to_owned(),
        "-p".to_owned(),
        mapping.protocol().to_owned(),
    ];
    if let Some(host_ip) = host_ip {
        args.push("-d".to_owned());
        args.push(host_ip.to_string());
    }
    args.extend([
        "--dport".to_owned(),
        mapping.host_port().to_string(),
        "-j".to_owned(),
        "DNAT".to_owned(),
        "--to-destination".to_owned(),
        format!("{}:{}", mapping.guest_ip(), mapping.guest_port()),
        "-m".to_owned(),
        "comment".to_owned(),
        "--comment".to_owned(),
        comment.to_owned(),
    ]);
    IptablesRule {
        table: "nat".to_owned(),
        args,
    }
}

fn build_port_forward_loopback_nat_rule(mapping: &PortMapping, comment: &str) -> IptablesRule {
    IptablesRule {
        table: "nat".to_owned(),
        args: vec![
            "-A".to_owned(),
            "POSTROUTING".to_owned(),
            "-p".to_owned(),
            mapping.protocol().to_owned(),
            "-s".to_owned(),
            "127.0.0.1/32".to_owned(),
            "-d".to_owned(),
            mapping.guest_ip().to_string(),
            "--dport".to_owned(),
            mapping.guest_port().to_string(),
            "-j".to_owned(),
            "MASQUERADE".to_owned(),
            "-m".to_owned(),
            "comment".to_owned(),
            "--comment".to_owned(),
            comment.to_owned(),
        ],
    }
}

fn build_port_forward_hairpin_nat_rule(mapping: &PortMapping, comment: &str) -> IptablesRule {
    IptablesRule {
        table: "nat".to_owned(),
        args: vec![
            "-A".to_owned(),
            "POSTROUTING".to_owned(),
            "-p".to_owned(),
            mapping.protocol().to_owned(),
            "-d".to_owned(),
            mapping.guest_ip().to_string(),
            "--dport".to_owned(),
            mapping.guest_port().to_string(),
            "-j".to_owned(),
            "MASQUERADE".to_owned(),
            "-m".to_owned(),
            "comment".to_owned(),
            "--comment".to_owned(),
            comment.to_owned(),
        ],
    }
}

fn build_port_forward_filter_rule(mapping: &PortMapping, comment: &str) -> IptablesRule {
    IptablesRule {
        table: "filter".to_owned(),
        args: vec![
            "-A".to_owned(),
            "FORWARD".to_owned(),
            "-p".to_owned(),
            mapping.protocol().to_owned(),
            "-d".to_owned(),
            mapping.guest_ip().to_string(),
            "--dport".to_owned(),
            mapping.guest_port().to_string(),
            "-j".to_owned(),
            "ACCEPT".to_owned(),
            "-m".to_owned(),
            "comment".to_owned(),
            "--comment".to_owned(),
            comment.to_owned(),
        ],
    }
}

// ── Low-level helpers ────────────────────────────────────────────────

/// Create a TAP device using `ip tuntap add`.
fn create_tap_device(name: &str) -> Result<(), NetError> {
    let output = Command::new("ip")
        .args(["tuntap", "add", "dev", name, "mode", "tap"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NetError::Interface(format!(
            "ip tuntap add failed: {stderr}"
        )));
    }

    Ok(())
}

fn ensure_bridge_device(
    name: &str,
    ip: std::net::Ipv4Addr,
    netmask: std::net::Ipv4Addr,
) -> Result<(), NetError> {
    if !link_exists(name)? {
        let output = Command::new("ip")
            .args(["link", "add", name, "type", "bridge"])
            .output()?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(NetError::Interface(format!(
                "ip link add bridge failed: {stderr}"
            )));
        }
    }

    if !interface_has_address(name, ip, netmask)? {
        configure_interface_ip(name, ip, netmask)?;
    }
    bring_interface_up(name)?;
    Ok(())
}

fn attach_interface_to_bridge(name: &str, bridge_name: &str) -> Result<(), NetError> {
    let output = Command::new("ip")
        .args(["link", "set", name, "master", bridge_name])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NetError::Interface(format!(
            "ip link set master failed: {stderr}"
        )));
    }

    Ok(())
}

/// Configure the IP address and netmask on a network interface via `ip` command.
fn configure_interface_ip(
    name: &str,
    ip: std::net::Ipv4Addr,
    netmask: std::net::Ipv4Addr,
) -> Result<(), NetError> {
    let prefix_len = netmask_to_prefix(netmask);
    let output = Command::new("ip")
        .args(["addr", "add", &format!("{ip}/{prefix_len}"), "dev", name])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NetError::Interface(format!("ip addr add failed: {stderr}")));
    }

    Ok(())
}

/// Bring a network interface up.
fn bring_interface_up(name: &str) -> Result<(), NetError> {
    let output = Command::new("ip")
        .args(["link", "set", name, "up"])
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(NetError::Interface(format!(
            "ip link set up failed: {stderr}"
        )));
    }

    Ok(())
}

/// Delete a network interface.
fn delete_interface(name: &str) -> Result<(), NetError> {
    let output = Command::new("ip").args(["link", "delete", name]).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        // Ignore "not found" errors during cleanup
        if !stderr.contains("Cannot find device") {
            return Err(NetError::Interface(format!(
                "ip link delete failed: {stderr}"
            )));
        }
    }

    Ok(())
}

fn link_exists(name: &str) -> Result<bool, NetError> {
    let output = Command::new("ip")
        .args(["link", "show", "dev", name])
        .output()?;
    Ok(output.status.success())
}

fn interface_has_address(
    name: &str,
    ip: std::net::Ipv4Addr,
    netmask: std::net::Ipv4Addr,
) -> Result<bool, NetError> {
    let prefix_len = netmask_to_prefix(netmask);
    let output = Command::new("ip")
        .args(["-4", "addr", "show", "dev", name])
        .output()?;
    if !output.status.success() {
        return Ok(false);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(stdout.contains(&format!("inet {ip}/{prefix_len}")))
}

fn bridge_has_member_interfaces(name: &str) -> Result<bool, NetError> {
    let output = Command::new("ip")
        .args(["-o", "link", "show", "master", name])
        .output()?;

    if !output.status.success() {
        return Ok(false);
    }

    Ok(!String::from_utf8_lossy(&output.stdout).trim().is_empty())
}

pub(crate) fn ensure_shared_bridge_nat(interface: &str, subnet: &str) -> Result<(), NetError> {
    ensure_docker_user_chain()?;
    let shared_supernet = visor_types::named_network_supernet().cidr();
    let desired_rules = generate_shared_nat_rules(interface, subnet, &shared_supernet);
    let comment_prefix = format!("visor-sharednat-{interface}");
    let existing_rules = list_visor_iptables_rules_with_comment(&comment_prefix)?;

    if existing_rules == desired_rules {
        return Ok(());
    }

    for rule in existing_rules.into_iter().rev() {
        rule.remove()?;
    }

    for rule in desired_rules {
        rule.apply()?;
    }
    Ok(())
}

fn cleanup_shared_bridge_nat(interface: &str) -> Result<(), NetError> {
    let comment_prefix = format!("visor-sharednat-{interface}");
    let rules = list_visor_iptables_rules_with_comment(&comment_prefix)?;
    for rule in rules.into_iter().rev() {
        let _ignore = rule.remove();
    }
    Ok(())
}

fn cleanup_bridge_if_unused(bridge_name: &str) -> Result<(), NetError> {
    if !link_exists(bridge_name)? || bridge_has_member_interfaces(bridge_name)? {
        return Ok(());
    }

    cleanup_shared_bridge_nat(bridge_name)?;
    delete_interface(bridge_name)
}

fn route_localnet_sysctl_path(interface: &str) -> String {
    format!("/proc/sys/net/ipv4/conf/{interface}/route_localnet")
}

/// Enable `route_localnet`, which only affects publishing guest ports on
/// loopback.
///
/// A sandbox that mounts `/proc/sys` read-only — every unprivileged container
/// — makes this unwritable, and refusing to boot the VM over it trades the
/// whole machine for one optional feature. So a refusal is a warning and the
/// caller continues; anything else is a real failure and propagates.
fn set_route_localnet(interface: &str) -> Result<(), NetError> {
    let path = route_localnet_sysctl_path(interface);
    match std::fs::write(&path, "1") {
        Ok(()) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ReadOnlyFilesystem | std::io::ErrorKind::PermissionDenied
            ) =>
        {
            tracing::warn!(
                path = %path,
                %error,
                "cannot enable route_localnet; publishing guest ports on loopback will not work"
            );
            Ok(())
        }
        Err(error) => Err(NetError::Interface(format!("write {path}: {error}"))),
    }
}

fn nix_err_to_io(error: nix::errno::Errno) -> std::io::Error {
    std::io::Error::from_raw_os_error(error as i32)
}

/// Convert a netmask (e.g. 255.255.255.0) to a prefix length (e.g. 24).
fn netmask_to_prefix(netmask: std::net::Ipv4Addr) -> u8 {
    u8::try_from(u32::from(netmask).leading_ones()).unwrap_or(32)
}

#[cfg(test)]
#[path = "linux_test.rs"]
mod tests;
