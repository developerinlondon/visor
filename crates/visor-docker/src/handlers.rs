//! Docker Engine API handler implementations.
//!
//! Each handler translates a Docker API request into one or more visor
//! [`ExecutionBackend`] calls and returns a Docker-shaped JSON response.

use axum::Json;
use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use std::collections::{BTreeSet, HashMap, HashSet};
use std::convert::Infallible;
use std::io::Read as _;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::debug;
use uuid::Uuid;
use visor_types::{ExecResult, FIRST_GUEST_CID, GuestNetworkLink, ImageInfo, VmInfo, VmState};

use crate::translate;
use crate::types::{
    ContainerCreateResponse, ContainerWaitResponse, DockerError, ExecCreateRequest,
    ExecCreateResponse, ExecInspectResponse, ExecStartRequest, ImageListEntry, InfoResponse,
    NetworkCreateRequest, NetworkCreateResponse, NetworkListEntry, PING_RESPONSE, VersionResponse,
    VolumeCreateRequest, VolumeEntry, VolumeListResponse,
};
use crate::{API_VERSION, DockerState, MIN_API_VERSION};

#[cfg(test)]
#[path = "handlers_test.rs"]
mod tests;

// ── Exec session storage ────────────────────────────────────────────

/// In-memory exec session for the two-phase exec create/start flow.
#[derive(Debug, Clone)]
pub struct ExecSession {
    /// Container (VM) this exec belongs to.
    pub container_id: String,
    /// Original create request.
    pub request: ExecCreateRequest,
    /// Result once execution completes.
    pub result: Option<ExecResult>,
}

/// Shared exec session map. Stored in `DockerState`.
pub type ExecSessions = Arc<Mutex<HashMap<String, ExecSession>>>;

#[derive(Debug, Clone)]
pub(crate) struct DockerNetworkRecord {
    pub id: String,
    pub name: String,
    pub driver: String,
    pub labels: HashMap<String, String>,
}

pub(crate) type DockerNetworks = Arc<Mutex<HashMap<String, DockerNetworkRecord>>>;

#[derive(Debug, Clone)]
pub(crate) struct DockerVolumeRecord {
    pub name: String,
    pub driver: String,
    pub labels: HashMap<String, String>,
    pub mountpoint: String,
}

pub(crate) type DockerVolumes = Arc<Mutex<HashMap<String, DockerVolumeRecord>>>;

#[derive(Debug, Clone)]
pub(crate) struct DockerContainerRecord {
    pub backend_id: Option<String>,
    pub config: visor_types::VmConfig,
    pub created_at: String,
    pub pending_archives: Vec<PendingArchive>,
}

pub(crate) type DockerContainers = Arc<Mutex<HashMap<String, DockerContainerRecord>>>;

#[derive(Debug, Clone)]
pub(crate) struct PendingArchive {
    pub dest: String,
    pub data: Vec<u8>,
}

#[derive(Debug, Default)]
struct ContainerListFilters {
    labels: Vec<String>,
    names: Vec<String>,
    ids: Vec<String>,
    statuses: Vec<String>,
}

#[derive(Debug, Default)]
struct EventFilters {
    labels: Vec<String>,
    types: Vec<String>,
    containers: Vec<String>,
    events: Vec<String>,
}

// ── Query parameter types ───────────────────────────────────────────

#[derive(Debug, serde::Deserialize, Default)]
pub struct ContainerListQuery {
    #[serde(default, deserialize_with = "deserialize_boolish_option")]
    pub all: Option<bool>,
    #[serde(default)]
    pub filters: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct ContainerCreateQuery {
    pub name: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct ContainerStopQuery {
    #[serde(default)]
    pub t: Option<u64>,
}

#[derive(Debug, serde::Deserialize)]
pub struct ContainerArchiveQuery {
    pub path: String,
    #[serde(
        rename = "noOverwriteDirNonDir",
        default,
        deserialize_with = "deserialize_boolish_option"
    )]
    pub no_overwrite_dir_non_dir: Option<bool>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct ContainerLogsQuery {
    #[serde(default, deserialize_with = "deserialize_boolish_option")]
    pub stdout: Option<bool>,
    #[serde(default, deserialize_with = "deserialize_boolish_option")]
    pub stderr: Option<bool>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct EventsQuery {
    #[serde(default)]
    pub filters: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub(crate) struct ImageCreateQuery {
    #[serde(rename = "fromImage")]
    pub from_image: Option<String>,
    pub tag: Option<String>,
}

#[derive(Debug, serde::Deserialize, Default)]
pub struct ImageLoadQuery {
    #[serde(default, deserialize_with = "deserialize_boolish_option")]
    pub quiet: Option<bool>,
}

/// Path parameter extractor for routes with an `{id}` segment.
///
/// Works with both versioned (`/v1.45/containers/{id}/...`) and
/// unversioned (`/containers/{id}/...`) routes — serde ignores the
/// extra `version` field that `Router::nest("/v{version}", ...)` injects.
#[derive(Debug, serde::Deserialize)]
pub struct IdPath {
    pub id: String,
}

/// Path parameter extractor for routes with a `{name}` segment.
#[derive(Debug, serde::Deserialize)]
pub struct NamePath {
    pub name: String,
}

fn deserialize_boolish_option<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw = Option::<String>::deserialize(deserializer)?;
    raw.map(|value| match value.as_str() {
        "1" | "true" | "TRUE" | "True" => Ok(true),
        "0" | "false" | "FALSE" | "False" => Ok(false),
        _ => Err(serde::de::Error::custom(format!(
            "provided string was not `true` or `false`: {value}"
        ))),
    })
    .transpose()
}

// ── Helpers ─────────────────────────────────────────────────────────

/// Builds a Docker API JSON error response.
fn docker_error(status: StatusCode, message: impl Into<String>) -> Response {
    let body = DockerError {
        message: message.into(),
    };
    (status, Json(body)).into_response()
}

fn raw_stream_frame(stream_type: u8, payload: &[u8]) -> Vec<u8> {
    let len = u32::try_from(payload.len()).unwrap_or(u32::MAX);
    let size = len.to_be_bytes();
    let mut frame = Vec::with_capacity(8 + payload.len());
    frame.extend_from_slice(&[stream_type, 0, 0, 0, size[0], size[1], size[2], size[3]]);
    frame.extend_from_slice(payload);
    frame
}

async fn write_raw_stream_frame<W>(
    writer: &mut W,
    stream_type: u8,
    payload: &[u8],
) -> std::io::Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncWriteExt as _;

    let frame = raw_stream_frame(stream_type, payload);
    writer.write_all(&frame).await?;
    writer.flush().await
}

async fn bridge_exec_stream<Io, Stream>(io: Io, stream: Stream) -> std::io::Result<(u64, u64)>
where
    Io: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
    Stream: tokio::io::AsyncRead + tokio::io::AsyncWrite + Send + Unpin + 'static,
{
    use tokio::io::AsyncWriteExt as _;

    let (mut io_read, mut io_write) = tokio::io::split(io);
    let (mut stream_read, mut stream_write) = tokio::io::split(stream);

    let mut client_to_guest = tokio::spawn(async move {
        let copied = tokio::io::copy(&mut io_read, &mut stream_write).await?;
        stream_write.shutdown().await?;
        Ok::<u64, std::io::Error>(copied)
    });
    let mut guest_to_client = tokio::spawn(async move {
        let copied = tokio::io::copy(&mut stream_read, &mut io_write).await?;
        io_write.shutdown().await?;
        Ok::<u64, std::io::Error>(copied)
    });

    let mut to_guest = None;
    loop {
        tokio::select! {
            result = &mut client_to_guest, if to_guest.is_none() => {
                match result {
                    Ok(Ok(copied)) => to_guest = Some(copied),
                    Ok(Err(error)) => {
                        guest_to_client.abort();
                        let _ = guest_to_client.await;
                        return Err(error);
                    }
                    Err(error) => {
                        guest_to_client.abort();
                        let _ = guest_to_client.await;
                        return Err(std::io::Error::other(format!(
                            "docker exec input bridge task failed: {error}"
                        )));
                    }
                }
            }
            result = &mut guest_to_client => {
                let from_guest = match result {
                    Ok(Ok(copied)) => copied,
                    Ok(Err(error)) => {
                        client_to_guest.abort();
                        let _ = client_to_guest.await;
                        return Err(error);
                    }
                    Err(error) => {
                        client_to_guest.abort();
                        let _ = client_to_guest.await;
                        return Err(std::io::Error::other(format!(
                            "docker exec output bridge task failed: {error}"
                        )));
                    }
                };

                if to_guest.is_none() {
                    client_to_guest.abort();
                    let _ = client_to_guest.await;
                }

                return Ok((to_guest.unwrap_or(0), from_guest));
            }
        }
    }
}

async fn bridge_raw_exec_stream(
    io: hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>,
    stream: Box<dyn visor_types::AsyncIoStream>,
    _exec_id: &str,
) -> std::io::Result<(u64, u64)> {
    bridge_exec_stream(io, stream).await
}

enum InitialClientInput {
    Eof,
    Pending,
    Data(Vec<u8>),
}

async fn read_initial_client_input(
    io: &mut hyper_util::rt::TokioIo<hyper::upgrade::Upgraded>,
) -> std::io::Result<InitialClientInput> {
    use tokio::io::AsyncReadExt as _;

    let mut buffer = [0u8; 8192];
    match tokio::time::timeout(std::time::Duration::from_millis(10), io.read(&mut buffer)).await {
        Ok(Ok(0)) => Ok(InitialClientInput::Eof),
        Ok(Ok(bytes_read)) => Ok(InitialClientInput::Data(buffer[..bytes_read].to_vec())),
        Ok(Err(error)) => Err(error),
        Err(_) => Ok(InitialClientInput::Pending),
    }
}

async fn read_exec_start_request(
    req: &mut axum::extract::Request,
) -> Result<ExecStartRequest, Response> {
    let body = std::mem::take(req.body_mut());
    let body_bytes = axum::body::to_bytes(body, usize::MAX).await.map_err(|e| {
        docker_error(
            StatusCode::BAD_REQUEST,
            format!("failed to read exec start request body: {e}"),
        )
    })?;

    if body_bytes.is_empty() {
        return Ok(ExecStartRequest::default());
    }

    serde_json::from_slice(&body_bytes).map_err(|e| {
        docker_error(
            StatusCode::BAD_REQUEST,
            format!("invalid exec start request body: {e}"),
        )
    })
}

fn switching_protocols_response() -> Response {
    let mut response = Body::empty().into_response();
    *response.status_mut() = StatusCode::SWITCHING_PROTOCOLS;
    let headers = response.headers_mut();
    headers.insert("Connection", HeaderValue::from_static("Upgrade"));
    headers.insert("Upgrade", HeaderValue::from_static("tcp"));
    headers.insert(
        "Content-Type",
        HeaderValue::from_static("application/vnd.docker.raw-stream"),
    );
    response
}

fn parse_container_list_filters(raw_filters: Option<&str>) -> Result<ContainerListFilters, String> {
    let Some(raw_filters) = raw_filters else {
        return Ok(ContainerListFilters::default());
    };

    let mut raw_map: HashMap<String, serde_json::Value> =
        serde_json::from_str(raw_filters).map_err(|e| format!("invalid filters query: {e}"))?;

    Ok(ContainerListFilters {
        labels: decode_filter_values(raw_map.remove("label")),
        names: decode_filter_values(raw_map.remove("name")),
        ids: decode_filter_values(raw_map.remove("id")),
        statuses: decode_filter_values(raw_map.remove("status")),
    })
}

fn decode_filter_values(value: Option<serde_json::Value>) -> Vec<String> {
    match value {
        Some(serde_json::Value::Array(values)) => values
            .into_iter()
            .filter_map(|value| value.as_str().map(ToOwned::to_owned))
            .collect(),
        Some(serde_json::Value::Object(values)) => values
            .into_iter()
            .filter_map(|(key, enabled)| enabled.as_bool().unwrap_or(true).then_some(key))
            .collect(),
        Some(serde_json::Value::String(value)) => vec![value],
        _ => Vec::new(),
    }
}

fn parse_event_filters(raw_filters: Option<&str>) -> Result<EventFilters, String> {
    let Some(raw_filters) = raw_filters else {
        return Ok(EventFilters::default());
    };

    let mut raw_map: HashMap<String, serde_json::Value> =
        serde_json::from_str(raw_filters).map_err(|e| format!("invalid filters query: {e}"))?;

    Ok(EventFilters {
        labels: decode_filter_values(raw_map.remove("label")),
        types: decode_filter_values(raw_map.remove("type")),
        containers: decode_filter_values(raw_map.remove("container")),
        events: decode_filter_values(raw_map.remove("event")),
    })
}

fn docker_state_name(vm: &VmInfo) -> &'static str {
    match vm.state {
        VmState::Creating => "created",
        VmState::Running => "running",
        VmState::Stopped => "exited",
        _ => "dead",
    }
}

fn labels_match_filter(labels: &HashMap<String, String>, filter: &str) -> bool {
    match filter.split_once('=') {
        Some((key, value)) => labels.get(key).is_some_and(|candidate| candidate == value),
        None => labels.contains_key(filter),
    }
}

const COMPOSE_PROJECT_LABEL: &str = "com.docker.compose.project";

fn compose_project_name(config: &visor_types::VmConfig) -> Option<&str> {
    config
        .labels
        .get(COMPOSE_PROJECT_LABEL)
        .map(String::as_str)
        .filter(|value| !value.is_empty())
}

fn compose_network_names(config: &visor_types::VmConfig) -> BTreeSet<String> {
    let mut names = config
        .networks
        .iter()
        .filter(|name| !name.is_empty())
        .cloned()
        .collect::<BTreeSet<_>>();

    if names.is_empty()
        && let Some(project) = compose_project_name(config)
    {
        names.insert(format!("{project}_default"));
    }

    names
}

fn share_compose_network(left: &visor_types::VmConfig, right: &visor_types::VmConfig) -> bool {
    let left_names = compose_network_names(left);
    let right_names = compose_network_names(right);

    left_names
        .iter()
        .any(|network_name| right_names.contains(network_name))
}

fn shared_compose_network_name(
    left: &visor_types::VmConfig,
    right: &visor_types::VmConfig,
) -> Option<String> {
    let left_names = compose_network_names(left);
    let right_names = compose_network_names(right);
    left_names
        .into_iter()
        .find(|network_name| right_names.contains(network_name))
}

fn guest_ip_for_config_network(cid: u32, network_name: Option<&str>) -> std::net::Ipv4Addr {
    if let Some(network_name) = network_name.filter(|name| !name.is_empty()) {
        return GuestNetworkLink::for_named_network(network_name, cid).guest_ip;
    }
    GuestNetworkLink::for_cid(cid).guest_ip
}

fn guest_visible_service_names(config: &visor_types::VmConfig) -> Vec<String> {
    let mut names = BTreeSet::new();
    let compose_project = compose_project_name(config);

    for name in config.service_names.iter().filter(|name| !name.is_empty()) {
        names.insert(name.clone());
        if let Some(project) = compose_project {
            names.insert(format!("{name}.{project}"));
        }
    }

    names.into_iter().collect()
}

fn service_discovery_names(config: &visor_types::VmConfig) -> Vec<String> {
    if compose_project_name(config).is_some() {
        guest_visible_service_names(config)
            .into_iter()
            .filter(|name| name.contains('.'))
            .collect()
    } else {
        guest_visible_service_names(config)
    }
}

async fn compose_project_peer_hosts(
    state: &DockerState,
    project: &str,
    requester_config: &visor_types::VmConfig,
) -> Vec<(String, std::net::Ipv4Addr)> {
    let candidate_records = {
        let containers = state.containers.lock().await;
        containers
            .values()
            .filter_map(|record| {
                (compose_project_name(&record.config) == Some(project)
                    && share_compose_network(&record.config, requester_config))
                .then_some((record.backend_id.clone(), record.config.clone()))
            })
            .collect::<Vec<_>>()
    };

    let mut entries = Vec::new();
    for (backend_id, config) in candidate_records {
        let Some(backend_id) = backend_id else {
            continue;
        };
        let Ok(vm) = state.backend.get(&backend_id).await else {
            continue;
        };
        let Some(cid) = vm.cid else {
            continue;
        };
        let shared_network = shared_compose_network_name(&config, requester_config);
        let guest_ip = guest_ip_for_config_network(cid, shared_network.as_deref());
        for hostname in guest_visible_service_names(&config) {
            entries.push((hostname, guest_ip));
        }
    }
    entries.sort_by(|left, right| {
        left.0
            .cmp(&right.0)
            .then(left.1.octets().cmp(&right.1.octets()))
    });
    entries
}

async fn register_service_names(state: &DockerState, vm: &VmInfo, config: &visor_types::VmConfig) {
    let Some(service_discovery) = state.service_discovery.as_ref() else {
        return;
    };
    let Some(cid) = vm.cid else {
        return;
    };
    let guest_ip = guest_ip_for_config_network(cid, config.networks.first().map(String::as_str));
    for name in service_discovery_names(config) {
        service_discovery.register_name(&name, guest_ip).await;
    }
}

async fn unregister_service_names(state: &DockerState, config: &visor_types::VmConfig) {
    let Some(service_discovery) = state.service_discovery.as_ref() else {
        return;
    };
    for name in service_discovery_names(config) {
        service_discovery.unregister_name(&name).await;
    }
}

async fn populate_discovered_extra_hosts(state: &DockerState, config: &mut visor_types::VmConfig) {
    let own_names = guest_visible_service_names(config)
        .iter()
        .map(|name| name.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut seen_hosts = config
        .extra_hosts
        .iter()
        .map(|entry| entry.hostname.to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let snapshot = if let Some(project) = compose_project_name(config) {
        compose_project_peer_hosts(state, project, config).await
    } else {
        let Some(service_discovery) = state.service_discovery.as_ref() else {
            return;
        };
        let mut snapshot = service_discovery.snapshot_names().await;
        snapshot.sort_by(|left, right| left.0.cmp(&right.0));
        snapshot
    };

    for (hostname, ip) in snapshot {
        let hostname_key = hostname.to_ascii_lowercase();
        if own_names.contains(&hostname_key) || !seen_hosts.insert(hostname_key) {
            continue;
        }
        config
            .extra_hosts
            .push(visor_types::HostEntry::new(hostname, ip.to_string()));
    }
}

fn vm_matches_filters(
    vm: &VmInfo,
    labels: &HashMap<String, String>,
    filters: &ContainerListFilters,
) -> bool {
    let name = vm.name.as_deref().unwrap_or(&vm.id);

    filters
        .labels
        .iter()
        .all(|filter| labels_match_filter(labels, filter))
        && (filters.names.is_empty() || filters.names.iter().any(|filter| name.contains(filter)))
        && (filters.ids.is_empty() || filters.ids.iter().any(|filter| vm.id.starts_with(filter)))
        && (filters.statuses.is_empty()
            || filters
                .statuses
                .iter()
                .any(|filter| docker_state_name(vm) == filter))
}

fn vm_matches_event_filters(
    vm: &VmInfo,
    labels: &HashMap<String, String>,
    filters: &EventFilters,
) -> bool {
    let name = vm.name.as_deref().unwrap_or(&vm.id);

    (filters.types.is_empty() || filters.types.iter().any(|filter| filter == "container"))
        && filters
            .labels
            .iter()
            .all(|filter| labels_match_filter(labels, filter))
        && (filters.containers.is_empty()
            || filters
                .containers
                .iter()
                .any(|filter| vm.id.starts_with(filter) || name == filter))
}

fn next_container_event(
    previous_state: Option<VmState>,
    current_state: VmState,
) -> Option<&'static str> {
    match (previous_state, current_state) {
        (None | Some(VmState::Creating), VmState::Running) => Some("start"),
        (None | Some(VmState::Running | VmState::Creating), VmState::Stopped | VmState::Failed)
        | (Some(VmState::Stopped), VmState::Failed) => Some("die"),
        _ => None,
    }
}

#[derive(Debug, serde::Serialize)]
struct DockerEventActor {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "Attributes")]
    attributes: HashMap<String, String>,
}

#[derive(Debug, serde::Serialize)]
struct DockerEvent {
    status: String,
    id: String,
    from: String,
    #[serde(rename = "Type")]
    event_type: String,
    #[serde(rename = "Action")]
    action: String,
    #[serde(rename = "Actor")]
    actor: DockerEventActor,
    scope: String,
    time: i64,
    #[serde(rename = "timeNano")]
    time_nano: i64,
}

fn container_event_payload(
    vm: &VmInfo,
    labels: &HashMap<String, String>,
    action: &str,
) -> DockerEvent {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let mut attributes = labels.clone();
    if let Some(name) = vm.name.as_deref() {
        attributes.insert("name".to_owned(), name.to_owned());
    }
    attributes.insert("image".to_owned(), vm.image.clone());

    DockerEvent {
        status: action.to_owned(),
        id: vm.id.clone(),
        from: vm.image.clone(),
        event_type: "container".to_owned(),
        action: action.to_owned(),
        actor: DockerEventActor {
            id: vm.id.clone(),
            attributes,
        },
        scope: "local".to_owned(),
        time: i64::try_from(now.as_secs()).unwrap_or(i64::MAX),
        time_nano: i64::try_from(now.as_nanos()).unwrap_or(i64::MAX),
    }
}

fn spawn_exec_stream_bridge(
    state: &DockerState,
    exec_id: &str,
    container_id: &str,
    exec_req: visor_types::ExecRequest,
    tty: bool,
    attach_stdout: bool,
    attach_stderr: bool,
    on_upgrade: hyper::upgrade::OnUpgrade,
) {
    let backend = state.backend.clone();
    let sessions = state.exec_sessions.clone();
    let exec_id_for_task = exec_id.to_owned();
    let container_id_for_task = container_id.to_owned();

    tokio::spawn(async move {
        let result = match on_upgrade.await {
            Ok(upgraded) => {
                use tokio::io::AsyncWriteExt as _;

                let mut io = hyper_util::rt::TokioIo::new(upgraded);
                match read_initial_client_input(&mut io).await {
                    Ok(InitialClientInput::Eof) if !tty => match backend
                        .exec(&container_id_for_task, exec_req.clone())
                        .await
                    {
                        Ok(result) => {
                            if attach_stdout && !result.stdout.is_empty() {
                                let _ =
                                    write_raw_stream_frame(&mut io, 1, result.stdout.as_bytes())
                                        .await;
                            }
                            if attach_stderr && !result.stderr.is_empty() {
                                let _ =
                                    write_raw_stream_frame(&mut io, 2, result.stderr.as_bytes())
                                        .await;
                            }
                            let _ = io.shutdown().await;
                            result
                        }
                        Err(error) => {
                            let message = format!("visor exec failed: {error:#}\n");
                            let _ = write_raw_stream_frame(&mut io, 2, message.as_bytes()).await;
                            let _ = io.shutdown().await;
                            ExecResult::new(1, String::new(), message)
                        }
                    },
                    Ok(initial_input) => match backend
                        .exec_stream(&container_id_for_task, exec_req)
                        .await
                    {
                        Ok(mut stream) => {
                            if let InitialClientInput::Data(initial_bytes) = initial_input {
                                if let Err(error) = stream.write_all(&initial_bytes).await {
                                    let message = format!(
                                        "visor exec stream initial write failed: {error}\n"
                                    );
                                    if tty {
                                        let _ = io.write_all(message.as_bytes()).await;
                                        let _ = io.flush().await;
                                    } else {
                                        let _ =
                                            write_raw_stream_frame(&mut io, 2, message.as_bytes())
                                                .await;
                                    }
                                    let _ = io.shutdown().await;
                                    ExecResult::new(1, String::new(), message)
                                } else {
                                    match bridge_raw_exec_stream(io, stream, &exec_id_for_task)
                                        .await
                                    {
                                        Ok((_to_guest, _from_guest)) => {
                                            ExecResult::new(0, String::new(), String::new())
                                        }
                                        Err(error) => {
                                            tracing::warn!(
                                                exec_id = %exec_id_for_task,
                                                error = %error,
                                                "docker exec stream bridge failed"
                                            );
                                            ExecResult::new(
                                                1,
                                                String::new(),
                                                format!(
                                                    "docker exec stream bridge failed: {error}"
                                                ),
                                            )
                                        }
                                    }
                                }
                            } else {
                                match bridge_raw_exec_stream(io, stream, &exec_id_for_task).await {
                                    Ok((_to_guest, _from_guest)) => {
                                        ExecResult::new(0, String::new(), String::new())
                                    }
                                    Err(error) => {
                                        tracing::warn!(
                                            exec_id = %exec_id_for_task,
                                            error = %error,
                                            "docker exec stream bridge failed"
                                        );
                                        ExecResult::new(
                                            1,
                                            String::new(),
                                            format!("docker exec stream bridge failed: {error}"),
                                        )
                                    }
                                }
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                exec_id = %exec_id_for_task,
                                error = %error,
                                error_debug = ?error,
                                "docker exec stream setup failed"
                            );
                            let message = format!("visor exec stream setup failed: {error:#}\n");
                            if tty {
                                let _ = io.write_all(message.as_bytes()).await;
                                let _ = io.flush().await;
                            } else {
                                let _ =
                                    write_raw_stream_frame(&mut io, 2, message.as_bytes()).await;
                            }
                            let _ = io.shutdown().await;
                            ExecResult::new(1, String::new(), message)
                        }
                    },
                    Err(error) => {
                        let message =
                            format!("failed to inspect docker exec client input: {error}\n");
                        if tty {
                            let _ = io.write_all(message.as_bytes()).await;
                            let _ = io.flush().await;
                        } else {
                            let _ = write_raw_stream_frame(&mut io, 2, message.as_bytes()).await;
                        }
                        let _ = io.shutdown().await;
                        ExecResult::new(1, String::new(), message)
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    exec_id = %exec_id_for_task,
                    error = %error,
                    "docker exec upgrade handshake failed"
                );
                ExecResult::new(
                    1,
                    String::new(),
                    format!("docker exec upgrade handshake failed: {error}"),
                )
            }
        };

        let mut sessions = sessions.lock().await;
        if let Some(session) = sessions.get_mut(&exec_id_for_task) {
            session.result = Some(result);
        }
    });
}

fn spawn_exec_output_bridge(
    state: &DockerState,
    exec_id: &str,
    container_id: &str,
    exec_req: visor_types::ExecRequest,
    attach_stdout: bool,
    attach_stderr: bool,
    on_upgrade: hyper::upgrade::OnUpgrade,
) {
    let backend = state.backend.clone();
    let sessions = state.exec_sessions.clone();
    let exec_id_for_task = exec_id.to_owned();
    let container_id_for_task = container_id.to_owned();

    tokio::spawn(async move {
        let result = match on_upgrade.await {
            Ok(upgraded) => {
                let mut io = hyper_util::rt::TokioIo::new(upgraded);
                match backend.exec(&container_id_for_task, exec_req).await {
                    Ok(result) => {
                        if attach_stdout && !result.stdout.is_empty() {
                            let _ =
                                write_raw_stream_frame(&mut io, 1, result.stdout.as_bytes()).await;
                        }
                        if attach_stderr && !result.stderr.is_empty() {
                            let _ =
                                write_raw_stream_frame(&mut io, 2, result.stderr.as_bytes()).await;
                        }
                        let _ = tokio::io::AsyncWriteExt::shutdown(&mut io).await;
                        result
                    }
                    Err(error) => {
                        let message = format!("visor exec failed: {error:#}\n");
                        let _ = write_raw_stream_frame(&mut io, 2, message.as_bytes()).await;
                        let _ = tokio::io::AsyncWriteExt::shutdown(&mut io).await;
                        ExecResult::new(1, String::new(), message)
                    }
                }
            }
            Err(error) => {
                tracing::warn!(
                    exec_id = %exec_id_for_task,
                    error = %error,
                    "docker exec upgrade handshake failed"
                );
                ExecResult::new(
                    1,
                    String::new(),
                    format!("docker exec upgrade handshake failed: {error}"),
                )
            }
        };

        let mut sessions = sessions.lock().await;
        if let Some(session) = sessions.get_mut(&exec_id_for_task) {
            session.result = Some(result);
        }
    });
}

fn image_list_entry_from_info(info: ImageInfo) -> ImageListEntry {
    ImageListEntry {
        id: info.id,
        repo_tags: info.repo_tags,
        created: info.created,
        size: info.size,
        labels: info.labels,
    }
}

fn docker_image_inspect_json(info: &ImageInfo) -> serde_json::Value {
    serde_json::json!({
        "Id": info.id,
        "RepoTags": info.repo_tags,
        "Created": "1970-01-01T00:00:00Z",
        "Size": info.size,
        "Architecture": info.architecture,
        "Os": info.os,
        "Config": {
            "Labels": info.labels,
        }
    })
}

async fn resolve_container_backend_id(state: &DockerState, id: &str) -> Option<String> {
    if let Some((_, record)) = load_managed_container_record(state, id).await {
        return match record.backend_id {
            Some(backend_id) if state.backend.get(&backend_id).await.is_ok() => Some(backend_id),
            _ => None,
        };
    }
    if state.backend.get(id).await.is_ok() {
        return Some(id.to_owned());
    }
    find_backend_vm_by_name(state, id).await.map(|vm| vm.id)
}

async fn load_container_config(state: &DockerState, id: &str) -> Option<visor_types::VmConfig> {
    if let Some((_, record)) = load_managed_container_record(state, id).await {
        return Some(record.config);
    }
    if let Ok(vm) = state.backend.get(id).await {
        return Some(recreate_vm_config_from_info(&vm));
    }
    find_backend_vm_by_name(state, id)
        .await
        .map(|vm| recreate_vm_config_from_info(&vm))
}

fn docker_created_at() -> String {
    time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "1970-01-01T00:00:00Z".to_owned())
}

fn synthesize_container_vm(
    id: &str,
    record: &DockerContainerRecord,
    backend_vm: Option<&VmInfo>,
) -> VmInfo {
    let mut info = VmInfo::new(
        id.to_owned(),
        record.config.image.clone(),
        VmState::Creating,
        record.created_at.clone(),
        record.config.memory_mib,
        record.config.vcpus,
    );
    info.name = record.config.name.clone();
    info.ports = record.config.ports.clone();

    if let Some(vm) = backend_vm {
        info.state = vm.state;
        info.exit_code = vm.exit_code;
        info.stdout = vm.stdout.clone();
        info.stderr = vm.stderr.clone();
        info.cid = vm.cid;
        if info.name.is_none() {
            info.name = vm.name.clone();
        }
        if info.ports.is_empty() {
            info.ports = vm.ports.clone();
        }
    }

    info
}

async fn load_container_view(state: &DockerState, id: &str) -> Option<VmInfo> {
    if let Some((logical_id, record)) = load_managed_container_record(state, id).await {
        let backend_vm = match record.backend_id.as_deref() {
            Some(backend_id) => state.backend.get(backend_id).await.ok(),
            None => None,
        };
        return Some(synthesize_container_vm(
            &logical_id,
            &record,
            backend_vm.as_ref(),
        ));
    }

    if let Ok(vm) = state.backend.get(id).await {
        return Some(vm);
    }
    find_backend_vm_by_name(state, id).await
}

async fn resolve_managed_container_id(state: &DockerState, id: &str) -> Option<String> {
    let containers = state.containers.lock().await;
    if containers.contains_key(id) {
        return Some(id.to_owned());
    }

    containers.iter().find_map(|(container_id, record)| {
        (record.config.name.as_deref() == Some(id)).then(|| container_id.clone())
    })
}

async fn load_managed_container_record(
    state: &DockerState,
    id: &str,
) -> Option<(String, DockerContainerRecord)> {
    let logical_id = resolve_managed_container_id(state, id).await?;
    let record = state.containers.lock().await.get(&logical_id).cloned()?;
    Some((logical_id, record))
}

async fn find_backend_vm_by_name(state: &DockerState, name: &str) -> Option<VmInfo> {
    state
        .backend
        .list()
        .await
        .ok()?
        .into_iter()
        .find(|vm| vm.name.as_deref() == Some(name))
}

async fn list_container_views(
    state: &DockerState,
) -> anyhow::Result<Vec<(VmInfo, HashMap<String, String>)>> {
    let backend_vms = state.backend.list().await?;
    let managed = state.containers.lock().await.clone();
    let backend_by_id = backend_vms
        .iter()
        .cloned()
        .map(|vm| (vm.id.clone(), vm))
        .collect::<HashMap<_, _>>();
    let managed_backend_ids = managed
        .values()
        .filter_map(|record| record.backend_id.clone())
        .collect::<HashSet<_>>();
    let mut containers = Vec::with_capacity(managed.len() + backend_vms.len());

    for (id, record) in managed {
        let backend_vm = record
            .backend_id
            .as_ref()
            .and_then(|backend_id| backend_by_id.get(backend_id));
        containers.push((
            synthesize_container_vm(&id, &record, backend_vm),
            record.config.labels,
        ));
    }

    for vm in backend_vms {
        if managed_backend_ids.contains(&vm.id) {
            continue;
        }
        containers.push((vm, HashMap::new()));
    }

    Ok(containers)
}

fn image_reference_from_query(query: &ImageCreateQuery) -> Result<String, &'static str> {
    let from_image = query
        .from_image
        .as_deref()
        .ok_or("missing required query parameter: fromImage")?;

    if let Some(tag) = query.tag.as_deref() {
        if from_image.contains('@')
            || from_image
                .rsplit('/')
                .next()
                .is_some_and(|part| part.contains(':'))
        {
            Ok(from_image.to_owned())
        } else if tag.strip_prefix("sha256:").is_some_and(|digest| {
            digest.len() == 64 && digest.bytes().all(|byte| byte.is_ascii_hexdigit())
        }) {
            Ok(format!("{from_image}@{tag}"))
        } else {
            Ok(format!("{from_image}:{tag}"))
        }
    } else {
        Ok(from_image.to_owned())
    }
}

fn default_volume_mountpoint(name: &str) -> String {
    std::env::temp_dir()
        .join("visor-docker-volumes")
        .join(name)
        .display()
        .to_string()
}

fn recreate_vm_config_from_info(vm: &VmInfo) -> visor_types::VmConfig {
    let mut config = visor_types::VmConfig::new(vm.image.clone());
    config.name.clone_from(&vm.name);
    config.memory_mib = vm.memory_mib;
    config.vcpus = vm.vcpus;
    config.ports.clone_from(&vm.ports);
    config.detach = true;
    config
}

// ── System endpoints ────────────────────────────────────────────────

/// `GET /_ping` — Returns `"OK"` with Docker version headers.
pub async fn ping() -> Response {
    let mut response = (StatusCode::OK, PING_RESPONSE).into_response();
    let headers = response.headers_mut();
    headers.insert("Content-Type", HeaderValue::from_static("text/plain"));
    headers.insert("Api-Version", HeaderValue::from_static(API_VERSION));
    headers.insert("Docker-Experimental", HeaderValue::from_static("false"));
    headers.insert("OSType", HeaderValue::from_static("linux"));
    response
}

/// `GET /version` — Returns Docker-compatible version information.
pub async fn version() -> Json<VersionResponse> {
    Json(VersionResponse {
        api_version: API_VERSION.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        min_a_p_i_version: MIN_API_VERSION.to_owned(),
        git_commit: String::new(),
        go_version: format!("rustc {}", rustc_version()),
        os: std::env::consts::OS.to_owned(),
        arch: std::env::consts::ARCH.to_owned(),
        kernel_version: String::new(),
        build_time: String::new(),
    })
}

/// `GET /info` — Returns system-wide information.
///
/// # Errors
///
/// Returns a Docker error response if the backend fails to list VMs.
pub async fn info(State(state): State<DockerState>) -> Result<Json<InfoResponse>, Response> {
    let containers = list_container_views(&state)
        .await
        .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let running = containers
        .iter()
        .filter(|(vm, _)| vm.state == VmState::Running)
        .count();
    let stopped = containers.len() - running;

    Ok(Json(InfoResponse {
        containers: containers.len() as u64,
        containers_running: running as u64,
        containers_stopped: stopped as u64,
        images: 0,
        server_version: env!("CARGO_PKG_VERSION").to_owned(),
        os_type: "linux".to_owned(),
        architecture: std::env::consts::ARCH.to_owned(),
        name: "visor".to_owned(),
        driver: "visor-vmm".to_owned(),
        mem_total: 0,
    }))
}

// ── Container CRUD ──────────────────────────────────────────────────

/// `GET /containers/json` — Lists containers (VMs).
///
/// # Errors
///
/// Returns a Docker error response if the backend fails to list VMs.
pub async fn container_list(
    State(state): State<DockerState>,
    Query(query): Query<ContainerListQuery>,
) -> Result<Json<Vec<crate::types::ContainerListEntry>>, Response> {
    let containers = list_container_views(&state)
        .await
        .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let show_all = query.all.unwrap_or(false);
    let filters = parse_container_list_filters(query.filters.as_deref())
        .map_err(|message| docker_error(StatusCode::BAD_REQUEST, message))?;
    let entries: Vec<_> = containers
        .into_iter()
        .filter(|(vm, _)| show_all || vm.state == VmState::Running)
        .filter_map(|(vm, labels)| {
            vm_matches_filters(&vm, &labels, &filters)
                .then(|| translate::vm_info_to_list_entry_with_labels(&vm, &labels))
        })
        .collect();

    Ok(Json(entries))
}

/// `GET /events` — Streams Docker-style container events.
///
/// # Errors
///
/// Returns 400 if the `filters` query parameter is invalid.
pub async fn events(
    State(state): State<DockerState>,
    Query(query): Query<EventsQuery>,
) -> Result<Response, Response> {
    let filters = parse_event_filters(query.filters.as_deref())
        .map_err(|message| docker_error(StatusCode::BAD_REQUEST, message))?;
    let state = state.clone();
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, Infallible>>(16);

    tokio::spawn(async move {
        let mut previous_states = HashMap::<String, VmState>::new();

        loop {
            let containers = match list_container_views(&state).await {
                Ok(containers) => containers,
                Err(error) => {
                    tracing::warn!(error = %error, "docker events poll failed");
                    break;
                }
            };
            let mut current_states = HashMap::new();

            for (vm, labels) in &containers {
                if !vm_matches_event_filters(vm, &labels, &filters) {
                    continue;
                }

                current_states.insert(vm.id.clone(), vm.state);
                let Some(action) =
                    next_container_event(previous_states.get(&vm.id).copied(), vm.state)
                else {
                    continue;
                };
                if !filters.events.is_empty()
                    && !filters.events.iter().any(|filter| filter == action)
                {
                    continue;
                }

                let payload =
                    match serde_json::to_vec(&container_event_payload(vm, &labels, action)) {
                        Ok(payload) => payload,
                        Err(error) => {
                            tracing::warn!(error = %error, "failed to serialize docker event");
                            continue;
                        }
                    };
                if tx
                    .send(Ok(Bytes::from([payload, vec![b'\n']].concat())))
                    .await
                    .is_err()
                {
                    return;
                }
            }

            previous_states = current_states;

            if tx.is_closed() {
                return;
            }

            tokio::time::sleep(std::time::Duration::from_millis(250)).await;
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(body)
        .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `POST /containers/create` — Creates a new container (VM).
///
/// # Errors
///
/// Returns a Docker error response if VM creation fails.
pub async fn container_create(
    State(state): State<DockerState>,
    Query(query): Query<ContainerCreateQuery>,
    Json(body): Json<crate::types::ContainerCreateRequest>,
) -> Result<Response, Response> {
    debug!(image = %body.image, name = ?query.name, cmd = ?body.cmd, entrypoint = ?body.entrypoint, env = ?body.env, "docker container create body");

    let config = translate::docker_create_to_vm_config(&body, query.name.as_deref(), true)
        .map_err(|error| docker_error(StatusCode::BAD_REQUEST, error.to_string()))?;
    debug!(vm_cmd = ?config.cmd, vm_env_count = config.env.len(), vm_memory = config.memory_mib, "translated VmConfig");
    let id = Uuid::new_v4().to_string();
    state.containers.lock().await.insert(
        id.clone(),
        DockerContainerRecord {
            backend_id: None,
            config,
            created_at: docker_created_at(),
            pending_archives: Vec::new(),
        },
    );

    let resp = ContainerCreateResponse {
        id,
        warnings: Vec::new(),
    };

    Ok((StatusCode::CREATED, Json(resp)).into_response())
}

/// `GET /containers/{id}/json` — Inspects a container (VM).
///
/// # Errors
///
/// Returns 404 if the container is not found.
pub async fn container_inspect(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
) -> Result<Json<crate::types::ContainerInspectResponse>, Response> {
    let vm = load_container_view(&state, &id)
        .await
        .ok_or_else(|| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;
    let inspect = match load_container_config(&state, &id).await {
        Some(config) => translate::vm_info_to_inspect_with_config(&vm, &config),
        None => translate::vm_info_to_inspect(&vm),
    };

    Ok(Json(inspect))
}

/// `POST /containers/{id}/start` — Starts a container.
///
/// visor VMs start on creation, so this is mostly a no-op.
/// Returns 304 if already running, 204 otherwise.
///
/// # Errors
///
/// Returns 404 if the container is not found.
pub async fn container_start(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
) -> Result<Response, Response> {
    if let Some((logical_id, mut record)) = load_managed_container_record(&state, &id).await {
        if let Some(backend_id) = record.backend_id.as_deref() {
            match state.backend.get(backend_id).await {
                Ok(vm) if vm.state == VmState::Running => {
                    return Ok(StatusCode::NOT_MODIFIED.into_response());
                }
                Ok(_) => {
                    unregister_service_names(&state, &record.config).await;
                    state.backend.destroy(backend_id).await.map_err(|e| {
                        docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                    })?;
                }
                Err(_) => {}
            }
        }

        let mut config = record.config.clone();
        populate_discovered_extra_hosts(&state, &mut config).await;
        config.detach = true;
        let pending_archives = record.pending_archives.clone();

        let vm = state.backend.create(config.clone()).await.map_err(|e| {
            tracing::error!(error = %e, error_debug = ?e, "container start failed");
            docker_error(StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}"))
        })?;

        for archive in &pending_archives {
            state
                .backend
                .copy_to_guest(&vm.id, archive.data.clone(), &archive.dest)
                .await
                .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }

        record.backend_id = Some(vm.id.clone());
        record.config = config.clone();
        record.pending_archives.clear();
        state.containers.lock().await.insert(logical_id, record);
        register_service_names(&state, &vm, &config).await;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    let vm = state
        .backend
        .get(&id)
        .await
        .map_err(|_| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;

    if vm.state == VmState::Running {
        return Ok(StatusCode::NOT_MODIFIED.into_response());
    }

    let mut config = recreate_vm_config_from_info(&vm);
    unregister_service_names(&state, &config).await;
    config.detach = true;
    if config.name.is_none() {
        config.name = vm.name.clone().or_else(|| Some(vm.id.clone()));
    }

    state
        .backend
        .destroy(&vm.id)
        .await
        .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let recreated = state
        .backend
        .create(config.clone())
        .await
        .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    register_service_names(&state, &recreated, &config).await;

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /containers/{id}/stop` — Stops a container (VM).
///
/// # Errors
///
/// Returns 404 if the container is not found.
pub async fn container_stop(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
    Query(query): Query<ContainerStopQuery>,
) -> Result<Response, Response> {
    let timeout = query.t.unwrap_or(10);
    debug!(id = %id, timeout, "docker container stop");
    if let Some((_, record)) = load_managed_container_record(&state, &id).await {
        let Some(backend_id) = record.backend_id else {
            return Ok(StatusCode::NOT_MODIFIED.into_response());
        };
        state
            .backend
            .stop(&backend_id, timeout)
            .await
            .map_err(|_| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;
        unregister_service_names(&state, &record.config).await;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    let vm = state
        .backend
        .get(&id)
        .await
        .map_err(|_| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;
    state
        .backend
        .stop(&vm.id, timeout)
        .await
        .map_err(|_| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;
    unregister_service_names(&state, &recreate_vm_config_from_info(&vm)).await;

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /containers/{id}/kill` — Force-kills a container (VM).
///
/// # Errors
///
/// Returns 404 if the container is not found.
pub async fn container_kill(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
) -> Result<Response, Response> {
    debug!(id = %id, "docker container kill");
    if let Some((_, record)) = load_managed_container_record(&state, &id).await {
        let Some(backend_id) = record.backend_id else {
            return Ok(StatusCode::NOT_MODIFIED.into_response());
        };
        state
            .backend
            .kill(&backend_id)
            .await
            .map_err(|_| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;
        unregister_service_names(&state, &record.config).await;
        return Ok(StatusCode::NO_CONTENT.into_response());
    }

    let vm = state
        .backend
        .get(&id)
        .await
        .map_err(|_| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;
    state
        .backend
        .kill(&vm.id)
        .await
        .map_err(|_| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;
    unregister_service_names(&state, &recreate_vm_config_from_info(&vm)).await;

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `DELETE /containers/{id}` — Removes a container (VM).
///
/// # Errors
///
/// Returns 404 if the container is not found.
pub async fn container_remove(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
) -> Result<Response, Response> {
    debug!(id = %id, "docker container remove");
    if let Some(logical_id) = resolve_managed_container_id(&state, &id).await {
        if let Some(record) = state.containers.lock().await.remove(&logical_id) {
            if let Some(backend_id) = record.backend_id {
                state.backend.destroy(&backend_id).await.map_err(|_| {
                    docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}"))
                })?;
                unregister_service_names(&state, &record.config).await;
            }
            return Ok(StatusCode::NO_CONTENT.into_response());
        }
    }

    let vm = state
        .backend
        .get(&id)
        .await
        .map_err(|_| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;
    state
        .backend
        .destroy(&vm.id)
        .await
        .map_err(|_| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;

    Ok(StatusCode::NO_CONTENT.into_response())
}

/// `POST /containers/{id}/wait` — Waits for a container to exit.
///
/// Docker CLI calls this **before** `/start` and expects HTTP headers to
/// be flushed immediately.  The response body is streamed later when the
/// container actually exits.  If we block before sending headers the CLI
/// will hang forever.
///
/// # Errors
///
/// Returns 404 if the container is not found.
pub async fn container_wait(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
) -> Result<Response, Response> {
    debug!(id = %id, "docker container wait");

    let vm = load_container_view(&state, &id)
        .await
        .ok_or_else(|| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;
    if vm.state != VmState::Running
        && vm.state != VmState::Creating
        && (vm.stdout.is_some() || vm.exit_code.is_some())
    {
        let wait_resp = translate::vm_info_to_wait(&vm);
        let json = serde_json::to_vec(&wait_resp)
            .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Response::builder()
            .status(StatusCode::OK)
            .header("Content-Type", "application/json")
            .body(Body::from(json))
            .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
    }

    // Container is running — return 200 headers immediately and stream
    // the JSON body when the container exits.  This unblocks Docker CLI
    // so it can proceed to call /start.
    let (tx, rx) = tokio::sync::mpsc::channel::<Result<Bytes, std::io::Error>>(1);
    let state = state.clone();
    let wait_id = id.clone();

    tokio::spawn(async move {
        let wait_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut saw_running = false;
        loop {
            match load_container_view(&state, &wait_id).await {
                Some(vm) if vm.state == VmState::Running => {
                    saw_running = true;
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                }
                Some(vm)
                    if vm.state != VmState::Running
                        && (saw_running
                            || vm.stdout.is_some()
                            || vm.exit_code.is_some()
                            || std::time::Instant::now() >= wait_deadline) =>
                {
                    let wait_resp = translate::vm_info_to_wait(&vm);
                    if let Ok(json) = serde_json::to_vec(&wait_resp) {
                        let _ = tx.send(Ok(Bytes::from(json))).await;
                    }
                    return;
                }
                None if saw_running || std::time::Instant::now() >= wait_deadline => {
                    // VM disappeared — report exit code 137 (killed).
                    let resp = ContainerWaitResponse { status_code: 137 };
                    if let Ok(json) = serde_json::to_vec(&resp) {
                        let _ = tx.send(Ok(Bytes::from(json))).await;
                    }
                    return;
                }
                _ => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = Body::from_stream(stream);

    Response::builder()
        .status(StatusCode::OK)
        .header("Content-Type", "application/json")
        .body(body)
        .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `POST /containers/{id}/attach` — Attaches to a VM's output stream.
///
/// Docker CLI sends `Upgrade: tcp` + `Connection: Upgrade` and expects a
/// `101 Switching Protocols` response, after which the raw connection carries
/// Docker's multiplexed stream (8-byte header + payload per frame).
///
/// We extract the [`hyper::upgrade::OnUpgrade`] future from the request
/// extensions (the same mechanism axum's WebSocket extractor uses), return
/// `101`, and then stream the VM's console output over the hijacked
/// connection once it finishes.
///
/// # Errors
///
/// Returns 404 if the VM is not found, 426 if the connection cannot be
/// upgraded, or 500 on internal errors.
pub async fn container_attach(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
    mut req: axum::extract::Request,
) -> Result<Response, Response> {
    debug!(id = %id, "docker container attach");

    load_container_view(&state, &id)
        .await
        .ok_or_else(|| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;

    // Extract the OnUpgrade future from request extensions.
    // This is exactly how axum's own WebSocket extractor works —
    // hyper inserts it when `serve_connection_with_upgrades` is used.
    let on_upgrade = req
        .extensions_mut()
        .remove::<hyper::upgrade::OnUpgrade>()
        .ok_or_else(|| {
            tracing::error!("attach: OnUpgrade extension missing — connection not upgradable");
            docker_error(
                StatusCode::UPGRADE_REQUIRED,
                "connection does not support protocol upgrade",
            )
        })?;

    let state = state.clone();
    let attach_id = id.clone();
    tokio::spawn(async move {
        // Wait for hyper to complete the 101 handshake and give us the raw I/O
        let upgraded = match on_upgrade.await {
            Ok(u) => u,
            Err(e) => {
                tracing::warn!(error = %e, "attach upgrade handshake failed");
                return;
            }
        };

        let mut io = hyper_util::rt::TokioIo::new(upgraded);

        // Docker attaches before `start`, so wait through the pre-start state.
        // Once the container has run at least once, break after it stops.
        let attach_deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut saw_running = false;
        loop {
            match load_container_view(&state, &attach_id).await {
                Some(vm) if vm.state == VmState::Running => {
                    saw_running = true;
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Some(vm)
                    if vm.state == VmState::Creating
                        && !saw_running
                        && std::time::Instant::now() < attach_deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                Some(vm)
                    if vm.state != VmState::Running
                        && (vm.stdout.is_some()
                            || vm.exit_code.is_some()
                            || std::time::Instant::now() >= attach_deadline) =>
                {
                    break;
                }
                None if !saw_running && std::time::Instant::now() < attach_deadline => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
                _ => {
                    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                }
            }
        }

        // Collect output — try live serial buffer first, fall back to
        // stored VmInfo.stdout.
        let output = match resolve_container_backend_id(&state, &attach_id).await {
            Some(backend_id) => {
                if let Ok(vm) = state.backend.get(&backend_id).await {
                    if let Some(stdout) = vm.stdout {
                        stdout.into_bytes()
                    } else {
                        state
                            .backend
                            .console_output(&backend_id)
                            .await
                            .unwrap_or_default()
                    }
                } else {
                    state
                        .backend
                        .console_output(&backend_id)
                        .await
                        .unwrap_or_default()
                }
            }
            None => Vec::new(),
        };

        // Write output as Docker multiplexed stdout frame.
        // Frame: [stream_type(1) | 0 | 0 | 0 | size(4 big-endian)] + payload
        {
            use tokio::io::AsyncWriteExt as _;
            if !output.is_empty() {
                let len = u32::try_from(output.len()).unwrap_or(u32::MAX);
                let size = len.to_be_bytes();
                let header = [1u8, 0, 0, 0, size[0], size[1], size[2], size[3]];
                let _ = io.write_all(&header).await;
                let _ = io.write_all(&output).await;
                let _ = io.flush().await;
            }
            let _ = io.shutdown().await;
        }
    });

    // Return 101 Switching Protocols — hyper completes the upgrade and
    // resolves the OnUpgrade future we spawned above.
    Response::builder()
        .status(StatusCode::SWITCHING_PROTOCOLS)
        .header("Connection", "Upgrade")
        .header("Upgrade", "tcp")
        .header("Content-Type", "application/vnd.docker.raw-stream")
        .body(Body::empty())
        .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))
}

/// `GET /containers/{id}/logs` — Returns container logs.
///
/// Simplified: returns raw console output as plain text (no multiplexed
/// stream header for P1).
///
/// # Errors
///
/// Returns 404 if the container is not found.
pub async fn container_logs(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
    Query(query): Query<ContainerLogsQuery>,
) -> Result<Response, Response> {
    let vm = load_container_view(&state, &id)
        .await
        .ok_or_else(|| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;

    let stdout = if vm.state == VmState::Running {
        match resolve_container_backend_id(&state, &id).await {
            Some(backend_id) => state
                .backend
                .console_output(&backend_id)
                .await
                .unwrap_or_else(|_| vm.stdout.clone().unwrap_or_default().into_bytes()),
            None => vm.stdout.clone().unwrap_or_default().into_bytes(),
        }
    } else {
        vm.stdout.clone().unwrap_or_default().into_bytes()
    };
    let stderr = vm.stderr.clone().unwrap_or_default().into_bytes();

    let include_stdout = query.stdout.unwrap_or(true);
    let include_stderr = query.stderr.unwrap_or(true);
    let mut output = Vec::new();
    if include_stdout && !stdout.is_empty() {
        output.extend_from_slice(&raw_stream_frame(1, &stdout));
    }
    if include_stderr && !stderr.is_empty() {
        output.extend_from_slice(&raw_stream_frame(2, &stderr));
    }

    Ok((
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.docker.raw-stream"),
        )],
        output,
    )
        .into_response())
}

/// `PUT /containers/{id}/archive` — Copy a tar archive into a running container.
///
/// # Errors
///
/// Returns a Docker error response if the container is not found or if the
/// archive transfer into the guest fails.
pub async fn container_archive_put(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
    Query(query): Query<ContainerArchiveQuery>,
    body: Bytes,
) -> Result<Response, Response> {
    let _ = query.no_overwrite_dir_non_dir;
    let needs_resolv_override = archive_needs_resolv_override(body.as_ref(), &query.path)
        .map_err(|error| docker_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    if let Some(logical_id) = resolve_managed_container_id(&state, &id).await {
        let mut containers = state.containers.lock().await;
        let record = containers.get_mut(&logical_id).ok_or_else(|| {
            docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}"))
        })?;

        if record.backend_id.is_none() {
            record.pending_archives.push(PendingArchive {
                dest: query.path.clone(),
                data: body.to_vec(),
            });
            if needs_resolv_override {
                let resolv_conf_archive = fallback_resolv_conf_archive()
                    .map_err(|error| docker_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
                record.pending_archives.push(PendingArchive {
                    dest: query.path.clone(),
                    data: resolv_conf_archive,
                });
            }
            return Ok(StatusCode::OK.into_response());
        }
    }

    let backend_id = resolve_container_backend_id(&state, &id)
        .await
        .ok_or_else(|| docker_error(StatusCode::NOT_FOUND, format!("No such container: {id}")))?;

    state
        .backend
        .copy_to_guest(&backend_id, body.to_vec(), &query.path)
        .await
        .map_err(|e| match e.to_string().as_str() {
            message if message.contains("not found") => {
                docker_error(StatusCode::NOT_FOUND, message.to_owned())
            }
            _ => docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()),
        })?;

    if needs_resolv_override {
        let resolv_conf_archive = fallback_resolv_conf_archive()
            .map_err(|error| docker_error(StatusCode::INTERNAL_SERVER_ERROR, error))?;
        state
            .backend
            .copy_to_guest(&backend_id, resolv_conf_archive, &query.path)
            .await
            .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }

    Ok(StatusCode::OK.into_response())
}

fn archive_needs_resolv_override(archive_bytes: &[u8], dest: &str) -> Result<bool, String> {
    if dest != "/etc" {
        return Ok(false);
    }

    let mut archive = tar::Archive::new(std::io::Cursor::new(archive_bytes));
    for entry_result in archive
        .entries()
        .map_err(|error| format!("failed to read tar entries: {error}"))?
    {
        let mut entry =
            entry_result.map_err(|error| format!("failed to read tar entry: {error}"))?;
        let entry_path = entry
            .path()
            .map_err(|error| format!("failed to read tar entry path: {error}"))?
            .into_owned();
        if entry_path.file_name().and_then(|name| name.to_str()) != Some("resolv.conf") {
            continue;
        }

        if !entry.header().entry_type().is_file() {
            return Ok(true);
        }

        let mut contents = String::new();
        entry
            .read_to_string(&mut contents)
            .map_err(|error| format!("failed to read resolv.conf archive entry: {error}"))?;
        return Ok(resolv_conf_uses_only_loopback_nameservers(&contents));
    }

    Ok(false)
}

fn resolv_conf_uses_only_loopback_nameservers(contents: &str) -> bool {
    let mut saw_nameserver = false;

    for line in contents.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        let Some(server) = trimmed
            .strip_prefix("nameserver")
            .and_then(|value| value.split_whitespace().next())
        else {
            continue;
        };

        let Ok(address) = server.parse::<std::net::IpAddr>() else {
            return false;
        };
        saw_nameserver = true;
        if !address.is_loopback() {
            return false;
        }
    }

    saw_nameserver
}

fn fallback_resolv_conf_archive() -> Result<Vec<u8>, String> {
    let fallback = b"nameserver 1.1.1.1\nnameserver 8.8.8.8\n";
    let buffer = Vec::new();
    let mut builder = tar::Builder::new(buffer);
    let mut header = tar::Header::new_gnu();
    header.set_mode(0o644);
    header.set_size(u64::try_from(fallback.len()).map_err(|error| error.to_string())?);
    header.set_cksum();
    builder
        .append_data(&mut header, "resolv.conf", &fallback[..])
        .map_err(|error| format!("failed to build fallback resolv.conf archive: {error}"))?;
    builder
        .into_inner()
        .map_err(|error| format!("failed to finalize fallback resolv.conf archive: {error}"))
}

// ── Exec ────────────────────────────────────────────────────────────

/// `POST /containers/{id}/exec` — Creates an exec instance.
///
/// # Errors
///
/// Returns 404 if the container is not found.
pub async fn exec_create(
    State(state): State<DockerState>,
    Path(IdPath { id: container_id }): Path<IdPath>,
    Json(body): Json<ExecCreateRequest>,
) -> Result<Response, Response> {
    resolve_container_backend_id(&state, &container_id)
        .await
        .ok_or_else(|| {
            docker_error(
                StatusCode::NOT_FOUND,
                format!("No such container: {container_id}"),
            )
        })?;

    let exec_id = Uuid::new_v4().to_string();
    debug!(exec_id = %exec_id, container_id = %container_id, "docker exec create");

    let session = ExecSession {
        container_id,
        request: body,
        result: None,
    };

    state
        .exec_sessions
        .lock()
        .await
        .insert(exec_id.clone(), session);

    Ok((
        StatusCode::CREATED,
        Json(ExecCreateResponse { id: exec_id }),
    )
        .into_response())
}

/// `POST /exec/{id}/start` — Starts a previously created exec instance.
///
/// # Errors
///
/// Returns 404 if the exec instance is not found, or 500 if execution fails.
pub async fn exec_start(
    State(state): State<DockerState>,
    Path(IdPath { id: exec_id }): Path<IdPath>,
    mut req: axum::extract::Request,
) -> Result<Response, Response> {
    let body = read_exec_start_request(&mut req).await?;

    let session = {
        let sessions = state.exec_sessions.lock().await;
        sessions.get(&exec_id).cloned().ok_or_else(|| {
            docker_error(StatusCode::NOT_FOUND, format!("No such exec: {exec_id}"))
        })?
    };

    debug!(exec_id = %exec_id, container_id = %session.container_id, "docker exec start");

    let tty = body.tty.or(session.request.tty).unwrap_or(false);
    let attach_stdin = session.request.attach_stdin.unwrap_or(false);
    let attach_stdout = session.request.attach_stdout.unwrap_or(true);
    let attach_stderr = session.request.attach_stderr.unwrap_or(true);
    let mut exec_req = translate::docker_exec_to_exec_request(&session.request);
    exec_req.tty = tty;
    let backend_id = resolve_container_backend_id(&state, &session.container_id)
        .await
        .ok_or_else(|| {
            docker_error(
                StatusCode::NOT_FOUND,
                format!("No such container: {}", session.container_id),
            )
        })?;
    if body.detach != Some(true) {
        if let Some(on_upgrade) = req.extensions_mut().remove::<hyper::upgrade::OnUpgrade>() {
            if !tty && !attach_stdin {
                spawn_exec_output_bridge(
                    &state,
                    &exec_id,
                    &backend_id,
                    exec_req.clone(),
                    attach_stdout,
                    attach_stderr,
                    on_upgrade,
                );
            } else {
                spawn_exec_stream_bridge(
                    &state,
                    &exec_id,
                    &backend_id,
                    exec_req.clone(),
                    tty,
                    attach_stdout,
                    attach_stderr,
                    on_upgrade,
                );
            }
            return Ok(switching_protocols_response());
        }
    }

    let result = state
        .backend
        .exec(&backend_id, exec_req)
        .await
        .map_err(|e| docker_error(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Store the result for later inspection
    {
        let mut sessions = state.exec_sessions.lock().await;
        if let Some(s) = sessions.get_mut(&exec_id) {
            s.result = Some(result.clone());
        }
    }

    // Return stdout as plain text (simplified — no multiplexed stream)
    Ok((StatusCode::OK, result.stdout).into_response())
}

/// `GET /exec/{id}/json` — Inspects an exec instance.
///
/// # Errors
///
/// Returns 404 if the exec instance is not found.
pub async fn exec_inspect(
    State(state): State<DockerState>,
    Path(IdPath { id: exec_id }): Path<IdPath>,
) -> Result<Json<ExecInspectResponse>, Response> {
    let sessions = state.exec_sessions.lock().await;
    let session = sessions
        .get(&exec_id)
        .ok_or_else(|| docker_error(StatusCode::NOT_FOUND, format!("No such exec: {exec_id}")))?;

    let resp = if let Some(ref result) = session.result {
        translate::exec_result_to_inspect(&exec_id, &session.container_id, result)
    } else {
        ExecInspectResponse {
            id: exec_id.clone(),
            running: true,
            exit_code: None,
            container_i_d: session.container_id.clone(),
        }
    };

    Ok(Json(resp))
}

// ── Images ────────────────────────────────────────────────────────────────

/// `GET /images/json` — Lists images from the tag store.
///
/// Returns real entries when an [`ImageStore`](visor_build::ImageStore) is
/// configured, otherwise an empty array.
pub async fn image_list(State(state): State<DockerState>) -> Json<Vec<ImageListEntry>> {
    if let Some(ref manager) = state.image_manager {
        return match manager.list_images().await {
            Ok(images) => Json(images.into_iter().map(image_list_entry_from_info).collect()),
            Err(_) => Json(Vec::new()),
        };
    }

    let Some(ref store) = state.image_store else {
        return Json(Vec::new());
    };

    let Ok(tags) = store.list_tags() else {
        return Json(Vec::new());
    };

    let entries: Vec<ImageListEntry> = tags
        .into_iter()
        .map(|(tag, digest)| ImageListEntry {
            id: digest,
            repo_tags: vec![tag],
            created: 0,
            size: 0,
            labels: HashMap::new(),
        })
        .collect();

    Json(entries)
}

/// `POST /images/create` — Pulls an image.
pub(crate) async fn image_create(
    State(state): State<DockerState>,
    Query(query): Query<ImageCreateQuery>,
) -> Response {
    let reference = match image_reference_from_query(&query) {
        Ok(reference) => reference,
        Err(message) => return docker_error(StatusCode::BAD_REQUEST, message),
    };

    if let Some(ref manager) = state.image_manager {
        match manager.pull_image(&reference).await {
            Ok(_) => {
                let body = format!(
                    "{{\"status\":\"Pulling from {reference}\"}}\n\
                     {{\"status\":\"Pull complete\"}}\n"
                );
                return (StatusCode::OK, body).into_response();
            }
            Err(error) => {
                return docker_error(StatusCode::BAD_REQUEST, error.to_string());
            }
        }
    }

    let body = "{\"status\":\"Pull complete\"}\n";
    (StatusCode::OK, body).into_response()
}

/// `GET /images/{name}/json` — Inspects an image by tag or digest.
///
/// Returns image metadata when the name matches a tag in the store,
/// or 404 if no match is found.
pub async fn image_inspect(
    State(state): State<DockerState>,
    Path(NamePath { name }): Path<NamePath>,
) -> Response {
    if let Some(ref manager) = state.image_manager {
        return match manager.inspect_image(&name).await {
            Ok(info) => (StatusCode::OK, Json(docker_image_inspect_json(&info))).into_response(),
            Err(_) => docker_error(StatusCode::NOT_FOUND, format!("No such image: {name}")),
        };
    }

    let Some(ref store) = state.image_store else {
        return docker_error(StatusCode::NOT_FOUND, format!("No such image: {name}"));
    };

    match store.get_by_tag(&name) {
        Ok(Some(digest)) => {
            let info = serde_json::json!({
                "Id": digest,
                "RepoTags": [name],
                "Created": "1970-01-01T00:00:00Z",
                "Size": 0,
                "Architecture": std::env::consts::ARCH,
                "Os": "linux"
            });
            (StatusCode::OK, Json(info)).into_response()
        }
        _ => docker_error(StatusCode::NOT_FOUND, format!("No such image: {name}")),
    }
}

/// `DELETE /images/{name}` — Removes an image.
pub async fn image_remove(
    State(state): State<DockerState>,
    Path(NamePath { name }): Path<NamePath>,
) -> Response {
    if let Some(ref manager) = state.image_manager {
        return match manager.remove_image(&name).await {
            Ok(()) => Json(Vec::<serde_json::Value>::new()).into_response(),
            Err(_) => docker_error(StatusCode::NOT_FOUND, format!("No such image: {name}")),
        };
    }

    if let Some(ref store) = state.image_store {
        return match store.remove_tag(&name) {
            Ok(true) => Json(Vec::<serde_json::Value>::new()).into_response(),
            Ok(false) | Err(_) => {
                docker_error(StatusCode::NOT_FOUND, format!("No such image: {name}"))
            }
        };
    }

    docker_error(StatusCode::NOT_FOUND, format!("No such image: {name}"))
}

/// `POST /images/load` — Loads a Docker image archive into the local store.
pub async fn image_load(
    State(state): State<DockerState>,
    Query(query): Query<ImageLoadQuery>,
    body: Body,
) -> Response {
    let Some(ref store) = state.image_store else {
        return docker_error(
            StatusCode::NOT_IMPLEMENTED,
            "image load requires a configured image store",
        );
    };

    let body_bytes = match axum::body::to_bytes(body, 1024 * 1024 * 1024).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return docker_error(
                StatusCode::BAD_REQUEST,
                format!("failed to read image archive: {error}"),
            );
        }
    };

    let loaded_tags = match store.load_docker_archive(&body_bytes) {
        Ok(tags) => tags,
        Err(error) => {
            return docker_error(
                StatusCode::BAD_REQUEST,
                format!("failed to load image archive: {error}"),
            );
        }
    };

    let quiet = query.quiet.unwrap_or(false);
    let body = if quiet {
        String::new()
    } else if loaded_tags.is_empty() {
        "{\"stream\":\"Loaded image\\n\"}\n".to_owned()
    } else {
        let mut stream = String::new();
        for tag in loaded_tags {
            use std::fmt::Write as _;
            let _ = writeln!(stream, "{{\"stream\":\"Loaded image: {tag}\\n\"}}");
        }
        stream
    };

    let mut response = (StatusCode::OK, body).into_response();
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    response
}

// ── Networks ────────────────────────────────────────────────────────

/// `GET /networks` — Lists networks.
pub async fn network_list(State(state): State<DockerState>) -> Json<Vec<NetworkListEntry>> {
    let networks = state.networks.lock().await;
    Json(
        networks
            .values()
            .map(|network| NetworkListEntry {
                id: network.id.clone(),
                name: network.name.clone(),
                driver: network.driver.clone(),
                scope: "local".to_owned(),
            })
            .collect(),
    )
}

/// `POST /networks/create` — Creates a network.
pub async fn network_create(
    State(state): State<DockerState>,
    Json(body): Json<NetworkCreateRequest>,
) -> Response {
    let id = Uuid::new_v4().to_string();
    let record = DockerNetworkRecord {
        id: id.clone(),
        name: body.name.clone(),
        driver: body.driver.unwrap_or_else(|| "bridge".to_owned()),
        labels: body.labels.unwrap_or_default(),
    };
    debug!(name = %record.name, id = %id, "docker network create");

    state.networks.lock().await.insert(id.clone(), record);

    (
        StatusCode::CREATED,
        Json(NetworkCreateResponse {
            id,
            warning: String::new(),
        }),
    )
        .into_response()
}

/// `GET /networks/{id}` — Inspects a network.
pub async fn network_inspect(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
) -> Response {
    let networks = state.networks.lock().await;
    let record = networks
        .get(&id)
        .or_else(|| networks.values().find(|network| network.name == id));

    match record {
        Some(network) => {
            let gateway =
                GuestNetworkLink::for_named_network(&network.name, FIRST_GUEST_CID).gateway_ip;
            (
                StatusCode::OK,
                Json(serde_json::json!({
                    "Id": network.id,
                    "Name": network.name,
                    "Driver": network.driver,
                    "Scope": "local",
                    "Labels": network.labels,
                    "Containers": {},
                    "Options": {},
                    "IPAM": {
                        "Driver": "default",
                        "Config": [{"Gateway": gateway}]
                    }
                })),
            )
                .into_response()
        }
        None => docker_error(StatusCode::NOT_FOUND, format!("No such network: {id}")),
    }
}

/// `DELETE /networks/{id}` — Removes a network.
pub async fn network_remove(
    State(state): State<DockerState>,
    Path(IdPath { id }): Path<IdPath>,
) -> Response {
    let mut networks = state.networks.lock().await;
    let removed = if networks.remove(&id).is_some() {
        true
    } else if let Some((network_id, _)) = networks
        .iter()
        .find(|(_, network)| network.name == id)
        .map(|(network_id, network)| (network_id.clone(), network.clone()))
    {
        networks.remove(&network_id).is_some()
    } else {
        false
    };

    if removed {
        StatusCode::NO_CONTENT.into_response()
    } else {
        docker_error(StatusCode::NOT_FOUND, format!("No such network: {id}"))
    }
}

// ── Volumes ─────────────────────────────────────────────────────────

/// `GET /volumes` — Lists volumes.
pub async fn volume_list(State(state): State<DockerState>) -> Json<VolumeListResponse> {
    let volumes = state.volumes.lock().await;
    Json(VolumeListResponse {
        volumes: volumes
            .values()
            .map(|volume| VolumeEntry {
                name: volume.name.clone(),
                driver: volume.driver.clone(),
                mountpoint: volume.mountpoint.clone(),
                labels: volume.labels.clone(),
                scope: "local".to_owned(),
            })
            .collect(),
        warnings: Vec::new(),
    })
}

/// `POST /volumes/create` — Creates a volume.
pub async fn volume_create(
    State(state): State<DockerState>,
    Json(body): Json<VolumeCreateRequest>,
) -> Response {
    let name = body
        .name
        .unwrap_or_else(|| Uuid::new_v4().to_string()[..12].to_owned());
    let mountpoint = default_volume_mountpoint(&name);
    if let Err(error) = std::fs::create_dir_all(&mountpoint) {
        return docker_error(StatusCode::INTERNAL_SERVER_ERROR, error.to_string());
    }

    let volume = DockerVolumeRecord {
        name: name.clone(),
        driver: body.driver.unwrap_or_else(|| "local".to_owned()),
        labels: body.labels.unwrap_or_default(),
        mountpoint: mountpoint.clone(),
    };
    state
        .volumes
        .lock()
        .await
        .insert(name.clone(), volume.clone());

    (
        StatusCode::CREATED,
        Json(VolumeEntry {
            name,
            driver: volume.driver,
            mountpoint,
            labels: volume.labels,
            scope: "local".to_owned(),
        }),
    )
        .into_response()
}

/// `GET /volumes/{name}` — Inspects a volume.
pub async fn volume_inspect(
    State(state): State<DockerState>,
    Path(NamePath { name }): Path<NamePath>,
) -> Response {
    let volumes = state.volumes.lock().await;
    match volumes.get(&name) {
        Some(volume) => (
            StatusCode::OK,
            Json(VolumeEntry {
                name: volume.name.clone(),
                driver: volume.driver.clone(),
                mountpoint: volume.mountpoint.clone(),
                labels: volume.labels.clone(),
                scope: "local".to_owned(),
            }),
        )
            .into_response(),
        None => docker_error(StatusCode::NOT_FOUND, format!("No such volume: {name}")),
    }
}

/// `DELETE /volumes/{name}` — Removes a volume.
pub async fn volume_remove(
    State(state): State<DockerState>,
    Path(NamePath { name }): Path<NamePath>,
) -> Response {
    let mut volumes = state.volumes.lock().await;
    match volumes.remove(&name) {
        Some(volume) => {
            let _ = std::fs::remove_dir_all(&volume.mountpoint);
            StatusCode::NO_CONTENT.into_response()
        }
        None => docker_error(StatusCode::NOT_FOUND, format!("No such volume: {name}")),
    }
}

// ── Build ───────────────────────────────────────────────────────────────

/// Query parameters for `POST /build`.
#[derive(Debug, serde::Deserialize, Default)]
pub struct BuildQuery {
    /// Path to Dockerfile within the context (default: `Dockerfile`).
    pub dockerfile: Option<String>,
    /// Image tag (e.g. `myapp:latest`).
    pub t: Option<String>,
    /// JSON-encoded build arguments: `{"KEY":"VALUE"}`.
    pub buildargs: Option<String>,
    /// Target build stage name.
    pub target: Option<String>,
    /// Disable cache (`"1"` or `"true"`).
    pub nocache: Option<String>,
    /// Quiet mode — suppress build output.
    pub q: Option<String>,
}

/// `POST /build` — Build an image from a Dockerfile.
///
/// Accepts a tar archive as the request body containing the build context.
/// Parses the Dockerfile and returns a Docker-compatible JSON progress stream.
///
/// When a [`BuildService`] is configured in [`DockerState`], delegates the
/// actual build to the service. Otherwise, falls back to fake progress
/// messages for API contract validation.
///
/// # Errors
///
/// Returns a Docker error response if the build context is invalid or
/// the Dockerfile cannot be parsed.
pub async fn build_image(
    State(state): State<DockerState>,
    Query(params): Query<BuildQuery>,
    body: Body,
) -> Response {
    debug!(
        dockerfile = ?params.dockerfile,
        tag = ?params.t,
        target = ?params.target,
        "docker build request"
    );

    // 1. Read body bytes (build context tar)
    let body_bytes = match axum::body::to_bytes(body, 512 * 1024 * 1024).await {
        Ok(b) => b,
        Err(e) => return build_error_response(&format!("failed to read request body: {e}")),
    };

    // 2. Extract tar to tempdir
    let context_dir = match extract_build_context(&body_bytes) {
        Ok(dir) => dir,
        Err(e) => return build_error_response(&format!("failed to extract build context: {e}")),
    };

    // 3. Read Dockerfile from context
    let dockerfile_path = params.dockerfile.as_deref().unwrap_or("Dockerfile");
    let dockerfile_full = context_dir.path().join(dockerfile_path);
    let Ok(dockerfile_content) = std::fs::read_to_string(&dockerfile_full) else {
        return build_error_response(&format!(
            "Cannot locate specified Dockerfile: {dockerfile_path}"
        ));
    };

    // 4. Parse Dockerfile
    let parsed = match visor_build::DockerfileParser::parse(&dockerfile_content) {
        Ok(p) => p,
        Err(e) => return build_error_response(&format!("Dockerfile parse error: {e}")),
    };

    // 5. Parse build args from JSON
    let build_args: HashMap<String, String> = params
        .buildargs
        .as_deref()
        .and_then(|s| serde_json::from_str(s).ok())
        .unwrap_or_default();

    let no_cache = params
        .nocache
        .as_deref()
        .is_some_and(|v| v == "1" || v == "true");

    let quiet = params.q.as_deref().is_some_and(|v| v == "1" || v == "true");

    debug!(
        build_args = ?build_args,
        no_cache,
        quiet,
        stages = parsed.stages.len(),
        "parsed Dockerfile"
    );

    // 6. Delegate to BuildService if available, otherwise use fake progress
    if let Some(ref build_service) = state.build_service {
        let mut request = visor_types::BuildRequest::new(dockerfile_content);
        request.context_dir = context_dir.path().to_path_buf();
        request.build_args = build_args;
        request.target.clone_from(&params.target);
        request.no_cache = no_cache;
        request.tag.clone_from(&params.t);

        match build_service.build_image(request).await {
            Ok(output) => {
                let messages = build_output_to_messages(&output, params.t.as_deref(), quiet);
                build_stream_response(messages)
            }
            Err(e) => {
                // Logged as well as returned: a build that fails inside the
                // guest leaves nothing else behind to diagnose it from.
                tracing::error!(error = ?e, "build failed");
                build_error_response(&format!("build failed: {e:#}"))
            }
        }
    } else {
        // Fallback: generate fake progress messages
        let messages = build_progress_messages(
            &parsed,
            params.t.as_deref(),
            params.target.as_deref(),
            quiet,
        );
        build_stream_response(messages)
    }
}

/// Converts [`BuildOutput`] into Docker-compatible progress messages.
fn build_output_to_messages(
    output: &visor_types::BuildOutput,
    tag: Option<&str>,
    quiet: bool,
) -> Vec<String> {
    let mut messages = Vec::new();

    for step in &output.steps {
        if !quiet {
            let cached = if step.cached { " (cached)" } else { "" };
            messages.push(format!(
                "Step {}/{} : {}{cached}\n",
                step.step, step.total, step.instruction,
            ));
        }

        if let Some(ref out) = step.output {
            if !quiet {
                messages.push(format!(" ---> {out}\n"));
            }
        }
    }

    messages.push(format!("Successfully built {}\n", output.image_id));

    if let Some(t) = tag {
        messages.push(format!("Successfully tagged {t}\n"));
    }

    messages
}

fn docker_temp_root() -> PathBuf {
    let home_dir = std::env::var_os("HOME").map(PathBuf::from);
    let override_dir = std::env::var_os("VISOR_TMPDIR").map(PathBuf::from);
    docker_temp_root_from_env(home_dir.as_deref(), override_dir.as_deref())
}

fn docker_temp_root_from_env(
    home_dir: Option<&std::path::Path>,
    override_dir: Option<&std::path::Path>,
) -> PathBuf {
    if let Some(override_dir) = override_dir {
        return override_dir.to_path_buf();
    }
    if let Some(home_dir) = home_dir {
        return home_dir.join(".visor").join("tmp");
    }
    std::env::temp_dir()
}

fn build_context_tempdir() -> Result<tempfile::TempDir, String> {
    let root = docker_temp_root();
    std::fs::create_dir_all(&root).map_err(|e| format!("mkdir {}: {e}", root.display()))?;
    tempfile::Builder::new()
        .prefix("visor-docker-build-")
        .tempdir_in(root)
        .map_err(|e| format!("tempdir: {e}"))
}

/// Extracts a tar archive from raw bytes into a temporary directory.
///
/// # Errors
///
/// Returns an error if the tar archive is malformed or extraction fails.
/// Unpacks a build-context tar.
///
/// Every entry goes through `unpack_in`, which is what makes directory and
/// symlink entries work: the docker CLI puts a `./` entry at the head of every
/// context it sends, and writing that as a regular file fails with EISDIR,
/// which took down every build from a stock client. It also refuses an entry
/// whose path escapes the destination, so a crafted context cannot write
/// outside the tempdir.
fn extract_build_context(data: &[u8]) -> Result<tempfile::TempDir, String> {
    let tmp = build_context_tempdir()?;
    let mut archive = tar::Archive::new(data);
    for entry_result in archive.entries().map_err(|e| format!("invalid tar: {e}"))? {
        let mut entry = entry_result.map_err(|e| format!("tar entry: {e}"))?;
        let path = entry
            .path()
            .map_err(|e| format!("tar path: {e}"))?
            .into_owned();
        if !entry
            .unpack_in(tmp.path())
            .map_err(|e| format!("unpack {}: {e}", path.display()))?
        {
            return Err(format!(
                "build context entry {} resolves outside the context directory",
                path.display()
            ));
        }
    }
    Ok(tmp)
}

/// Generates Docker-compatible progress messages from a parsed Dockerfile.
fn build_progress_messages(
    parsed: &visor_build::ParsedDockerfile,
    tag: Option<&str>,
    target: Option<&str>,
    quiet: bool,
) -> Vec<String> {
    let mut messages = Vec::new();

    // Collect all instructions across target stages
    let stages = filter_target_stages(&parsed.stages, target);
    let total_steps = count_steps(&stages);
    let mut step = 0;

    for stage in &stages {
        step += 1;
        let from_line = format_from(&stage.from);
        if !quiet {
            messages.push(format!("Step {step}/{total_steps} : {from_line}\n"));
        }

        for instr in &stage.instructions {
            step += 1;
            let instr_line = format_instruction(instr);
            if !quiet {
                messages.push(format!("Step {step}/{total_steps} : {instr_line}\n"));
            }

            // For RUN/COPY/ADD: note that execution is pending
            if !quiet && requires_execution(instr) {
                messages.push(
                    " ---> [build execution pending: vsock agent not yet available]\n".to_owned(),
                );
            }
        }
    }

    messages.push("Successfully built [pending]\n".to_owned());

    if let Some(t) = tag {
        messages.push(format!("Successfully tagged {t}\n"));
    }

    messages
}

/// Filters stages to only those up to and including the target stage.
fn filter_target_stages<'a>(
    stages: &'a [visor_build::Stage],
    target: Option<&str>,
) -> Vec<&'a visor_build::Stage> {
    match target {
        Some(name) => {
            let mut result = Vec::new();
            for stage in stages {
                result.push(stage);
                if stage.from.alias.as_deref() == Some(name) {
                    break;
                }
            }
            result
        }
        None => stages.iter().collect(),
    }
}

/// Counts total steps (FROM + instructions) across stages.
fn count_steps(stages: &[&visor_build::Stage]) -> usize {
    stages
        .iter()
        .map(|s| 1 + s.instructions.len()) // 1 for FROM
        .sum()
}

/// Formats a FROM instruction for the progress stream.
fn format_from(from: &visor_build::FromInstr) -> String {
    match &from.alias {
        Some(alias) => format!("FROM {} AS {alias}", from.image),
        None => format!("FROM {}", from.image),
    }
}

/// Formats a build instruction for the progress stream.
fn format_instruction(instr: &visor_build::BuildInstruction) -> String {
    match instr {
        visor_build::BuildInstruction::Run(r) => {
            format!("RUN {}", format_command_form(&r.command))
        }
        visor_build::BuildInstruction::Copy(c) => {
            format!("COPY {} {}", c.sources.join(" "), c.dest)
        }
        visor_build::BuildInstruction::Add(a) => {
            format!("ADD {} {}", a.sources.join(" "), a.dest)
        }
        visor_build::BuildInstruction::Cmd(c) => {
            format!("CMD {}", format_command_form(&c.command))
        }
        visor_build::BuildInstruction::Entrypoint(e) => {
            format!("ENTRYPOINT {}", format_command_form(&e.command))
        }
        visor_build::BuildInstruction::Env(e) => {
            let pairs: Vec<String> = e.vars.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("ENV {}", pairs.join(" "))
        }
        visor_build::BuildInstruction::Arg(a) => match &a.default_value {
            Some(val) => format!("ARG {}={val}", a.name),
            None => format!("ARG {}", a.name),
        },
        visor_build::BuildInstruction::Workdir(w) => format!("WORKDIR {}", w.path),
        visor_build::BuildInstruction::User(u) => match &u.group {
            Some(g) => format!("USER {}:{g}", u.user),
            None => format!("USER {}", u.user),
        },
        visor_build::BuildInstruction::Expose(e) => {
            let ports: Vec<String> = e
                .ports
                .iter()
                .map(|p| format!("{}/{}", p.port, p.protocol))
                .collect();
            format!("EXPOSE {}", ports.join(" "))
        }
        visor_build::BuildInstruction::Label(l) => {
            let pairs: Vec<String> = l.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("LABEL {}", pairs.join(" "))
        }
        visor_build::BuildInstruction::Shell(s) => {
            format!("SHELL {:?}", s.shell)
        }
        visor_build::BuildInstruction::Stopsignal(s) => {
            format!("STOPSIGNAL {}", s.signal)
        }
        visor_build::BuildInstruction::Healthcheck(h) => {
            if h.disable {
                "HEALTHCHECK NONE".to_owned()
            } else {
                "HEALTHCHECK CMD ...".to_owned()
            }
        }
        visor_build::BuildInstruction::Volume(v) => {
            format!("VOLUME {}", v.paths.join(" "))
        }
        _ => "[unknown instruction]".to_owned(),
    }
}

/// Formats a [`CommandForm`] for display.
fn format_command_form(form: &visor_build::CommandForm) -> String {
    match form {
        visor_build::CommandForm::Shell(s) => s.clone(),
        visor_build::CommandForm::Exec(parts) => {
            let inner: Vec<String> = parts.iter().map(|p| format!("\"{p}\"")).collect();
            format!("[{}]", inner.join(", "))
        }
        _ => "[unknown command form]".to_owned(),
    }
}

/// Returns `true` if an instruction requires VM execution.
fn requires_execution(instr: &visor_build::BuildInstruction) -> bool {
    matches!(
        instr,
        visor_build::BuildInstruction::Run(_)
            | visor_build::BuildInstruction::Copy(_)
            | visor_build::BuildInstruction::Add(_)
    )
}

/// Builds a Docker-compatible JSON stream response from progress messages.
fn build_stream_response(messages: Vec<String>) -> Response {
    use std::fmt::Write as _;

    let mut body = String::new();
    for msg in messages {
        let json = serde_json::json!({"stream": msg});
        let _ = write!(body, "{json}\r\n");
    }

    let mut resp = (StatusCode::OK, body).into_response();
    resp.headers_mut()
        .insert("Content-Type", HeaderValue::from_static("application/json"));
    resp
}

/// Builds a Docker error response for build failures.
fn build_error_response(message: &str) -> Response {
    let body = serde_json::json!({
        // Docker's HTTP error convention is `message`; without it the CLI
        // reports "provided no error-message" and the reason is lost. The
        // `error`/`errorDetail` pair is the in-stream shape, kept alongside
        // so clients reading either one still get the text.
        "message": message,
        "error": message,
        "errorDetail": { "message": message }
    });
    let mut resp = (StatusCode::BAD_REQUEST, body.to_string()).into_response();
    resp.headers_mut()
        .insert("Content-Type", HeaderValue::from_static("application/json"));
    resp
}

// ── Utilities ───────────────────────────────────────────────────────

/// Returns the Rust compiler version (best-effort).
fn rustc_version() -> &'static str {
    option_env!("RUSTC_VERSION").unwrap_or("unknown")
}
