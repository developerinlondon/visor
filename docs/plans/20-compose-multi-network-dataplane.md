# 20 — Compose Multi-Network Dataplane

| Field        | Value                                                                                   |
| ------------ | --------------------------------------------------------------------------------------- |
| Status       | Complete                                                                                |
| Created      | 2026-03-10                                                                              |
| Dependencies | Plans 19, 04, 03, 01                                                                    |
| Scope        | True multi-network guest dataplane for Docker Compose, with portable attachment types   |
| Goal         | Turn Compose network membership into real guest network attachments instead of metadata |

## Problem Statement

visor already understands Compose network membership at the control-plane level:

- service aliases are scoped to shared networks,
- project-qualified names do not leak across unrelated services,
- Compose stacks can express `frontend`, `backend`, and bridge-like topologies.

The missing piece was the dataplane. Compose network membership already worked
at the control-plane level, but it did not yet produce real guest attachments.
This follow-on closes that gap on Linux.

This plan closes that gap on Linux while keeping the type system portable for a
later macOS backend.

## Product Goal

The target behavior is:

- a service attached only to `frontend` has only a `frontend` guest attachment,
- a service attached only to `backend` has only a `backend` guest attachment,
- a bridge service attached to both sees two guest interfaces,
- only services sharing a declared network can reach each other on that network,
- published host ports still work for the service's primary attachment.

## Architecture Frame

```text
+---------------- Docker / Compose ----------------+
| compose.yaml networks -> endpoint memberships    |
+-------------------------+------------------------+
                          |
                          v
+--------------- Visor Runtime -------------------+
| VmConfig networks[] -> resolved attachments[]    |
| primary attachment | optional secondary links    |
+-------------------------+------------------------+
                          |
                          v
+---------------- Linux Host ----------------------+
| bridge per logical network                        |
| tap per guest attachment                          |
| NAT per bridge subnet                             |
| DNAT to primary guest attachment for host ports   |
+-------------------------+------------------------+
                          |
                          v
+---------------- Guest VM ------------------------+
| eth0 / eth1 / ethN                                |
| one default route on primary attachment           |
| per-network reachability from real interfaces     |
+--------------------------------------------------+
```

## Design Decisions

### Portable Attachment Model

The shared types should move from a single guest network to a list of guest
network attachments.

That model should be portable:

- Linux can implement shared-network bridges now,
- macOS can keep a smaller subset later,
- Docker, Compose, native CLI, future Kubernetes, and broader VM work can all
  target the same attachment model.

### Linux Implementation Strategy

For Linux, the first real dataplane should use:

- one bridge per logical network,
- one TAP device per guest attachment,
- guest IP allocation inside the network subnet,
- a single primary attachment that owns the guest default route,
- host port publishing against the primary attachment.

This is more honest than keeping a flat host supernet and trying to fake
network isolation with alias filtering.

### Rollout Boundary

The existing single-network path should remain valid for ordinary `visor run`
and simple Docker workloads while multi-network Compose traffic moves onto the
new attachment model.

## Exit Criteria

- [x] `VmConfig`, guest init config, and VMM config all accept multiple attachments
- [x] Linux guests boot with multiple virtio-net devices when requested
- [x] Compose network membership produces real guest attachments
- [x] frontend-only services cannot reach backend-only services by dataplane
- [x] bridge services attached to both networks can reach both peers
- [x] published host ports still work after the multi-network changes
- [x] focused unit tests and Docker e2e coverage pass

## Task List

- [x] add portable multi-attachment types in `visor-types`, `visor-init`, and `visor-vmm`
- [x] keep default single-network CLI and Docker flows working on the new type shape
- [x] implement Linux bridge-per-network host resources and attachment lifecycle
- [x] teach the guest init path to configure multiple interfaces and a primary route
- [x] map Docker Compose network memberships to real attachments
- [x] preserve host port publishing against the primary attachment
- [x] add unit tests for attachment resolution and Linux network shape
- [x] add Docker Compose e2e coverage for `frontend`, `backend`, and `bridge`
- [x] update beta compatibility and operations docs with the new network truth

## Result

Linux Compose services now get real guest network attachments for each declared
network:

- frontend-only services boot with a frontend NIC only
- backend-only services boot with a backend NIC only
- bridge services boot with both attachments
- service aliases are only injected on shared networks
- host port publishing still targets the primary attachment

The Linux bridge rule setup is now reconciled, not append-only, so stale
bridge-filter rules from older runs are refreshed into the correct order before
traffic starts flowing.

## Validation

- `cargo test -p visor-types --quiet`
- `cargo test -p visor-init --quiet`
- `cargo test -p visor-vmm --quiet`
- `cargo test -p visor-runtime --quiet`
- `cargo check --workspace --quiet`
- `cargo test --test e2e e2e_nested_builder_vm_reaches_alpine_mirrors_and_runs_qemu_img -- --exact --quiet`
- `cargo test --test e2e_docker docker_compose_multi_network_scopes_service_resolution -- --exact --quiet`
