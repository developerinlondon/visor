# 18 — Linux-First Recovery and Execution Plan

| Field        | Value                                                                                         |
| ------------ | --------------------------------------------------------------------------------------------- |
| Status       | Implemented for Linux-first baseline; follow-up hardening items remain                        |
| Created      | 2026-03-08                                                                                    |
| Dependencies | Plans 00, 02, 03, 08, 10, 14; current `dev` branch implementation                             |
| Scope        | Linux KVM first; preserve trait boundaries needed for later macOS                             |
| Goal         | Get visor to a credible Linux-first product baseline before resuming cross-platform expansion |

## Problem Statement

visor has real progress in three areas:

- KVM boot works on Linux.
- OCI-to-VM execution works in the main runtime path.
- The cross-platform trait refactor created usable abstraction points.

visor is not yet in a product-ready state because the implementation is split
between:

- code that exists but is not fully wired into the Linux execution path,
- code that compiles only partially at workspace scope,
- plan documents that describe a broader end state than the current branch
  actually delivers.

The immediate requirement is not more abstraction. It is a Linux-first
stabilization pass that keeps the trait seams but optimizes for working Linux
behavior, green quality gates, and a clear path to competitive features.

## 2026-03-08 Update

This recovery pass landed the Linux-first baseline in code:

- workspace quality gates are green:
  - `cargo check --workspace`
  - `cargo test --workspace`
  - `cargo clippy --workspace -- -D warnings`
  - `dprint check`
- Linux runtime networking now carries explicit guest network configuration and
  a real Linux virtio-net/TAP path backed by actual `/dev/net/tun` TAP file
  descriptors
- volume handling uses the staged Linux subset:
  - read-only host directories are staged into ext4 data disks
  - file-backed and named volumes attach as virtio-blk data disks
  - guest mount behavior no longer assumes host paths are directly visible
- snapshot save now writes reusable bundle artifacts, the daemon runs pool
  refill and health loops, and snapshot lookup is keyed by VM config rather
  than bare image name
- configs with external volumes skip snapshot fast-paths because mutable host
  state is not part of the snapshot bundle
- `/v1/info` and `/v1/metrics` now report real runtime state for pool,
  health-monitoring, volume, and snapshot capabilities
- real Linux boot coverage now includes staged bind-volume and file-backed
  data-disk mounts
- real Linux networking coverage now includes a host-to-guest forwarded-port
  reachability test against a running guest HTTP service

Deferred follow-up after this recovery pass:

- daemon self-sandboxing via seccomp is still disabled in the current
  single-process runtime because the existing daemon path still mixes API,
  orchestration, OCI, and VMM syscalls in one process
- direct shared-fs bind mounts are still a future enhancement; the shipped
  Linux-first contract is staged read-only directory mounts
- Linux networking still needs bridge/topology refinement for larger multi-VM
  and compose-heavy setups even though the single-VM TAP/NAT/port-forward path
  is now real and test-covered

## Current Assessment

### What Is Real Today

| Area                         | Status               | Evidence                                                                       |
| ---------------------------- | -------------------- | ------------------------------------------------------------------------------ |
| Linux KVM boot               | Working              | `crates/visor-runtime/tests/boot.rs`                                           |
| OCI pull/build/run pipeline  | Working in main path | `crates/visor-runtime/src/backend.rs`                                          |
| Runtime platform abstraction | Partially successful | `crates/visor-vmm/src/platform/mod.rs`                                         |
| Pool acquire path            | Present              | `crates/visor-runtime/src/pool/manager.rs`                                     |
| Snapshot restore entry point | Present              | `crates/visor-runtime/src/backend.rs`                                          |
| Shell and console routes     | Present              | `crates/visor-runtime/src/api/ws.rs`, `crates/visor-runtime/src/cli/`          |
| Embedded DNS server          | Present              | `crates/visor-runtime/src/daemon.rs`, `crates/visor-runtime/src/net/server.rs` |

### What Is Not Yet Product-Ready

| Area                      | Current Issue                                                        |
| ------------------------- | -------------------------------------------------------------------- |
| Daemon isolation          | Runtime self-sandboxing via seccomp is still disabled                |
| Shared-fs bind mounts     | Direct bind sharing is not shipped; staged read-only mounts are used |
| Linux networking topology | Current TAP/NAT path works, but bridge/topology work remains         |
| Advanced differentiators  | Some modules still exist without full runtime wiring                 |
| Tests                     | Real Linux coverage improved, but broader multi-VM e2e is still thin |

## Guiding Principles

1. Linux correctness beats cross-platform elegance.
2. Preserve trait boundaries that are already paying off.
3. Prefer working end-to-end slices over adding more dormant modules.
4. Treat green workspace quality gates as a product feature, not cleanup.
5. Do not re-expand scope into macOS until Linux daily-use paths are solid.

## Architecture Focus

The Linux-first program keeps the existing crate boundary:

```text
+------------------------ visor-runtime ------------------------+
| CLI | API | OCI | pool | compose | daemon | state | volume   |
|                                                          ^    |
| Linux-first work: wire real product paths               |    |
+------------------------------+---------------------------|----+
                               | trait boundary            |
+------------------------------v-------------------------------+
|                         visor-vmm                            |
| platform | vm | memory | devices | transport | net | comms |
|                                                          ^   |
| Linux-first work: finish Linux path, keep traits intact  |   |
+--------------------------------------------------------------+
```

The operating rule is:

- `visor-runtime` must not regress back to raw KVM imports.
- `visor-vmm` may remain Linux-heavy internally while Linux is the shipping
  target, but new work should not make the public trait seams worse.

## Workstreams

### WS1 — Restore Workspace Health

Objective: make the workspace buildable and testable again before feature work.

- [ ] Fix `visor-docker` compile break against `visor-types`
- [ ] Add missing crate dependencies required by current code
- [ ] Remove or replace stale trait calls that no longer exist
- [ ] Run `cargo check --workspace`
- [ ] Run `cargo test --workspace`
- [ ] Run `cargo clippy --workspace -- -D warnings`
- [ ] Record remaining environmental blockers separately from repo bugs

Acceptance:

- Workspace compiles without local code errors.
- Failures, if any, are due only to environment or external tooling.

### WS2 — Finish Linux Networking

Objective: make Linux networking real, not just described.

- [ ] Wire virtio-net into the Linux VM boot path
- [ ] Replace hard-coded guest IP assumptions where runtime depends on actual
      network allocation
- [ ] Connect runtime network management to real VM interfaces
- [ ] Make port forwarding depend on actual guest network state
- [ ] Add Linux integration tests for guest connectivity and forwarded ports

Acceptance:

- `visor run -p 8080:80 ...` works on Linux against a running guest service.
- The runtime’s network metadata matches the VM’s actual network path.

### WS3 — Fix Volumes End-to-End

Objective: make bind mounts and named volumes behave correctly on Linux.

- [ ] Decide the Linux-first volume contract: virtio-fs for bind mounts, block
      device path for named volumes, or staged subset
- [ ] Align runtime `VmConfig`, VMM device wiring, and guest mount behavior
- [ ] Remove invalid assumptions about host paths being directly mountable inside
      the guest
- [ ] Add end-to-end tests for read-only and read-write mounts

Acceptance:

- Host bind mount is visible inside guest at the requested guest path.
- Named volume lifecycle and guest mount behavior are test-covered.

### WS4 — Complete Snapshot Save + Warm Pool

Objective: turn snapshot restore into a product feature instead of a partial fast path.

- [ ] Wire snapshot save after successful cold boot
- [ ] Persist snapshot bundles into the snapshot cache
- [ ] Add background pool refill in the daemon
- [ ] Wire health monitoring into pool management
- [ ] Prove warm acquisition and snapshot restore with timing-oriented tests

Acceptance:

- First boot creates a reusable snapshot artifact.
- Subsequent launches can come from warm pool or snapshot restore.

### WS5 — Harden Linux Product Surface

Objective: turn the working Linux core into something marketable.

- [ ] Wire runtime metrics collection to `/v1/metrics`
- [ ] Install seccomp / sandboxing in the Linux daemon path
- [ ] Audit info/health endpoints so they report real capability state
- [ ] Tighten CLI/API behavior around detach, shutdown, console, and exec
- [ ] Expand real Linux integration coverage for core workflows

Acceptance:

- The product story is backed by observable runtime behavior, not only modules.

## Priority Order

| Priority | Workstream | Why                                                   |
| -------- | ---------- | ----------------------------------------------------- |
| P1       | WS1        | Nothing else matters while the workspace is broken    |
| P1       | WS2        | Networking is core to real container-like usability   |
| P1       | WS3        | Volumes are table stakes for developer workflows      |
| P1       | WS4        | Snapshots and pool are core differentiators           |
| P2       | WS5        | Hardening and observability raise product credibility |

## Execution Sequence

1. Make the workspace green again.
2. Finish Linux networking and port-forward realism.
3. Fix Linux volume behavior end-to-end.
4. Wire snapshot save, cache, and pool refill.
5. Wire hardening and observability.
6. Reassess the trait seams only after Linux product paths are solid.

## Success Criteria

- `cargo check --workspace` passes.
- `cargo test --workspace` passes on the Linux development host.
- `cargo clippy --workspace -- -D warnings` passes.
- Linux KVM path supports:
  - OCI image run
  - `exec`
  - console access
  - real guest networking
  - working port forwarding
  - working volume mounts
  - snapshot save/restore and warm pool reuse
- `/v1/info` and `/v1/metrics` describe real capability state.
- The repo state matches the Linux-first product story we can market.

## Out of Scope for This Plan

- Resuming macOS feature-completion as the main track
- New cross-platform abstractions beyond what is required to avoid regression
- Kubernetes/operator work
- New headline features that do not improve Linux daily-use viability

## Immediate Next Step

Move from recovery into product hardening:

- add real Linux forwarded-port reachability e2e coverage
- design daemon isolation so seccomp can be enabled without breaking OCI and
  control-plane paths
- decide when to replace staged read-only directory mounts with direct shared-fs
  bind mounts
