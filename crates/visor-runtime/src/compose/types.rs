//! Types for docker-compose.yml project configuration.
//!
//! Defines the internal representation of a Compose project including
//! services, networks, volumes, and their relationships. All types
//! support serde deserialization from YAML.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// A parsed docker-compose.yml project.
///
/// Contains all services, networks, and volumes defined in a compose file.
/// Use [`super::parser::parse_compose`] or [`super::parser::parse_compose_file`]
/// to construct this from YAML.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ComposeProject {
    /// Optional project name (from top-level `name:` field).
    pub name: Option<String>,

    /// Map of service name to service definition.
    pub services: HashMap<String, ComposeService>,

    /// Map of network name to network configuration.
    #[serde(default)]
    pub networks: HashMap<String, ComposeNetwork>,

    /// Map of volume name to volume configuration.
    #[serde(default)]
    pub volumes: HashMap<String, ComposeVolumeConfig>,
}

impl ComposeProject {
    /// Validates the compose project for internal consistency.
    ///
    /// Checks that:
    /// - Every service has a non-empty `image` field
    /// - All `depends_on` targets reference existing services
    ///
    /// # Errors
    ///
    /// Returns an error describing the first validation failure found.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (name, service) in &self.services {
            anyhow::ensure!(
                !service.image.is_empty(),
                "service '{name}' is missing a required 'image' field"
            );

            let dep_names = match &service.depends_on {
                ComposeDependsOn::Empty => Vec::new(),
                ComposeDependsOn::Simple(names) => names.clone(),
                ComposeDependsOn::Extended(map) => map.keys().cloned().collect(),
            };

            for dep in &dep_names {
                anyhow::ensure!(
                    self.services.contains_key(dep),
                    "service '{name}' depends on '{dep}', which is not defined"
                );
            }
        }

        Ok(())
    }
}

/// A service definition within a compose project.
///
/// Represents a single container/microVM to be run, with its image,
/// configuration, resource limits, and relationships to other services.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ComposeService {
    /// Container image reference (e.g. `"nginx:latest"`).
    pub image: String,

    /// Override command (can be a string or list in YAML).
    #[serde(default, deserialize_with = "deserialize_command")]
    pub command: Option<Vec<String>>,

    /// Environment variables (list or map format).
    #[serde(default)]
    pub environment: ComposeEnvironment,

    /// Port mappings (short or long syntax).
    #[serde(default)]
    pub ports: Vec<ComposePort>,

    /// Volume mounts (`"host:guest"` or `"named:guest"` format).
    #[serde(default)]
    pub volumes: Vec<String>,

    /// Service dependencies (simple list or extended with conditions).
    #[serde(default)]
    pub depends_on: ComposeDependsOn,

    /// Networks this service connects to.
    #[serde(default)]
    pub networks: Vec<String>,

    /// Memory limit (e.g. `"512m"`, `"1g"`).
    pub mem_limit: Option<String>,

    /// CPU limit as a float (e.g. `1.5`).
    pub cpus: Option<f64>,

    /// Container hostname.
    pub hostname: Option<String>,

    /// Working directory inside the container.
    pub working_dir: Option<String>,

    /// Key-value labels.
    #[serde(default)]
    pub labels: HashMap<String, String>,
}

/// Deserializes `command` as either a single string or a list of strings.
///
/// In docker-compose, `command` can be:
/// - A string: `command: "python app.py"` → split on whitespace
/// - A list: `command: ["python", "app.py"]`
fn deserialize_command<'de, D>(deserializer: D) -> Result<Option<Vec<String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrVec {
        String(String),
        Vec(Vec<String>),
    }

    let value: Option<StringOrVec> = Option::deserialize(deserializer)?;
    Ok(value.map(|v| match v {
        StringOrVec::String(s) => s.split_whitespace().map(String::from).collect(),
        StringOrVec::Vec(v) => v,
    }))
}

/// Environment variables — supports both list and map formats.
///
/// Docker Compose allows two formats:
/// - List: `["KEY=VALUE", "FOO=BAR"]`
/// - Map: `{KEY: VALUE, FOO: BAR}`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ComposeEnvironment {
    /// No environment variables specified.
    #[default]
    Empty,
    /// List format: `["KEY=VALUE", ...]`.
    List(Vec<String>),
    /// Map format: `{KEY: VALUE, ...}`.
    Map(HashMap<String, String>),
}

/// Port mapping — supports both short and long syntax.
///
/// Docker Compose allows two formats:
/// - Short: `"8080:80"` (published:target)
/// - Long: `{target: 80, published: 8080, protocol: tcp}`
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ComposePort {
    /// Short syntax: `"8080:80"`.
    Short(String),
    /// Long syntax with explicit fields.
    Long {
        /// Container port.
        target: u16,
        /// Host port (optional).
        published: Option<u16>,
        /// Protocol (`"tcp"` or `"udp"`).
        protocol: Option<String>,
    },
}

/// Service dependency — supports both simple list and extended format.
///
/// Docker Compose allows two formats:
/// - Simple: `[db, redis]`
/// - Extended: `{db: {condition: service_healthy}}`
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum ComposeDependsOn {
    /// No dependencies specified.
    #[default]
    Empty,
    /// Simple list of service names.
    Simple(Vec<String>),
    /// Extended format with conditions per dependency.
    Extended(HashMap<String, DependsOnCondition>),
}

/// Condition for an extended `depends_on` entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DependsOnCondition {
    /// Condition type: `"service_started"`, `"service_healthy"`, etc.
    pub condition: Option<String>,
}

/// Network configuration within a compose project.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ComposeNetwork {
    /// Network driver (e.g. `"bridge"`, `"overlay"`).
    pub driver: Option<String>,

    /// IPAM (IP Address Management) configuration.
    #[serde(default)]
    pub ipam: Option<ComposeIpam>,

    /// Whether this network is externally managed.
    #[serde(default)]
    pub external: bool,
}

/// IPAM configuration for a network.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ComposeIpam {
    /// IPAM driver name.
    pub driver: Option<String>,

    /// List of IPAM pool configurations.
    #[serde(default)]
    pub config: Vec<ComposeIpamConfig>,
}

/// A single IPAM pool configuration (subnet, gateway).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ComposeIpamConfig {
    /// Subnet in CIDR notation (e.g. `"172.28.0.0/16"`).
    pub subnet: Option<String>,

    /// Gateway IP address.
    pub gateway: Option<String>,
}

/// Named volume configuration.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ComposeVolumeConfig {
    /// Volume driver (e.g. `"local"`).
    pub driver: Option<String>,

    /// Whether this volume is externally managed.
    #[serde(default)]
    pub external: bool,
}

#[cfg(test)]
#[path = "types_test.rs"]
mod tests;
