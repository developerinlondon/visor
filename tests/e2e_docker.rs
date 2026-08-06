//! End-to-end Docker CLI smoke tests against the compatibility server.
//!
//! These tests boot the Docker HTTP shim with the real VM-backed build service,
//! then drive it through the stock Docker CLI to verify command compatibility
//! at the client boundary.

use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Context as _;
use axum::body::{Body, to_bytes};
use axum::extract::Request;
use axum::middleware::{self, Next};
use axum::response::Response;
use futures_util::FutureExt as _;
use serial_test::serial;
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::TcpListener;
use tokio::process::Command;
use tokio::sync::Notify;
use visor_build::ImageStore;
use visor_docker::docker_router_with_image_manager;
use visor_runtime::backend::{
    ExecRequest, ExecutionBackend, GuestNetworkLink, PortMapping, VmConfig, VmmBackend,
};
use visor_runtime::image_manager::RuntimeImageManager;
use visor_runtime::oci::layers::LayerMerger;
use visor_runtime::oci::registry::Manifest;
use visor_runtime::vsock::build_service::VmmBuildService;

fn workspace_tempdir() -> std::io::Result<tempfile::TempDir> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(".tmp")
        .join("docker-e2e-tests");
    std::fs::create_dir_all(&root)?;
    tempfile::Builder::new()
        .prefix("visor-docker-e2e-")
        .tempdir_in(root)
        .map_err(std::io::Error::from)
}

fn command_output(output: &std::process::Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    format!("stdout:\n{stdout}\n\nstderr:\n{stderr}")
}

fn docker_command(host: &str) -> Command {
    let mut command = Command::new("docker");
    command.env("DOCKER_HOST", host);
    command.env("DOCKER_BUILDKIT", "1");
    command
}

fn docker_compose_command(host: &str, project: &str, compose_file: &Path) -> Command {
    let mut command = docker_command(host);
    command.args([
        "compose",
        "-p",
        project,
        "-f",
        compose_file.to_str().expect("compose file should be utf-8"),
    ]);
    command
}

fn classic_docker_command(host: &str) -> Command {
    let mut command = Command::new("docker");
    command.env("DOCKER_HOST", host);
    command.env("DOCKER_BUILDKIT", "0");
    command
}

async fn log_requests(request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let uri = request.uri().clone();
    let is_streaming_response = uri.path().ends_with("/wait")
        || uri.path().ends_with("/attach")
        || uri.path().contains("/exec/") && uri.path().ends_with("/start");
    let should_log_headers = uri.path().ends_with("/images/load")
        || uri.path().contains("/exec/") && uri.path().ends_with("/start")
        || uri.path().contains("/containers/") && uri.path().ends_with("/exec");
    let should_log_body = uri.path().ends_with("/images/load")
        || uri.path().contains("/containers/") && uri.path().ends_with("/exec");
    let request = if should_log_headers {
        eprintln!("docker API request headers for {method} {uri}:");
        for (name, value) in request.headers() {
            eprintln!("  {}: {}", name, value.to_str().unwrap_or("<binary>"));
        }
        if uri.path().ends_with("/images/load") {
            let (parts, body) = request.into_parts();
            let body_bytes = to_bytes(body, usize::MAX)
                .await
                .expect("read logged request body");
            let capture_path = Path::new("/tmp/visor-buildx-load-capture.tar");
            std::fs::write(capture_path, &body_bytes)
                .unwrap_or_else(|error| panic!("write {capture_path:?}: {error}"));
            eprintln!(
                "captured {} bytes from {method} {uri} to {}",
                body_bytes.len(),
                capture_path.display(),
            );
            Request::from_parts(parts, Body::from(body_bytes))
        } else if should_log_body && method == axum::http::Method::POST {
            let (parts, body) = request.into_parts();
            let body_bytes = to_bytes(body, usize::MAX)
                .await
                .expect("read logged JSON request body");
            eprintln!(
                "docker API request body for {method} {uri}: {}",
                String::from_utf8_lossy(&body_bytes)
            );
            Request::from_parts(parts, Body::from(body_bytes))
        } else {
            request
        }
    } else {
        request
    };
    eprintln!("docker API request: {method} {uri}");
    let response = next.run(request).await;
    let status = response.status();
    if is_streaming_response {
        eprintln!("docker API response: {method} {uri} -> {status}");
        return response;
    }
    let (parts, body) = response.into_parts();
    let body_bytes = to_bytes(body, usize::MAX)
        .await
        .expect("read logged response body");
    if status.is_client_error() || status.is_server_error() {
        eprintln!(
            "docker API response body: {method} {uri} -> {}",
            String::from_utf8_lossy(&body_bytes)
        );
    }
    eprintln!("docker API response: {method} {uri} -> {status}");
    Response::from_parts(parts, Body::from(body_bytes))
}

async fn image_exists(host: &str, tag: &str) -> anyhow::Result<bool> {
    let output = docker_command(host)
        .args(["image", "inspect", tag])
        .output()
        .await
        .context("run docker image inspect")?;
    Ok(output.status.success())
}

async fn remove_image(host: &str, tag: &str) -> anyhow::Result<()> {
    let output = docker_command(host)
        .args(["image", "rm", "-f", tag])
        .output()
        .await
        .context("run docker image rm")?;
    if output.status.success() || !image_exists(host, tag).await? {
        return Ok(());
    }
    anyhow::bail!("docker image rm failed:\n{}", command_output(&output));
}

async fn docker_output(host: &str, args: &[&str]) -> String {
    match docker_command(host).args(args).output().await {
        Ok(output) => format!("status: {}\n{}", output.status, command_output(&output)),
        Err(error) => format!("stdout:\n\n\nstderr:\nfailed to run docker command: {error}"),
    }
}

async fn wait_for_logs(
    host: &str,
    container_name: &str,
    needle: &str,
) -> anyhow::Result<std::process::Output> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let output = docker_command(host)
            .args(["logs", container_name])
            .output()
            .await
            .context("run docker logs")?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        if output.status.success() && stdout.contains(needle) {
            return Ok(output);
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "docker logs for {container_name} did not contain {needle:?}:\n{}",
                command_output(&output)
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn buildkit_image_tag(store_dir: &Path) -> anyhow::Result<String> {
    for tag in [
        "docker.io/moby/buildkit:buildx-stable-1",
        "moby/buildkit:buildx-stable-1",
    ] {
        if ImageStore::new(store_dir.to_path_buf())
            .get_by_tag(tag)
            .with_context(|| format!("read image tag {tag}"))?
            .is_some()
        {
            return Ok(tag.to_owned());
        }
    }

    let tags_path = store_dir.join("tags.json");
    let tags_bytes = std::fs::read(&tags_path)
        .with_context(|| format!("read local tags {}", tags_path.display()))?;
    let tags: std::collections::HashMap<String, String> =
        serde_json::from_slice(&tags_bytes).context("parse local tags.json")?;
    tags.keys()
        .find(|tag| tag.contains("moby/buildkit"))
        .cloned()
        .context("find buildkit tag in local image store")
}

fn unpack_buildkit_rootfs(
    store_dir: &Path,
    rootfs_dir: &Path,
) -> anyhow::Result<(PathBuf, Manifest)> {
    let tag = buildkit_image_tag(store_dir)?;
    let store = ImageStore::new(store_dir.to_path_buf());
    let manifest_digest = store
        .get_by_tag(&tag)
        .with_context(|| format!("resolve buildkit tag {tag}"))?
        .with_context(|| format!("buildkit tag {tag} missing from image store"))?;
    let image_dir = store_dir.join(
        manifest_digest
            .strip_prefix("sha256:")
            .unwrap_or(&manifest_digest),
    );
    let manifest_path = image_dir.join("blobs").join("sha256").join(
        manifest_digest
            .strip_prefix("sha256:")
            .unwrap_or(&manifest_digest),
    );
    let manifest_bytes = std::fs::read(&manifest_path)
        .with_context(|| format!("read buildkit manifest {}", manifest_path.display()))?;
    let manifest: Manifest =
        serde_json::from_slice(&manifest_bytes).context("parse buildkit manifest")?;
    let merger = LayerMerger::new(rootfs_dir).context("create buildkit rootfs merger")?;
    for layer in &manifest.layers {
        let layer_path = image_dir.join("blobs").join("sha256").join(
            layer
                .digest
                .strip_prefix("sha256:")
                .unwrap_or(&layer.digest),
        );
        merger
            .unpack_layer(&layer_path)
            .with_context(|| format!("unpack buildkit layer {}", layer.digest))?;
    }
    Ok((image_dir, manifest))
}

async fn run_host_command_with_input(
    program: &Path,
    args: &[&str],
    input: &[u8],
) -> anyhow::Result<std::process::Output> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("spawn {}", program.display()))?;
    let mut stdin = child
        .stdin
        .take()
        .context("capture child stdin for host probe")?;
    stdin
        .write_all(input)
        .await
        .with_context(|| format!("write stdin for {}", program.display()))?;
    drop(stdin);
    child
        .wait_with_output()
        .await
        .with_context(|| format!("wait for {}", program.display()))
}

async fn host_buildctl_diagnostics(store_dir: &Path) -> String {
    let rootfs_dir = match workspace_tempdir() {
        Ok(dir) => dir,
        Err(error) => {
            return format!("failed to create host buildctl tempdir: {error:#}");
        }
    };
    let (image_dir, manifest) = match unpack_buildkit_rootfs(store_dir, rootfs_dir.path()) {
        Ok(value) => value,
        Err(error) => {
            return format!("failed to unpack buildkit image: {error:#}");
        }
    };
    let buildctl_path = [
        rootfs_dir.path().join("usr/bin/buildctl"),
        rootfs_dir.path().join("bin/buildctl"),
        rootfs_dir.path().join("usr/local/bin/buildctl"),
    ]
    .into_iter()
    .find(|path| path.exists());
    let Some(buildctl_path) = buildctl_path else {
        return format!(
            "buildkit image unpacked to {} but buildctl was not found; manifest has {} layers",
            image_dir.display(),
            manifest.layers.len()
        );
    };

    let version = run_host_command_with_input(&buildctl_path, &["--version"], b"").await;
    let single_byte_probe =
        run_host_command_with_input(&buildctl_path, &["dial-stdio"], b"x").await;
    let http2_preface = run_host_command_with_input(
        &buildctl_path,
        &["dial-stdio"],
        b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\0\0\0\x04\0\0\0\0\0",
    )
    .await;
    let binary_strings = std::process::Command::new("strings")
        .arg(&buildctl_path)
        .output()
        .with_context(|| format!("run strings on {}", buildctl_path.display()));

    let matching_strings = binary_strings
        .ok()
        .map(|output| {
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter(|line| {
                    line.contains("Unrecognized input header")
                        || line.contains("dial-stdio")
                        || line.contains("grpchijack")
                })
                .take(12)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_else(|| "failed to extract strings".to_owned());

    format!(
        "buildctl path: {}\nmanifest layers: {}\n\nhost buildctl --version:\n{}\n\nhost buildctl dial-stdio with 'x':\n{}\n\nhost buildctl dial-stdio with HTTP/2 preface:\n{}\n\nmatching buildctl strings:\n{}",
        buildctl_path.display(),
        manifest.layers.len(),
        version
            .map(|output| format!("status: {}\n{}", output.status, command_output(&output)))
            .unwrap_or_else(|error| format!("error: {error:#}")),
        single_byte_probe
            .map(|output| format!("status: {}\n{}", output.status, command_output(&output)))
            .unwrap_or_else(|error| format!("error: {error:#}")),
        http2_preface
            .map(|output| format!("status: {}\n{}", output.status, command_output(&output)))
            .unwrap_or_else(|error| format!("error: {error:#}")),
        matching_strings,
    )
}

fn write_build_context(context_dir: &Path) -> anyhow::Result<()> {
    let busybox_source = Path::new("/usr/bin/busybox");
    let busybox_dest = context_dir.join("busybox");
    std::fs::copy(busybox_source, &busybox_dest).context("copy busybox into build context")?;
    let mut permissions = std::fs::metadata(&busybox_dest)
        .context("read busybox metadata")?
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&busybox_dest, permissions).context("set busybox executable bit")?;
    std::fs::write(context_dir.join("hello.txt"), "buildx-load-ok\n").context("write hello.txt")?;
    std::fs::write(
        context_dir.join("Dockerfile"),
        "FROM scratch\nCOPY busybox /bin/busybox\nCOPY hello.txt /hello.txt\nCMD [\"/bin/busybox\", \"cat\", \"/hello.txt\"]\n",
    )
    .context("write Dockerfile")?;
    Ok(())
}

fn write_runtime_smoke_context(context_dir: &Path) -> anyhow::Result<()> {
    std::fs::write(
        context_dir.join("Dockerfile"),
        "FROM alpine:latest\nRUN printf '#!/bin/sh\\necho boot-log\\ntrap \"exit 0\" TERM INT\\nwhile true; do sleep 1; done\\n' >/usr/local/bin/loop.sh && chmod +x /usr/local/bin/loop.sh\nENTRYPOINT [\"/bin/sh\", \"/usr/local/bin/loop.sh\"]\n",
    )
    .context("write smoke Dockerfile")?;
    Ok(())
}

fn write_compose_fixture(project_dir: &Path, host_port: u16) -> anyhow::Result<PathBuf> {
    let compose_path = project_dir.join("compose.yaml");
    std::fs::write(
        &compose_path,
        format!(
            "services:\n  api:\n    image: nginx:alpine\n    ports:\n      - \"{host_port}:80\"\n  probe:\n    image: alpine:latest\n    depends_on:\n      - api\n    command:\n      - sh\n      - -lc\n      - \"trap 'exit 0' TERM INT; while true; do sleep 1; done\"\n",
        )
    )
    .with_context(|| format!("write compose fixture {}", compose_path.display()))?;
    Ok(compose_path)
}

fn write_multi_network_compose_fixture(project_dir: &Path) -> anyhow::Result<PathBuf> {
    let compose_path = project_dir.join("compose.yaml");
    std::fs::write(
        &compose_path,
        "services:\n  api:\n    image: nginx:alpine\n    networks:\n      - frontend\n  db:\n    image: nginx:alpine\n    networks:\n      - backend\n  frontend_probe:\n    image: alpine:latest\n    depends_on:\n      - api\n    networks:\n      - frontend\n    command:\n      - sh\n      - -lc\n      - \"trap 'exit 0' TERM INT; while true; do sleep 1; done\"\n  backend_probe:\n    image: alpine:latest\n    depends_on:\n      - db\n    networks:\n      - backend\n    command:\n      - sh\n      - -lc\n      - \"trap 'exit 0' TERM INT; while true; do sleep 1; done\"\n  bridge:\n    image: alpine:latest\n    depends_on:\n      - api\n      - db\n    networks:\n      - frontend\n      - backend\n    command:\n      - sh\n      - -lc\n      - \"trap 'exit 0' TERM INT; while true; do sleep 1; done\"\nnetworks:\n  frontend: {}\n  backend: {}\n",
    )
    .with_context(|| format!("write multi-network compose fixture {}", compose_path.display()))?;
    Ok(compose_path)
}

async fn spawn_docker_server(
    image_store_dir: PathBuf,
) -> anyhow::Result<(
    String,
    Arc<Notify>,
    tokio::task::JoinHandle<anyhow::Result<()>>,
    Arc<dyn ExecutionBackend>,
)> {
    let backend: Arc<dyn ExecutionBackend> =
        Arc::new(VmmBackend::with_image_store_path(image_store_dir.clone()));
    let image_store = Arc::new(ImageStore::new(image_store_dir.clone()));
    let image_manager = Arc::new(RuntimeImageManager::new(image_store_dir.clone()));
    let build_service = Arc::new(VmmBuildService::new(Arc::clone(&backend), image_store_dir));
    let app = docker_router_with_image_manager(
        Arc::clone(&backend),
        Some(build_service),
        Some(image_store),
        Some(image_manager),
    )
    .layer(middleware::from_fn(log_requests));

    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .context("bind docker compatibility listener")?;
    let addr = listener
        .local_addr()
        .context("read docker compatibility listener addr")?;
    let host = format!("tcp://{addr}");

    let shutdown = Arc::new(Notify::new());
    let shutdown_signal = Arc::clone(&shutdown);
    let task = tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                shutdown_signal.notified().await;
            })
            .await
            .context("serve docker compatibility app")
    });

    Ok((host, shutdown, task, backend))
}

#[derive(Clone)]
struct DockerServerContext {
    host: String,
    backend: Arc<dyn ExecutionBackend>,
}

async fn cleanup_backend_vms(backend: &Arc<dyn ExecutionBackend>) -> anyhow::Result<()> {
    let mut errors = Vec::new();
    let vms = backend
        .list()
        .await
        .context("list backend VMs for cleanup")?;
    for vm in vms {
        if let Err(error) = backend.destroy(&vm.id).await {
            errors.push(format!("destroy {}: {error:#}", vm.id));
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    anyhow::bail!("backend VM cleanup failed:\n{}", errors.join("\n"));
}

async fn shutdown_docker_server(
    backend: &Arc<dyn ExecutionBackend>,
    shutdown: Arc<Notify>,
    server_task: tokio::task::JoinHandle<anyhow::Result<()>>,
) -> anyhow::Result<()> {
    let mut errors = Vec::new();

    if let Err(error) = cleanup_backend_vms(backend).await {
        errors.push(format!("{error:#}"));
    }

    shutdown.notify_one();
    match server_task.await {
        Ok(Ok(())) => {}
        Ok(Err(error)) => {
            errors.push(format!("docker compatibility server error: {error:#}"));
        }
        Err(error) => {
            errors.push(format!("join docker compatibility server: {error}"));
        }
    }

    if errors.is_empty() {
        return Ok(());
    }

    anyhow::bail!("{}", errors.join("\n"));
}

async fn wait_for_docker_host(host: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let output = docker_command(host)
            .args(["version", "--format", "{{.Server.APIVersion}}"])
            .output()
            .await
            .context("run docker version")?;
        if output.status.success() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            anyhow::bail!(
                "docker host {host} did not become ready:\n{}",
                command_output(&output)
            );
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn with_docker_server<F, Fut>(image_store_dir: PathBuf, test: F) -> anyhow::Result<()>
where
    F: FnOnce(DockerServerContext) -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<()>>,
{
    let (host, shutdown, server_task, backend) = spawn_docker_server(image_store_dir)
        .await
        .context("spawn docker compatibility server")?;

    if let Err(error) = wait_for_docker_host(&host).await {
        let cleanup_result = shutdown_docker_server(&backend, shutdown, server_task).await;
        if let Err(cleanup_error) = cleanup_result {
            return Err(anyhow::anyhow!(
                "docker host did not become ready: {error:#}\ncleanup error:\n{cleanup_error:#}"
            ));
        }
        return Err(error.context("docker host did not become ready"));
    }

    let context = DockerServerContext {
        host,
        backend: Arc::clone(&backend),
    };
    let test_result = std::panic::AssertUnwindSafe(test(context))
        .catch_unwind()
        .await;
    let cleanup_result = shutdown_docker_server(&backend, shutdown, server_task).await;

    match test_result {
        Ok(Ok(())) => cleanup_result,
        Ok(Err(error)) => {
            if let Err(cleanup_error) = cleanup_result {
                return Err(anyhow::anyhow!(
                    "test body failed: {error:#}\ncleanup error:\n{cleanup_error:#}"
                ));
            }
            Err(error)
        }
        Err(panic) => {
            if let Err(cleanup_error) = cleanup_result {
                eprintln!("docker server cleanup after panic failed: {cleanup_error:#}");
            }
            std::panic::resume_unwind(panic);
        }
    }
}

fn reserve_local_port() -> anyhow::Result<u16> {
    let listener =
        std::net::TcpListener::bind(("127.0.0.1", 0)).context("bind ephemeral local port")?;
    let port = listener
        .local_addr()
        .context("read ephemeral local port")?
        .port();
    Ok(port)
}

const HTTP_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const HTTP_READ_IDLE_TIMEOUT: Duration = Duration::from_secs(1);
const HTTP_RESPONSE_TIMEOUT: Duration = Duration::from_secs(5);

fn http_headers_end(response: &[u8]) -> Option<usize> {
    response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
}

fn http_response_complete(response: &[u8]) -> bool {
    let Some(headers_end) = http_headers_end(response) else {
        return false;
    };

    let headers = String::from_utf8_lossy(&response[..headers_end]);
    let content_length = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.trim()
            .eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });

    match content_length {
        Some(length) => response.len() >= headers_end + length,
        None => false,
    }
}

async fn read_http_response(
    stream: &mut tokio::net::TcpStream,
    peer: &str,
    deadline: Instant,
) -> anyhow::Result<String> {
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];

    loop {
        let now = Instant::now();
        if now >= deadline {
            if response.is_empty() {
                anyhow::bail!("timed out waiting for HTTP response from {peer}");
            }
            break;
        }

        let remaining = deadline.saturating_duration_since(now);
        let read_timeout = if response.is_empty() {
            remaining.min(HTTP_CONNECT_TIMEOUT)
        } else {
            remaining.min(HTTP_READ_IDLE_TIMEOUT)
        };

        match tokio::time::timeout(read_timeout, stream.read(&mut buf)).await {
            Ok(Ok(0)) => break,
            Ok(Ok(bytes)) => {
                response.extend_from_slice(&buf[..bytes]);
                if http_response_complete(&response) {
                    break;
                }
            }
            Ok(Err(error)) if response.is_empty() => {
                return Err(error).with_context(|| format!("read HTTP response from {peer}"));
            }
            Ok(Err(_)) if !response.is_empty() => break,
            Ok(Err(error)) => {
                return Err(error).with_context(|| format!("read HTTP response from {peer}"));
            }
            Err(_) if response.is_empty() => continue,
            Err(_) => {
                if http_response_complete(&response) {
                    break;
                }
                continue;
            }
        }
    }

    Ok(String::from_utf8_lossy(&response).into_owned())
}

async fn http_get(port: u16) -> anyhow::Result<String> {
    let peer = format!("localhost:{port}");
    let mut stream = tokio::time::timeout(
        HTTP_CONNECT_TIMEOUT,
        tokio::net::TcpStream::connect(("127.0.0.1", port)),
    )
    .await
    .with_context(|| format!("connect to {peer}"))?
        .with_context(|| format!("connect to localhost:{port}"))?;
    tokio::time::timeout(
        HTTP_CONNECT_TIMEOUT,
        stream.write_all(b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n"),
    )
    .await
    .with_context(|| format!("write HTTP request to {peer}"))?
    .with_context(|| format!("write HTTP request to {peer}"))?;
    read_http_response(
        &mut stream,
        &peer,
        Instant::now() + HTTP_RESPONSE_TIMEOUT,
    )
    .await
}

async fn wait_for_http_body(port: u16, needle: &str) -> anyhow::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match http_get(port).await {
            Ok(response) if response.contains(needle) => return Ok(response),
            Ok(_) | Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Ok(response) => {
                anyhow::bail!(
                    "HTTP response from localhost:{port} did not contain {needle:?}:\n{response}"
                );
            }
            Err(error) => {
                anyhow::bail!(
                    "HTTP endpoint localhost:{port} did not become ready for {needle:?}: {error:#}"
                );
            }
        }
    }
}

async fn wait_for_http_unreachable(port: u16) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        match http_get(port).await {
            Err(_) => return Ok(()),
            Ok(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Ok(response) => {
                anyhow::bail!(
                    "HTTP endpoint localhost:{port} remained reachable after stop:\n{response}"
                );
            }
        }
    }
}

async fn wait_for_http_response(
    host_ip: std::net::Ipv4Addr,
    host_port: u16,
    timeout: Duration,
) -> anyhow::Result<String> {
    let deadline = Instant::now() + timeout;
    let addr = std::net::SocketAddrV4::new(host_ip, host_port);
    let peer = format!("{host_ip}:{host_port}");

    loop {
        match tokio::time::timeout(HTTP_CONNECT_TIMEOUT, tokio::net::TcpStream::connect(addr)).await
        {
            Ok(Ok(mut stream)) => {
                tokio::time::timeout(
                    HTTP_CONNECT_TIMEOUT,
                    stream.write_all(
                        b"GET / HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n",
                    ),
                )
                .await
                .with_context(|| format!("write HTTP request to {peer}"))?
                .with_context(|| format!("write HTTP request to {peer}"))?;
                return read_http_response(&mut stream, &peer, deadline).await;
            }
            Ok(Err(_)) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Err(_) if Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(250)).await;
            }
            Ok(Err(error)) => return Err(error).map_err(Into::into),
            Err(error) => {
                return Err(error).with_context(|| format!("connect to {peer}"));
            }
        }
    }
}

#[tokio::test]
async fn http_get_returns_partial_response_when_peer_keeps_connection_open() {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind local HTTP test listener");
    let port = listener
        .local_addr()
        .expect("read local HTTP test listener address")
        .port();

    tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept HTTP test client");
        let mut request = [0u8; 512];
        let _ = stream
            .read(&mut request)
            .await
            .expect("read HTTP test request");
        stream
            .write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 17\r\nConnection: keep-alive\r\n\r\nWelcome to nginx!",
            )
            .await
            .expect("write HTTP test response");
        tokio::time::sleep(Duration::from_secs(30)).await;
    });

    let response = wait_for_http_body(port, "Welcome to nginx!")
        .await
        .expect("HTTP helper should not require EOF when body is already present");
    assert!(
        response.contains("Welcome to nginx!"),
        "response should contain the expected body:\n{response}"
    );
}

async fn backend_exec_output(
    backend: &Arc<dyn ExecutionBackend>,
    vm_id: &str,
    cmd: Vec<String>,
) -> String {
    let result = tokio::time::timeout(
        Duration::from_secs(20),
        backend.exec(vm_id, ExecRequest::new(cmd)),
    )
    .await;
    match result {
        Ok(Ok(result)) => format!(
            "exit_code: {}\nstdout:\n{}\n\nstderr:\n{}",
            result.exit_code, result.stdout, result.stderr
        ),
        Ok(Err(error)) => format!("backend exec error: {error:#}"),
        Err(_) => "backend exec timed out after 20s".to_owned(),
    }
}

async fn backend_exec_stream_probe(
    backend: &Arc<dyn ExecutionBackend>,
    vm_id: &str,
    cmd: Vec<String>,
) -> String {
    let mut stream = match backend.exec_stream(vm_id, ExecRequest::new(cmd)).await {
        Ok(stream) => stream,
        Err(error) => return format!("backend exec stream error: {error:#}"),
    };

    let client_preface = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n\0\0\0\x04\0\0\0\0\0";
    if let Err(error) = stream.write_all(client_preface).await {
        return format!("write client preface failed: {error}");
    }
    if let Err(error) = stream.flush().await {
        return format!("flush client preface failed: {error}");
    }

    let mut first_read = [0u8; 64];
    let first_bytes =
        match tokio::time::timeout(Duration::from_secs(1), stream.read(&mut first_read)).await {
            Ok(Ok(bytes)) => bytes,
            Ok(Err(error)) => return format!("read server preface failed: {error}"),
            Err(_) => return "timed out waiting for server preface".to_owned(),
        };

    let mut second_read = [0u8; 64];
    let follow_up =
        match tokio::time::timeout(Duration::from_millis(500), stream.read(&mut second_read)).await
        {
            Ok(Ok(bytes)) => format!(
                "read returned {bytes} bytes: {:02x?}",
                &second_read[..bytes.min(32)]
            ),
            Ok(Err(error)) => format!("read errored: {error}"),
            Err(_) => "stream stayed open (timeout waiting for more data)".to_owned(),
        };

    format!(
        "first read: {first_bytes} bytes {:02x?}\nfollow-up: {follow_up}",
        &first_read[..first_bytes.min(32)]
    )
}

#[cfg(target_os = "linux")]
fn has_kvm() -> bool {
    Path::new("/dev/kvm").exists()
}

#[cfg(target_os = "linux")]
async fn docker_cli_available() -> bool {
    let docker_ok = Command::new("docker")
        .arg("--version")
        .status()
        .await
        .is_ok_and(|status| status.success());
    docker_ok
}

#[cfg(target_os = "linux")]
async fn docker_compose_available() -> bool {
    let docker_ok = docker_cli_available().await;
    let compose_ok = Command::new("docker")
        .args(["compose", "version"])
        .status()
        .await
        .is_ok_and(|status| status.success());
    docker_ok && compose_ok
}

#[cfg(target_os = "linux")]
async fn script_available() -> bool {
    Command::new("script")
        .arg("--version")
        .status()
        .await
        .is_ok_and(|status| status.success())
}

#[cfg(target_os = "linux")]
async fn docker_and_buildx_available() -> bool {
    let docker_ok = docker_cli_available().await;
    let buildx_ok = Command::new("docker")
        .args(["buildx", "version"])
        .status()
        .await
        .is_ok_and(|status| status.success());
    docker_ok && buildx_ok
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn vmm_backend_detached_nginx_port_mapping_reaches_host() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    let host_port = reserve_local_port().expect("reserve host port");
    let backend: Arc<dyn ExecutionBackend> = Arc::new(VmmBackend::with_image_store_path(
        image_store_dir.path().to_path_buf(),
    ));

    let mut config = VmConfig::new("nginx:alpine");
    config.detach = true;
    config.name = Some("direct-nginx".to_owned());
    config.ports = vec![PortMapping::new(host_port, 80)];

    let vm = backend
        .create(config)
        .await
        .expect("create detached nginx VM with host port");
    let vm_info = backend
        .get(&vm.id)
        .await
        .expect("read detached nginx VM state");
    let guest_link = GuestNetworkLink::for_cid(vm_info.cid.expect("running VM should expose CID"));

    let host_http = wait_for_http_body(host_port, "Welcome to nginx!").await;
    let direct_guest_http =
        wait_for_http_response(guest_link.guest_ip, 80, Duration::from_secs(5)).await;
    let guest_http = backend_exec_output(
        &backend,
        &vm.id,
        vec![
            "sh".to_owned(),
            "-lc".to_owned(),
            "wget -qO- http://127.0.0.1:80 >/tmp/index.html && grep -q 'Welcome to nginx!' /tmp/index.html && printf guest-http-ok"
                .to_owned(),
        ],
    )
    .await;
    let guest_network = backend_exec_output(
        &backend,
        &vm.id,
        vec![
            "sh".to_owned(),
            "-lc".to_owned(),
            "ifconfig -a 2>/dev/null || true; printf '\\n---\\n'; cat /proc/net/route 2>/dev/null || true"
                .to_owned(),
        ],
    )
    .await;
    let serial_output = backend
        .console_output(&vm.id)
        .await
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_else(|error| format!("failed to read serial output: {error:#}"));

    let stop_result = backend.stop(&vm.id, 5).await;
    if stop_result.is_err() {
        let _ = backend.kill(&vm.id).await;
    }

    assert!(
        host_http
            .as_ref()
            .is_ok_and(|response| response.contains("Welcome to nginx!")),
        "direct VmmBackend port mapping should serve nginx on localhost:{host_port}:\nresult: {host_http:#?}\ndirect guest probe: {direct_guest_http:#?}\nguest exec:\n{guest_http}\nguest network:\n{guest_network}\nserial output:\n{serial_output}\nstop result: {stop_result:#?}"
    );
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn docker_run_with_published_port_reaches_host() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }
    if !docker_cli_available().await {
        eprintln!("skipping: docker CLI not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    let host_port = reserve_local_port().expect("reserve host port");
    let container_name = format!("visor-docker-nginx-port-{}", std::process::id());
    with_docker_server(
        image_store_dir.path().to_path_buf(),
        move |context| async move {
            let DockerServerContext { host, backend } = context;

            let run_output = docker_command(&host)
                .args([
                    "run",
                    "-d",
                    "--name",
                    &container_name,
                    "-p",
                    &format!("{host_port}:80"),
                    "nginx:alpine",
                ])
                .output()
                .await
                .expect("run nginx container with published port");
            let container_id = String::from_utf8_lossy(&run_output.stdout)
                .trim()
                .to_owned();
            let inspect_output = docker_command(&host)
                .args(["inspect", &container_name])
                .output()
                .await
                .expect("inspect nginx container");
            let host_http = wait_for_http_body(host_port, "Welcome to nginx!").await;
            let guest_http = if container_id.is_empty() {
                "container id missing from docker run output".to_owned()
            } else {
                backend_exec_output(
                    &backend,
                    &container_id,
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "wget -qO- http://127.0.0.1:80 >/tmp/index.html && grep -q 'Welcome to nginx!' /tmp/index.html && printf guest-http-ok"
                            .to_owned(),
                    ],
                )
                .await
            };
            let guest_network = if container_id.is_empty() {
                "container id missing from docker run output".to_owned()
            } else {
                backend_exec_output(
                    &backend,
                    &container_id,
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "ifconfig -a 2>/dev/null || true; printf '\\n---\\n'; cat /proc/net/route 2>/dev/null || true"
                            .to_owned(),
                    ],
                )
                .await
            };
            let rm_output = docker_command(&host)
                .args(["rm", "-f", &container_name])
                .output()
                .await
                .expect("remove nginx container");

            assert!(
                run_output.status.success(),
                "docker run with published port should succeed:\n{}",
                command_output(&run_output)
            );
            assert!(
                host_http
                    .as_ref()
                    .is_ok_and(|response| response.contains("Welcome to nginx!")),
                "docker published port should serve nginx:\nresult: {host_http:#?}\ninspect:\n{}\nguest exec:\n{}\nguest network:\n{}",
                command_output(&inspect_output),
                guest_http,
                guest_network
            );
            assert!(
                rm_output.status.success(),
                "docker rm should succeed for published-port nginx container:\n{}",
                command_output(&rm_output)
            );

            Ok(())
        },
    )
    .await
    .expect("docker published-port e2e should complete");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn docker_smoke_matrix_covers_run_exec_logs_stop_rm_and_build() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }
    if !docker_cli_available().await {
        eprintln!("skipping: docker CLI not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    let context_dir = workspace_tempdir().expect("create docker smoke context tempdir");
    write_runtime_smoke_context(context_dir.path()).expect("write docker smoke context");

    let tag = format!("visor-docker-smoke:test-{}", std::process::id());
    let container_name = format!("visor-docker-smoke-{}", std::process::id());
    with_docker_server(
        image_store_dir.path().to_path_buf(),
        move |context| async move {
            let DockerServerContext { host, .. } = context;

            let build_output = classic_docker_command(&host)
                .args([
                    "build",
                    "-t",
                    &tag,
                    context_dir
                        .path()
                        .to_str()
                        .expect("context dir should be utf-8"),
                ])
                .output()
                .await
                .expect("run docker build");

            let run_output = docker_command(&host)
                .args(["run", "-d", "--name", &container_name, &tag])
                .output()
                .await
                .expect("run smoke container");

            let logs_output = wait_for_logs(&host, &container_name, "boot-log")
                .await
                .expect("wait for boot-log from docker logs");

            let exec_output = docker_command(&host)
                .args(["exec", &container_name, "sh", "-lc", "printf exec-ok"])
                .output()
                .await
                .expect("run docker exec");

            let stop_output = docker_command(&host)
                .args(["stop", &container_name])
                .output()
                .await
                .expect("run docker stop");

            let rm_output = docker_command(&host)
                .args(["rm", &container_name])
                .output()
                .await
                .expect("run docker rm");

            remove_image(&host, &tag)
                .await
                .expect("cleanup smoke image");

            assert!(
                build_output.status.success(),
                "docker build should succeed:\n{}",
                command_output(&build_output)
            );
            assert!(
                run_output.status.success(),
                "docker run should succeed:\n{}",
                command_output(&run_output)
            );
            assert!(
                exec_output.status.success(),
                "docker exec should succeed:\n{}",
                command_output(&exec_output)
            );
            assert_eq!(
                String::from_utf8_lossy(&exec_output.stdout),
                "exec-ok",
                "docker exec should return command output:\n{}",
                command_output(&exec_output)
            );
            assert!(
                logs_output.status.success(),
                "docker logs should succeed:\n{}",
                command_output(&logs_output)
            );
            assert!(
                String::from_utf8_lossy(&logs_output.stdout).contains("boot-log"),
                "docker logs should include container stdout:\n{}",
                command_output(&logs_output)
            );
            assert!(
                stop_output.status.success(),
                "docker stop should succeed:\n{}",
                command_output(&stop_output)
            );
            assert!(
                rm_output.status.success(),
                "docker rm should succeed:\n{}",
                command_output(&rm_output)
            );

            Ok(())
        },
    )
    .await
    .expect("docker smoke matrix should complete");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn docker_exec_i_returns_stdout_without_tty() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }
    if !docker_cli_available().await {
        eprintln!("skipping: docker CLI not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    let container_name = format!("visor-exec-stdin-{}", std::process::id());
    with_docker_server(
        image_store_dir.path().to_path_buf(),
        move |context| async move {
            let DockerServerContext { host, .. } = context;

            let run_output = docker_command(&host)
                .args([
                    "run",
                    "-d",
                    "--name",
                    &container_name,
                    "alpine:latest",
                    "sleep",
                    "300",
                ])
                .output()
                .await
                .expect("start stdin-attached exec test container");
            assert!(
                run_output.status.success(),
                "docker run should succeed for stdin-attached exec test:\n{}",
                command_output(&run_output)
            );

            let exec_output = docker_command(&host)
                .args([
                    "exec",
                    "-i",
                    &container_name,
                    "sh",
                    "-lc",
                    "printf stdin-ok",
                ])
                .output()
                .await
                .expect("run docker exec -i");

            let cleanup_output = docker_command(&host)
                .args(["rm", "-f", &container_name])
                .output()
                .await
                .expect("cleanup stdin-attached exec test container");

            assert!(
                cleanup_output.status.success(),
                "docker rm -f should succeed for stdin-attached exec test container:\n{}",
                command_output(&cleanup_output)
            );
            assert!(
                exec_output.status.success(),
                "docker exec -i should succeed:\n{}",
                command_output(&exec_output)
            );
            assert_eq!(
                String::from_utf8_lossy(&exec_output.stdout),
                "stdin-ok",
                "docker exec -i should return stdout:\n{}",
                command_output(&exec_output)
            );

            Ok(())
        },
    )
    .await
    .expect("docker exec -i e2e should complete");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn docker_exec_it_allocates_guest_tty() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }
    if !docker_cli_available().await {
        eprintln!("skipping: docker CLI not available");
        return;
    }
    if !script_available().await {
        eprintln!("skipping: script(1) not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    let container_name = format!("visor-exec-tty-{}", std::process::id());
    with_docker_server(
        image_store_dir.path().to_path_buf(),
        move |context| async move {
            let DockerServerContext { host, .. } = context;

            let run_output = docker_command(&host)
                .args([
                    "run",
                    "-d",
                    "--name",
                    &container_name,
                    "alpine:latest",
                    "sleep",
                    "300",
                ])
                .output()
                .await
                .expect("start tty test container");
            assert!(
                run_output.status.success(),
                "docker run should succeed for tty test:\n{}",
                command_output(&run_output)
            );

            let script_dir = workspace_tempdir().expect("create script transcript tempdir");
            let transcript_path = script_dir.path().join("docker-exec-it.log");
            let command = format!(
                "env DOCKER_HOST={host} docker exec -it {container_name} sh -lc 'tty >/dev/null && printf tty-ok'"
            );
            let tty_output = Command::new("script")
                .args([
                    "-qfec",
                    &command,
                    transcript_path
                        .to_str()
                        .expect("transcript path should be utf-8"),
                ])
                .output()
                .await
                .expect("run docker exec -it inside PTY");
            let transcript = std::fs::read_to_string(&transcript_path)
                .expect("read script transcript")
                .replace('\r', "");

            let cleanup_output = docker_command(&host)
                .args(["rm", "-f", &container_name])
                .output()
                .await
                .expect("cleanup tty test container");

            assert!(
                cleanup_output.status.success(),
                "docker rm -f should succeed for tty test container:\n{}",
                command_output(&cleanup_output)
            );
            assert!(
                tty_output.status.success(),
                "docker exec -it should succeed inside PTY:\n{}\ntranscript:\n{}",
                command_output(&tty_output),
                transcript
            );
            assert!(
                transcript.contains("tty-ok"),
                "docker exec -it should observe a guest tty:\n{}\ntranscript:\n{}",
                command_output(&tty_output),
                transcript
            );

            Ok(())
        },
    )
    .await
    .expect("docker exec -it e2e should complete");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn docker_pull_then_run_uses_cached_local_image() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }
    if !docker_cli_available().await {
        eprintln!("skipping: docker CLI not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    with_docker_server(
        image_store_dir.path().to_path_buf(),
        move |context| async move {
            let DockerServerContext { host, .. } = context;

            let pull_output = docker_command(&host)
                .args(["pull", "alpine:latest"])
                .output()
                .await
                .expect("run docker pull");
            let run_output = docker_command(&host)
                .args([
                    "run",
                    "--rm",
                    "alpine:latest",
                    "sh",
                    "-lc",
                    "printf pulled-ok",
                ])
                .output()
                .await
                .expect("run pulled alpine image");

            let _ = remove_image(&host, "alpine:latest").await;

            assert!(
                pull_output.status.success(),
                "docker pull should succeed:\n{}",
                command_output(&pull_output)
            );
            assert!(
                run_output.status.success(),
                "docker run after pull should succeed:\n{}",
                command_output(&run_output)
            );
            assert_eq!(
                String::from_utf8_lossy(&run_output.stdout),
                "pulled-ok\n",
                "pulled image should boot and run normally:\n{}",
                command_output(&run_output)
            );

            Ok(())
        },
    )
    .await
    .expect("docker pull/run e2e should complete");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn docker_compose_projects_are_isolated_and_reachable() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }
    if !docker_compose_available().await {
        eprintln!("skipping: docker compose plugin not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    let alpha_dir = workspace_tempdir().expect("create alpha compose tempdir");
    let beta_dir = workspace_tempdir().expect("create beta compose tempdir");
    let alpha_port = reserve_local_port().expect("reserve alpha host port");
    let beta_port = reserve_local_port().expect("reserve beta host port");
    let alpha_compose =
        write_compose_fixture(alpha_dir.path(), alpha_port).expect("write alpha compose fixture");
    let beta_compose =
        write_compose_fixture(beta_dir.path(), beta_port).expect("write beta compose fixture");
    with_docker_server(
        image_store_dir.path().to_path_buf(),
        move |context| async move {
            let DockerServerContext { host, backend } = context;

            let alpha_up = docker_compose_command(&host, "alpha", &alpha_compose)
                .args(["up", "-d"])
                .output()
                .await
                .expect("run docker compose up for alpha");
            let beta_up = docker_compose_command(&host, "beta", &beta_compose)
                .args(["up", "-d"])
                .output()
                .await
                .expect("run docker compose up for beta");
            let alpha_ps = docker_compose_command(&host, "alpha", &alpha_compose)
                .arg("ps")
                .output()
                .await
                .expect("run docker compose ps for alpha");
            let beta_ps = docker_compose_command(&host, "beta", &beta_compose)
                .arg("ps")
                .output()
                .await
                .expect("run docker compose ps for beta");

            let alpha_exec = docker_compose_command(&host, "alpha", &alpha_compose)
                .args([
                    "exec",
                    "-T",
                    "probe",
                    "sh",
                    "-lc",
                    "wget -T 2 -qO- http://api:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && wget -T 2 -qO- http://api.alpha:80 >/tmp/api-alpha.html && grep -q 'Welcome to nginx!' /tmp/api-alpha.html && printf resolve-ok",
                ])
                .output()
                .await
                .expect("run docker compose exec for alpha probe");
            let alpha_cross_project = docker_compose_command(&host, "alpha", &alpha_compose)
                .args([
                    "exec",
                    "-T",
                    "probe",
                    "sh",
                    "-lc",
                    "wget -T 2 -qO- http://api.beta:80",
                ])
                .output()
                .await
                .expect("run docker compose exec cross-project probe");

            let alpha_http = wait_for_http_body(alpha_port, "Welcome to nginx!").await;
            let beta_http = wait_for_http_body(beta_port, "Welcome to nginx!").await;
            let alpha_direct_guest_http = wait_for_http_response(
                GuestNetworkLink::for_cid(3).guest_ip,
                80,
                Duration::from_secs(5),
            )
            .await;
            let alpha_probe_hosts = backend_exec_output(
                &backend,
                "alpha-probe-1",
                vec![
                    "sh".to_owned(),
                    "-lc".to_owned(),
                    "cat /etc/hosts 2>/dev/null; printf '\\n---\\n'; cat /etc/resolv.conf 2>/dev/null; printf '\\n---\\n'; ifconfig -a 2>/dev/null || true; printf '\\n---\\n'; cat /proc/net/route 2>/dev/null || true"
                        .to_owned(),
                ],
            )
            .await;
            let alpha_bare_service = backend_exec_output(
                &backend,
                "alpha-probe-1",
                vec![
                    "sh".to_owned(),
                    "-lc".to_owned(),
                    "wget -T 2 -qO- http://api:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && printf bare-ok"
                        .to_owned(),
                ],
            )
            .await;
            let alpha_qualified_service = backend_exec_output(
                &backend,
                "alpha-probe-1",
                vec![
                    "sh".to_owned(),
                    "-lc".to_owned(),
                    "wget -T 2 -qO- http://api.alpha:80 >/tmp/api-alpha.html && grep -q 'Welcome to nginx!' /tmp/api-alpha.html && printf qualified-ok"
                        .to_owned(),
                ],
            )
            .await;

            assert!(
                alpha_up.status.success(),
                "docker compose up should succeed for alpha:\n{}",
                command_output(&alpha_up)
            );
            assert!(
                beta_up.status.success(),
                "docker compose up should succeed for beta:\n{}",
                command_output(&beta_up)
            );
            assert!(
                alpha_ps.status.success(),
                "docker compose ps should succeed for alpha:\n{}",
                command_output(&alpha_ps)
            );
            assert!(
                beta_ps.status.success(),
                "docker compose ps should succeed for beta:\n{}",
                command_output(&beta_ps)
            );
            assert!(
                String::from_utf8_lossy(&alpha_ps.stdout).contains("alpha-api-1"),
                "alpha compose ps should list the project-scoped api service:\n{}",
                command_output(&alpha_ps)
            );
            assert!(
                String::from_utf8_lossy(&beta_ps.stdout).contains("beta-api-1"),
                "beta compose ps should list the project-scoped api service:\n{}",
                command_output(&beta_ps)
            );
            if !alpha_http
                .as_ref()
                .is_ok_and(|response| response.contains("Welcome to nginx!"))
            {
                let alpha_guest_http = backend_exec_output(
                    &backend,
                    "alpha-api-1",
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "wget -qO- http://127.0.0.1:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && printf guest-http-ok"
                            .to_owned(),
                    ],
                )
                .await;
                let alpha_guest_network = backend_exec_output(
                    &backend,
                    "alpha-api-1",
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "ifconfig -a 2>/dev/null || true; printf '\\n---\\n'; cat /proc/net/route 2>/dev/null || true"
                            .to_owned(),
                    ],
                )
                .await;
                let alpha_peer_http = backend_exec_output(
                    &backend,
                    "alpha-probe-1",
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "wget -qO- http://api:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && printf peer-http-ok"
                            .to_owned(),
                    ],
                )
                .await;
                let alpha_inspect = docker_command(&host)
                    .args(["inspect", "alpha-api-1"])
                    .output()
                    .await
                    .expect("inspect alpha api container");
                let alpha_vm = backend
                    .get("alpha-api-1")
                    .await
                    .expect("alpha api VM should still be present while diagnostics run");
                let host_iptables = tokio::process::Command::new("iptables-save")
                    .output()
                    .await
                    .expect("capture host iptables after alpha host port failure");
                let host_routes = tokio::process::Command::new("ip")
                    .args(["-4", "route", "show"])
                    .output()
                    .await
                    .expect("capture host IPv4 routes after alpha host port failure");
                panic!(
                    "alpha host port should serve the api HTTP endpoint:\nresult: {alpha_http:#?}\nalpha vm:\n{alpha_vm:#?}\nalpha inspect:\n{}\nalpha guest exec:\n{}\nalpha guest network:\n{}\nalpha peer exec:\n{}\nalpha compose exec:\n{}\nhost iptables:\n{}\nhost routes:\n{}",
                    command_output(&alpha_inspect),
                    alpha_guest_http,
                    alpha_guest_network,
                    alpha_peer_http,
                    command_output(&alpha_exec),
                    String::from_utf8_lossy(&host_iptables.stdout),
                    String::from_utf8_lossy(&host_routes.stdout)
                );
            }
            if !beta_http
                .as_ref()
                .is_ok_and(|response| response.contains("Welcome to nginx!"))
            {
                let beta_guest_http = backend_exec_output(
                    &backend,
                    "beta-api-1",
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "wget -qO- http://127.0.0.1:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && printf guest-http-ok"
                            .to_owned(),
                    ],
                )
                .await;
                let beta_inspect = docker_command(&host)
                    .args(["inspect", "beta-api-1"])
                    .output()
                    .await
                    .expect("inspect beta api container");
                panic!(
                    "beta host port should serve the api HTTP endpoint:\nresult: {beta_http:#?}\nbeta inspect:\n{}\nbeta guest exec:\n{}",
                    command_output(&beta_inspect),
                    beta_guest_http
                );
            }
            if !alpha_exec.status.success()
                || String::from_utf8_lossy(&alpha_exec.stdout).trim() != "resolve-ok"
            {
                panic!(
                    "docker compose exec should resolve same-project bare and qualified service names:\ncompose exec:\n{}\nhost direct guest probe:\n{:#?}\nprobe hosts:\n{}\nbare service probe:\n{}\nqualified service probe:\n{}",
                    command_output(&alpha_exec),
                    alpha_direct_guest_http,
                    alpha_probe_hosts,
                    alpha_bare_service,
                    alpha_qualified_service
                );
            }
            assert!(
                !alpha_cross_project.status.success(),
                "cross-project qualified alias should not resolve from alpha:\n{}",
                command_output(&alpha_cross_project)
            );
            let alpha_down = docker_compose_command(&host, "alpha", &alpha_compose)
                .args(["down", "-v"])
                .output()
                .await
                .expect("run docker compose down for alpha");
            let beta_down = docker_compose_command(&host, "beta", &beta_compose)
                .args(["down", "-v"])
                .output()
                .await
                .expect("run docker compose down for beta");
            assert!(
                alpha_down.status.success(),
                "docker compose down should succeed for alpha:\n{}",
                command_output(&alpha_down)
            );
            assert!(
                beta_down.status.success(),
                "docker compose down should succeed for beta:\n{}",
                command_output(&beta_down)
            );

            Ok(())
        },
    )
    .await
    .expect("docker compose isolation e2e should complete");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn docker_compose_lifecycle_covers_logs_stop_and_start() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }
    if !docker_compose_available().await {
        eprintln!("skipping: docker compose plugin not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    let project_dir = workspace_tempdir().expect("create compose tempdir");
    let host_port = reserve_local_port().expect("reserve host port");
    let compose_path =
        write_compose_fixture(project_dir.path(), host_port).expect("write compose fixture");

    with_docker_server(
        image_store_dir.path().to_path_buf(),
        move |context| async move {
            let DockerServerContext { host, backend } = context;

            let up_output = docker_compose_command(&host, "gamma", &compose_path)
                .args(["up", "-d"])
                .output()
                .await
                .expect("run docker compose up");
            let initial_http = wait_for_http_body(host_port, "Welcome to nginx!").await;
            let access_log_probe = wait_for_http_body(host_port, "Welcome to nginx!").await;
            if let Err(error) = &access_log_probe {
                let compose_ps = docker_compose_command(&host, "gamma", &compose_path)
                    .arg("ps")
                    .output()
                    .await
                    .expect("run docker compose ps after host port failure");
                let inspect_output = docker_command(&host)
                    .args(["inspect", "gamma-api-1"])
                    .output()
                    .await
                    .expect("inspect gamma api after host port failure");
                let guest_http = backend_exec_output(
                    &backend,
                    "gamma-api-1",
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "wget -qO- http://127.0.0.1:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && printf guest-http-ok"
                            .to_owned(),
                    ],
                )
                .await;
                let guest_network = backend_exec_output(
                    &backend,
                    "gamma-api-1",
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "ifconfig -a 2>/dev/null || true; printf '\\n---\\n'; cat /proc/net/route 2>/dev/null || true"
                            .to_owned(),
                    ],
                )
                .await;
                let backend_vms = backend
                    .list()
                    .await
                    .expect("list backend VMs after host port failure");
                panic!(
                    "issue second request to generate compose logs: {error:#}\ncompose ps:\n{}\ninspect:\n{}\nguest http:\n{}\nguest network:\n{}\nbackend vms:\n{backend_vms:#?}",
                    command_output(&compose_ps),
                    command_output(&inspect_output),
                    guest_http,
                    guest_network,
                );
            }
            let access_log_probe = access_log_probe.expect("compose host port should stay reachable");
            let logs_output = docker_compose_command(&host, "gamma", &compose_path)
                .args(["logs", "api"])
                .output()
                .await
                .expect("run docker compose logs for api");
            let stop_output = docker_compose_command(&host, "gamma", &compose_path)
                .args(["stop", "api"])
                .output()
                .await
                .expect("run docker compose stop for api");
            let stopped_http = wait_for_http_unreachable(host_port).await;
            let start_output = docker_compose_command(&host, "gamma", &compose_path)
                .args(["start", "api"])
                .output()
                .await
                .expect("run docker compose start for api");
            let restarted_http = wait_for_http_body(host_port, "Welcome to nginx!").await;
            let down_output = docker_compose_command(&host, "gamma", &compose_path)
                .args(["down", "-v"])
                .output()
                .await
                .expect("run docker compose down");

            assert!(
                up_output.status.success(),
                "docker compose up should succeed:\n{}",
                command_output(&up_output)
            );
            assert!(
                initial_http
                    .as_ref()
                    .is_ok_and(|response| response.contains("Welcome to nginx!")),
                "compose host port should serve nginx before lifecycle operations:\nresult: {initial_http:#?}",
            );
            assert!(
                access_log_probe.contains("Welcome to nginx!"),
                "follow-up HTTP probe should still reach nginx before logs check:\n{access_log_probe}",
            );
            assert!(
                logs_output.status.success(),
                "docker compose logs should succeed:\n{}",
                command_output(&logs_output)
            );
            let logs_text = String::from_utf8_lossy(&logs_output.stdout);
            assert!(
                logs_text.contains("/docker-entrypoint.sh")
                    || logs_text.contains("Configuration complete; ready for start up"),
                "docker compose logs should return service logs:\n{}",
                command_output(&logs_output)
            );
            assert!(
                stop_output.status.success(),
                "docker compose stop should succeed:\n{}",
                command_output(&stop_output)
            );
            assert!(
                stopped_http.is_ok(),
                "published port should become unreachable after docker compose stop:\nresult: {stopped_http:#?}",
            );
            assert!(
                start_output.status.success(),
                "docker compose start should succeed:\n{}",
                command_output(&start_output)
            );
            assert!(
                restarted_http
                    .as_ref()
                    .is_ok_and(|response| response.contains("Welcome to nginx!")),
                "published port should recover after docker compose start:\nresult: {restarted_http:#?}",
            );
            assert!(
                down_output.status.success(),
                "docker compose down should succeed:\n{}",
                command_output(&down_output)
            );

            Ok(())
        },
    )
    .await
    .expect("docker compose lifecycle e2e should complete");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn docker_compose_multi_network_scopes_service_resolution() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }
    if !docker_compose_available().await {
        eprintln!("skipping: docker compose plugin not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    let project_dir = workspace_tempdir().expect("create compose tempdir");
    let compose_path =
        write_multi_network_compose_fixture(project_dir.path()).expect("write compose fixture");

    with_docker_server(
        image_store_dir.path().to_path_buf(),
        move |context| async move {
            let DockerServerContext { host, backend } = context;

            let pull_alpine = docker_command(&host)
                .args(["pull", "alpine:latest"])
                .output()
                .await
                .expect("pull alpine image");
            let pull_nginx = docker_command(&host)
                .args(["pull", "nginx:alpine"])
                .output()
                .await
                .expect("pull nginx image");
            let up_output = docker_compose_command(&host, "delta", &compose_path)
                .args(["up", "-d"])
                .output()
                .await
                .expect("run docker compose up");

            let frontend_probe = docker_compose_command(&host, "delta", &compose_path)
                .args([
                    "exec",
                    "-T",
                    "frontend_probe",
                    "sh",
                    "-lc",
                    "for i in 1 2 3 4 5 6 7 8 9 10; do wget -T 2 -qO- http://api:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && ! wget -T 2 -qO- http://db:80 >/tmp/db.html && printf frontend-ok && exit 0; sleep 1; done; exit 1",
                ])
                .output()
                .await
                .expect("run docker compose exec for frontend probe");

            let backend_probe = docker_compose_command(&host, "delta", &compose_path)
                .args([
                    "exec",
                    "-T",
                    "backend_probe",
                    "sh",
                    "-lc",
                    "for i in 1 2 3 4 5 6 7 8 9 10; do wget -T 2 -qO- http://db:80 >/tmp/db.html && grep -q 'Welcome to nginx!' /tmp/db.html && ! wget -T 2 -qO- http://api:80 >/tmp/api.html && printf backend-ok && exit 0; sleep 1; done; exit 1",
                ])
                .output()
                .await
                .expect("run docker compose exec for backend probe");

            let bridge_probe = docker_compose_command(&host, "delta", &compose_path)
                .args([
                    "exec",
                    "-T",
                    "bridge",
                    "sh",
                    "-lc",
                    "for i in 1 2 3 4 5 6 7 8 9 10; do wget -T 2 -qO- http://api:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && wget -T 2 -qO- http://db:80 >/tmp/db.html && grep -q 'Welcome to nginx!' /tmp/db.html && printf bridge-ok && exit 0; sleep 1; done; exit 1",
                ])
                .output()
                .await
                .expect("run docker compose exec for bridge");

            let api_info = backend
                .get("delta-api-1")
                .await
                .expect("inspect delta api vm");
            let api_frontend_ip = GuestNetworkLink::for_named_network(
                "delta_frontend",
                api_info.cid.expect("delta api should have a CID"),
            )
            .guest_ip;
            let db_info = backend
                .get("delta-db-1")
                .await
                .expect("inspect delta db vm");
            let db_backend_ip = GuestNetworkLink::for_named_network(
                "delta_backend",
                db_info.cid.expect("delta db should have a CID"),
            )
            .guest_ip;
            let frontend_probe_network = backend_exec_output(
                &backend,
                "delta-frontend_probe-1",
                vec![
                    "sh".to_owned(),
                    "-lc".to_owned(),
                    "cat /etc/hosts 2>/dev/null; printf '\\n---\\n'; ifconfig -a 2>/dev/null || true; printf '\\n---\\n'; cat /proc/net/route 2>/dev/null || true; printf '\\n---\\n'; cat /proc/net/arp 2>/dev/null || true"
                        .to_owned(),
                ],
            )
            .await;
            let frontend_direct_api = backend_exec_output(
                &backend,
                "delta-frontend_probe-1",
                vec![
                    "sh".to_owned(),
                    "-lc".to_owned(),
                    format!(
                        "wget -T 2 -qO- http://{api_frontend_ip}:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && printf direct-api-ok"
                    ),
                ],
            )
            .await;
            let backend_direct_db = backend_exec_output(
                &backend,
                "delta-backend_probe-1",
                vec![
                    "sh".to_owned(),
                    "-lc".to_owned(),
                    format!(
                        "wget -T 2 -qO- http://{db_backend_ip}:80 >/tmp/db.html && grep -q 'Welcome to nginx!' /tmp/db.html && printf direct-db-ok"
                    ),
                ],
            )
            .await;
            let api_guest_http = backend_exec_output(
                &backend,
                "delta-api-1",
                vec![
                    "sh".to_owned(),
                    "-lc".to_owned(),
                    "wget -T 2 -qO- http://127.0.0.1:80 >/tmp/api.html && grep -q 'Welcome to nginx!' /tmp/api.html && printf api-guest-http-ok"
                        .to_owned(),
                ],
            )
            .await;
            let db_guest_http = backend_exec_output(
                &backend,
                "delta-db-1",
                vec![
                    "sh".to_owned(),
                    "-lc".to_owned(),
                    "wget -T 2 -qO- http://127.0.0.1:80 >/tmp/db.html && grep -q 'Welcome to nginx!' /tmp/db.html && printf db-guest-http-ok"
                        .to_owned(),
                ],
            )
            .await;

            let down_output = docker_compose_command(&host, "delta", &compose_path)
                .args(["down", "-v"])
                .output()
                .await
                .expect("run docker compose down");

            assert!(
                pull_alpine.status.success(),
                "docker pull alpine should succeed before compose up:\n{}",
                command_output(&pull_alpine)
            );
            assert!(
                pull_nginx.status.success(),
                "docker pull nginx should succeed before compose up:\n{}",
                command_output(&pull_nginx)
            );
            assert!(
                up_output.status.success(),
                "docker compose up should succeed:\n{}",
                command_output(&up_output)
            );
            assert!(
                frontend_probe.status.success(),
                "frontend-only service should resolve only frontend peers:\ncompose exec:\n{}\nfrontend probe network:\n{}\nfrontend direct api:\n{}\napi guest http:\n{}",
                command_output(&frontend_probe),
                frontend_probe_network,
                frontend_direct_api,
                api_guest_http
            );
            assert_eq!(
                String::from_utf8_lossy(&frontend_probe.stdout).trim(),
                "frontend-ok",
                "frontend-only service should not resolve backend-only peers:\n{}",
                command_output(&frontend_probe)
            );
            assert!(
                backend_probe.status.success(),
                "backend-only service should resolve only backend peers:\ncompose exec:\n{}\nbackend direct db:\n{}\ndb guest http:\n{}",
                command_output(&backend_probe),
                backend_direct_db,
                db_guest_http
            );
            assert_eq!(
                String::from_utf8_lossy(&backend_probe.stdout).trim(),
                "backend-ok",
                "backend-only service should not resolve frontend-only peers:\n{}",
                command_output(&backend_probe)
            );
            assert!(
                bridge_probe.status.success(),
                "bridge service should resolve both shared-network peers:\n{}",
                command_output(&bridge_probe)
            );
            assert_eq!(
                String::from_utf8_lossy(&bridge_probe.stdout).trim(),
                "bridge-ok",
                "bridge service should resolve both frontend and backend peers:\n{}",
                command_output(&bridge_probe)
            );
            assert!(
                down_output.status.success(),
                "docker compose down should succeed:\n{}",
                command_output(&down_output)
            );

            Ok(())
        },
    )
    .await
    .expect("docker compose multi-network e2e should complete");
}

#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[serial]
async fn docker_buildx_load_imports_image_into_visor() {
    if !has_kvm() {
        eprintln!("skipping: /dev/kvm not available");
        return;
    }
    if !docker_and_buildx_available().await {
        eprintln!("skipping: docker CLI or buildx plugin not available");
        return;
    }

    let image_store_dir = workspace_tempdir().expect("create image store tempdir");
    let context_dir = workspace_tempdir().expect("create docker build context tempdir");
    write_build_context(context_dir.path()).expect("write docker build context");

    let tag = format!("visor-buildx-load:test-{}", std::process::id());
    with_docker_server(
        image_store_dir.path().to_path_buf(),
        move |context| async move {
            let DockerServerContext { host, backend } = context;

            let build_output = docker_command(&host)
                .args([
                    "buildx",
                    "build",
                    "--load",
                    "--progress=plain",
                    "-t",
                    &tag,
                    context_dir
                        .path()
                        .to_str()
                        .expect("context dir should be utf-8"),
                ])
                .output()
                .await
                .expect("run docker buildx build --load");

            if !build_output.status.success() {
                let buildkit_logs = docker_command(&host)
                    .args(["logs", "buildx_buildkit_default"])
                    .output()
                    .await
                    .ok()
                    .map(|output| command_output(&output))
                    .unwrap_or_else(|| {
                        "stdout:\n\n\nstderr:\nfailed to collect buildkit logs".to_owned()
                    });
                let buildkit_inspect =
                    docker_output(&host, &["inspect", "buildx_buildkit_default"]).await;
                let buildctl_version = docker_output(
                    &host,
                    &["exec", "buildx_buildkit_default", "buildctl", "--version"],
                )
                .await;
                let buildctl_help = docker_output(
                    &host,
                    &[
                        "exec",
                        "buildx_buildkit_default",
                        "buildctl",
                        "dial-stdio",
                        "--help",
                    ],
                )
                .await;
                let buildctl_probe = docker_output(
                    &host,
                    &[
                        "exec",
                        "buildx_buildkit_default",
                        "sh",
                        "-lc",
                        "printf 'x' | buildctl dial-stdio",
                    ],
                )
                .await;
                let backend_buildctl_version = backend_exec_output(
                    &backend,
                    "buildx_buildkit_default",
                    vec!["buildctl".to_owned(), "--version".to_owned()],
                )
                .await;
                let backend_buildctl_probe = backend_exec_output(
                    &backend,
                    "buildx_buildkit_default",
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "printf 'x' | buildctl dial-stdio".to_owned(),
                    ],
                )
                .await;
                let backend_buildctl_preface = backend_exec_output(
                    &backend,
                    "buildx_buildkit_default",
                    vec![
                        "sh".to_owned(),
                        "-lc".to_owned(),
                        "printf 'PRI * HTTP/2.0\\r\\n\\r\\nSM\\r\\n\\r\\n\\0\\0\\0\\004\\0\\0\\0\\0\\0' | buildctl dial-stdio"
                            .to_owned(),
                    ],
                )
                .await;
                let backend_stream_probe = backend_exec_stream_probe(
                    &backend,
                    "buildx_buildkit_default",
                    vec!["buildctl".to_owned(), "dial-stdio".to_owned()],
                )
                .await;
                let host_buildctl = host_buildctl_diagnostics(image_store_dir.path()).await;
                panic!(
                    "docker buildx build --load should succeed:\n{}\n\nbuildkit inspect:\n{buildkit_inspect}\n\nbuildctl version:\n{buildctl_version}\n\nbuildctl help:\n{buildctl_help}\n\nbuildctl probe:\n{buildctl_probe}\n\nbackend buildctl --version:\n{backend_buildctl_version}\n\nbackend buildctl probe:\n{backend_buildctl_probe}\n\nbackend buildctl HTTP/2 probe:\n{backend_buildctl_preface}\n\nbackend buildctl stream probe:\n{backend_stream_probe}\n\nhost buildctl diagnostics:\n{host_buildctl}\n\nbuildkit logs:\n{buildkit_logs}",
                    command_output(&build_output),
                );
            }

            let inspect_output = docker_command(&host)
                .args(["image", "inspect", &tag])
                .output()
                .await
                .expect("run docker image inspect");
            assert!(
                inspect_output.status.success(),
                "buildx-loaded image should be inspectable:\n{}",
                command_output(&inspect_output)
            );

            let run_output = docker_command(&host)
                .args(["run", "--rm", &tag])
                .output()
                .await
                .expect("run built image");
            assert!(
                run_output.status.success(),
                "docker run should succeed for buildx-loaded image:\n{}",
                command_output(&run_output)
            );
            assert_eq!(
                String::from_utf8_lossy(&run_output.stdout).trim(),
                "buildx-load-ok",
            );

            remove_image(&host, &tag)
                .await
                .expect("cleanup built image");

            Ok(())
        },
    )
    .await
    .expect("docker buildx --load e2e should complete");
}
