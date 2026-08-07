//! Run configuration for the guest VM.
//!
//! Defines the [`RunConfig`] struct that describes what the guest should
//! execute, including the command, environment, networking, and volumes.

use anyhow::{Context, bail};
use base64::Engine as _;
use serde::{Deserialize, Serialize};

/// Configuration for a guest VM run, received from the host via vsock.
///
/// Contains everything visor-init needs to set up the guest environment
/// and execute the user's command.
///
/// # Examples
///
/// ```
/// use visor_init::config::RunConfig;
///
/// let json = r#"{"cmd": ["/bin/echo", "hello"]}"#;
/// let config = RunConfig::from_json(json).unwrap();
/// assert_eq!(config.cmd, vec!["/bin/echo", "hello"]);
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct RunConfig {
    /// Command and arguments to execute (e.g., `["/bin/echo", "hello"]`).
    #[serde(rename = "c", alias = "cmd")]
    #[serde(skip_serializing_if = "is_default_cmd")]
    pub cmd: Vec<String>,
    /// Environment variables as `KEY=VALUE` pairs.
    #[serde(rename = "e", alias = "env")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<String>,
    /// Working directory inside the guest.
    #[serde(rename = "w", alias = "workdir")]
    #[serde(skip_serializing_if = "is_default_workdir")]
    pub workdir: String,
    /// Network configuration for the guest.
    #[serde(rename = "n", alias = "network")]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub network: Option<NetworkConfig>,
    /// Network attachments for the guest.
    #[serde(rename = "ns", alias = "networks")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub networks: Vec<NetworkConfig>,
    /// Static host entries written to `/etc/hosts`.
    #[serde(rename = "h", alias = "extra_hosts")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub extra_hosts: Vec<HostEntry>,
    /// Volume mounts from host to guest.
    #[serde(rename = "v", alias = "volumes")]
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<VolumeConfig>,
    /// Operating mode: `"run"` (default) executes a command, `"agent"` starts the vsock listener.
    #[serde(rename = "m", alias = "mode")]
    #[serde(skip_serializing_if = "is_default_mode")]
    pub mode: String,
    /// When `true`, also start the vsock exec listener while running the main command.
    #[serde(rename = "x", alias = "exec_listener")]
    #[serde(skip_serializing_if = "is_false")]
    pub exec_listener: bool,
}

/// Static hostname mapping written to `/etc/hosts`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct HostEntry {
    /// Hostname that should resolve inside the guest.
    #[serde(rename = "h", alias = "hostname")]
    pub hostname: String,
    /// IPv4 address for the hostname.
    #[serde(rename = "a", alias = "address")]
    pub address: String,
}

impl HostEntry {
    /// Create a new static host entry.
    #[must_use]
    pub fn new(hostname: impl Into<String>, address: impl Into<String>) -> Self {
        Self {
            hostname: hostname.into(),
            address: address.into(),
        }
    }
}

/// Network configuration for the guest interface.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NetworkConfig {
    /// Optional logical network name.
    #[serde(rename = "n", alias = "name")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Guest interface name. Defaults to `eth0`, `eth1`, ... by attachment order.
    #[serde(rename = "i", alias = "interface")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interface: Option<String>,
    /// IPv4 address (e.g., `"10.0.0.2"`).
    #[serde(rename = "a", alias = "address")]
    pub address: String,
    /// Subnet mask (e.g., `"255.255.255.0"`).
    #[serde(rename = "m", alias = "netmask")]
    pub netmask: String,
    /// Default gateway (e.g., `"10.0.0.1"`).
    #[serde(rename = "g", alias = "gateway")]
    pub gateway: String,
    /// DNS server addresses (e.g., `["10.0.0.1"]`).
    ///
    /// Written to `/etc/resolv.conf` on guest boot. Defaults to
    /// the gateway address if empty.
    #[serde(rename = "d", alias = "dns_servers")]
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dns_servers: Vec<String>,
    /// Whether this attachment should install the guest default route.
    #[serde(rename = "r", alias = "default_route")]
    #[serde(
        default = "default_network_default_route",
        skip_serializing_if = "is_default_network_default_route"
    )]
    pub default_route: bool,
}

const fn default_network_default_route() -> bool {
    true
}

fn is_default_cmd(cmd: &[String]) -> bool {
    cmd.len() == 1 && cmd.first().is_some_and(|value| value == "/bin/sh")
}

fn is_default_workdir(workdir: &str) -> bool {
    workdir == "/"
}

fn is_default_mode(mode: &str) -> bool {
    mode == "run"
}

// serde's skip_serializing_if takes a fn(&T) -> bool, so the reference is
// the external contract rather than a choice clippy can improve on.
#[expect(clippy::trivially_copy_pass_by_ref, reason = "serde skip_serializing_if signature")]
const fn is_false(value: &bool) -> bool {
    !*value
}

#[expect(clippy::trivially_copy_pass_by_ref, reason = "serde skip_serializing_if signature")]
const fn is_default_network_default_route(value: &bool) -> bool {
    *value
}

/// Volume mount configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(default)]
#[non_exhaustive]
pub struct VolumeConfig {
    /// Path on the host.
    #[serde(rename = "h", alias = "host_path")]
    pub host_path: String,
    /// Mount point inside the guest.
    #[serde(rename = "g", alias = "guest_path")]
    pub guest_path: String,
    /// Whether the mount is read-only.
    #[serde(rename = "r", alias = "read_only")]
    #[serde(skip_serializing_if = "is_false")]
    pub read_only: bool,
    /// Virtio-fs tag exposed by the VMM for directory sharing.
    #[serde(rename = "t", alias = "mount_tag")]
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub mount_tag: String,
    /// Guest-visible block device path for file-backed volumes.
    #[serde(rename = "d", alias = "device_path")]
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub device_path: String,
    /// Filesystem type for block-device mounts.
    #[serde(rename = "f", alias = "fs_type")]
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub fs_type: String,
}

impl RunConfig {
    /// Returns the effective guest network attachments.
    #[must_use]
    pub fn effective_networks(&self) -> Vec<NetworkConfig> {
        if !self.networks.is_empty() {
            return self.networks.clone();
        }

        self.network.iter().cloned().collect()
    }

    /// Parse a [`RunConfig`] from a JSON string.
    ///
    /// Missing optional fields are filled with defaults via `#[serde(default)]`.
    /// Unknown fields are silently ignored.
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON is malformed or field types do not match.
    #[must_use = "parsing result should be checked"]
    pub fn from_json(json: &str) -> anyhow::Result<Self> {
        serde_json::from_str(json).context("failed to parse RunConfig from JSON")
    }

    /// Serialize this [`RunConfig`] to a JSON string.
    ///
    /// # Errors
    ///
    /// Returns an error if serialization fails (should not happen for valid configs).
    #[must_use = "serialization result should be checked"]
    pub fn to_json(&self) -> anyhow::Result<String> {
        serde_json::to_string(self).context("failed to serialize RunConfig to JSON")
    }

    /// Parse a [`RunConfig`] from any reader (e.g., a file, socket, or byte buffer).
    ///
    /// # Errors
    ///
    /// Returns an error if reading fails or the content is not valid JSON.
    #[must_use = "parsing result should be checked"]
    pub fn from_reader(reader: impl std::io::Read) -> anyhow::Result<Self> {
        serde_json::from_reader(reader).context("failed to parse RunConfig from reader")
    }

    /// Read configuration from the kernel command line.
    ///
    /// Reads `/proc/cmdline` and looks for `visor.config=<base64>`. The value
    /// is base64-decoded (standard alphabet, no padding required) and parsed
    /// as JSON into a [`RunConfig`].
    ///
    /// If `/proc/cmdline` is unreadable or `visor.config=` is absent,
    /// returns [`RunConfig::default()`].
    #[must_use]
    pub fn from_kernel_cmdline() -> Self {
        let Ok(cmdline) = std::fs::read_to_string("/proc/cmdline") else {
            return Self::default();
        };
        Self::parse_cmdline(&cmdline).unwrap_or_default()
    }

    /// Parse a `visor.config=<base64>` parameter from a raw cmdline string.
    ///
    /// Splits on whitespace and looks for a parameter starting with `visor.config=`.
    /// Returns `None` if the parameter is not found.
    ///
    /// # Errors
    ///
    /// Returns an error if the base64 decoding or JSON parsing fails.
    #[must_use]
    pub fn parse_cmdline(cmdline: &str) -> Option<Self> {
        let encoded = cmdline
            .split_whitespace()
            .find_map(|param| param.strip_prefix("visor.config="))?;

        let engine = base64::engine::general_purpose::STANDARD_NO_PAD;
        let bytes = engine.decode(encoded).ok()?;
        let json = std::str::from_utf8(&bytes).ok()?;
        Self::from_json(json).ok()
    }

    /// Validate that this configuration is semantically correct.
    ///
    /// Checks:
    /// - `cmd` must not be empty
    /// - `workdir` must start with `/`
    /// - If `network` is present, `address`, `netmask`, and `gateway` must be non-empty
    /// - Each volume must specify a guest path and at least one mount source
    ///
    /// # Errors
    ///
    /// Returns an error describing the first validation failure found.
    pub fn validate(&self) -> anyhow::Result<()> {
        if self.cmd.is_empty() {
            bail!("cmd must not be empty");
        }
        if !self.workdir.starts_with('/') {
            bail!("workdir must start with '/' but got '{}'", self.workdir);
        }
        for net in self.effective_networks() {
            if net.address.is_empty() {
                bail!("network address must not be empty");
            }
            if net.netmask.is_empty() {
                bail!("network netmask must not be empty");
            }
            if net.gateway.is_empty() {
                bail!("network gateway must not be empty");
            }
        }
        for volume in &self.volumes {
            if volume.guest_path.is_empty() {
                bail!("volume guest_path must not be empty");
            }
            if !volume.guest_path.starts_with('/') {
                bail!("volume guest_path must be absolute: {}", volume.guest_path);
            }
            if volume.host_path.is_empty()
                && volume.mount_tag.is_empty()
                && volume.device_path.is_empty()
            {
                bail!(
                    "volume {} must provide host_path, mount_tag, or device_path",
                    volume.guest_path
                );
            }
        }
        for host in &self.extra_hosts {
            if host.hostname.is_empty() {
                bail!("extra_hosts hostname must not be empty");
            }
            if host.address.is_empty() {
                bail!("extra_hosts address must not be empty");
            }
        }
        Ok(())
    }
}

impl Default for RunConfig {
    fn default() -> Self {
        Self {
            cmd: vec!["/bin/sh".to_owned()],
            env: Vec::new(),
            workdir: "/".to_owned(),
            network: None,
            networks: Vec::new(),
            extra_hosts: Vec::new(),
            volumes: Vec::new(),
            mode: "run".to_owned(),
            exec_listener: false,
        }
    }
}

impl Default for NetworkConfig {
    fn default() -> Self {
        Self {
            name: None,
            interface: None,
            address: "10.0.0.2".to_owned(),
            netmask: "255.255.255.0".to_owned(),
            gateway: "10.0.0.1".to_owned(),
            dns_servers: Vec::new(),
            default_route: default_network_default_route(),
        }
    }
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
