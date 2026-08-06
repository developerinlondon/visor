//! Translation between Docker Engine API types and visor types.
//!
//! This module is the core of the compatibility layer — it converts
//! Docker-shaped JSON into visor's `VmConfig`, `ExecRequest`, etc.
//! and converts visor's `VmInfo`, `ExecResult` back into Docker JSON.

use std::collections::{BTreeSet, HashMap};

use time::{Date, Month, PrimitiveDateTime, Time};
use visor_types::{
    ExecRequest, ExecResult, PortMapping, ServicePort, VmConfig, VmInfo, VmState, VolumeMount,
};

use crate::types::{
    ContainerConfig, ContainerCreateRequest, ContainerInspectResponse, ContainerListEntry,
    ContainerPort, ContainerState, ContainerWaitResponse, ExecCreateRequest, ExecInspectResponse,
    HealthState, HostConfigResponse, NetworkEntry, NetworkSettings,
};

#[cfg(test)]
#[path = "translate_test.rs"]
mod tests;

/// Converts a Docker `ContainerCreateRequest` into a visor `VmConfig`.
///
/// Maps Docker container concepts to VM configuration:
/// - `Image` → `VmConfig.image`
/// - `Cmd` → `VmConfig.cmd`
/// - `Env` → `VmConfig.env`
/// - `WorkingDir` → `VmConfig.working_dir`
/// - `HostConfig.PortBindings` → `VmConfig.ports`
/// - `HostConfig.Binds` → `VmConfig.volumes`
/// - `HostConfig.Memory` → `VmConfig.memory_mib` (bytes → MiB)
#[must_use]
pub fn docker_create_to_vm_config(
    req: &ContainerCreateRequest,
    name: Option<&str>,
    detach: bool,
) -> VmConfig {
    let mut config = VmConfig::new(&req.image);

    if let Some(ref entrypoint) = req.entrypoint {
        config.entrypoint.clone_from(entrypoint);
    }

    if let Some(ref cmd) = req.cmd {
        config.cmd.clone_from(cmd);
    }

    if let Some(ref env) = req.env {
        config.env.clone_from(env);
    }

    // Only set working_dir if Docker sent a non-empty value; empty or
    // absent means "use image default", which VmConfig handles as None.
    if req.working_dir.as_deref().is_some_and(|s| !s.is_empty()) {
        config.working_dir.clone_from(&req.working_dir);
    }
    config.name = name.map(String::from);
    config.networks = collect_network_names(req);
    config.service_names = collect_service_names(req, name);
    config.service_ports = collect_service_ports(req);
    config.detach = detach;
    config.network_enabled = docker_network_enabled(req);
    if let Some(ref labels) = req.labels {
        config.labels.clone_from(labels);
    }

    if let Some(ref hc) = req.host_config {
        // Port bindings: {"80/tcp": [{"HostPort": "8080"}]} → Vec<PortMapping>
        if let Some(ref bindings) = hc.port_bindings {
            config.ports = parse_port_bindings(bindings);
        }

        // Bind mounts: ["/host:/guest:ro"] → Vec<VolumeMount>
        if let Some(ref binds) = hc.binds {
            config.volumes = binds.iter().filter_map(|b| parse_bind_mount(b)).collect();
        }

        // Memory: bytes → MiB. Docker sends 0 for "no limit", which
        // should use the visor default (512 MiB), not the 64 MiB minimum.
        if let Some(mem_bytes) = hc.memory {
            if mem_bytes > 0 {
                let mib = u32::try_from(mem_bytes / (1024 * 1024)).unwrap_or(512);
                config.memory_mib = mib.max(64);
            }
        }

        // CPU: nanoCPUs → vcpus (1e9 nanoCPUs = 1 CPU)
        if let Some(nano) = hc.nano_cpus {
            let cpus = u32::try_from(nano / 1_000_000_000).unwrap_or(1);
            config.vcpus = cpus.max(1);
        }
    }

    config
}

fn docker_network_enabled(req: &ContainerCreateRequest) -> bool {
    req.host_config
        .as_ref()
        .and_then(|host_config| host_config.network_mode.as_deref())
        != Some("none")
}

fn collect_network_names(req: &ContainerCreateRequest) -> Vec<String> {
    let mut names = BTreeSet::new();

    if let Some(endpoints) = req
        .networking_config
        .as_ref()
        .and_then(|networking| networking.endpoints_config.as_ref())
    {
        for network_name in endpoints.keys() {
            if !network_name.is_empty() {
                names.insert(network_name.clone());
            }
        }
    }

    if names.is_empty()
        && let Some(project) = req
            .labels
            .as_ref()
            .and_then(|labels| labels.get("com.docker.compose.project"))
            .filter(|value| !value.is_empty())
    {
        names.insert(format!("{project}_default"));
    }

    names.into_iter().collect()
}

fn collect_service_names(req: &ContainerCreateRequest, name: Option<&str>) -> Vec<String> {
    let mut names = BTreeSet::new();

    if let Some(name) = name.filter(|value| !value.is_empty()) {
        names.insert(name.to_owned());
    }
    if let Some(hostname) = req.hostname.as_deref().filter(|value| !value.is_empty()) {
        names.insert(hostname.to_owned());
    }
    if let Some(service) = req
        .labels
        .as_ref()
        .and_then(|labels| labels.get("com.docker.compose.service"))
        .filter(|value| !value.is_empty())
    {
        names.insert(service.clone());
    }
    if let Some(endpoints) = req
        .networking_config
        .as_ref()
        .and_then(|networking| networking.endpoints_config.as_ref())
    {
        for endpoint in endpoints.values() {
            for alias in endpoint.aliases.iter().flatten() {
                if !alias.is_empty() {
                    names.insert(alias.clone());
                }
            }
        }
    }

    names.into_iter().collect()
}

fn collect_service_ports(req: &ContainerCreateRequest) -> Vec<ServicePort> {
    let mut ports = BTreeSet::new();

    if let Some(exposed_ports) = &req.exposed_ports {
        for key in exposed_ports.keys() {
            if let Some(port) = parse_service_port(key) {
                ports.insert(port);
            }
        }
    }
    if let Some(port_bindings) = req
        .host_config
        .as_ref()
        .and_then(|host_config| host_config.port_bindings.as_ref())
    {
        for key in port_bindings.keys() {
            if let Some(port) = parse_service_port(key) {
                ports.insert(port);
            }
        }
    }

    ports.into_iter().collect()
}

fn parse_service_port(spec: &str) -> Option<ServicePort> {
    let (port_str, protocol) = if let Some((port, protocol)) = spec.split_once('/') {
        (port, protocol)
    } else {
        (spec, "tcp")
    };
    let port = port_str.parse().ok()?;
    Some(ServicePort::new(port, protocol))
}

/// Converts a visor `VmInfo` into a Docker container list entry.
#[must_use]
pub fn vm_info_to_list_entry(info: &VmInfo) -> ContainerListEntry {
    let name = info.name.as_deref().unwrap_or(&info.id).to_owned();

    let (state, status) = vm_state_to_docker(info.state, info.exit_code);

    let ports: Vec<ContainerPort> = info
        .ports
        .iter()
        .map(|p| ContainerPort {
            private_port: p.guest_port,
            public_port: Some(p.host_port),
            port_type: p.protocol.clone(),
        })
        .collect();

    // Parse created_at ISO 8601 to Unix timestamp (best-effort).
    let created = parse_iso_timestamp(&info.created_at).unwrap_or(0);

    ContainerListEntry {
        id: info.id.clone(),
        names: vec![format!("/{name}")],
        image: info.image.clone(),
        image_i_d: String::new(),
        command: String::new(),
        created,
        state,
        status,
        ports,
        labels: HashMap::new(),
    }
}

/// Converts a visor `VmInfo` into a Docker container list entry with labels.
#[must_use]
pub fn vm_info_to_list_entry_with_labels<S: std::hash::BuildHasher>(
    info: &VmInfo,
    labels: &HashMap<String, String, S>,
) -> ContainerListEntry {
    let mut entry = vm_info_to_list_entry(info);
    entry.labels = labels
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    entry
}

/// Converts a visor `VmInfo` into a Docker container inspect response.
#[must_use]
pub fn vm_info_to_inspect(info: &VmInfo) -> ContainerInspectResponse {
    let name = info.name.as_deref().unwrap_or(&info.id).to_owned();

    let (status_str, _) = vm_state_to_docker(info.state, info.exit_code);
    let running = info.state == VmState::Running;

    ContainerInspectResponse {
        id: info.id.clone(),
        name: format!("/{name}"),
        created: info.created_at.clone(),
        state: ContainerState {
            status: status_str,
            running,
            paused: false,
            restarting: false,
            oom_killed: false,
            dead: info.state == VmState::Failed,
            pid: u64::from(running),
            exit_code: info.exit_code.unwrap_or(0),
            error: String::new(),
            started_at: info.created_at.clone(),
            finished_at: if running {
                String::new()
            } else {
                info.created_at.clone()
            },
            health: vm_state_to_health(info.state),
        },
        config: ContainerConfig {
            image: info.image.clone(),
            cmd: None,
            env: None,
            working_dir: String::new(),
            labels: HashMap::new(),
        },
        host_config: HostConfigResponse {
            port_bindings: ports_to_bindings(&info.ports),
            binds: Vec::new(),
        },
        network_settings: NetworkSettings {
            networks: HashMap::from([(
                "bridge".to_owned(),
                NetworkEntry {
                    network_i_d: String::new(),
                    ip_address: String::new(),
                    gateway: String::new(),
                },
            )]),
        },
        mounts: Vec::new(),
    }
}

/// Converts a visor `VmInfo` into a Docker container inspect response with labels.
#[must_use]
pub fn vm_info_to_inspect_with_labels<S: std::hash::BuildHasher>(
    info: &VmInfo,
    labels: &HashMap<String, String, S>,
) -> ContainerInspectResponse {
    let mut inspect = vm_info_to_inspect(info);
    inspect.config.labels = labels
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    inspect
}

/// Converts a Docker `ExecCreateRequest` into a visor `ExecRequest`.
#[must_use]
pub fn docker_exec_to_exec_request(req: &ExecCreateRequest) -> ExecRequest {
    let mut exec = ExecRequest::new(req.cmd.clone());
    if let Some(ref env) = req.env {
        exec.env.clone_from(env);
    }
    if req.working_dir.as_deref().is_some_and(|s| !s.is_empty()) {
        exec.working_dir.clone_from(&req.working_dir);
    }
    exec.tty = req.tty.unwrap_or(false);
    exec
}

/// Converts a visor `ExecResult` into a Docker exec inspect response.
#[must_use]
pub fn exec_result_to_inspect(
    exec_id: &str,
    container_id: &str,
    result: &ExecResult,
) -> ExecInspectResponse {
    ExecInspectResponse {
        id: exec_id.to_owned(),
        running: false,
        exit_code: Some(result.exit_code),
        container_i_d: container_id.to_owned(),
    }
}

/// Converts a visor `VmInfo` into a Docker wait response.
#[must_use]
pub fn vm_info_to_wait(info: &VmInfo) -> ContainerWaitResponse {
    ContainerWaitResponse {
        status_code: info.exit_code.unwrap_or(0),
    }
}

// ── Internal Helpers ────────────────────────────────────────────

/// Maps `VmState` to Docker status strings.
fn vm_state_to_docker(state: VmState, exit_code: Option<i32>) -> (String, String) {
    match state {
        VmState::Creating => ("created".to_owned(), "Created".to_owned()),
        VmState::Running => ("running".to_owned(), "Up".to_owned()),
        VmState::Stopped => {
            let code = exit_code.unwrap_or(0);
            ("exited".to_owned(), format!("Exited ({code})"))
        }
        VmState::Failed => (
            "exited".to_owned(),
            format!("Exited ({})", exit_code.unwrap_or(1)),
        ),
        // VmState is #[non_exhaustive]
        _ => ("unknown".to_owned(), "Unknown".to_owned()),
    }
}

/// Maps `VmState` to Docker health check state.
///
/// Running VMs report `"healthy"`, creating VMs report `"starting"`,
/// stopped/failed VMs return `None` (Docker omits health when not running).
fn vm_state_to_health(state: VmState) -> Option<HealthState> {
    match state {
        VmState::Running => Some(HealthState {
            status: "healthy".to_owned(),
            failing_streak: 0,
            log: Vec::new(),
        }),
        VmState::Creating => Some(HealthState {
            status: "starting".to_owned(),
            failing_streak: 0,
            log: Vec::new(),
        }),
        _ => None,
    }
}

/// Parses Docker port bindings into visor `PortMapping`s.
///
/// Docker format: `{"80/tcp": [{"HostPort": "8080"}]}`
/// visor format: `PortMapping { host_port: 8080, guest_port: 80 }`
fn parse_port_bindings(
    bindings: &HashMap<String, Vec<crate::types::PortBinding>>,
) -> Vec<PortMapping> {
    let mut ports = Vec::new();
    for (container_port_str, host_bindings) in bindings {
        // Parse "80/tcp" or "80" → guest_port=80
        let protocol = container_port_str.split('/').nth(1).unwrap_or("tcp");
        let guest_port: u16 = container_port_str
            .split('/')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);

        if guest_port == 0 {
            continue;
        }

        for binding in host_bindings {
            let host_port: u16 = binding
                .host_port
                .as_deref()
                .and_then(|s| s.parse().ok())
                .unwrap_or(guest_port);

            ports.push(PortMapping::with_protocol(host_port, guest_port, protocol));
        }
    }
    ports
}

/// Converts visor `PortMapping`s back to Docker port binding format.
fn ports_to_bindings(ports: &[PortMapping]) -> HashMap<String, Vec<crate::types::PortBinding>> {
    let mut bindings = HashMap::new();
    for p in ports {
        let key = format!("{}/tcp", p.guest_port);
        bindings
            .entry(key)
            .or_insert_with(Vec::new)
            .push(crate::types::PortBinding {
                host_ip: Some("0.0.0.0".to_owned()),
                host_port: Some(p.host_port.to_string()),
            });
    }
    bindings
}

/// Parses a Docker bind mount string: `/host:/guest` or `/host:/guest:ro`.
fn parse_bind_mount(s: &str) -> Option<VolumeMount> {
    let parts: Vec<&str> = s.splitn(3, ':').collect();
    match parts.len() {
        2 => Some(VolumeMount::new(parts[0], parts[1])),
        3 => {
            if parts[2] == "ro" {
                Some(VolumeMount::read_only(parts[0], parts[1]))
            } else {
                Some(VolumeMount::new(parts[0], parts[1]))
            }
        }
        _ => None,
    }
}

/// Best-effort parse of ISO 8601 timestamp to Unix epoch seconds.
fn parse_iso_timestamp(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let datetime = s.strip_suffix('Z')?;
    let (date, time) = datetime.split_once('T')?;
    let (year, month, day) = parse_iso_date(date)?;
    let (hour, minute, second) = parse_iso_time(time)?;
    let month = Month::try_from(month).ok()?;
    let date = Date::from_calendar_date(year, month, day).ok()?;
    let time = Time::from_hms(hour, minute, second).ok()?;
    Some(
        PrimitiveDateTime::new(date, time)
            .assume_utc()
            .unix_timestamp(),
    )
}

fn parse_iso_date(s: &str) -> Option<(i32, u8, u8)> {
    let mut parts = s.split('-');
    let year = parts.next()?.parse().ok()?;
    let month = parts.next()?.parse().ok()?;
    let day = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((year, month, day))
}

fn parse_iso_time(s: &str) -> Option<(u8, u8, u8)> {
    let whole_seconds = s.split_once('.').map_or(s, |(prefix, _)| prefix);
    let mut parts = whole_seconds.split(':');
    let hour = parts.next()?.parse().ok()?;
    let minute = parts.next()?.parse().ok()?;
    let second = parts.next()?.parse().ok()?;
    if parts.next().is_some() {
        return None;
    }
    Some((hour, minute, second))
}
