# Running visor in a Kubernetes pod

Updated: 2026-08-06

How to build the production image (repo-root `Dockerfile`) and shape a pod so
the visor daemon can run microVMs inside it. Every behavioral claim cites the
source (`file:line`) at the tree this document was written against.

The image is built and published by the `image` workflow to
`ghcr.io/developerinlondon/visor`. The guest kernel is compiled from public
sources inside the build, so no credentials are involved; set the
`VISOR_KERNEL_URL` build argument to a published `vmlinux-x86_64` to skip that
stage.

## Image contract

| Item              | Value                                      | Evidence                                                                                            |
| ----------------- | ------------------------------------------ | --------------------------------------------------------------------------------------------------- |
| Entrypoint        | `visor start --foreground` (PID 1)         | background mode forks and the parent exits — `crates/visor-runtime/src/cli/start.rs:60-86`          |
| API listener      | `0.0.0.0:7800` (plain HTTP)                | `crates/visor-runtime/src/daemon.rs:26`, `crates/visor-runtime/src/cli/mod.rs:120-126`              |
| Docker Engine API | same listener, `/v1.XX/*` routes           | routers merged then bound once — `crates/visor-runtime/src/daemon.rs:197-210`                       |
| Embedded DNS      | UDP `0.0.0.0:53`, upstream `8.8.8.8`       | `crates/visor-runtime/src/daemon.rs:152-168,235-241`, `crates/visor-runtime/src/net/dns.rs:106-114` |
| State dir         | `VISOR_HOME` (image sets `/var/lib/visor`) | `VISOR_HOME`, else `$HOME/.visor` — `crates/visor-runtime/src/paths.rs:5-15`                        |
| Scratch dir       | `VISOR_TMPDIR`, else system temp           | `crates/visor-runtime/src/backend.rs:2029`                                                          |
| Guest init        | `/usr/libexec/visor/visor-init`            | runtime lookup chain — `crates/visor-runtime/src/vm.rs:283-314`                                     |
| Guest kernel      | baked at builder `OUT_DIR` path            | `crates/visor-kernel/src/lib.rs:29-32`, loaded at `crates/visor-runtime/src/vm.rs:706`              |
| Stop signal       | `SIGINT` (image `STOPSIGNAL`)              | daemon handles ctrl_c + API shutdown only — `crates/visor-runtime/src/daemon.rs:265-278`            |
| Health            | `GET /v1/health`                           | `crates/visor-runtime/src/cli/start.rs:29-38`                                                       |
| Swagger UI        | `GET /docs`                                | `crates/visor-runtime/src/daemon.rs:214`                                                            |

Subdirectories created under `VISOR_HOME`: `images`
(`crates/visor-runtime/src/daemon.rs:99-100`), `volumes`
(`crates/visor-runtime/src/volume.rs:76`), `cache`
(`crates/visor-runtime/src/oci/cache.rs:72`), `state`
(`crates/visor-runtime/src/state/persistence.rs:51`), plus
`visor-daemon.log` when daemonized (`crates/visor-runtime/src/paths.rs:33-45`).

## Building the image

The build is linux/amd64 only: the guest kernel is `vmlinux-x86_64` and
visor-init targets `x86_64-unknown-linux-musl` (`Makefile:13,20`,
`crates/visor-kernel/build.rs:42-45`).

The kernel is resolved at **build time, never at runtime**
(`crates/visor-kernel/build.rs:67-131`). The repo ships only `Image-aarch64`,
so an amd64 build must provide `vmlinux-x86_64` one of two ways:

```bash
# Option A: pre-fetch the kernel into the build context root
cp /path/to/vmlinux-x86_64 ./vmlinux-x86_64
docker build --platform linux/amd64 -t visor:dev .

# Option B: let build.rs download it from the GitLab release
docker build --platform linux/amd64 -t visor:dev \
  --secret id=gitlab_token,src=/path/to/token .
```

The token needs `read_api` scope; `CI_JOB_TOKEN` is also accepted
(`crates/visor-kernel/build.rs:155-160`). Download URL base:
`crates/visor-kernel/build.rs:29-30`.

## Pod shape

```text
client container                     visor container (one pod or peer pod)
DOCKER_HOST=tcp://visor:7800  --->   :7800  native /v1 API + Docker /v1.XX API
                                       |
                                       | needs: /dev/kvm, /dev/net/tun,
                                       | NET_ADMIN, root, writable VISOR_HOME
                                       v
                                     microVM workers (re-exec'd `visor vm-worker`)
```

### Devices and privileges

- `/dev/kvm` — KVM VM creation (`crates/visor-vmm/src/platform/linux.rs:26`).
  Expose via a device plugin resource, or `privileged: true` with a hostPath
  mount. Nodes must be bare-metal (e.g. EKS `*.metal`) or have nested
  virtualization enabled.
- `/dev/net/tun` — TAP interfaces are created with `ip tuntap add`
  (`crates/visor-vmm/src/net/linux.rs:774-783`) and opened via `/dev/net/tun`
  (`crates/visor-vmm/src/platform/linux.rs:209-217`).
- `NET_ADMIN` capability — TAP creation plus iptables NAT and port-forward
  rules (`crates/visor-vmm/src/net/linux.rs:349-411`). All rules live in the
  pod's own network namespace, not the node's.
- Run as root — required for the above and for binding DNS on port 53.
- No `/dev/vhost-vsock` needed: guest comms use a userspace vsock muxer
  (`crates/visor-vmm/src/comms/linux.rs:1-7`).
- The daemon applies no seccomp filter in the Linux beta
  (`docs/linux-beta-operations.md:45`). The pod-level `RuntimeDefault`
  seccomp profile is untested with the KVM ioctl surface.

A minimally privileged variant (device plugin for both devices +
`NET_ADMIN` only) should work in principle; `privileged: true` is the
low-friction starting point for a beta.

### Volumes

Mount a writable volume at `/var/lib/visor` (the image's `VISOR_HOME`).
Persist it (PVC) if pulled images, volumes, and VM restart metadata should
survive pod restarts; `emptyDir` otherwise. Size for the OCI image store plus
per-VM rootfs images — multi-GB.

### Termination

The daemon has no SIGTERM handler (see BLOCKERS). The image sets
`STOPSIGNAL SIGINT`, which containerd honors, and the SIGINT path runs the
clean shutdown: persist VM metadata, then release TAP/NAT resources
(`crates/visor-runtime/src/daemon.rs:217-231`). As belt and braces add a
preStop hook that calls the API shutdown route used by `visor stop`
(`POST /v1/shutdown`, `crates/visor-runtime/src/cli/stop.rs:75-80`) and give
`terminationGracePeriodSeconds` enough room to stop live VMs.

### Resources

Guest memory is host-process memory in the daemon's cgroup. Defaults per
microVM: 512 MiB, 1 vCPU (`crates/visor-types/src/lib.rs:19-24,88-92`). The
warm pool keeps up to 3 pre-warmed VMs per image by default
(`crates/visor-runtime/src/pool/manager.rs:39-49`), refilled every 30s
(`crates/visor-runtime/src/daemon.rs:243-263`). Budget:

```text
memory limit >= (expected concurrent VMs + warm pool) x VM size + ~512Mi daemon headroom
cpu          >= sum of guest vCPUs actually busy + 0.5 for the daemon
```

### Client containers

Point any stock Docker client at the pod:

```yaml
env:
  - name: DOCKER_HOST
    value: tcp://<visor-service>:7800
```

There is no Unix socket and no TLS on this listener — the Docker compat
router and the native API are one plain-HTTP axum app on the single 7800
bind (`crates/visor-runtime/src/daemon.rs:197-210`,
`crates/visor-docker/src/lib.rs:16-17`).

### Example manifest

```yaml
apiVersion: v1
kind: Pod
metadata:
  name: visor
  labels: { app: visor }
spec:
  terminationGracePeriodSeconds: 60
  containers:
    - name: visor
      image: registry.example.com/visor:dev
      securityContext:
        privileged: true # beta shortcut; see "Devices and privileges"
      ports:
        - containerPort: 7800
      env:
        - name: VISOR_HOME
          value: /var/lib/visor
      volumeMounts:
        - name: visor-home
          mountPath: /var/lib/visor
      readinessProbe:
        httpGet: { path: /v1/health, port: 7800 }
      lifecycle:
        preStop:
          exec:
            command: ["/usr/local/bin/visor", "stop"]
      resources:
        requests: { cpu: "1", memory: 3Gi }
        limits: { memory: 6Gi }
  volumes:
    - name: visor-home
      emptyDir: {}
---
apiVersion: v1
kind: Service
metadata:
  name: visor
spec:
  selector: { app: visor }
  ports:
    - port: 7800
      targetPort: 7800
```

## BLOCKERS

1. **No SIGTERM handler — pod deletion would SIGKILL without mitigation.**
   `shutdown_signal` selects only `tokio::signal::ctrl_c()` and the API
   shutdown notify (`crates/visor-runtime/src/daemon.rs:265-278`); the doc
   comment at `crates/visor-runtime/src/daemon.rs:76` claims SIGTERM but no
   handler is installed. PID 1 ignores unhandled SIGTERM, so Kubernetes would
   wait out the grace period and SIGKILL, skipping metadata persistence and
   TAP/NAT cleanup (`crates/visor-runtime/src/daemon.rs:223-229`). Mitigated
   here via `STOPSIGNAL SIGINT` + preStop `visor stop`; the durable fix is a
   SIGTERM handler in the daemon.

2. **Guest kernel path is baked at compile time with no runtime override.**
   `kernel_path()` returns the builder's `OUT_DIR` joined with the kernel
   filename via `env!` (`crates/visor-kernel/src/lib.rs:29-32`) and the file
   is mmap'd from that absolute path at VM boot
   (`crates/visor-runtime/src/vm.rs:706`,
   `crates/visor-vmm/src/boot/x86_64.rs:62-65`). `VISOR_KERNEL_PATH` is only
   consulted by `build.rs` (`crates/visor-kernel/build.rs:68-78`). The
   Dockerfile works around this by recreating the builder's
   `/build/target/release/build/visor-kernel-*/out/` tree in the runtime
   image; copying the binary anywhere else breaks it. A runtime env override
   for the kernel path would remove this fragility.

3. **The x86_64 kernel artifact is not in the repo.** Only `Image-aarch64`
   ships at the workspace root; amd64 builds must supply `vmlinux-x86_64` or
   a GitLab token so `build.rs` can download it
   (`crates/visor-kernel/build.rs:67-131`, URL at `build.rs:29-30`, token at
   `build.rs:155-160`). CI building this image needs that secret provisioned.

4. **The API is unauthenticated plain HTTP, and the Docker shim shares it.**
   One listener serves both surfaces (`crates/visor-runtime/src/daemon.rs:197-210`);
   no auth middleware exists in `crates/visor-runtime/src/api/router.rs`, and
   the mTLS support in `crates/visor-runtime/src/tls/mod.rs:1-5` is not
   referenced from the daemon's serve path. Anyone who can reach `pod:7800`
   controls VM lifecycle and exec. A NetworkPolicy restricting ingress to
   intended clients is mandatory.

5. **Guest DNS upstream is hardcoded to 8.8.8.8.**
   `DnsResolverConfig::new` sets `8.8.8.8` (`crates/visor-runtime/src/net/dns.rs:106-114`)
   and the daemon constructs it with no override
   (`crates/visor-runtime/src/daemon.rs:157`). Clusters with restricted
   egress must allow UDP 53 to 8.8.8.8 or guest name resolution fails. No
   flag exists to point it at cluster DNS.

6. **DNS bind on 0.0.0.0:53 requires root in the pod.** Linux binds the
   wildcard address (`crates/visor-runtime/src/daemon.rs:235-241`) on port 53
   (`crates/visor-runtime/src/net/dns.rs:111`). Bind failure is non-fatal
   (`crates/visor-runtime/src/daemon.rs:164-168`) but guests then lose DNS.

7. **Node prerequisites are outside the image's control.** `/dev/kvm`
   (bare-metal or nested virt) and kernel support for TAP (`tun` module) and
   iptables NAT must be present on the node. The image's `iptables` is the
   Debian nft-backed build; if rules fail to apply on a legacy-only node,
   switch the image to `iptables-legacy` via `update-alternatives`.
