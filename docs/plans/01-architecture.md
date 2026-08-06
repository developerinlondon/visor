# 01 — Architecture

## Single Binary, Subcommand-Driven

One binary: `visor`. Subcommands determine behavior.

```
visor start [--foreground]     Start daemon (blocks or daemonizes)
visor stop                     Stop daemon
visor run <image> [cmd]        Run command in new VM
visor exec <vm-id> <cmd>       Execute in existing VM
visor attach <vm-id>           Attach to main process stdin/stdout
visor shell <vm-id>            Interactive shell (toybox, works on all images)
visor console <vm-id>          Serial console (boot logs, low-level access)
visor ps                       List VMs
visor top <vm-id>              Show guest processes
visor info                     Show host capabilities and mode
visor tui                      Terminal UI (live dashboard)
visor compose up               Multi-VM from docker-compose.yml
visor volume create <name>     Create persistent volume
visor images                   List cached images
```

`visor start` becomes the daemon. All other subcommands talk to the daemon
via HTTP. One binary, one `cargo install`, one thing to distribute.

## Why In-Process Matters (livecontainers vs visor)

The fundamental architectural shift: livecontainers spawns a separate process
per VM. visor runs all VMs as threads inside one process.

```
LIVECONTAINERS (process-per-VM):

  lc process (PID 100)
    +-- fork/exec -> firecracker (PID 101)    <- VM-1, separate process
    +-- fork/exec -> firecracker (PID 102)    <- VM-2, separate process
    +-- fork/exec -> firecracker (PID 103)    <- VM-3, separate process

  Every operation crosses a process boundary:
    lc --HTTP PUT--> firecracker PID 101    (configure memory)
    lc --HTTP PUT--> firecracker PID 101    (add drive)
    lc --HTTP PUT--> firecracker PID 101    (add network)
    lc --HTTP PUT--> firecracker PID 101    (start VM)
    lc --SIGKILL--> firecracker PID 101     (destroy VM)


VISOR (all-in-one-process):

  visor process (PID 100)
    +-- main thread (API, pool, metrics)
    +-- vm-1-vcpu-0 thread                  <- just a thread
    +-- vm-1-vcpu-1 thread
    +-- vm-1-io thread
    +-- vm-2-vcpu-0 thread                  <- just a thread
    +-- vm-2-io thread
    +-- vm-3-vcpu-0 thread                  <- just a thread
    +-- ...

  Every operation is a Rust function call:
    vmm.create_vm(config)         // no fork, no exec, no HTTP
    vm.start()                    // direct KVM ioctl
    vm.destroy()                  // drop memory, stop threads
```

What this speeds up:

| Operation           | livecontainers   | visor | Why                                               |
| ------------------- | ---------------- | ----- | ------------------------------------------------- |
| Create VM           | ~5-10ms          | <1ms  | No fork/exec, no REST config                      |
| Snapshot restore    | ~125ms           | <5ms  | No process spawn, no deserialization, direct mmap |
| Destroy VM          | ~5ms (SIGKILL)   | <1ms  | Drop Rust structs, munmap                         |
| Exec command        | ~5ms (vsock UDS) | ~2ms  | vsock already in-process                          |
| VM-to-VM networking | ~100μs (TAP)     | ~10μs | Memory copy between virtqueues                    |
| Pool grab           | N/A              | <1ms  | VM is already running in our threads              |

The other big win is **shared memory**. All VMs' memory lives in visor's address
space. Snapshot restore is just `mmap(MAP_PRIVATE)` — the kernel shares unmodified
pages between all VMs cloned from the same golden snapshot via CoW. With separate
processes (livecontainers), each Firecracker instance has its own isolated address
space — no page sharing possible.

## Daemon-First Design

The daemon must be running before VMs can be created. Everything goes through it.

```
+-----------------------------------------------------------------------+
|  visor start (ONE process — the daemon)                               |
|                                                                       |
|  Main thread                                                          |
|  +-- HTTP API server (REST + SSE)                                     |
|  +-- Pool manager (snapshot cache, pre-warming)                       |
|  +-- Metrics collector (Prometheus export)                            |
|  +-- Disk cache manager (~/.visor/cache/)                             |
|  +-- Network manager (virtual switches, NAT)                         |
|                                                                       |
|  VM-1 threads              VM-2 threads              VM-N threads     |
|  +-- vCPU-0 thread         +-- vCPU-0 thread         +-- ...          |
|  +-- vCPU-1 thread         +-- vCPU-1 thread                         |
|  +-- device I/O thread     +-- device I/O thread                     |
|  +-- vsock handler         +-- vsock handler                         |
|                                                                       |
+---+-------------------------------------------------------------------+
    |
    | HTTP API (REST + SSE)
    |
+---+-------------------------------------------------------------------+
|  visor CLI                 K8s Operator         External Agents       |
|  visor run alpine echo hi  visor-operator       curl /v1/vms          |
|  visor ps                  (CRD reconciler)     SSE /v1/events        |
|  visor tui                                                            |
+-----------------------------------------------------------------------+
```

Why daemon-first (not hybrid):

- **Warm pool** — pre-booted VMs ready in <5ms (requires long-lived process)
- **Disk cache** — snapshots persist to `~/.visor/cache/`. Pool refills from
  cache on restart
- **HTTP API** — agents, K8s operator, external tools talk to daemon
- **SSE events** — real-time VM lifecycle events
- **Metrics** — Prometheus-compatible per-VM metrics
- **Shared networks** — daemon manages virtual switches, VMs communicate internally
- **VM management** — list, inspect, destroy. Only possible with central state

## Process Model — Two Separate Worlds

The host and each VM live in completely separate worlds. They share NO process
tables.

```
+--HOST WORLD (what `ps` sees)----------------------------------------------+
|                                                                           |
|  visor (PID 1234) ← ONE process, that's ALL you see                      |
|    +-- main thread         (API server, pool manager)                     |
|    +-- vm-1-vcpu-0 thread  (stuck in KVM_RUN ioctl)                      |
|    +-- vm-1-vcpu-1 thread  (stuck in KVM_RUN ioctl)                      |
|    +-- vm-1-io thread      (handles virtio I/O)                           |
|    +-- vm-2-vcpu-0 thread  (stuck in KVM_RUN ioctl)                      |
|    +-- vm-2-io thread                                                     |
|                                                                           |
|  $ ps aux | grep visor                                                    |
|  root  1234  visor start        ← that's it. nothing else.               |
|                                                                           |
+---------------------------------------------------------------------------+

+--VM-1 WORLD (completely invisible from host)------------------------------+
|                                                                           |
|  This is a REAL Linux kernel running on virtual hardware.                 |
|  It has its own /proc, its own process table, its own scheduler.          |
|  The host CANNOT see any of this.                                         |
|                                                                           |
|  visor-init (PID 1)        ← guest PID 1                                 |
|    +-- vsock-agent thread  ← listens for commands from daemon             |
|    +-- nginx (PID 45)      ← user's workload                             |
|    |   +-- nginx worker (PID 46)                                          |
|    |   +-- nginx worker (PID 47)                                          |
|    +-- postgres (PID 50)                                                  |
|                                                                           |
+---------------------------------------------------------------------------+
```

**How it works at the CPU level:**

When a vCPU thread calls `ioctl(vcpu_fd, KVM_RUN)`, the CPU hardware switches
to guest context (VMENTER). The guest kernel schedules its own processes —
nginx, postgres, cron — all inside that one vCPU thread. When the guest does
I/O, the CPU traps back to host context (VMEXIT), visor handles the I/O, then
re-enters guest mode.

A single vCPU thread can run hundreds of guest processes. The number of guest
processes has zero effect on host process count.

**Guest processes are invisible from the host.** They exist only in the guest
kernel's memory. `ps` on the host shows only the visor process. To see guest
processes, use `visor top <vm-id>` (queries visor-init over vsock).

## Memory Model

### Demand Paging

When visor creates a VM with `memory = 10GB`, it calls `mmap()` with
`MAP_ANONYMOUS | MAP_NORESERVE | MAP_PRIVATE`. This reserves 10GB of virtual
address space but consumes zero physical RAM. Pages are allocated one at a time
(4KB each) only when the guest touches them.

A 10GB VM running `echo hello` uses ~30-50MB of physical RAM. The same VM
running a build might use 4GB. You only pay for what's touched.

### Copy-on-Write Snapshots

Snapshot restore uses `mmap(MAP_PRIVATE, snapshot_file)`:

- **Reads** → shared via page cache (all VMs from same snapshot share pages)
- **Writes** → triggers CoW copy to a private page for that VM only

5 idle pool VMs share 95%+ of their memory pages. Each consumes only ~5-20MB
of dirty pages, not 5× the full memory.

### Overcommit

`MAP_NORESERVE` tells the kernel not to reserve swap. You can run 20 VMs
configured at 4GB each on a 32GB host — as long as their combined working
sets fit. Ballooning (P1) reclaims unused memory from idle VMs.

## visor-init (Guest PID 1)

Minimal static musl binary (~1MB) that runs INSIDE each VM:

1. Mount /proc, /sys, /dev, /dev/pts, /dev/shm, /tmp, /run
2. Mount rootfs from /dev/vdb (OCI image as ext4)
3. Configure guest networking via raw ioctls (no iproute2 needed)
4. Start vsock agent on port 52 (JSON-RPC 2.0)
5. Mount additional volumes at specified paths
6. Spawn user command
7. Reap zombies, forward signals

The user never talks to visor-init directly. They talk to the daemon, which
proxies via vsock. visor-init has no business logic, no HTTP, no pool knowledge.

## User Flow

```
$ visor start                              # Start daemon
  Auto-detects KVM or container mode
  Loads disk cache, pre-warms pool

$ visor run alpine:3.20 echo hello         # <3ms if pool hit
  CLI ──POST /v1/vms──> daemon
  Daemon grabs pre-warmed VM
  Daemon ──vsock──> visor-init ──exec──> echo hello
  stdout: "hello"

$ visor run -p 8080:80 nginx:alpine        # Port forwarding
  Daemon boots VM, maps host:8080 → guest:80
  curl localhost:8080 → nginx inside VM

$ visor exec <vm-id> bash                  # Interactive shell
  PTY proxied: CLI ──WebSocket──> daemon ──vsock──> visor-init

$ visor tui                                # Live dashboard
  Terminal UI showing all VMs, metrics, events
```

## Built-in VM Access (No SSH Required)

visor owns PID 1 (visor-init) in every VM. This means we can provide access to
ANY image — even scratch/distroless/single-binary images with no shell and no
SSH. Docker can't do this because it doesn't control the container's PID 1.

Three levels of access:

### `visor exec` — run a command (needs shell in image)

```bash
visor exec <vm-id> bash        # works if image has bash
visor exec <vm-id> sh          # works if image has sh
# Doesn't work on scratch/distroless — no shell to exec
```

### `visor attach` — serial console (works on everything)

Connect directly to the VM's serial TTY. See boot logs, interact with console.
Zero additional cost — serial device is already a P0 virtio device.

```bash
visor attach <vm-id>
[    0.000000] Linux version 7.0.0-rc1 ...
[    0.100000] visor-init: mounting rootfs
[    0.120000] visor-init: starting assay binary
assay output here...
```

### `visor shell` — interactive shell (works on everything)

The killer feature. A full interactive shell inside any VM — even scratch/
distroless images with no shell. visor-init invokes a bundled toybox binary
that provides sh + ~200 Unix commands.

This works because visor-init runs inside the user's rootfs (after pivot_root).
The shell operates on the user's actual files, processes, and network.

```bash
visor shell <vm-id>

/ # ls /app              <- browsing the user's rootfs
mybin  config.yaml

/ # cat /app/config.yaml  <- reading the user's files
database: db:5432

/ # ps aux                <- seeing the user's processes
PID   CMD
1     visor-init
42    /app/mybin

/ # wget http://db:5432/health   <- testing connectivity from inside the VM
200 OK

/ # top                   <- what's eating memory
/ # netstat -tlnp         <- what ports are open
/ # grep ERROR /var/log/*  <- searching logs
```

**Toybox** (BSD-0 license, ~200 KB static binary) provides the shell and Unix
commands. It ships as a separate binary on the init drive — NOT linked into
visor-init. No licensing concerns for commercial use. Used by Android since 6.0.

Why toybox over Rust alternatives: uutils/coreutils (Rust) compiles to ~14 MB
static multicall and includes no shell. nushell is ~70 MB. No Rust project comes
within 10x of toybox's 200 KB for 200 commands — C's minimal runtime wins here.

```
Init drive (/dev/vda, 5 MiB):
  /visor-init          ~1 MiB    our binary (PID 1, vsock agent, boot)
  /toybox              ~200 KB   sh + 200 Unix commands (BSD-0 licensed)
  /run.json            ~200 B    execution config (cmd, env, network)
```

Cost: ~200 KB on the init drive. Zero runtime overhead when not in use.
Shared via CoW across all VMs from the same golden snapshot — 100 VMs = one
physical copy in RAM.

Kubernetes comparison: `kubectl debug` injects a separate sidecar container
(clunky, doesn't share PID/filesystem namespace cleanly). visor-init is already
inside the VM with full access — no injection needed.

### Shell Security Model

The shell runs inside KVM hardware isolation — an attacker with shell access
is sandboxed in the VM, not on the host. This is strictly more secure than
`docker exec` which runs in the same kernel namespace.

Configurable guardrails:

```
# Global daemon config (~/.visor/config.toml)
[shell]
enabled = true                 # Master switch (default: true)
network_tools = true           # Include wget/nc/curl in toybox (default: true)
idle_timeout = "30m"           # Auto-disconnect after idle period
audit_log = true               # Log every shell session (default: true)

# Per-VM override
visor run --shell=false alpine  # Disable shell for this VM
visor run --shell-network=false alpine  # Shell without network tools
```

| Guardrail             | Default     | Description                                      |
| --------------------- | ----------- | ------------------------------------------------ |
| `shell.enabled`       | `true`      | Master switch — disable globally or per-VM       |
| `shell.network_tools` | `true`      | Include wget/nc/curl in toybox build             |
| `shell.idle_timeout`  | `30m`       | Auto-disconnect after idle period                |
| `shell.audit_log`     | `true`      | Structured log: timestamp, user, VM ID, duration |
| Init drive mount      | `ro,noexec` | Remounted exec only during active session        |

For hardened deployments: set `enabled = false` globally, override per-VM
as needed. The toybox binary is inert data on the init drive when disabled —
visor-init refuses the shell vsock command entirely.
