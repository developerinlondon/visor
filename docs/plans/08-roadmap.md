# 08 — Feature Roadmap

Nothing cut. Enterprise features are requirements, not nice-to-haves.

## P0 — Foundation

Must work end-to-end: `visor start` → `visor run alpine echo hello` → output.

### visor-machine (VMM core)

| Feature               | File(s)                  | Description                             |
| --------------------- | ------------------------ | --------------------------------------- |
| KVM VM lifecycle      | platform/linux.rs, vm.rs | Create VM, set memory, start/stop       |
| Guest memory          | memory.rs                | mmap(MAP_ANONYMOUS), demand paging      |
| Kernel loading        | boot/x86_64.rs           | Load vmlinux ELF, set up GDT/MSRs/CPUID |
| vCPU run loop         | vcpu.rs                  | KVM_RUN loop, VMEXIT handling           |
| virtio-blk            | devices/block.rs         | Host file → guest /dev/vdX              |
| virtio-net            | devices/net.rs           | Connect to virtual switch               |
| virtio-vsock          | devices/vsock.rs         | Host↔guest communication                |
| Serial console        | devices/serial.rs        | UART 16550 for boot logs                |
| virtio-mmio transport | transport/mmio.rs        | Device discovery for guest kernel       |

### visor-runtime (daemon + CLI)

| Feature                | File(s)               | Description                                |
| ---------------------- | --------------------- | ------------------------------------------ |
| ExecutionBackend trait | backend.rs            | KVM impl (P0), container impl (P2)         |
| Daemon (`visor start`) | daemon.rs             | HTTP server, pool, metrics loop            |
| CLI (subcommands)      | cli/*.rs              | run, exec, shell, attach, ps, info, stop   |
| HTTP API (REST + SSE)  | api/*.rs              | Full CRUD + events                         |
| SSE event filtering    | api/sse.rs            | Query params, per-VM streams               |
| `/v1/info` endpoint    | api/routes/info.rs    | Host capabilities, mode                    |
| OCI pull → ext4 → boot | oci/*.rs              | Registry, cache, layer merge, rootfs build |
| Shared networking      | net/switch.rs, nat.rs | Virtual switch, one TAP per network        |
| Embedded DNS           | net/dns.rs            | Service discovery + upstream forwarding    |
| Port forwarding        | net/port_forward.rs   | -p host:guest mapping                      |
| IP allocation          | net/ip_alloc.rs       | Per-network subnet management              |
| Auto-detect KVM        | backend.rs            | /dev/kvm check, fallback to container      |
| Unix socket API        | daemon.rs             | Default listener (no TLS needed)           |
| TCP + TLS API          | daemon.rs             | Optional remote access                     |

### visor-init (guest PID 1)

| Feature                  | File(s)       | Description                             |
| ------------------------ | ------------- | --------------------------------------- |
| Mount sequence           | mount.rs      | /proc, /sys, /dev, pivot_root           |
| Guest networking         | network.rs    | Raw ioctls, no iproute2                 |
| vsock agent              | agent.rs      | JSON-RPC 2.0 on port 52                 |
| Exec + signal forwarding | entrypoint.rs | Spawn user command, reap zombies        |
| Volume mounts            | volume.rs     | Mount additional /dev/vdX at paths      |
| Shell access             | shell.rs      | Toybox interactive shell (configurable) |

## P1 — Enterprise Essentials

Pool, snapshots, metrics, observability.

### visor-machine

| Feature                 | Description                               |
| ----------------------- | ----------------------------------------- |
| Custom snapshot/restore | <5ms save/restore, golden snapshot format |
| I/O rate limiting       | Per-drive and per-NIC throttling          |
| Per-VM metrics          | CPU, memory, disk I/O, network counters   |
| virtio-rng              | Entropy source for guest                  |
| Memory ballooning       | Reclaim unused guest memory               |
| Huge pages              | 2 MiB / 1 GiB pages for large VMs         |
| Seccomp filtering       | Restrict VMM syscalls                     |
| virtio-fs               | Host directory passthrough (bind mounts)  |

### visor-runtime

| Feature            | Description                                      |
| ------------------ | ------------------------------------------------ |
| Warm pool          | Per-image pools, background refill, <5ms acquire |
| Disk cache         | Persist golden snapshots to ~/.visor/cache/      |
| Prometheus export  | /v1/metrics endpoint                             |
| `visor top`        | Guest process list via vsock                     |
| `visor tui`        | Terminal dashboard (ratatui)                     |
| Volume management  | visor volume create/ls/rm/resize                 |
| Compose networking | Per-project isolated networks                    |
| Health checks      | Pool VM health via vsock ping                    |
| mTLS               | Client cert auth for API                         |

## P2 — Scale + GPU + Cross-Platform

| Feature                 | Crate          | Description                      |
| ----------------------- | -------------- | -------------------------------- |
| PCI transport           | visor-machine  | virtio-pci (needed for VFIO)     |
| VFIO/GPU passthrough    | visor-machine  | Pass host GPU to guest           |
| CPU templates           | visor-machine  | Standardize CPUID across hosts   |
| Dirty page tracking     | visor-machine  | For live migration               |
| Live migration          | visor-runtime  | Move running VM between hosts    |
| Apple HVF backend       | visor-machine  | macOS support (Apple Silicon)    |
| Container backend       | visor-runtime  | Namespace isolation fallback     |
| Internal virtual switch | visor-runtime  | In-process inter-VM fast path    |
| K8s operator            | visor-operator | CRD reconciler                   |
| Compose                 | visor-runtime  | docker-compose.yml support       |
| Hostname routing        | visor-runtime  | Simple Host-header based routing |
| systemd/launchd install | visor-runtime  | `visor install-service`          |
