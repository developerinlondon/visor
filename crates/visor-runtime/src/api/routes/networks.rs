//! Network CRUD routes: create, list, get, delete, connect, disconnect.
//!
//! Manages user-defined virtual networks. Each network has a subnet,
//! gateway, and a set of connected VMs. Uses [`NetworkManager`] for
//! state management.

use std::collections::HashMap;

use anyhow::{Context, bail};
use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;
use uuid::Uuid;

use crate::api::router::AppState;
use crate::api::routes::vms::ApiError;

// ── Types ─────────────────────────────────────────────────────────

/// Configuration for creating a new network.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct NetworkConfig {
    /// Human-readable network name (must be unique).
    pub name: String,
    /// Subnet in CIDR notation (e.g. `"10.0.0.0/24"`). Auto-assigned if omitted.
    #[serde(default)]
    pub subnet: Option<String>,
    /// Gateway address. Auto-assigned if omitted.
    #[serde(default)]
    pub gateway: Option<String>,
}

/// Runtime information about a network.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct NetworkInfo {
    /// Unique network identifier (UUID v4).
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// Subnet in CIDR notation.
    pub subnet: String,
    /// Gateway address.
    pub gateway: String,
    /// Current network state.
    pub state: NetworkState,
    /// IDs of VMs connected to this network.
    pub connected_vms: Vec<String>,
}

/// Lifecycle state of a network.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum NetworkState {
    /// Network is active and accepting connections.
    #[default]
    Active,
    /// Network is being removed.
    Removing,
}

/// Request to connect a VM to a network.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
#[non_exhaustive]
pub struct ConnectRequest {
    /// VM identifier to connect.
    pub vm_id: String,
}

// ── Network manager ───────────────────────────────────────────────

/// Default subnet base for auto-assigned networks (incremented per network).
const AUTO_SUBNET_BASE: [u8; 4] = [172, 21, 0, 0];

/// In-memory network state manager.
///
/// Stores network metadata and connected VM lists. Thread-safe access
/// is provided by wrapping in `Arc<RwLock<...>>` in [`AppState`].
#[non_exhaustive]
pub struct NetworkManager {
    /// Network ID → info.
    networks: HashMap<String, NetworkInfo>,
    /// Name → ID reverse lookup (enforce unique names).
    name_index: HashMap<String, String>,
    /// Counter for auto-assigning subnets.
    next_subnet_index: u8,
}

impl NetworkManager {
    /// Create a new, empty network manager.
    #[must_use]
    pub fn new() -> Self {
        Self {
            networks: HashMap::new(),
            name_index: HashMap::new(),
            next_subnet_index: 0,
        }
    }

    /// Create a new network from the given configuration.
    ///
    /// # Errors
    ///
    /// Returns an error if a network with the same name already exists.
    pub fn create(&mut self, config: NetworkConfig) -> anyhow::Result<NetworkInfo> {
        if self.name_index.contains_key(&config.name) {
            bail!("network '{}' already exists", config.name);
        }

        let id = Uuid::new_v4().to_string();

        let (subnet, gateway) = match (config.subnet, config.gateway) {
            (Some(s), Some(g)) => (s, g),
            (Some(s), None) => {
                // Derive gateway from subnet (first usable IP)
                let gw = derive_gateway(&s).unwrap_or_else(|| {
                    format!(
                        "{}.{}.{}.1",
                        AUTO_SUBNET_BASE[0], AUTO_SUBNET_BASE[1], self.next_subnet_index
                    )
                });
                (s, gw)
            }
            _ => {
                // Auto-assign subnet
                let idx = self.next_subnet_index;
                self.next_subnet_index = self.next_subnet_index.wrapping_add(1);
                let subnet = format!(
                    "{}.{}.{}.0/24",
                    AUTO_SUBNET_BASE[0], AUTO_SUBNET_BASE[1], idx
                );
                let gateway = format!("{}.{}.{}.1", AUTO_SUBNET_BASE[0], AUTO_SUBNET_BASE[1], idx);
                (subnet, gateway)
            }
        };

        let info = NetworkInfo {
            id: id.clone(),
            name: config.name.clone(),
            subnet,
            gateway,
            state: NetworkState::Active,
            connected_vms: Vec::new(),
        };

        self.name_index.insert(config.name, id.clone());
        self.networks.insert(id, info.clone());

        Ok(info)
    }

    /// List all networks.
    #[must_use]
    pub fn list(&self) -> Vec<NetworkInfo> {
        self.networks.values().cloned().collect()
    }

    /// Get a network by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the network is not found.
    pub fn get(&self, id: &str) -> anyhow::Result<NetworkInfo> {
        self.networks
            .get(id)
            .cloned()
            .context(format!("network not found: {id}"))
    }

    /// Delete a network by ID.
    ///
    /// # Errors
    ///
    /// Returns an error if the network is not found.
    pub fn delete(&mut self, id: &str) -> anyhow::Result<NetworkInfo> {
        let info = self
            .networks
            .remove(id)
            .context(format!("network not found: {id}"))?;
        self.name_index.remove(&info.name);
        Ok(info)
    }

    /// Connect a VM to a network.
    ///
    /// # Errors
    ///
    /// Returns an error if the network is not found.
    pub fn connect_vm(&mut self, network_id: &str, vm_id: &str) -> anyhow::Result<()> {
        let info = self
            .networks
            .get_mut(network_id)
            .context(format!("network not found: {network_id}"))?;

        if !info.connected_vms.contains(&vm_id.to_owned()) {
            info.connected_vms.push(vm_id.to_owned());
        }
        Ok(())
    }

    /// Disconnect a VM from a network.
    ///
    /// # Errors
    ///
    /// Returns an error if the network is not found.
    pub fn disconnect_vm(&mut self, network_id: &str, vm_id: &str) -> anyhow::Result<()> {
        let info = self
            .networks
            .get_mut(network_id)
            .context(format!("network not found: {network_id}"))?;

        info.connected_vms.retain(|id| id != vm_id);
        Ok(())
    }
}

impl Default for NetworkManager {
    fn default() -> Self {
        Self::new()
    }
}

/// Derive the gateway address from a CIDR subnet string.
///
/// Takes the base IP and sets the last octet to 1.
fn derive_gateway(subnet: &str) -> Option<String> {
    let ip_part = subnet.split('/').next()?;
    let mut octets: Vec<&str> = ip_part.split('.').collect();
    if octets.len() == 4 {
        octets[3] = "1";
        Some(octets.join("."))
    } else {
        None
    }
}

// ── Route handlers ────────────────────────────────────────────────

/// Create a new network.
///
/// # Errors
///
/// Returns an error if the network name already exists.
#[utoipa::path(
    post,
    path = "/v1/networks",
    tag = "networks",
    request_body = NetworkConfig,
    responses(
        (status = 201, description = "Network created", body = NetworkInfo),
        (status = 500, description = "Failed to create network")
    )
)]
pub async fn create_network(
    State(state): State<AppState>,
    Json(config): Json<NetworkConfig>,
) -> Result<(StatusCode, Json<NetworkInfo>), ApiError> {
    let info = state
        .networks
        .write()
        .await
        .create(config)
        .context("create network")?;
    Ok((StatusCode::CREATED, Json(info)))
}

/// List all networks.
///
/// # Errors
///
/// Currently infallible.
#[utoipa::path(
    get,
    path = "/v1/networks",
    tag = "networks",
    responses(
        (status = 200, description = "List of all networks", body = Vec<NetworkInfo>)
    )
)]
pub async fn list_networks(
    State(state): State<AppState>,
) -> Result<Json<Vec<NetworkInfo>>, ApiError> {
    let networks = state.networks.read().await.list();
    Ok(Json(networks))
}

/// Get a network by ID.
///
/// # Errors
///
/// Returns an error if the network is not found.
#[utoipa::path(
    get,
    path = "/v1/networks/{id}",
    tag = "networks",
    params(
        ("id" = String, Path, description = "Network ID")
    ),
    responses(
        (status = 200, description = "Network info", body = NetworkInfo),
        (status = 500, description = "Network not found")
    )
)]
pub async fn get_network(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<NetworkInfo>, ApiError> {
    let info = state
        .networks
        .read()
        .await
        .get(&id)
        .context("get network")?;
    Ok(Json(info))
}

/// Delete a network.
///
/// # Errors
///
/// Returns an error if the network is not found.
#[utoipa::path(
    delete,
    path = "/v1/networks/{id}",
    tag = "networks",
    params(
        ("id" = String, Path, description = "Network ID")
    ),
    responses(
        (status = 200, description = "Network deleted", body = NetworkInfo),
        (status = 500, description = "Network not found")
    )
)]
pub async fn delete_network(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<Json<NetworkInfo>, ApiError> {
    let info = state
        .networks
        .write()
        .await
        .delete(&id)
        .context("delete network")?;
    Ok(Json(info))
}

/// Connect a VM to a network.
///
/// # Errors
///
/// Returns an error if the network or VM is not found.
#[utoipa::path(
    post,
    path = "/v1/networks/{id}/connect",
    tag = "networks",
    params(
        ("id" = String, Path, description = "Network ID")
    ),
    request_body = ConnectRequest,
    responses(
        (status = 200, description = "VM connected"),
        (status = 500, description = "Network not found")
    )
)]
pub async fn connect_vm(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ConnectRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .networks
        .write()
        .await
        .connect_vm(&id, &req.vm_id)
        .context("connect VM to network")?;
    Ok(StatusCode::OK)
}

/// Disconnect a VM from a network.
///
/// # Errors
///
/// Returns an error if the network is not found.
#[utoipa::path(
    post,
    path = "/v1/networks/{id}/disconnect",
    tag = "networks",
    params(
        ("id" = String, Path, description = "Network ID")
    ),
    request_body = ConnectRequest,
    responses(
        (status = 200, description = "VM disconnected"),
        (status = 500, description = "Network not found")
    )
)]
pub async fn disconnect_vm(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(req): Json<ConnectRequest>,
) -> Result<StatusCode, ApiError> {
    state
        .networks
        .write()
        .await
        .disconnect_vm(&id, &req.vm_id)
        .context("disconnect VM from network")?;
    Ok(StatusCode::OK)
}

// ── Router builder ────────────────────────────────────────────────

/// Builds the network sub-router with CRUD + connect/disconnect routes.
pub fn network_routes() -> axum::Router<AppState> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/v1/networks", post(create_network).get(list_networks))
        .route("/v1/networks/{id}", get(get_network).delete(delete_network))
        .route("/v1/networks/{id}/connect", post(connect_vm))
        .route("/v1/networks/{id}/disconnect", post(disconnect_vm))
}

#[cfg(test)]
#[path = "networks_test.rs"]
mod tests;
