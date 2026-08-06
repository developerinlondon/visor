# 06 — Snapshots and Warm Pool

## Golden Snapshot

A golden snapshot is the **template** from which all pool VMs are cloned.
Created once per image:

1. Daemon cold-boots `alpine:3.20` (~120ms)
2. Waits until visor-init's vsock agent says "I'm ready"
3. Pauses the VM and dumps state to disk:
   - CPU register state (~500 bytes per vCPU)
   - Guest memory (file, e.g., 256 MiB — sparse, only touched pages non-zero)
   - Device state (~1KB — virtio queue positions, serial, vsock)
4. Saves to `~/.visor/cache/<image_digest>/golden/`

Every subsequent VM of that image is restored from this snapshot via
`mmap(MAP_PRIVATE)` instead of cold-booting. One golden snapshot per unique
image. Created once, cloned thousands of times.

Think of it as `fork()` for VMs — reads share pages, writes diverge (CoW).

## Snapshot Files

```
~/.visor/cache/
+-- sha256_abc123/                  # keyed by OCI image config digest
|   +-- golden/
|   |   +-- memory.bin              # Guest RAM (sparse file)
|   |   +-- cpu_state.bin           # vCPU registers
|   |   +-- device_state.bin        # Virtio queue positions, vsock state
|   +-- rootfs.ext4                 # OCI layers merged to ext4 (cached)
|   +-- init.ext4                   # Init drive (cached)
+-- sha256_def456/
    +-- golden/
    |   +-- memory.bin
    |   +-- cpu_state.bin
    |   +-- device_state.bin
    +-- rootfs.ext4
    +-- init.ext4
```

## Restore Path (<5ms)

```
1. mmap(MAP_PRIVATE, memory.bin)           [~0.1ms]
   Kernel sets up page table entries only.
   NO DATA COPIED. Pages load on-demand (CoW).
   Works for 256 MiB or 32 GiB — mmap is O(1).

2. KVM_SET_USER_MEMORY_REGION ioctl        [~0.2ms]
   Point KVM at mmap'd region.

3. KVM_SET_REGS + KVM_SET_SREGS            [~0.1ms]
   Write ~100 bytes of vCPU registers.

4. bincode::deserialize(device_state.bin)   [~0.1ms]
   ~1KB of virtio queue positions.

5. KVM_RUN (resume vCPUs)                   [~0.1ms]

6. vsock reconnect                          [~0.5ms]

Total: ~1.4ms (with margin → "<5ms")
```

## What the User Experiences

| Scenario                                       | Latency                       |
| ---------------------------------------------- | ----------------------------- |
| Pool hit (pre-warmed VM available)             | <3ms                          |
| Pool miss, golden snapshot exists              | <7ms (5ms restore + 2ms exec) |
| First ever use of image (cold boot + snapshot) | ~170ms (then <5ms forever)    |

## Warm Pool

### How It Works

```
1. GOLDEN SNAPSHOT (one-time per image, ~120ms)

   visor boots alpine:3.20 cold
   → waits for visor-init ready
   → takes snapshot → saves to disk

2. POOL FILL (automatic, background)

   Pool manager restores N copies from golden snapshot.
   Each restore: <5ms (CoW mmap).
   VMs are RUNNING and IDLE — vsock agent listening.

   Pool:  [VM-1: ready] [VM-2: ready] [VM-3: ready]

3. USER RUNS COMMAND

   $ visor run alpine:3.20 echo hello

   Daemon grabs VM-1 from pool → exec immediately.
   Pool auto-refills:
   Pool:  [VM-2: ready] [VM-3: ready] [VM-4: restoring...]

4. DAEMON RESTART

   $ visor start
   Pool manager finds disk cache
   Restores from cached snapshots (no cold boot needed)
```

### Pool Configuration

```toml
# ~/.visor/config.toml
[pool]
default_size = 3

[pool.images."alpine:3.20"]
size = 10 # 10 pre-warmed VMs
memory = 256 # 256 MiB each

[pool.images."python:3.12"]
size = 5
memory = 512

[pool.images."builder"]
size = 2 # 2 beefy build VMs
memory = 32768 # 32 GiB each
cpus = 16
```

### Pool Memory Economics

5 idle `alpine:3.20` VMs (256 MiB configured each):

| What                                | Cost                                      |
| ----------------------------------- | ----------------------------------------- |
| Golden snapshot on disk             | 256 MiB file (sparse)                     |
| Virtual address space (5 × 256 MiB) | 1.25 GiB (free — just page table entries) |
| Physical RAM (5 idle VMs)           | ~50-100 MiB total (shared CoW pages)      |
| Physical RAM if guest uses it all   | 5 × 256 MiB                               |

Generous pools for small images, small pools for large images.

### Pool Manager

```
visor daemon
+-- PoolManager
    +-- monitors pool levels per image
    +-- restores from snapshot when pool drops below target
    +-- creates golden snapshots for new images on first use
    +-- ballooning: reclaims memory from idle pool VMs (P1)
    +-- health-checks idle VMs (vsock ping)
    +-- evicts stale VMs (configurable TTL)
    +-- persists snapshots to disk cache
    +-- tracks host memory pressure — stops pre-warming if RAM is low
```

## Firecracker Comparison

```
FIRECRACKER RESTORE (~125ms)
============================
1. fork()+exec() firecracker binary                [~2ms]
2. Initialize Rust runtime, parse args             [~4ms]
3. HTTP PUT /snapshot/load                         [~2ms]
4. Deserialize MicrovmState (all devices, config)  [~15ms]
5. Create KVM VM + set up memory regions           [~15ms]
6. Restore vCPU registers                          [~5ms]
7. Recreate device models                          [~10ms]
8. Resume vCPUs                                    [~2ms]
9. Connect to vsock UDS                            [~5ms]
Total: ~63-125ms

VISOR RESTORE (<5ms)
====================
1. mmap(MAP_PRIVATE, snapshot_file)               [~0.1ms]
2. KVM_SET_USER_MEMORY_REGION                     [~0.2ms]
3. KVM_SET_REGS + KVM_SET_SREGS                   [~0.1ms]
4. bincode::deserialize(~1KB device state)        [~0.1ms]
5. KVM_RUN                                        [~0.1ms]
6. vsock reconnect                                [~0.5ms]
Total: ~1.4ms

Why the difference:
- No process spawn (in-process)
- No REST API (direct function call)
- No full state deserialization (just ~1KB vs Firecracker's full MicrovmState)
- No device model recreation (devices stay in memory)
- mmap is O(1) — no data copy regardless of VM size
```
