# 00 — visor Overview

> Single binary. Daemon-first. <5ms snapshot restore. Linux + macOS.

## What is visor?

visor runs OCI container images as isolated microVMs. One binary (`visor`) handles
everything — daemon, CLI, VM management, networking, snapshots, metrics.

## Why Build Our Own VMM?

We currently spawn Firecracker and Cloud Hypervisor as external binaries. Problems:

- **3 external binary dependencies** — users install firecracker + jailer +
  cloud-hypervisor at the right versions and paths
- **Process boundary overhead** — every VM operation crosses a process boundary.
  To create a VM, we fork/exec the VMM binary, then send 4-6 HTTP PUTs to
  configure it (memory, kernel, drives, network, vsock), then another PUT to
  start it. That internal REST API adds ~5-10ms per VM and complicates error
  handling
- **No snapshot control** — Firecracker's restore is ~125ms, Cloud Hypervisor's
  is ~180-350ms. Both are opaque — we can't optimize what we don't own
- **No cross-platform** — both are Linux-only. Mac requires a completely
  different stack
- **Firecracker dead weight** — designed for AWS Lambda. Ships with MMDS
  metadata service, jailer sandbox, its own metrics/logging system. Enterprise
  features we need (ballooning, rate limiting, GPU passthrough, CPU templates)
  are either missing or designed for Lambda, not us
- **Cloud Hypervisor overhead** — designed for general-purpose cloud VMs. Has
  features we DO want (PCI transport, VFIO/GPU passthrough) but bundled with
  stuff we don't need (PCI hotplug, NUMA topology, vDPA, PMem/DAX, full UEFI
  boot, ACPI tables). Can't cherry-pick. Slower snapshot restore than Firecracker
- **Neither embeds in-process** — both are standalone binaries. visor exposes
  a REST API too — but that's the external API for users/CLI/K8s. Internally,
  VM management is direct Rust function calls. No serialization, no HTTP, no
  process boundary

## What visor Gives Us

- **Single binary** — `visor` ships as one binary. No external deps
- **Cross-platform** — same crate, `#[cfg(target_os)]` for Linux (KVM) vs Mac (HVF)
- **Custom snapshots** — <5ms restore (25x faster than Firecracker's ~125ms)
- **Full feature control** — ballooning, rate limiting, GPU, metrics designed
  for AI agent workloads
- **In-process VMM** — VMs are threads in the daemon, not separate processes
- **Shared networking** — internal virtual switch for inter-VM communication,
  one NAT per network instead of per VM
- **Dual mode** — KVM (hardware isolation) or container (namespace isolation),
  auto-detected

## Key Numbers

| Metric                | Firecracker              | Cloud Hypervisor     | visor                |
| --------------------- | ------------------------ | -------------------- | -------------------- |
| Cold boot             | ~125ms                   | ~130ms               | ~110ms               |
| Snapshot restore      | ~125ms                   | ~180-350ms           | <5ms                 |
| Pool hit (pre-warmed) | N/A                      | N/A                  | <3ms                 |
| Process model         | 1 process per VM         | 1 process per VM     | All VMs in 1 process |
| Binary deps           | 2 (firecracker + jailer) | 1 (cloud-hypervisor) | 0                    |
| Cross-platform        | Linux only               | Linux only           | Linux + macOS        |

## Project Origin

visor is a fresh codebase, not a refactored livecontainers. We reference
livecontainers for lessons learned (OCI pipeline, networking edge cases, vsock
protocol, VmGuard RAII pattern) but write everything from scratch to leverage
the in-process daemon-first architecture.

## Distribution

One codebase, separate binaries per OS/arch. Rust `#[cfg(target_os)]` handles
platform divergence at compile time — Linux uses KVM, macOS uses Apple HVF.
Same source, same Cargo.toml.

```
Releases:
  visor-v0.1.0-linux-x86_64.tar.gz      <- Linux (KVM)
  visor-v0.1.0-darwin-arm64.tar.gz      <- macOS (Apple HVF)
  visor-v0.1.0-linux-aarch64.tar.gz     <- Linux ARM (future)
```

## Related Plans

| Doc                                                 | Contents                                          |
| --------------------------------------------------- | ------------------------------------------------- |
| [01-architecture](01-architecture.md)               | Process model, daemon design, memory model        |
| [02-visor-machine](02-visor-machine.md)             | VMM core: platform abstraction, devices, boot     |
| [03-visor-runtime](03-visor-runtime.md)             | Daemon, pool, API, CLI, OCI pipeline              |
| [04-networking](04-networking.md)                   | Shared networks, internal switch, port forwarding |
| [05-disks-and-volumes](05-disks-and-volumes.md)     | virtio-blk, virtio-fs, volume management          |
| [06-snapshot-and-pool](06-snapshot-and-pool.md)     | Golden snapshots, warm pool, disk cache           |
| [07-deployment](07-deployment.md)                   | KVM/Container modes, auto-detect, AWS/cloud       |
| [08-roadmap](08-roadmap.md)                         | P0/P1/P2 feature breakdown                        |
| [09-dependencies](09-dependencies.md)               | rust-vmm crates, workspace layout                 |
| [10-risks-and-decisions](10-risks-and-decisions.md) | Risk assessment, decided/open questions           |
