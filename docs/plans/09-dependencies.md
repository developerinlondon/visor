# 09 — Dependencies and Workspace Layout

## Workspace Structure

```
visor/
+-- Cargo.toml                          # Workspace root
+-- AGENTS.md                           # AI agent rules
+-- docs/plans/                         # These planning docs
+-- crates/
|   +-- visor-machine/                  # VMM core
|   |   +-- src/
|   |       +-- lib.rs
|   |       +-- vm.rs
|   |       +-- vcpu.rs
|   |       +-- memory.rs
|   |       +-- snapshot.rs
|   |       +-- config.rs
|   |       +-- metrics.rs
|   |       +-- platform/
|   |       |   +-- mod.rs
|   |       |   +-- linux.rs            # KVM
|   |       |   +-- macos.rs            # Apple HVF
|   |       +-- devices/
|   |       |   +-- mod.rs
|   |       |   +-- block.rs            # virtio-blk
|   |       |   +-- net.rs              # virtio-net
|   |       |   +-- vsock.rs            # virtio-vsock
|   |       |   +-- serial.rs           # UART 16550
|   |       |   +-- rng.rs              # virtio-rng
|   |       |   +-- balloon.rs          # virtio-balloon
|   |       |   +-- gpu.rs              # VFIO passthrough
|   |       |   +-- fs.rs               # virtio-fs
|   |       +-- transport/
|   |       |   +-- mmio.rs             # virtio-mmio (P0)
|   |       |   +-- pci.rs              # virtio-pci (P2)
|   |       +-- rate_limit/
|   |       |   +-- disk.rs
|   |       |   +-- net.rs
|   |       +-- seccomp.rs
|   |       +-- boot/
|   |           +-- x86_64.rs
|   |           +-- aarch64.rs
|   |
|   +-- visor-runtime/                  # Daemon + CLI + orchestration
|   |   +-- src/
|   |       +-- lib.rs
|   |       +-- main.rs                 # Single binary entry point
|   |       +-- daemon.rs
|   |       +-- backend.rs              # ExecutionBackend trait
|   |       +-- oci/
|   |       +-- net/
|   |       +-- vsock/
|   |       +-- pool/
|   |       +-- api/
|   |       +-- cli/
|   |       +-- compose/
|   |       +-- tui/
|   |
|   +-- visor-init/                     # Guest PID 1 (static musl)
|   +-- visor-kernel/                   # Kernel download/build
|   +-- visor-operator/                 # K8s operator (P2)
```

## Crate Naming

| Crate            | Purpose                                                 |
| ---------------- | ------------------------------------------------------- |
| `visor-machine`  | VMM core — KVM/HVF, vCPU, devices, snapshots            |
| `visor-runtime`  | Orchestration — daemon, OCI, networking, pool, API, CLI |
| `visor-init`     | Guest PID 1 (static musl binary, runs inside VM)        |
| `visor-kernel`   | Guest kernel download/build/resolution                  |
| `visor-operator` | K8s operator (CRD reconciler, P2)                       |

Single binary output: `visor` (from visor-runtime's main.rs).

## rust-vmm Crate Versions

Pinned to Firecracker's proven versions (Feb 2026):

| Crate           | Version | Purpose                            |
| --------------- | ------- | ---------------------------------- |
| kvm-ioctls      | 0.24.0  | KVM VM/vCPU/device ioctls          |
| kvm-bindings    | 0.14.0  | KVM struct bindings                |
| vm-memory       | 0.17.1  | Guest memory (mmap, GuestAddress)  |
| linux-loader    | 0.13.2  | Load vmlinux ELF into guest memory |
| vm-superio      | 0.8.1   | Serial console (UART 16550)        |
| vmm-sys-util    | 0.15.0  | EventFd, epoll, terminal utils     |
| event-manager   | 0.4.2   | epoll event loop                   |
| vm-allocator    | 0.1.3   | MMIO/PIO address space allocator   |
| vm-device       | 0.1.0   | Device bus traits (IoManager)      |
| virtio-queue    | 0.17.0  | Virtqueue implementation           |
| virtio-bindings | 0.2.7   | Virtio spec constants              |
| virtio-blk      | latest  | Block device                       |
| virtio-vsock    | 0.11.0  | Vsock device                       |
| seccompiler     | latest  | Seccomp BPF generation             |
| applevisor      | 1.0     | Apple HVF bindings (macOS only)    |

## Runtime Dependencies

| Crate                | Version | Purpose                       |
| -------------------- | ------- | ----------------------------- |
| tokio                | 1.49+   | Async runtime                 |
| axum                 | 0.8+    | HTTP API server               |
| clap                 | 4.5+    | CLI argument parsing          |
| serde / serde_json   | 1.0+    | Serialization                 |
| thiserror            | 2.0+    | Library error types           |
| anyhow               | 1.0+    | Binary error context          |
| tracing              | 0.1+    | Structured logging            |
| ratatui              | latest  | TUI dashboard                 |
| hickory-dns          | latest  | Embedded DNS resolver         |
| bincode              | 2.0+    | Snapshot serialization        |
| reqwest              | 0.13+   | OCI registry client           |
| oci-distribution     | 0.11+   | OCI pull protocol             |
| nix                  | 0.31+   | Unix syscalls (ioctl, signal) |
| metrics + prometheus | 0.24+   | Prometheus metrics export     |
| rustls               | 0.23+   | TLS (FIPS via aws-lc-rs)      |
| aws-lc-rs            | 1.16+   | FIPS 140-3 crypto backend     |
| rustls-pemfile       | 2.0+    | PEM certificate parsing       |
| rustls-pki-types     | 1.0+    | PKI type definitions          |

## Rust Toolchain

- Edition: 2024
- Resolver: 3
- Nightly: 1.95+
- Targets: `x86_64-unknown-linux-gnu` (daemon), `x86_64-unknown-linux-musl` (visor-init)
- Future: `aarch64-apple-darwin` (macOS)
- FIPS build deps: CMake, Go, C compiler (CI-only, for aws-lc-fips-sys)

## References from livecontainers

Ideas and lessons, not code:

| Area           | What we learned                                               |
| -------------- | ------------------------------------------------------------- |
| OCI pipeline   | Layer merge order, ext4 sizing (1.2× + 50 MiB), cache layout  |
| Networking     | TAP+iptables NAT, DNS DNAT, cleanup on drop                   |
| Guest init     | Mount sequence, vsock agent, pivot_root, raw ioctl networking |
| vsock protocol | JSON-RPC 2.0, ping/exec/write_file, port 52                   |
| Testing        | VmGuard RAII pattern, atomic subnet offsets                   |
| Integration    | iptables eval for `!` rules, kernel boot args per backend     |

## CI Security Tooling

| Tool          | Purpose                                          |
| ------------- | ------------------------------------------------ |
| `cargo-deny`  | License audit + banned crates + vulnerability DB |
| `cargo-audit` | RustSec advisory database CVE check              |
| `cargo-sbom`  | SBOM generation (CycloneDX + SPDX)               |
| `cosign`      | Binary signing (Sigstore)                        |
| `syft`        | Binary-level SBOM scanning (secondary)           |
| `dprint`      | Code formatting (non-negotiable)                 |
