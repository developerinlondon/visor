# visor-docker — Docker Engine API Compatibility Layer

## Overview

`visor-docker` is a translation layer that lets stock Docker tooling (`docker` CLI,
`docker-compose`, Testcontainers, CI pipelines) communicate with visor's microVM
runtime. No Docker daemon is involved — visor speaks the Docker Engine API protocol
directly, mapping "container" operations to VM operations.

Users set `DOCKER_HOST=unix:///var/run/visor.sock` and their existing workflows run
on hardware-isolated microVMs instead of Linux namespaces.

## Architecture

```text
┌─────────────┐     ┌─────────────┐     ┌──────────────┐
│  docker CLI  │     │  visor CLI   │     │ docker-compose│
│  docker-compose│   │              │     │ Testcontainers│
└──────┬───────┘     └──────┬───────┘     └──────┬───────┘
       │                     │                     │
       │ Docker Engine API   │ visor native API    │ Docker Engine API
       │ /v1.XX/containers/* │ /v1/vms/*           │ /v1.XX/containers/*
       │                     │                     │
       ▼                     ▼                     ▼
  ┌──────────────────────────────────────────────────────┐
  │              Unix socket / TCP                        │
  │              /var/run/visor.sock                       │
  │              (or 127.0.0.1:7800)                      │
  └──────────────────────┬───────────────────────────────┘
                         │
                    axum Router
                    (path-based routing)
                         │
            ┌────────────┴────────────┐
            │                         │
   ┌────────▼─────────┐    ┌─────────▼──────────┐
   │  visor-docker     │    │  visor-runtime      │
   │  (this crate)     │    │  native API routes  │
   │                   │    │                     │
   │  Translates:      │    │  /v1/vms/*          │
   │  Docker JSON  ──► │    │  /v1/pool/*         │
   │  visor types      │    │  /v1/dns/*          │
   └────────┬──────────┘    └─────────┬───────────┘
            │                         │
            └────────────┬────────────┘
                         │
                         ▼
              ┌─────────────────────┐
              │  ExecutionBackend    │
              │  (visor-types trait) │
              └──────────┬──────────┘
                         │
                         ▼
              ┌─────────────────────┐
              │     VmmBackend       │
              │  (visor-runtime)     │
              └──────────┬──────────┘
                         │
                    ┌────┴────┐
                    │ visor-vmm│
                    │ KVM / HVF│
                    └─────────┘
```

### Key Design Decisions

**Single socket, path-based routing.** Docker API paths (`/v1.45/containers/*`)
and visor paths (`/v1/vms/*`) don't collide. One process, one socket, one router.
No second daemon to manage.

**No Docker dependency.** visor-docker is a pure HTTP translation layer. It
receives Docker-shaped JSON, converts it to `VmConfig`/`ExecRequest`/etc., calls
`ExecutionBackend` methods, and returns Docker-shaped JSON responses. Zero Docker
code is involved.

**Crate boundary.** `visor-docker` depends only on `visor-types` (for the
`ExecutionBackend` trait and shared types). It has no dependency on `visor-vmm`,
`visor-runtime`, or any platform-specific code. Changes to Docker API handling
don't recompile the VMM.

## Docker → visor Translation

### Concept Mapping

| Docker Concept    | visor Equivalent    | Notes                                      |
| ----------------- | ------------------- | ------------------------------------------ |
| Container         | VM (microVM)        | Each "container" is a hardware-isolated VM |
| Image             | OCI Image           | Same image format, same registries         |
| Volume bind mount | Volume mount (`-v`) | Host→guest directory sharing via virtio-fs |
| Named volume      | VolumeManager       | Persistent named volumes                   |
| Network           | Subnet + DNS        | Built-in subnet allocator + DNS registry   |
| Exec              | Exec (vsock)        | Command execution via guest vsock          |
| Logs              | Serial output       | Console output captured from VM serial     |

### API Version Negotiation

Docker clients perform version negotiation:

1. Client sends `GET /_ping`
2. Server responds with `Api-Version` header (e.g. `1.45`)
3. Client uses `min(client_version, server_version)`
4. All subsequent requests use `/v{version}/` prefix

visor advertises API version `1.45`. We accept any `/v1.XX/` prefix and serve
the same implementation — Docker's API is backward-compatible.

## Supported Endpoints

### Phase 1 (Core — makes `docker run` / `docker ps` work)

| Endpoint                            | Docker CLI Command             |
| ----------------------------------- | ------------------------------ |
| `GET /_ping`                        | Connection test                |
| `GET /version`                      | `docker version`               |
| `GET /info`                         | `docker info`                  |
| `POST /v1.45/containers/create`     | `docker create` / `docker run` |
| `GET /v1.45/containers/json`        | `docker ps`                    |
| `GET /v1.45/containers/{id}/json`   | `docker inspect`               |
| `POST /v1.45/containers/{id}/start` | `docker start`                 |
| `POST /v1.45/containers/{id}/stop`  | `docker stop`                  |
| `POST /v1.45/containers/{id}/kill`  | `docker kill`                  |
| `DELETE /v1.45/containers/{id}`     | `docker rm`                    |
| `POST /v1.45/containers/{id}/wait`  | `docker wait`                  |
| `GET /v1.45/containers/{id}/logs`   | `docker logs`                  |

### Phase 2 (Exec — makes `docker exec` work)

| Endpoint                           | Docker CLI Command     |
| ---------------------------------- | ---------------------- |
| `POST /v1.45/containers/{id}/exec` | `docker exec` (create) |
| `POST /v1.45/exec/{id}/start`      | `docker exec` (start)  |
| `GET /v1.45/exec/{id}/json`        | Exec inspect           |

### Phase 3 (Images — makes `docker pull` / `docker images` work)

| Endpoint                        | Docker CLI Command     |
| ------------------------------- | ---------------------- |
| `GET /v1.45/images/json`        | `docker images`        |
| `POST /v1.45/images/create`     | `docker pull`          |
| `DELETE /v1.45/images/{name}`   | `docker rmi`           |
| `GET /v1.45/images/{name}/json` | `docker image inspect` |

### Phase 4 (Compose support — makes `docker-compose up` work)

| Endpoint                            | Purpose                      |
| ----------------------------------- | ---------------------------- |
| `GET /v1.45/networks`               | List networks                |
| `POST /v1.45/networks/create`       | Create network               |
| `DELETE /v1.45/networks/{id}`       | Remove network               |
| `POST /v1.45/networks/{id}/connect` | Connect container to network |
| `GET /v1.45/volumes`                | List volumes                 |
| `POST /v1.45/volumes/create`        | Create volume                |
| `DELETE /v1.45/volumes/{name}`      | Remove volume                |

### Not Supported (returns 501)

| Endpoint           | Reason                                              |
| ------------------ | --------------------------------------------------- |
| `POST /build`      | Use Docker/Podman for builds, visor for runtime     |
| `POST /swarm/*`    | Multi-node orchestration — use K8s + visor-operator |
| `POST /services/*` | Swarm services — not applicable                     |
| `POST /plugins/*`  | Docker plugins — not applicable                     |

## Response Format

Docker API responses include specific headers:

```http
HTTP/1.1 200 OK
Content-Type: application/json
Api-Version: 1.45
Docker-Experimental: false
Ostype: linux
Server: visor/0.0.8
```

All responses include the `Api-Version` header. The `Server` header identifies
visor. `Ostype` is always `linux` (the guest OS).

## Benefits Over Native Docker

| Aspect           | Docker                                              | visor                                    |
| ---------------- | --------------------------------------------------- | ---------------------------------------- |
| Isolation        | Shared kernel (namespaces)                          | Full VM (separate kernel per container)  |
| Security         | Container escapes are real                          | Hardware-enforced isolation (VT-x / HVF) |
| Startup          | ~300ms                                              | ~200ms (HVF), <5ms (snapshot restore)    |
| Snapshot/restore | Not supported                                       | CoW memory, sub-5ms restore from pool    |
| macOS overhead   | Docker Desktop = Linux VM + containers (two layers) | Direct HVF (one layer)                   |
| Compatibility    | Full Docker ecosystem                               | Same ecosystem via this compat layer     |

## File Structure

```text
crates/visor-docker/
├── Cargo.toml
├── DESIGN.md              ← This file
└── src/
    ├── lib.rs             ← Public API: docker_router(), DockerState
    ├── types.rs           ← Docker API request/response structs
    ├── types_test.rs      ← Tests for type serialization
    ├── handlers.rs        ← Route handlers (translate Docker → visor)
    ├── handlers_test.rs   ← Handler tests with mock backend
    ├── translate.rs       ← Docker ↔ visor type conversion functions
    └── translate_test.rs  ← Translation tests
```

## Usage

```rust
use visor_docker::docker_router;

// In visor-runtime daemon.rs:
let backend: Arc<dyn ExecutionBackend> = /* ... */;
let docker_routes = docker_router(backend);

// Mount alongside native visor routes
let app = Router::new()
    .merge(native_router)
    .merge(docker_routes);

// Serve on Unix socket
let listener = tokio::net::UnixListener::bind("/var/run/visor.sock")?;
axum::serve(listener, app).await?;
```
