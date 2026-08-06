# visor

An in-process VMM that runs OCI containers as microVMs.

Every container gets its own kernel and its own virtual machine, and the whole thing is one
binary: no separate hypervisor process to fork, supervise, and talk to over HTTP. Point the
Docker CLI at visor and `docker run` gives you a VM instead of a namespace.

```sh
visor start &                       # daemon: API, DNS, warm pool
DOCKER_HOST=tcp://127.0.0.1:7800 \
  docker run --rm alpine uname -r   # a guest kernel, not the host's
```

## Why in-process

The usual way to build this is a supervisor that forks a hypervisor per VM and drives it over a
local socket. Every operation then crosses a process boundary — configure memory, add a drive,
add a network, boot, kill — and every VM costs a process to babysit.

visor runs VMs as threads inside one process. Booting a VM is a function call. There is one
binary to install, one process to supervise, and one place where state lives.

## What works today

Linux is the only host platform in the current beta, and it needs KVM. The Docker surface is a
deliberately narrow, test-backed subset rather than "Docker compatible" in the abstract:

- `docker run` (including `-d` and `-p`), `pull`, `exec` (`-i`, `-it`), `logs`, `stop`, `rm`
- `docker build`, and `buildx build --load`
- Compose with default-project networking
- Native lifecycle CLI: `start`, `stop`, `run`, `ps`, `info`, `inspect`, `logs`, `exec`, `shell`,
  `console`, plus a TUI

The precise boundary — what is promised, what merely exists, and the known limits — is in
[`docs/beta-compatibility.md`](docs/beta-compatibility.md). Operational behaviour, including
restart recovery and the supported control surfaces, is in
[`docs/linux-beta-operations.md`](docs/linux-beta-operations.md). Read those before depending on
anything not listed above.

## Requirements

- Linux host with a usable KVM device
- `ip` and `iptables` — the network backend uses TAP devices and iptables NAT
- Rust stable (edition 2024) to build

## Building

```sh
cargo build --release
```

The guest kernel is resolved at build time. In order: `VISOR_KERNEL_PATH`, a kernel beside the
root `Cargo.toml`, a local cache in `/var/lib/visor/kernel/`, a release named by
`VISOR_KERNEL_URL`, a previous build's cache — and finally a build from source, which needs no
credentials but takes roughly 15 minutes cold. To skip the compile, point at a published binary:

```sh
VISOR_KERNEL_URL=https://example.invalid/downloads cargo build --release
```

`VISOR_KERNEL_NO_BUILD=1` makes the build fail rather than compile a kernel, which is usually
what you want in CI that has an artifact to fetch.

## Layout

| Crate           | What it is                                                   |
| --------------- | ------------------------------------------------------------ |
| `visor-vmm`     | The VMM: hypervisor, devices, transport, networking, sandbox |
| `visor-runtime` | Daemon, CLI, HTTP API, OCI handling, warm pool               |
| `visor-docker`  | Docker Engine API compatibility layer                        |
| `visor-init`    | Guest PID 1, static musl, runs inside the VM                 |
| `visor-kernel`  | Guest kernel resolution and build                            |
| `visor-build`   | Image build support                                          |
| `visor-types`   | Shared types                                                 |

Architecture and design notes live in [`docs/plans/`](docs/plans/).

## Security

The daemon's HTTP API — which includes the Docker endpoints — is unauthenticated and unencrypted
by default, so bind it to a loopback or otherwise confine it. Report vulnerabilities as described
in [`SECURITY.md`](SECURITY.md).

## Licence

Apache-2.0. See [`LICENSE`](LICENSE) and [`NOTICE`](NOTICE); contributions are governed by
[`CLA.md`](CLA.md).
