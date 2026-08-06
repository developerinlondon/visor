# 11 — Security and Compliance

Enterprise-ready from day one. visor ships FIPS-validated cryptography by default,
generates SBOMs on every release, and is architecturally designed to enable customer
SOC 2, FedRAMP, and HIPAA compliance.

## Security Architecture Principles

### 1. KVM Is the Boundary

Every VM runs inside hardware-enforced isolation (Intel VT-x / AMD-V). Guest
processes, memory, and filesystem are invisible from the host. This exceeds
NIST 800-190's container isolation recommendations — we provide hypervisor-level
separation, not namespace-level.

### 2. Don't Trust the Guest

All host↔guest communication over vsock is validated on the host side. The guest
kernel and visor-init are within the threat model — a compromised guest cannot
influence the daemon beyond its vsock message interface. Every vsock message is:

- Length-bounded (reject oversized payloads)
- Schema-validated (reject malformed JSON-RPC)
- Permission-checked (guest cannot request operations outside its VM scope)

This principle was reinforced by CVE-2026-24834 (Kata Containers, CVSS 9.4), where
trusting guest-supplied data led to a host-side escape.

### 3. Minimal Attack Surface

- Single static binary (~15-17 MiB)
- No shell, no scripting engine, no plugin system in the daemon
- Seccomp filter restricts the visor process to ~30 syscalls
- visor-init inside the VM is ~1 MiB with no network listeners

## FIPS 140-3 Cryptography

visor uses FIPS 140-3 validated cryptography for all TLS operations by default.
No special flags, no separate binary, no customer action required.

### Crypto Stack

```
visor binary
  +-- rustls 0.23 (TLS 1.2/1.3, memory-safe, no OpenSSL)
       +-- aws-lc-rs (FIPS feature enabled)
            +-- aws-lc-fips-sys
                 +-- AWS-LC FIPS module (NIST CMVP Certificate #4631)
```

### What This Means

| Question                      | Answer                                                       |
| ----------------------------- | ------------------------------------------------------------ |
| Is the crypto FIPS validated? | Yes — NIST CMVP #4631, FIPS 140-3 Level 1                    |
| Which operations use FIPS?    | All TLS (API server, mTLS), all crypto in visor binary       |
| Does the guest need FIPS?     | No — guest crypto is the guest's responsibility              |
| Why not ring?                 | ring has no FIPS validation and no timeline for one          |
| Why not OpenSSL?              | Large C dependency, platform-specific, harder to static link |
| Binary size impact?           | ~1-2 MiB larger than non-FIPS (negligible)                   |
| Build dependencies?           | Rust + CMake + Go + C compiler (CI-only concern)             |

### Cargo Configuration

```toml
[dependencies]
rustls = { version = "0.23", default-features = false, features = ["fips", "logging"] }
rustls-pemfile = "2"
rustls-pki-types = "1"
```

The `fips` feature transitively enables `aws-lc-rs` with its FIPS module and
forces `require_ems = true` (Extended Master Secret, required by FIPS 140-3 IG).

## SBOM (Software Bill of Materials)

Every release ships with a machine-readable inventory of all dependencies.
Required by US Executive Order 14028 for government procurement and increasingly
expected by enterprise buyers.

### Generation

```bash
# CI release step — generate from Cargo.lock
cargo sbom --output-format cyclone_dx_json_1_6 > visor-${VERSION}.cdx.json

# Secondary format (if customer requests SPDX)
cargo sbom --output-format spdx_json_2_3 > visor-${VERSION}.spdx.json
```

### Format

Primary: **CycloneDX 1.6 JSON** — best tooling support, native VEX integration,
government-accepted. SPDX generated on request.

### Tools

| Tool          | Purpose                                         | License    |
| ------------- | ----------------------------------------------- | ---------- |
| `cargo-sbom`  | Generate SBOM from Cargo.lock (primary)         | MIT        |
| `syft`        | Scan final binary for embedded deps (secondary) | Apache-2.0 |
| `cargo-deny`  | License audit + vulnerability check in CI       | Apache-2.0 |
| `cargo-audit` | Known CVE check against RustSec advisory DB     | Apache-2.0 |

## Binary Signing and Supply Chain (SLSA)

Target: **SLSA Level 3** — signed provenance from hardened CI.

### What We Ship

```
visor-1.0.0-x86_64-linux.tar.gz
  +-- visor                          # Static binary (FIPS-enabled)
  +-- visor.cdx.json                 # CycloneDX SBOM
  +-- visor.intoto.jsonl             # SLSA provenance attestation
  +-- visor.sig                      # cosign signature
```

### SLSA Levels

| Level | Requirement                          | Status                         |
| ----- | ------------------------------------ | ------------------------------ |
| L1    | Provenance exists, documents build   | Day one                        |
| L2    | Signed provenance from hosted CI     | Day one (GitLab CI + Sigstore) |
| L3    | Hardened, isolated build environment | Target within 6 months         |

### CI Pipeline

```yaml
# Simplified release pipeline
build:
  - cargo build --release --locked
  - cargo sbom --output-format cyclone_dx_json_1_6 > visor.cdx.json
  - cosign sign-blob --yes visor
  - cosign attest --predicate visor.cdx.json --type cyclonedx visor
```

## Audit Logging

Every operation produces structured JSON audit events. Required for SOC 2 (AU
controls), FedRAMP (AU-2, AU-3, AU-6), and HIPAA audit requirements.

### What Gets Logged

| Category     | Events                                                       |
| ------------ | ------------------------------------------------------------ |
| VM lifecycle | create, start, stop, destroy, snapshot, restore              |
| API access   | every HTTP request (method, path, status, actor, duration)   |
| Shell access | every `visor shell` session (start, end, duration, user, VM) |
| Exec         | every `visor exec` invocation (command, exit code, user, VM) |
| Network      | network create/delete, port forward add/remove               |
| Pool         | warm, drain, refill, snapshot cache hit/miss                 |
| Config       | daemon config changes, per-VM overrides                      |
| Auth         | mTLS handshake success/failure, cert subject                 |

### Log Format

```json
{
    "timestamp": "2026-02-28T16:30:00.000Z",
    "level": "info",
    "event": "vm.created",
    "actor": "client_cert:admin@company.com",
    "resource": "vm-abc123",
    "details": {
        "image": "alpine:3.20",
        "memory_mib": 512,
        "vcpus": 1,
        "network": "default"
    },
    "duration_ms": 3,
    "outcome": "success"
}
```

Uses `tracing` crate with structured fields. Exportable to any log aggregator
(stdout JSON → Fluentd/Vector/Loki).

## Seccomp (System Call Filtering)

The visor daemon runs with a tight seccomp-bpf filter restricting allowed syscalls.
This limits damage if the daemon itself is compromised.

### Allowlist (~30 syscalls)

Following Firecracker's model:

```
read, write, close, fstat, mmap, mprotect, munmap, brk,
ioctl (KVM_*), epoll_create, epoll_ctl, epoll_wait,
socket, bind, listen, accept4, sendto, recvfrom,
clone, futex, sigaltstack, rt_sigaction, rt_sigprocmask,
exit, exit_group, openat, fcntl, getrandom,
clock_gettime, nanosleep, madvise, mremap
```

Everything else → SIGSYS (kill the process). The seccomp filter is applied after
daemon initialization (listen sockets, KVM fd, file handles are opened first).

### Implementation

Uses `seccompiler` crate (same as Firecracker). Applied in `daemon.rs` after
all initialization is complete but before accepting API connections.

Priority: **P1** (after core VM lifecycle works end-to-end).

## AppArmor / SELinux Profiles

Ship default confinement profiles for the visor daemon process.

### AppArmor (Ubuntu/Debian)

```
# /etc/apparmor.d/visor
profile visor /usr/bin/visor {
  # KVM access
  /dev/kvm rw,
  # Networking
  /dev/net/tun rw,
  # Cache directory
  owner ~/.visor/cache/** rw,
  # Unix socket
  owner /run/visor.sock rw,
  # Deny everything else by default
  deny /** w,
}
```

### SELinux (RHEL/Fedora)

Custom `visor_t` domain confining the daemon. Required for OpenShift integration
(Red Hat mandates SELinux).

Priority: **P1** (ship alongside seccomp).

## Compliance Framework Mapping

visor is a **product**, not a platform. Most frameworks certify the **operator**.
Our job: make visor the runtime that compliance teams approve fastest.

### How visor enables each framework

```
+---------------------------+------------------------------------------+------------------+
| Framework                 | What visor provides                      | Who certifies    |
+---------------------------+------------------------------------------+------------------+
| SOC 2 Type II             | Audit logs, RBAC (mTLS), encryption in   | The company      |
|                           | transit, workload isolation proof         | operating visor |
+---------------------------+------------------------------------------+------------------+
| FedRAMP                   | FIPS crypto, audit logs, RBAC, hardened   | The CSP using   |
|                           | defaults, SBOM, vulnerability mgmt       | visor in their   |
|                           |                                          | boundary         |
+---------------------------+------------------------------------------+------------------+
| HIPAA                     | Encryption (TLS/mTLS), audit trail,      | The covered      |
|                           | access controls, VM isolation            | entity           |
+---------------------------+------------------------------------------+------------------+
| PCI DSS                   | Network segmentation (KVM isolation),    | The merchant /   |
|                           | encryption, logging, access controls     | processor        |
+---------------------------+------------------------------------------+------------------+
| NIST 800-190              | Exceeds all recommendations — KVM        | N/A (guidance,   |
|                           | isolation > namespace isolation          |not certification)|
+---------------------------+------------------------------------------+------------------+
| Common Criteria (PP_VIRT) | Architecture aligns with PP_VIRT v1.1    | visor (future,   |
|                           | SFRs: audit, crypto, isolation, mgmt     | $200-500K)       |
+---------------------------+------------------------------------------+------------------+
```

### NIST 800-190 Alignment

NIST SP 800-190 recommends container runtime security controls. visor exceeds
every recommendation because KVM provides stronger isolation than namespaces:

| 800-190 Recommendation    | visor Implementation                                  |
| ------------------------- | ----------------------------------------------------- |
| Vulnerability management  | cargo-audit in CI, CVE policy, patch SLAs             |
| Least-privilege execution | VMs run with minimal capabilities, seccomp on daemon  |
| Mandatory access controls | AppArmor/SELinux profiles shipped                     |
| Syscall filtering         | Seccomp BPF on daemon process (~30 syscalls)          |
| Resource limits           | Per-VM memory/CPU limits via KVM, rate limiting (P1)  |
| Network segmentation      | KVM isolation + shared networks with vswitch          |
| Immutable containers      | Read-only rootfs via virtio-blk, init drive ro,noexec |
| Runtime monitoring        | Structured audit logs, Prometheus metrics, SSE events |

### Common Criteria PP_VIRT (Future)

NIAP Protection Profile for Virtualization v1.1 is the most directly applicable
certification for a VMM product. Oracle KVM and SUSE KVM have achieved it.
Neither Kata Containers nor gVisor has any CC certification.

If visor pursues CC evaluation:

- Target: PP_VIRT v1.1 (EAL 2+ equivalent)
- Cost: $200-500K with CCTL lab
- Timeline: 12-24 months
- Competitive advantage: First microVM product with CC certification

Architecture alignment with PP_VIRT Security Functional Requirements:

| SFR Category          | visor Feature                                        |
| --------------------- | ---------------------------------------------------- |
| FAU (Audit)           | Structured audit logging of all VM lifecycle events  |
| FCS (Crypto)          | FIPS 140-3 validated TLS (rustls + aws-lc-rs)        |
| FDP (Data Protection) | KVM memory isolation, CoW snapshots, secure teardown |
| FIA (Auth)            | mTLS for API, certificate-based identity             |
| FMT (Management)      | Role-based API access, config validation             |
| FPT (Self-Protection) | Seccomp, AppArmor/SELinux, minimal attack surface    |
| FTP (Trusted Channel) | TLS 1.2/1.3 for all remote management                |

## OCI Image Specification Compliance

visor consumes OCI container images (pull, unpack, boot as VM). We implement
the OCI Image Specification for image compatibility. This is table stakes for
enterprise adoption — customers expect any OCI image to work.

We do NOT implement the OCI Runtime Specification (we are not a runc replacement).
visor manages VM lifecycle through its own API, not the OCI runtime CLI.

## Security Policy (SECURITY.md)

Ship in the repository root:

- Vulnerability reporting process (security@visor.rs)
- Supported versions receiving security patches
- Disclosure timeline (90-day coordinated disclosure)
- Patch SLA (critical: 48h, high: 7d, medium: 30d)
- CVE tracking and advisory publication

## CI Security Pipeline

```
cargo check --workspace
cargo clippy --workspace -- -D warnings
cargo test --workspace
cargo deny check licenses        # No GPL in commercial binary
cargo deny check bans             # Banned crate list
cargo deny check advisories       # RustSec CVE database
cargo audit                       # Known vulnerability check
cargo sbom                        # SBOM generation
dprint check                      # Formatting
cosign sign                       # Binary signing (release only)
```

## Day-One Checklist

Decisions baked into P0 architecture:

```
[x] rustls + aws-lc-rs (FIPS by default) — not ring, not openssl
[x] Structured audit logging on every API endpoint
[x] Don't-trust-guest: validate all vsock messages host-side
[x] cargo-deny in CI (license + vulnerability)
[x] cargo-sbom in release CI (CycloneDX alongside binary)
[x] SECURITY.md in repository root
[ ] Seccomp filter on daemon process (P1)
[ ] AppArmor/SELinux default profiles (P1)
[ ] cosign binary signing (first release)
[ ] SLSA L2 provenance (first release)
[ ] Hardening guide (first release)
```
