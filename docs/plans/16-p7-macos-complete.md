# P7 — Feature-Complete macOS & Integration Wiring

> **Status**: Planning
> **Phase**: P7 (follows P5/P6 cross-platform VMM refactor)
> **Goal**: Get visor fully working on macOS, wire all disconnected features, then port to Linux

## Problem Statement

The P5/P6 cross-platform refactor delivered working HVF boot (`visor run alpine echo hello` works
on macOS). However, extensive auditing reveals a systemic pattern: **most features are built as
isolated modules but never wired into the VM creation pipeline.**

| Feature                  | Module Exists                 | Wired to VM Creation                     | Working E2E |
| ------------------------ | ----------------------------- | ---------------------------------------- | ----------- |
| Pool pre-warming         | `pool/manager.rs`             | NO — `acquire()` never called            | NO          |
| Snapshots (<5ms restore) | `visor-vmm/snapshot.rs`       | NO — dead code in runtime                | NO          |
| Port forwarding          | `net/backend.rs`              | NO — `setup_port_forward()` never called | NO          |
| Volume mounting          | `volume.rs` + `devices/fs.rs` | NO — volumes created but never attached  | NO          |
| DNS resolution           | `net/dns.rs`                  | NO — registry exists, no server runs     | NO          |
| Shell                    | `cli/shell.rs`                | NO — prints "not implemented"            | NO          |
| Console                  | `cli/console.rs`              | NO — bails with error                    | NO          |
| Compose volumes          | `compose/orchestrator.rs`     | NO — parsed but `volumes: []` hardcoded  | NO          |
| Compose ports            | `compose/orchestrator.rs`     | NO — parsed but `ports: []` hardcoded    | NO          |
| TUI actions              | `tui/app.rs`                  | NO — read-only, no stop/kill/delete      | NO          |

The pool not being wired is why boot takes 2.6s every time — each `visor run` does full OCI
pull + rootfs build + cold boot instead of acquiring a pre-warmed VM.

## Architecture Decisions

### Crate Split: Extract `visor-types`

Extract shared types into a platform-agnostic leaf crate so CLI, API, TUI, compose, and pool
don't transitively depend on visor-vmm.

```text
BEFORE:                           AFTER:
cli/ → backend.rs → visor-vmm    cli/ → visor-types (no platform deps)
api/ → backend.rs → visor-vmm    api/ → visor-types
tui/ → backend.rs → visor-vmm    tui/ → visor-types
```

**Move to `visor-types`:**

- `VmConfig`, `VmInfo`, `VmState`, `PortMapping`, `VolumeMount`
- `ExecRequest`, `ExecResult`, `ExecutionBackend` trait
- Default helpers (`default_memory()`, `default_vcpus()`, `default_protocol()`)

**Keep in `visor-runtime/backend.rs`:**

- `VmmBackend` struct + impl (concrete implementation)
- `VmLiveState`, `VsockConnector` (impl details)

### Backend Rename: `KvmBackend` -> `VmmBackend`

The current `KvmBackend` is already platform-agnostic — it calls `visor_vmm::vm::boot()` which
dispatches via `#[cfg(target_os)]`. The name is misleading. Mechanical rename.

### Container Mode: Always VMs

On macOS, "container mode" IS VM mode (Apple's own containerization framework is VM-backed). On
Linux, microVM isolation is the differentiator. Keep `ContainerBackend` stub for future
optionality but don't invest in namespace runtime.

### Rootless Networking: vmnet-helper (macOS 26+)

macOS 26 (current OS) supports rootless vmnet via "virtual networks." Use `vmnet-helper`
subprocess with socketpair pattern. On macOS <26 (legacy), `visor service install` sets up a
launchd plist for one-time privileged setup.

### Docker Replacement Path: API Compat Layer

Implement a thin Docker Engine API translation layer (~15 endpoints) so `docker compose` can talk
to visor directly. This is more valuable than containerd shimv2 for the target audience (Mac
developers).

## Workstreams

### WS1 — Architecture Foundation

#### WS1.1 — Extract `visor-types` Crate

**Priority**: P1 | **Effort**: Short (2-4h)

Create `crates/visor-types/` with shared types currently in `backend.rs`:

- `VmConfig`, `VmInfo`, `VmState` (with serde + utoipa derives)
- `PortMapping`, `VolumeMount`
- `ExecRequest`, `ExecResult`
- `ExecutionBackend` trait
- Default value helpers

Update all imports in visor-runtime modules (cli/, api/, tui/, compose/, pool/) to use
`visor_types::*` instead of `crate::backend::*`.

**Acceptance**: `cargo check --workspace` passes. No module in cli/, api/, tui/, compose/, pool/
imports `visor_vmm` directly.

#### WS1.2 — Rename `KvmBackend` -> `VmmBackend`

**Priority**: P1 | **Effort**: Quick (<1h)

Rename struct, all impl blocks, doc comments, test references. Fix `daemon.rs:52`.

**Acceptance**: `cargo test --workspace` passes. No references to `KvmBackend` remain.

### WS2 — Wire Disconnected Features

This is the highest-impact workstream. These features are built but not connected.

#### WS2.1 — Wire Pool into VM Creation

**Priority**: P1 | **Effort**: Short (2-4h)

The pool's `acquire()` method is never called. `create_vm()` in `api/routes/vms.rs` always calls
`backend.create()` directly.

**Changes:**

1. Modify `create_vm()` to try `pool.acquire(image)` first
2. Fall back to `backend.create()` if pool is empty
3. Add auto-warming: background task in daemon that maintains target pool sizes per image
4. Emit SSE events: `pool.warmed`, `pool.acquired`, `pool.exhausted`

**Acceptance**: Second `visor run alpine echo hello` completes in <500ms (uses pooled VM).

#### WS2.2 — Wire Snapshots into Pool

**Priority**: P1 | **Effort**: High (2-3 weeks)

Snapshot save/restore works in visor-vmm (tested) but is dead code in runtime. The pool creates
fresh VMs every time instead of restoring from golden snapshots.

**Changes:**

1. **Golden snapshot creation**: After first cold boot of an image + visor-init ready signal,
   call `snapshot::save_bundle()` to `~/.visor/cache/snapshots/<digest>/`
2. **Snapshot-based pool**: Rewrite `PoolManager::warm()` to call `snapshot::restore_bundle()`
   with `mmap(MAP_PRIVATE)` instead of `backend.create()`
3. **Device state serialization**: Implement virtio queue state save/restore (positions, avail
   idx, used idx) for block, vsock, serial devices
4. **Wire `SnapshotCache`**: Connect the existing `snapshot_cache.rs` to `PoolManager`

**Acceptance**: `visor run` from snapshot completes in <5ms (after golden snapshot exists).

#### WS2.3 — Wire Port Forwarding

**Priority**: P1 | **Effort**: Short (2-4h)

`setup_port_forward()` is implemented in both Linux (iptables) and macOS (pfctl) backends but
**never called** during VM boot.

**Changes:**

1. Pass `VmConfig::ports` to `boot()` in visor-vmm
2. After `create_interface()`, call `net_backend.setup_port_forward(mappings, guest_ip)`
3. Store `PortForwardHandle` in `BootedVm` (keeps rules alive for VM lifetime)
4. Drop handle on VM stop (removes rules)

**Acceptance**: `visor run -p 8080:80 nginx` → `curl localhost:8080` returns nginx response.

#### WS2.4 — Wire Volume Mounting

**Priority**: P1 | **Effort**: High (1-2 weeks)

Volumes are created as ext4 files (`~/.visor/volumes/`) but never mounted into VMs. Virtio-fs
device exists (read-only) but is never attached to VMs.

**Changes:**

1. **Virtio-blk path** (named volumes): Attach ext4 volume file as second virtio-blk device
   during boot. Guest sees `/dev/vdb`. visor-init mounts it.
2. **Virtio-fs path** (bind mounts): Attach host directory via virtio-fs device. Guest mounts
   via `mount -t virtiofs <tag> /mnt`.
3. **Write support for virtio-fs**: Implement `FUSE_WRITE`, `FUSE_CREATE`, `FUSE_MKDIR`,
   `FUSE_UNLINK`, `FUSE_RENAME` in `devices/fs.rs` (currently read-only)
4. **Wire into VmConfig**: When `VmConfig::volumes` is non-empty, attach devices during boot
5. **visor-init mount**: Add volume mount logic to visor-init entrypoint

**Acceptance**: `visor run -v /tmp/data:/data alpine ls /data` shows host directory contents.
Writing to `/data` inside VM creates files on host.

#### WS2.5 — Wire Compose Volumes and Ports

**Priority**: P2 | **Effort**: Short (2-4h)

Compose orchestrator parses volumes and ports from YAML but hardcodes `volumes: []` and
`ports: []` when building `VmConfig`.

**Changes:**

1. Convert compose service `volumes:` to `VmConfig::volumes`
2. Convert compose service `ports:` to `VmConfig::ports`
3. Create named volumes during `compose up` if they don't exist

**Acceptance**: `visor compose up` with a service using `ports: ["8080:80"]` and
`volumes: ["./src:/app"]` works end-to-end.

#### WS2.6 — Wire DNS Resolution

**Priority**: P2 | **Effort**: Medium (3-5 days)

`DnsRegistry` exists but no DNS server runs. VMs can't resolve each other by name.

**Changes:**

1. Start embedded DNS server (hickory-dns, already in deps) on gateway IP:53
2. Register VM name → IP when VM boots
3. Deregister on VM stop
4. Forward unknown queries to upstream (host DNS or 8.8.8.8)
5. Configure guest DNS via visor-init (`/etc/resolv.conf`)

**Acceptance**: Two VMs can `ping <other-vm-name>` successfully.

### WS3 — Interactive Sessions

#### WS3.1 — Implement `visor exec` Enhancement

**Priority**: P1 | **Effort**: Short (2-4h)

Exec works but CLI doesn't expose env/workdir flags. The API and guest agent already support them.

**Changes:**

1. Add `--env` / `-e` flag to `ExecArgs` (repeatable)
2. Add `--workdir` / `-w` flag to `ExecArgs`
3. Pass through to `ExecRequest` (currently hardcoded to empty)

**Acceptance**: `visor exec <vm> -e FOO=bar -w /tmp env` shows `FOO=bar` and CWD `/tmp`.

#### WS3.2 — Implement `visor shell`

**Priority**: P1 | **Effort**: Medium (3-5 days)

Guest already has shell support (`find_shell()`, `spawn_shell()` in visor-init). Need to wire
interactive I/O.

**Changes:**

1. New module: `visor-runtime/src/vsock/interactive.rs` — bidirectional vsock I/O with terminal
   raw mode
2. Add `shell` RPC method to guest agent (calls `spawn_shell()`, pipes I/O)
3. CLI enters raw mode, forwards stdin to vsock, prints vsock output to stdout
4. Handle escape key (`^]` default) to detach
5. Handle `SIGWINCH` for terminal resize

**Acceptance**: `visor shell <vm>` drops into interactive `/bin/sh` inside VM.

#### WS3.3 — Implement `visor console`

**Priority**: P2 | **Effort**: Medium (3-5 days)

Serial console attach via WebSocket.

**Changes:**

1. Add WebSocket endpoint `GET /v1/vms/{id}/console` (upgrade to WS)
2. Pipe serial output buffer to WebSocket
3. Pipe WebSocket input to serial device
4. CLI connects via WebSocket, enters raw mode
5. Handle escape key to detach

**Acceptance**: `visor console <vm>` shows serial console output, keyboard input reaches guest.

### WS4 — TUI Enhancement

#### WS4.1 — TUI Actions

**Priority**: P2 | **Effort**: Medium (3-5 days)

TUI is read-only. Developers need to stop/kill/delete VMs and view logs without leaving TUI.

**Changes:**

1. Add action keybindings:
   - `s` — Stop selected VM (POST /v1/vms/{id}/stop)
   - `k` — Kill selected VM (POST /v1/vms/{id}/kill)
   - `d` — Delete selected VM (DELETE /v1/vms/{id}) with confirmation
   - `r` — Restart selected VM
2. Add confirmation dialog widget for destructive actions
3. Add inline log view: `Enter` on VM detail shows serial output
4. Add search/filter: `/` to filter VM list by name/image
5. Add pool status pane: show warm VMs per image

**Acceptance**: User can manage full VM lifecycle from TUI without CLI.

#### WS4.2 — TUI Resource Display

**Priority**: P3 | **Effort**: Short (2-4h)

Add real-time resource monitoring.

**Changes:**

1. Poll `/v1/metrics` for CPU/memory per VM
2. Add sparkline charts for memory usage
3. Show pool status (warm count, target, fill rate)
4. Show network status (port mappings, IP addresses)

**Acceptance**: TUI shows live resource usage per VM.

### WS5 — Rootless Networking

#### WS5.1 — vmnet-helper Integration (macOS 26+)

**Priority**: P2 | **Effort**: Medium (3-5 days)

Replace direct `vmnet` crate usage with `vmnet-helper` subprocess for rootless operation on
macOS 26.

**Changes:**

1. New networking mode in `net/macos.rs`: `VmnetHelperBackend`
2. Create `socketpair(AF_UNIX, SOCK_DGRAM)` for frame I/O
3. Spawn `vmnet-helper --unprivileged --fd 3` with helper fd as fd 3
4. Parse JSON response (MAC, MTU, gateway) from helper stdout
5. Wire socketpair fd into virtio-net device for frame I/O
6. Auto-detect macOS version: rootless on 26+, legacy `sudo` path on older

**Acceptance**: `visor run` works without `sudo` on macOS 26.

#### WS5.2 — Legacy macOS Support (launchd)

**Priority**: P3 | **Effort**: Short (2-4h)

For macOS <26, install a launchd daemon for one-time privileged setup.

**Changes:**

1. `visor service install` writes launchd plist to `/Library/LaunchDaemons/`
2. Plist runs `vmnet-helper` as root daemon with socket at `/var/run/visor-vmnet.sock`
3. `visor service uninstall` removes plist and stops daemon

**Acceptance**: After `sudo visor service install`, `visor run` works without `sudo`.

### WS6 — Virtio Device Completion (macOS)

#### WS6.1 — Wire virtio-net on macOS

**Priority**: P1 | **Effort**: Medium (3-5 days)

virtio-net device exists but is stub on macOS. Network backend (vmnet) exists and works.

**Changes:**

1. Attach virtio-net device during macOS boot (like Linux path does)
2. Wire `MacosNetworkBackend` frame I/O to virtio-net virtqueues
3. Configure guest network via visor-init (IP from vmnet DHCP)
4. Test: VM can reach internet, host can reach VM

**Acceptance**: `visor run alpine ping -c1 8.8.8.8` works on macOS.

#### WS6.2 — virtio-rng on macOS

**Priority**: P3 | **Effort**: Short (1-2h)

Entropy source for guest. Device exists, just needs macOS attachment.

**Changes:**

1. Attach virtio-rng device during macOS boot
2. Wire to `/dev/urandom` on host

**Acceptance**: Guest `/dev/random` works without blocking.

### WS7 — Boot Performance

#### WS7.1 — Profile and Optimize Cold Boot

**Priority**: P2 | **Effort**: Medium (3-5 days)

Current cold boot: 2.6s. Target with pool: <100ms. Target with snapshots: <5ms.

**Breakdown of 2.6s** (estimated):

- OCI manifest/config resolution: ~200ms (cached: ~10ms)
- Layer download: 0ms (cached)
- Layer merge + rootfs build: ~800ms
- Kernel load: ~100ms
- VM boot to init ready: ~500ms
- visor-init startup: ~200ms
- Remaining overhead: ~800ms

**Changes:**

1. Add timing instrumentation to each boot phase
2. Pre-build rootfs on image pull (not on every run)
3. Cache merged rootfs alongside layers
4. Eliminate unnecessary file copies during rootfs build
5. Lazy-load kernel (mmap, don't read entire file)

**Acceptance**: Cold boot (no pool) <1s. Pool hit <100ms. Snapshot restore <5ms.

### WS8 — Testing and Verification

#### WS8.1 — End-to-End Test Suite

**Priority**: P1 | **Effort**: Medium (3-5 days)

Automated tests for all features on macOS. Currently most features are untested E2E.

**Changes:**

1. Test `visor run` with port forwarding, volumes, and env vars
2. Test `visor exec` with all flag combinations
3. Test `visor compose up/down` with multi-service project
4. Test pool pre-warming and acquisition
5. Test snapshot save/restore round-trip
6. Test DNS resolution between VMs

**Note**: Integration tests requiring VM boot need the daemon running. Tests requiring vmnet need
sudo (macOS <26) or vmnet-helper (macOS 26+).

**Acceptance**: `cargo test --workspace` passes on macOS (AArch64) and Linux (x86_64).

#### WS8.2 — Custom Dockerfile Testing

**Priority**: P3 | **Effort**: Short (2-4h)

Verify visor can run images built from custom Dockerfiles (not just registry images).

**Changes:**

1. Build test images with `docker build` (external)
2. Push to local registry or load via `visor pull`
3. Test: multi-stage builds, COPY, RUN, ENTRYPOINT, CMD, ENV, WORKDIR, EXPOSE

**Acceptance**: Custom-built images run correctly in visor.

### WS9 — Docker Replacement

#### WS9.1 — Docker Engine API Compatibility Layer

**Priority**: P3 | **Effort**: High (1-2 weeks)

Thin translation layer so `docker compose` can talk to visor directly.

**Changes:**

1. Listen on `unix:///var/run/visor.sock`
2. Implement subset of Docker Engine API v1.45:
   - `POST /containers/create` → `POST /v1/vms`
   - `POST /containers/{id}/start` → no-op (visor creates running)
   - `POST /containers/{id}/stop` → `POST /v1/vms/{id}/stop`
   - `DELETE /containers/{id}` → `DELETE /v1/vms/{id}`
   - `GET /containers/json` → `GET /v1/vms`
   - `POST /containers/{id}/exec` → `POST /v1/vms/{id}/exec`
   - `GET /containers/{id}/logs` → `GET /v1/vms/{id}/logs`
   - `POST /images/create` → `POST /v1/images/pull`
   - `GET /images/json` → `GET /v1/images`
   - `GET /_ping` → `GET /v1/health`
   - `GET /version` → version info
   - `GET /info` → `GET /v1/info`
3. Handle Docker-specific semantics (container vs VM naming)

**Acceptance**: `DOCKER_HOST=unix:///var/run/visor.sock docker compose up` works with a basic
compose file.

## Priority Summary

### P1 — Must Have (blocking daily use)

| ID    | Task                           | Effort | Impact                   |
| ----- | ------------------------------ | ------ | ------------------------ |
| WS1.1 | Extract visor-types crate      | Short  | Architecture hygiene     |
| WS1.2 | Rename KvmBackend → VmmBackend | Quick  | Correctness              |
| WS2.1 | Wire pool into VM creation     | Short  | Boot time: 2.6s → <500ms |
| WS2.2 | Wire snapshots into pool       | High   | Boot time: <500ms → <5ms |
| WS2.3 | Wire port forwarding           | Short  | `-p` flag actually works |
| WS2.4 | Wire volume mounting           | High   | `-v` flag actually works |
| WS3.1 | Exec enhancement (-e, -w)      | Short  | Dev workflow             |
| WS3.2 | Shell implementation           | Medium | Dev workflow             |
| WS6.1 | virtio-net on macOS            | Medium | Guest networking         |
| WS8.1 | E2E test suite                 | Medium | Quality gate             |

### P2 — Should Have (better experience)

| ID    | Task                           | Effort | Impact                 |
| ----- | ------------------------------ | ------ | ---------------------- |
| WS2.5 | Compose volumes and ports      | Short  | Compose workflow       |
| WS2.6 | DNS resolution                 | Medium | Inter-VM communication |
| WS3.3 | Console implementation         | Medium | Debugging              |
| WS4.1 | TUI actions                    | Medium | UX                     |
| WS5.1 | Rootless networking (macOS 26) | Medium | No sudo                |
| WS7.1 | Boot performance profiling     | Medium | Performance            |

### P3 — Nice to Have (polish)

| ID    | Task                      | Effort | Impact             |
| ----- | ------------------------- | ------ | ------------------ |
| WS4.2 | TUI resource display      | Short  | UX                 |
| WS5.2 | Legacy macOS launchd      | Short  | Backward compat    |
| WS6.2 | virtio-rng on macOS       | Short  | Guest entropy      |
| WS8.2 | Custom Dockerfile testing | Short  | Verification       |
| WS9.1 | Docker API compat layer   | High   | Docker replacement |

## Implementation Order

Recommended sequence for maximum velocity:

```text
Phase A (Foundation):     WS1.1 → WS1.2
Phase B (Wiring):         WS2.3 → WS2.1 → WS6.1 → WS2.4
Phase C (Interaction):    WS3.1 → WS3.2 → WS4.1
Phase D (Performance):    WS2.2 → WS7.1
Phase E (Integration):    WS2.5 → WS2.6 → WS3.3
Phase F (Polish):         WS5.1 → WS8.1 → WS9.1
```

**Rationale**: Wire the broken features first (port forwarding, pool, networking, volumes) since
they're already built. Then add interactive sessions. Then performance (snapshots). Then polish.

## Dependencies

```text
WS2.2 (snapshots) depends on WS2.1 (pool wiring)
WS2.5 (compose volumes) depends on WS2.4 (volume mounting)
WS5.1 (rootless net) depends on WS6.1 (virtio-net macOS)
WS7.1 (boot perf) depends on WS2.1 (pool) and WS2.2 (snapshots)
WS9.1 (Docker API) depends on WS2.3, WS2.4 (port/volume wiring)
```

## Success Criteria

P7 is complete when:

- [ ] `visor run -p 8080:80 -v ./data:/data alpine sh` works on macOS without sudo
- [ ] `visor shell <vm>` provides interactive shell
- [ ] `visor compose up` with ports, volumes, depends_on works
- [ ] Second `visor run` of same image completes in <5ms (snapshot restore)
- [ ] TUI allows stop/kill/delete of VMs
- [ ] VMs can resolve each other by name
- [ ] All quality gates pass on both macOS (AArch64) and Linux (x86_64)
- [ ] No `unwrap()` in production paths
- [ ] Test coverage for all new code
