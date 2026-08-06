# 03 — visor-runtime (Daemon + CLI + Orchestration)

The product layer. Builds on visor-machine to provide the full daemon, CLI,
OCI pipeline, pool management, API, and networking.

## Crate Layout

```
visor-runtime/
+-- src/
    +-- lib.rs
    +-- main.rs                 # Binary entry: parse subcommand, dispatch
    +-- daemon.rs               # `visor start` — HTTP server, pool, metrics loop
    +-- backend.rs              # ExecutionBackend trait (KVM impl P0, container P2)
    +-- oci/
    |   +-- registry.rs         # OCI Distribution pull (manifests, layers, auth)
    |   +-- cache.rs            # Content-addressable blob cache (~/.visor/cache/)
    |   +-- layer.rs            # Layer unpacking, whiteout processing
    |   +-- rootfs.rs           # ext4 builder (mke2fs -d), init drive builder
    +-- net/
    |   +-- switch.rs           # Internal virtual switch (per-network)
    |   +-- nat.rs              # One TAP + iptables NAT per network
    |   +-- dns.rs              # Embedded DNS resolver (service discovery + upstream)
    |   +-- ip_alloc.rs         # IP allocation per network subnet
    |   +-- port_forward.rs     # Host port → guest port mapping
    +-- vsock/
    |   +-- client.rs           # Host-side JSON-RPC 2.0 over vsock
    |   +-- protocol.rs         # Request/response types (ping, exec, write_file, etc.)
    +-- pool/
    |   +-- manager.rs          # PoolManager — per-image pools, background refill
    |   +-- snapshot_cache.rs   # Disk cache at ~/.visor/cache/, golden snapshot mgmt
    |   +-- health.rs           # Pool VM health checks (vsock ping)
    +-- api/
    |   +-- server.rs           # axum HTTP server setup
    |   +-- routes/
    |   |   +-- vms.rs          # CRUD + exec + processes
    |   |   +-- pool.rs         # Pool status + warm
    |   |   +-- events.rs       # SSE stream with filtering
    |   |   +-- info.rs         # Host capabilities, mode, features
    |   |   +-- metrics.rs      # Prometheus endpoint
    |   |   +-- health.rs       # Health check
    |   +-- sse.rs              # SSE broadcaster with per-subscriber filtering
    |   +-- openapi.rs          # OpenAPI spec generation
    +-- cli/
    |   +-- run.rs              # visor run
    |   +-- exec.rs             # visor exec
    |   +-- shell.rs            # visor shell (toybox interactive shell)
    |   +-- attach.rs           # visor attach (main process stdin/stdout)
    |   +-- console.rs          # visor console (serial TTY, boot logs)
    |   +-- ps.rs               # visor ps
    |   +-- top.rs              # visor top (guest process list)
    |   +-- info.rs             # visor info
    |   +-- compose.rs          # visor compose up/down
    |   +-- volume.rs           # visor volume create/ls/rm
    |   +-- images.rs           # visor images
    |   +-- tui.rs              # visor tui (terminal dashboard)
    +-- compose/
    |   +-- parser.rs           # docker-compose.yml parser
    |   +-- orchestrator.rs     # Multi-VM lifecycle, network wiring
    +-- tui/
        +-- app.rs              # TUI application state
        +-- views/
            +-- dashboard.rs    # VM list, metrics, events
            +-- vm_detail.rs    # Single VM detail view
            +-- logs.rs         # VM log viewer
```

## ExecutionBackend Trait

Designed at P0, container implementation at P2:

```rust
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    /// Create a new VM/container instance.
    async fn create(&self, config: &VmConfig) -> Result<InstanceHandle>;

    /// Execute a command inside a running instance.
    async fn exec(&self, id: &str, cmd: &ExecRequest) -> Result<ExecResult>;

    /// Destroy an instance and clean up all resources.
    async fn destroy(&self, id: &str) -> Result<()>;

    /// Take a snapshot for pool caching.
    async fn snapshot(&self, id: &str) -> Result<SnapshotHandle>;

    /// Restore from a snapshot (<5ms for KVM, ~1ms for container).
    async fn restore(&self, snap: &SnapshotHandle) -> Result<InstanceHandle>;

    /// Get current metrics for an instance.
    async fn metrics(&self, id: &str) -> Result<InstanceMetrics>;
}
```

KVM backend wraps visor-machine. Container backend uses clone(2) + namespaces.
The API, CLI, pool, and networking layers are backend-agnostic.

## API Endpoints

```
POST   /v1/vms                  Create VM
GET    /v1/vms                  List VMs
GET    /v1/vms/:id              Get VM details
DELETE /v1/vms/:id              Destroy VM
POST   /v1/vms/:id/exec        Execute command in VM
GET    /v1/vms/:id/processes    Guest process list (via vsock)
GET    /v1/vms/:id/metrics      Per-VM metrics
GET    /v1/vms/:id/logs         VM serial console output

GET    /v1/pool                 Pool status
POST   /v1/pool/warm            Warm an image

GET    /v1/networks             List networks
POST   /v1/networks             Create network
DELETE /v1/networks/:id         Delete network

GET    /v1/events               SSE stream (filterable)
GET    /v1/vms/:id/events       SSE stream (single VM)

GET    /v1/info                 Host capabilities, mode, features
GET    /v1/metrics              Prometheus metrics
GET    /v1/health               Health check
GET    /docs                    OpenAPI / Swagger UI
```

### Shell / Console Access

```
POST   /v1/vms/:id/shell       Open toybox shell session (WebSocket upgrade)
POST   /v1/vms/:id/attach      Attach to main process (WebSocket upgrade)
GET    /v1/vms/:id/console     Serial console stream (WebSocket upgrade)
```

All three upgrade to WebSocket for bidirectional TTY streaming.
`visor shell` works on any image (toybox bundled on init drive).
`visor exec` requires a command; `visor shell` drops into interactive sh.

### SSE Filtering

```
GET /v1/events                              # All events
GET /v1/events?type=vm.created              # Only creation events
GET /v1/events?type=vm.created,vm.destroyed # Multiple types
GET /v1/vms/:id/events                      # All events for one VM
GET /v1/vms/:id/events?type=vm.exec         # Exec events for one VM
```

### Info Endpoint

```json
{
    "version": "0.1.0",
    "mode": "kvm",
    "features": {
        "kvm": true,
        "snapshots": true,
        "warm_pool": true,
        "ballooning": false,
        "gpu_passthrough": false,
        "rate_limiting": false,
        "virtio_fs": true
    },
    "host": {
        "arch": "x86_64",
        "cpus": 12,
        "memory_total_mib": 65536,
        "memory_available_mib": 48200,
        "kernel": "6.1.155"
    },
    "pool": {
        "images": ["alpine:3.20", "python:3.12"],
        "total_ready": 15,
        "total_active": 3
    }
}
```

## TUI Dashboard

`visor tui` — terminal UI built with ratatui. Live dashboard showing:

- VM list with status, image, uptime, CPU%, memory
- Real-time event stream
- Pool status per image
- Host resource usage
- Drill into individual VMs (processes, logs, metrics)

Similar to `k9s` for Kubernetes or `lazydocker` for Docker.
