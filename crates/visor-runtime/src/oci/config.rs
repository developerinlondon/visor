//! OCI image configuration extraction.
//!
//! Parses the OCI image config JSON to extract CMD, ENTRYPOINT, ENV,
//! WORKDIR, USER, and other runtime settings.

use std::collections::HashMap;

use anyhow::Context;

/// Raw OCI image config JSON structure (top-level).
///
/// This is the intermediate deserialization target matching the OCI image
/// config spec. Only the `config` field is extracted; other top-level
/// fields (architecture, os, rootfs, history) are ignored.
#[derive(Debug, serde::Deserialize)]
struct RawOciImageConfig {
    config: Option<RawContainerConfig>,
}

/// Raw container runtime config section from the OCI image config.
///
/// Field names use OCI spec capitalization (e.g. `Cmd`, `Entrypoint`).
#[derive(Debug, Default, serde::Deserialize)]
#[serde(default)]
struct RawContainerConfig {
    #[serde(rename = "Cmd")]
    cmd: Option<Vec<String>>,

    #[serde(rename = "Entrypoint")]
    entrypoint: Option<Vec<String>>,

    #[serde(rename = "Env")]
    env: Option<Vec<String>>,

    #[serde(rename = "WorkingDir")]
    working_dir: Option<String>,

    #[serde(rename = "User")]
    user: Option<String>,

    #[serde(rename = "ExposedPorts")]
    exposed_ports: Option<HashMap<String, serde_json::Value>>,

    #[serde(rename = "Labels")]
    labels: Option<HashMap<String, String>>,

    #[serde(rename = "StopSignal")]
    stop_signal: Option<String>,
}

/// Parsed OCI image configuration.
///
/// Extracted from the OCI image config JSON blob returned by a registry.
/// Contains the runtime-relevant fields: command, entrypoint, environment
/// variables, working directory, user, exposed ports, labels, and stop signal.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ImageConfig {
    /// Default command arguments (Dockerfile `CMD`).
    pub cmd: Option<Vec<String>>,

    /// Container entrypoint (Dockerfile `ENTRYPOINT`).
    pub entrypoint: Option<Vec<String>>,

    /// Environment variables in `KEY=VALUE` format (Dockerfile `ENV`).
    pub env: Vec<String>,

    /// Working directory inside the container (Dockerfile `WORKDIR`).
    pub working_dir: Option<String>,

    /// User to run as inside the container (Dockerfile `USER`).
    pub user: Option<String>,

    /// TCP/UDP ports exposed by the image (Dockerfile `EXPOSE`), sorted ascending.
    pub exposed_ports: Vec<u16>,

    /// Key-value labels attached to the image (Dockerfile `LABEL`).
    pub labels: HashMap<String, String>,

    /// Signal sent to the container on stop (Dockerfile `STOPSIGNAL`).
    pub stop_signal: Option<String>,
}

impl ImageConfig {
    /// Parse an OCI image config JSON blob into an [`ImageConfig`].
    ///
    /// The JSON is expected to have the top-level OCI image config structure
    /// with a `"config"` key containing runtime settings.
    ///
    /// # Errors
    ///
    /// Returns an error if the JSON is malformed or cannot be deserialized
    /// into the expected OCI image config structure.
    pub fn from_json(json: &[u8]) -> anyhow::Result<Self> {
        let raw: RawOciImageConfig =
            serde_json::from_slice(json).context("failed to parse OCI image config JSON")?;

        let container = raw
            .config
            .context("OCI image config JSON missing required 'config' key")?;

        let exposed_ports = parse_exposed_ports(container.exposed_ports.as_ref());

        Ok(Self {
            cmd: container.cmd,
            entrypoint: container.entrypoint,
            env: container.env.unwrap_or_default(),
            working_dir: normalize_optional_string(container.working_dir),
            user: normalize_optional_string(container.user),
            exposed_ports,
            labels: container.labels.unwrap_or_default(),
            stop_signal: normalize_optional_string(container.stop_signal),
        })
    }

    /// Compute the effective command to run in the container.
    ///
    /// Combines entrypoint and cmd following Docker/OCI semantics:
    /// - If both are set, entrypoint args come first, then cmd args
    /// - If only entrypoint is set, returns entrypoint
    /// - If only cmd is set, returns cmd
    /// - If neither is set, returns an empty vector
    #[must_use]
    pub fn effective_command(&self) -> Vec<String> {
        match (&self.entrypoint, &self.cmd) {
            (Some(ep), Some(cmd)) => {
                let mut result = ep.clone();
                result.extend(cmd.iter().cloned());
                result
            }
            (Some(ep), None) => ep.clone(),
            (None, Some(cmd)) => cmd.clone(),
            (None, None) => Vec::new(),
        }
    }
}

/// Parse OCI `ExposedPorts` map keys like `"8080/tcp"` into port numbers.
///
/// Invalid entries (missing `/`, non-numeric port) are silently skipped.
/// The returned list is sorted in ascending order.
fn parse_exposed_ports(ports: Option<&HashMap<String, serde_json::Value>>) -> Vec<u16> {
    let Some(ports) = ports else {
        return Vec::new();
    };

    let mut result: Vec<u16> = ports
        .keys()
        .filter_map(|key| {
            let port_str = key.split('/').next()?;
            port_str.parse::<u16>().ok()
        })
        .collect();

    result.sort_unstable();
    result
}

/// Convert an `Option<String>` to `None` if the string is empty.
fn normalize_optional_string(value: Option<String>) -> Option<String> {
    value.filter(|s| !s.is_empty())
}

#[cfg(test)]
#[path = "config_test.rs"]
mod tests;
