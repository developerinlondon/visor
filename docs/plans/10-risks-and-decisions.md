# 10 — Risks, Decisions, and Open Questions

## Risk Assessment

| Risk                                   | Severity | Mitigation                                         |
| -------------------------------------- | -------- | -------------------------------------------------- |
| x86_64 boot setup bugs (GDT/MSR/CPUID) | HIGH     | Reference vmm-reference (proven code)              |
| Guest hangs with no error output       | HIGH     | Serial console first, logging                      |
| virtio-vsock not in vmm-reference      | MEDIUM   | Reference Firecracker's vsock impl                 |
| rust-vmm crate API changes             | MEDIUM   | Pin exact versions, track upstream                 |
| Scope creep                            | MEDIUM   | Strict P0/P1/P2 phasing                            |
| Apple HVF GICv3 complexity             | MEDIUM   | macOS 15+, defer to P2                             |
| Internal vswitch packet bugs           | MEDIUM   | Start with TAP fallback, add vswitch incrementally |
| DNS resolver edge cases                | LOW      | Use proven crate (hickory-dns), not custom parser  |

## Decided Questions

Closed during architecture Q&A sessions:

| Decision               | Answer                                                                 |
| ---------------------- | ---------------------------------------------------------------------- |
| Binary count           | ONE binary: `visor` with subcommands                                   |
| Daemon command         | `visor start` (not separate `visord`)                                  |
| Deployment modes       | KVM (full) + Container (degraded), auto-detected                       |
| `/v1/info` endpoint    | Yes — exposes mode, features, host capabilities (P0)                   |
| SSE filtering          | Query params on `/v1/events` + per-VM endpoint                         |
| Process visibility     | Guest processes invisible from host; `visor top` for introspection     |
| Memory model           | Demand-paged, CoW snapshots, overcommit-friendly                       |
| Large images           | Work fine — only touched pages consume RAM                             |
| Networking model       | Shared networks with internal vswitch, not TAP-per-VM                  |
| VM-to-VM communication | Internal vswitch (in-process memory copy)                              |
| DNS                    | Embedded resolver in daemon, service discovery by VM name              |
| User access            | `visor shell` (toybox), `visor exec`, `visor attach`, port forwarding  |
| Volumes                | `virtio-fs` for bind mounts, `virtio-blk` for named volumes            |
| Backend trait          | Design at P0, container implementation at P2                           |
| Fresh codebase         | Yes — new repo, reference livecontainers for lessons only              |
| TUI                    | Yes — `visor tui` for terminal dashboard (P1)                          |
| TLS                    | Unix socket default, optional TCP+TLS, mTLS for production             |
| Ingress                | Outside scope — use Traefik/Caddy. Simple hostname routing P2          |
| Guest DNS config       | visor-init writes /etc/resolv.conf (standard practice)                 |
| API listener           | Unix socket by default, TCP+TLS optional                               |
| Shell naming           | `visor shell` (not debug/connect). exec/attach/console separate        |
| Shell toolkit          | Toybox (BSD-0, ~200KB, ~200 cmds). Rust alternatives 50-100x larger    |
| Shell network tools    | Included by default (wget/nc/curl). Configurable per-VM/globally       |
| Shell security         | Config: enabled, network_tools, idle_timeout, audit_log. ro,noexec     |
| FIPS cryptography      | Single FIPS binary (rustls + aws-lc-rs, CMVP #4631). No non-FIPS build |
| TLS library            | rustls 0.23 with aws-lc-rs backend. Not ring, not OpenSSL              |
| SBOM                   | CycloneDX 1.6 JSON, generated in CI, shipped alongside binary          |
| Binary signing         | cosign/Sigstore on every release. SLSA Level 2-3 target                |
| Audit logging          | Structured JSON for all API, shell, VM lifecycle events                |
| Don't-trust-guest      | All vsock messages validated host-side. Guest is in threat model       |
| Seccomp                | Tight BPF allowlist (~30 syscalls) on daemon process (P1)              |
| Compliance approach    | Product enables customer compliance. Ship hardening guide, not certs   |

## Open Questions

1. **Crate naming**: `visor-machine` (descriptive) or `visor-core` (concise)?
2. **Domain**: Confirm visor.rs acquisition
3. **GitLab path**: `gitlab.com/agentx.rs/visor` or new org?
4. **License**: Apache-2.0 + MIT dual license (same as rust-vmm)?
5. **Timing**: Start visor now or finish livecontainers K8s phase first?
6. **Compose format**: Keep Docker-compatible docker-compose.yml or define visor-native?
7. **Auto-start**: Install as systemd/launchd service via `visor install-service`?

## Performance Targets

| Metric                    | Target | How                                  |
| ------------------------- | ------ | ------------------------------------ |
| Snapshot restore          | <5ms   | mmap(MAP_PRIVATE), direct KVM ioctls |
| Pool hit (pre-warmed)     | <3ms   | Grab running VM, exec via vsock      |
| Cold boot                 | <125ms | Direct kernel load, no process spawn |
| Inter-VM latency          | <20μs  | Internal vswitch, memory copy        |
| DNS resolution (internal) | <1ms   | In-process resolver, no network hop  |
| API response (list VMs)   | <5ms   | In-memory state, no DB               |
| Golden snapshot creation  | <5s    | Boot + pause + dump (one-time)       |

## What We Reference from livecontainers

Ideas and lessons, NOT code migration:

| Area           | What We Learned                                                        |
| -------------- | ---------------------------------------------------------------------- |
| OCI pipeline   | Layer merge order, ext4 sizing formula, cache layout                   |
| Networking     | TAP+iptables NAT, DNS DNAT, cleanup on drop, eval for `!` rules        |
| Guest init     | Mount sequence, vsock agent protocol, pivot_root, raw ioctl networking |
| vsock protocol | JSON-RPC 2.0, ping/exec/write_file, port 52 convention                 |
| Testing        | VmGuard RAII pattern, atomic subnet offsets, stale rule cleanup        |
| Integration    | Kernel boot args per backend, two-drive layout                         |
| Architecture   | Process-spawn model limitations (why we're building visor)             |
