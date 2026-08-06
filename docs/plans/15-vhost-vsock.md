# vhost-vsock: In-Kernel Vsock Data Path (Linux)

| Field        | Value                                             |
| ------------ | ------------------------------------------------- |
| Status       | Draft                                             |
| Created      | 2026-03-02                                        |
| Author       | AI Agent                                          |
| Dependencies | P6.1 (vsock queue processing, macOS HVF) complete |
| Crates       | `visor-vmm`                                       |
| Targets      | Linux (KVM) only                                  |

## 1. The Problem

Our current vsock implementation processes virtio-vsock queues entirely in userspace. Every packet
traverses this path:

1. Guest writes to virtqueue descriptor
2. VM exit (MMIO write for `QueueNotify`)
3. Userspace `VsockDevice::process_queue()` walks the descriptor chain
4. Data copied through `VsockMuxer` to a UDS socket

This works, and it's what we ship on macOS (HVF has no vhost). On Linux, KVM provides
`vhost-vsock` — a kernel module that processes vsock virtqueues in-kernel, eliminating VM exits
for data transfer. Firecracker uses this exclusively.

The performance gap is real:

| Path             | VM exits per packet | Descriptor walk | Memory copies |
| ---------------- | ------------------- | --------------- | ------------- |
| Userspace (ours) | 1 per packet        | Userspace       | 2+            |
| vhost-vsock      | 0 (data path)       | Kernel          | 0 (zero-copy) |

VM exits are expensive: each one is a full context switch between guest and host. At high vsock
throughput (e.g., streaming logs, large file transfers), the userspace path becomes the bottleneck.
vhost-vsock moves the hot path into the kernel, where the virtqueue is processed directly without
exiting the VM.

## 2. Architecture

### Current (userspace, all platforms)

```
Guest
  → virtqueue write
  → MMIO exit (QueueNotify)
  → VsockDevice::process_queue()
  → VsockMuxer
  → UDS socket
```

### Proposed (Linux with vhost-vsock)

```
Guest
  → virtqueue write
  → vhost-vsock kernel module
  → /dev/vhost-vsock
  → host AF_VSOCK socket
```

The userspace path remains for macOS (HVF). No vhost on macOS.

### Key Design Decisions

**`VsockDevice` stays.** Feature negotiation, config space reads (guest CID), and virtqueue
setup still happen in userspace. What changes is who processes the queues after activation.
On Linux with vhost available, `process_queue()` becomes a no-op — the kernel handles it.

**`CommsBackend` is already the right abstraction.** `comms/linux.rs` implements `AF_VSOCK`
client connections. With vhost-vsock active, visor-runtime connects to the guest via `AF_VSOCK`
directly instead of going through the UDS muxer. The trait surface doesn't change.

**`VsockMuxer` is bypassed on Linux.** The muxer exists to translate between UDS (host side)
and vsock (guest side) in the userspace path. With vhost-vsock, the kernel handles that
translation. The muxer code stays for macOS but is not instantiated on Linux when vhost is
available.

**Fallback is automatic.** If `/dev/vhost-vsock` is absent (older kernel, container without
the device node), we fall back to the userspace path transparently. The caller sees no
difference.

## 3. Phases

### Phase 0: vhost-vsock Kernel Interface

**Files:** `crates/visor-vmm/src/comms/linux.rs` (MODIFY)

Open `/dev/vhost-vsock` and configure it:

- `VHOST_SET_OWNER` — claim ownership of the vhost instance
- `VHOST_VSOCK_SET_GUEST_CID` — assign the guest CID
- `VHOST_SET_MEM_TABLE` — register guest memory regions with the kernel
- Wire the vhost fd to KVM via `VHOST_SET_VRING_*` ioctls (see Phase 1)

The ioctl numbers are stable and match what Firecracker uses. We pin them as constants rather
than pulling in a vhost bindings crate.

Estimated: ~200-300 LOC (production) + ~100 LOC (tests with mock fd)

### Phase 1: VirtQueue Handoff

**Files:** `crates/visor-vmm/src/devices/vsock.rs` (MODIFY), `vm.rs` (MODIFY)

After `VsockDevice::activate()`, on Linux with vhost available, hand the virtqueues to the
kernel instead of processing them in userspace:

- `VHOST_SET_VRING_NUM` — queue depth
- `VHOST_SET_VRING_ADDR` — descriptor table, avail ring, used ring addresses
- `VHOST_SET_VRING_BASE` — last used index (for resume after snapshot)
- `VHOST_SET_VRING_KICK` — eventfd the guest kicks to notify the kernel
- `VHOST_SET_VRING_CALL` — eventfd the kernel uses to interrupt the guest

`VsockDevice::process_queue()` gains a `vhost_active: bool` field. When true, it returns
`Ok(false)` immediately — the kernel already handled it.

Estimated: ~200-300 LOC (production) + ~150 LOC (tests)

### Phase 2: Host Socket Integration

**Files:** `crates/visor-vmm/src/comms/linux.rs` (MODIFY)

Replace the UDS muxer path with direct `AF_VSOCK` host-side sockets. When vhost is active,
visor-runtime connects to the guest using `AF_VSOCK` with the guest's CID and the target port.
`LinuxCommsBackend::connect()` already does this — no trait changes needed.

What changes: the muxer is not started when vhost is active. The `CommsBackend` is used
directly by visor-runtime's connection handler.

Estimated: ~100-200 LOC

### Phase 3: Fallback and Feature Detection

**Files:** `crates/visor-vmm/src/comms/linux.rs` (MODIFY), `vm.rs` (MODIFY)

Detect `/dev/vhost-vsock` at VM boot time:

```
if /dev/vhost-vsock exists and opens successfully:
    use vhost path
    log::info!("vsock: using vhost-vsock (in-kernel)")
else:
    use userspace path
    log::info!("vsock: using userspace muxer (vhost-vsock unavailable)")
```

This handles:

- Older kernels without `vhost_vsock` module
- Containers that don't expose `/dev/vhost-vsock`
- CI environments where the device node is absent

Estimated: ~50-100 LOC

## 4. Risks

| Risk                                           | Impact | Mitigation                                              |
| ---------------------------------------------- | ------ | ------------------------------------------------------- |
| `/dev/vhost-vsock` absent in some environments | MEDIUM | Automatic fallback to userspace path (Phase 3)          |
| vhost-vsock ioctl interface changes            | LOW    | Pin stable ioctl numbers; same constants as Firecracker |
| Snapshot interaction with vhost state          | MEDIUM | Pause vhost before snapshot; restore vring base after   |
| CID collision between VMs                      | LOW    | Unique CIDs per VM, same as current userspace path      |
| vhost fd leak on VM teardown                   | LOW    | Wrap in RAII struct; drop closes the fd                 |

**Snapshot interaction** deserves detail. When taking a snapshot, the vhost kernel thread must
be paused before dumping vring state (`last_used_idx`, `last_avail_idx`). On restore, we call
`VHOST_SET_VRING_BASE` with the saved indices before re-enabling the vhost thread. This is the
same approach Firecracker uses.

## 5. File Map

| File                                         | Action | Description                                          |
| -------------------------------------------- | ------ | ---------------------------------------------------- |
| `crates/visor-vmm/src/comms/linux.rs`        | MODIFY | vhost-vsock device open, ioctl wrappers, fallback    |
| `crates/visor-vmm/src/comms/linux_test.rs`   | MODIFY | Tests for vhost open, ioctl sequence, fallback logic |
| `crates/visor-vmm/src/devices/vsock.rs`      | MODIFY | `vhost_active` flag, skip `process_queue` when set   |
| `crates/visor-vmm/src/devices/vsock_test.rs` | MODIFY | Tests for vhost-active no-op path                    |
| `crates/visor-vmm/src/vm.rs`                 | MODIFY | Detect vhost, wire virtqueues, pass fds to kernel    |

`VsockMuxer` and `vsock_muxer.rs` are unchanged. They remain the active path on macOS and on
Linux when vhost is unavailable.

## 6. Estimated Effort

| Phase                            | LOC (prod + test) | Priority |
| -------------------------------- | ----------------- | -------- |
| Phase 0: vhost kernel interface  | ~300-400          | P1       |
| Phase 1: VirtQueue handoff       | ~350-450          | P1       |
| Phase 2: Host socket integration | ~150-250          | P1       |
| Phase 3: Fallback detection      | ~100-150          | P1       |
| **Total**                        | **~900-1,250**    |          |

## 7. Reference Material

- Firecracker vhost-vsock: `src/vmm/src/devices/virtio/vsock/` in the Firecracker repo
- Linux vhost-vsock driver: `drivers/vhost/vsock.c` in kernel source
- vhost ioctl interface: `include/uapi/linux/vhost.h`
- Virtio 1.2 spec §5.10: vsock device
- Our userspace implementation: `crates/visor-vmm/src/devices/vsock.rs` (current baseline)
