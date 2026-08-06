# 07 — Deployment Modes

## Dual Mode: KVM + Container

visor auto-detects the environment and picks the best isolation:

| Mode      | Isolation                         | Requires   | Restore Speed |
| --------- | --------------------------------- | ---------- | ------------- |
| KVM       | Hardware virtualization (microVM) | `/dev/kvm` | <5ms          |
| Container | Linux namespaces + cgroups        | Any Linux  | ~1ms          |

## KVM Mode (Full Isolation)

Hardware-virtualized microVMs. Each VM is a real Linux kernel running on
virtual hardware, completely isolated from the host.

### Where KVM Works

| Platform                           | How                                |
| ---------------------------------- | ---------------------------------- |
| Bare metal (Hetzner, OVH, on-prem) | Native KVM, always works           |
| AWS `.metal` instances             | i3.metal, c5.metal, m5.metal, etc. |
| AWS C8i / M8i / R8i                | Nested KVM (Feb 2026 — new!)       |
| GCP N1/N2 with nested virt flag    | Supported                          |
| Azure Dv3/Ev3                      | Nested virt supported              |
| Linux laptops/desktops             | Native KVM if CPU has VT-x/AMD-V   |
| macOS (Apple Silicon)              | Apple HVF (separate platform impl) |

## Container Mode (Degraded Isolation)

Linux namespaces + cgroups instead of KVM. Runs anywhere. Weaker isolation but
compatible with any environment — including non-metal cloud instances and CI/CD.

### What Changes

| KVM                          | Container                                        |
| ---------------------------- | ------------------------------------------------ |
| `KVM_CREATE_VM`              | `clone(CLONE_NEWPID\|CLONE_NEWNS\|CLONE_NEWNET)` |
| `KVM_SET_USER_MEMORY_REGION` | Regular process memory                           |
| `virtio-blk`                 | bind-mount or overlayfs rootfs                   |
| `virtio-net`                 | veth pair + netns                                |
| `virtio-vsock`               | Unix domain socket                               |
| Real Linux kernel per VM     | Shared host kernel                               |

visor-init still runs as PID 1, but inside a namespace, not a VM. The API
surface stays identical — users don't need to know which mode is active.

## Auto-Detection

On `visor start`, the daemon checks for `/dev/kvm`:

```rust
fn detect_backend() -> Backend {
    match File::open("/dev/kvm") {
        Ok(f) => {
            let version = unsafe { kvm_get_api_version(f.as_raw_fd()) };
            if version == 12 { Backend::Kvm } else { Backend::Container }
        }
        Err(_) => Backend::Container,
    }
}
```

Override via flag or config:

```bash
visor start --backend kvm          # Force KVM (fails if unavailable)
visor start --backend container    # Force container mode
visor start                        # Auto-detect (default)
```

Logged clearly on startup:

```
INFO  visor started in KVM mode (hardware isolation)
WARN  visor started in container mode (/dev/kvm not available)
```

## /v1/info Endpoint

Clients can check what's available:

```json
{
    "version": "0.1.0",
    "mode": "kvm",
    "features": {
        "kvm": true,
        "container": true,
        "snapshots": true,
        "warm_pool": true,
        "ballooning": false,
        "gpu_passthrough": false,
        "virtio_fs": true
    },
    "host": {
        "arch": "x86_64",
        "cpus": 12,
        "memory_total_mib": 65536,
        "memory_available_mib": 48200,
        "kernel": "6.1.155"
    }
}
```

## Backend Trait

Designed at P0, container implementation at P2:

```rust
#[async_trait]
pub trait ExecutionBackend: Send + Sync {
    async fn create(&self, config: &VmConfig) -> Result<InstanceHandle>;
    async fn exec(&self, id: &str, cmd: &ExecRequest) -> Result<ExecResult>;
    async fn destroy(&self, id: &str) -> Result<()>;
    async fn snapshot(&self, id: &str) -> Result<SnapshotHandle>;
    async fn restore(&self, snap: &SnapshotHandle) -> Result<InstanceHandle>;
}
```

KVM is the P0 implementation. Container slots in at P2 with zero API changes.
Pool, networking, API, CLI — all backend-agnostic.

## Deployment Recommendations

| Use Case                         | Backend   | Instance Type         |
| -------------------------------- | --------- | --------------------- |
| Production (max isolation)       | KVM       | Bare metal or .metal  |
| Production (AWS, cost-sensitive) | KVM       | C8i/M8i/R8i (nested)  |
| Development                      | KVM       | Local Linux with VT-x |
| CI/CD (no KVM available)         | Container | Any Linux instance    |
| macOS development                | KVM (HVF) | Apple Silicon Mac     |

## systemd / launchd Integration

```bash
# Install as systemd service
visor install-service

# Creates /etc/systemd/system/visor.service:
# [Service]
# ExecStart=/usr/local/bin/visor start --foreground
# Restart=always
# ...

# macOS: creates ~/Library/LaunchAgents/rs.visor.plist
```
