# 05 — Disks and Volumes

## How virtio-blk Works

A host file appears as a block device inside the guest:

```
Host:     /tmp/visor/vm-abc/rootfs.ext4     (200 MiB file)
Guest:    /dev/vdb                          (200 MiB block device)
```

The VMM opens the host file. Guest read/write requests become `pread`/`pwrite`
(sync) or `io_uring` submissions (async) against the host file. Multiple host
files = multiple guest block devices.

## Drive Layout

```
/dev/vda  →  init drive (visor-init binary + run.json)    [5 MiB, ephemeral]
/dev/vdb  →  rootfs from OCI image                       [auto-sized, ephemeral]
/dev/vdc  →  volume: /host/data mounted at /data          [persistent]
/dev/vdd  →  another volume                               [persistent]
```

- **Init drive** (/dev/vda): 5 MiB ext4 containing visor-init binary, run.json
  config, and empty mountpoints. Created fresh per VM.
- **Rootfs** (/dev/vdb): OCI image layers merged into ext4. Auto-sized at
  `image_bytes × 1.2 + 50 MiB`. Cached by OCI config digest.
- **Volumes** (/dev/vdc+): User-specified persistent storage.

## Persistent Volumes

Two mechanisms:

### Bind Mounts (virtio-fs)

Map a host directory directly into the guest filesystem. No disk image needed.
Best for mounting code directories, config files, build outputs.

```bash
visor run -v /host/src:/app/src alpine build.sh
```

How it works: `virtio-fs` uses FUSE passthrough — guest filesystem operations
become host filesystem operations on the mapped directory. Changes are visible
on both sides immediately.

### Named Volumes (virtio-blk)

Persistent block device that survives VM restarts. Best for databases, caches,
stateful data.

```bash
visor volume create mydata --size 10G
visor run -v mydata:/data postgres
```

How it works: daemon creates a raw sparse ext4 file at
`~/.visor/volumes/mydata.ext4`. Attached as an additional virtio-blk drive.
visor-init mounts it at the specified guest path.

Sparse files only consume disk space for written blocks — a 10G volume with
500 MiB of data uses ~500 MiB on disk.

### Comparison

| Feature     | Bind mount (virtio-fs)          | Named volume (virtio-blk)      |
| ----------- | ------------------------------- | ------------------------------ |
| Mechanism   | FUSE host dir passthrough       | ext4 file as block device      |
| Performance | Good (FUSE overhead)            | Best (direct block I/O)        |
| Persistence | Host dir persists               | Volume file persists           |
| Sharing     | Multiple VMs can mount same dir | One VM at a time               |
| Resize      | Automatic (host filesystem)     | Manual (`visor volume resize`) |
| Best for    | Code, config, build artifacts   | Databases, stateful apps       |

## Large Images

An Ubuntu Docker image (~200 MiB uncompressed) as a VM:

| Resource               | Cost                                 |
| ---------------------- | ------------------------------------ |
| Disk (rootfs ext4)     | ~300 MiB                             |
| Disk (golden snapshot) | ~512 MiB (configured memory, sparse) |
| RAM (idle)             | ~60-80 MiB                           |
| RAM (under load)       | Whatever workload uses               |
| Cold boot              | ~2-3s (systemd)                      |
| Snapshot restore       | <5ms                                 |

Alpine (~8 MiB image, ~10 MiB idle RAM) is recommended for agent workloads.
Ubuntu works fine — snapshot restore is still <5ms regardless of size.

## Resize

Live resize is possible:

1. `truncate` / `fallocate` the host file to new size
2. Update `nsectors` in virtio config space
3. Send config change interrupt to guest
4. Guest runs `resize2fs` to expand filesystem

Could be exposed as: `visor volume resize mydata 20G`

## Volume CLI

```bash
visor volume create mydata --size 10G     # Create 10G sparse volume
visor volume ls                           # List volumes
visor volume inspect mydata               # Show size, used, attached VM
visor volume resize mydata 20G            # Resize
visor volume rm mydata                    # Delete (must be detached)
```
