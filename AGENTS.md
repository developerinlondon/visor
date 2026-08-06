# AGENTS.md — Visor

## Post-Edit Hooks (MANDATORY)

After creating or editing ANY file, run:

```bash
dprint fmt
```

This formats all markdown, TOML, JSON files. Config lives in `dprint.json`.

**This is non-negotiable. Every file change must be followed by `dprint fmt`.**

To check without modifying:

```bash
dprint check
```

## Quality Gates (Every Task)

A task is **DONE** when ALL pass:

- [ ] `cargo check --workspace` passes
- [ ] `cargo test --workspace` passes
- [ ] `cargo clippy --workspace -- -D warnings` zero warnings
- [ ] `dprint check` passes (markdown/json/toml)
- [ ] No `unwrap()` / `expect()` in production paths (only in `#[cfg(test)]`)
- [ ] No type safety bypasses (`#[allow(clippy::*)]` in production)
- [ ] Public API has doc comments (including `# Errors` for Result-returning functions)
- [ ] Test coverage for new code (companion `_test.rs` files)
- [ ] No file exceeds 1,500 lines (split at 1,000)
- [ ] `.context()` on every `?` in binary crate code (visor-runtime)
- [ ] Conventional commit message
- [ ] LSP diagnostics clean on all changed files

## Project Context

- **Name**: Visor — in-process VMM for OCI containers as microVMs
- **Repo**: `git@gitlab.com:agentx.rs/visor.git`
- **Binary**: `visor`
- **Stack**: Rust stable, edition 2024, resolver 3
- **Branch**: `dev` (feature work on `phase1/*` branches, MR to `dev`)
- **Methodology**: TDD (tests FIRST — red, green, refactor)
- **Plans**: `docs/plans/` (12 architecture docs, 00-11)
- **Phase**: P5 complete — Cross-platform VMM refactor done

## Crate Namespace

All crates live under `crates/` and use `visor-` prefix:

| Crate            | Type    | Description                                                    |
| ---------------- | ------- | -------------------------------------------------------------- |
| `visor-vmm`      | Library | VMM core — hypervisor, devices, transport, net, comms, sandbox |
| `visor-runtime`  | Binary  | Daemon + CLI + API + OCI + networking + pool                   |
| `visor-init`     | Binary  | Guest PID 1 (static musl, runs inside VM)                      |
| `visor-kernel`   | Library | Guest kernel download/build/resolution                         |
| `visor-operator` | Binary  | K8s operator (P2, stub for now)                                |

Single binary output: `visor` (from visor-runtime's main.rs).

## Testing (TDD — NON-NEGOTIABLE)

**Tests are written FIRST.** Red-green-refactor. Every feature starts with a failing test.

### TDD Workflow (MANDATORY for every change)

1. **RED**: Write the test(s) first — they MUST fail
2. **Verify RED**: Run `cargo test` to confirm the test fails for the right reason
3. **GREEN**: Write the MINIMAL implementation to make the test pass
4. **Verify GREEN**: Run `cargo test` to confirm all tests pass
5. **REFACTOR**: Clean up while keeping tests green

**No code without a test. No test without seeing it fail first.**

This applies to:

- New features (test the behavior before writing it)
- Bug fixes (write a test that reproduces the bug, then fix it)
- Refactoring (ensure tests cover the behavior before changing it)
- API endpoints (test request/response contract before implementing)
- CLI commands (test arg parsing before wiring the command)

### Anti-Patterns (TDD violations — BLOCKING)

- Writing implementation before tests
- Writing tests that pass immediately (never saw RED)
- Skipping the RED verification step
- Writing tests after the fact to "backfill coverage"
- Deleting or modifying tests to make them pass instead of fixing code

### Test File Convention

Tests live in **companion files**, NEVER inline modules. This keeps production files lean and
prevents AI agents from wasting tokens reading test code when they only need the implementation
(and vice versa).

```
crates/visor-machine/src/
  memory.rs              # production code ONLY — no #[cfg(test)] mod tests {} here
  memory_test.rs         # ALL tests for memory.rs
```

Source file includes **one line** at the bottom to link the companion test file:

```rust
#[cfg(test)]
#[path = "memory_test.rs"]
mod tests;
```

**Rules:**

- Production files contain ZERO test code — no `#[cfg(test)]` blocks, no test helpers
- Test files contain ALL tests — unit tests, edge cases, property tests
- Test files CAN import from the parent module: `use super::*;`
- Test helper functions shared across test files go in a `testutil.rs` or `tests/common/mod.rs`
- Integration tests live in `tests/` at the workspace root

### Test Environment

Development and testing happens on **AX41** (AMD Ryzen 5 3600, 64GB, `/dev/kvm`). All tests run
directly — no `#[ignore]` markers needed. Every test is expected to pass on this machine.

```bash
cargo test --workspace            # ALL tests, including those needing /dev/kvm
```

## Dependency Management (MANDATORY)

### Always Use Latest Stable Versions

**Before writing any code that introduces a dependency:**

1. Look up the latest stable version on crates.io
2. Check for breaking changes (major version bumps)
3. Read the crate's changelog or migration guide if major version changed
4. Use `cargo search <crate>` or check crates.io directly

**Never assume a version from memory. Always verify.**

### Workspace Dependencies

All versions pinned in root `Cargo.toml` via `[workspace.dependencies]`. Member crates use
`{ workspace = true }`:

```toml
# Root Cargo.toml
[workspace.dependencies]
tokio = { version = "1.49", features = ["full"] }

# crates/visor-machine/Cargo.toml
[dependencies]
tokio = { workspace = true }
```

## Rust Coding Standards

### Error Handling

- **Library crates** (`visor-machine`, `visor-kernel`): `thiserror` for typed errors
- **Binary crates** (`visor-runtime`, `visor-init`): `anyhow` with `.context("what failed")`
  on every `?`
- **Never**: `Box<dyn Error>`, bare `unwrap()`, empty `catch {}`

```rust
// Library crate — typed errors
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum VmError {
    #[error("KVM ioctl failed: {0}")]
    Kvm(#[from] kvm_ioctls::Error),
    #[error("memory region {index} overlaps existing region")]
    MemoryOverlap { index: usize },
}

// Binary crate — anyhow with context
let vm = machine.create_vm(&config)
    .context("failed to create VM")?;
```

### Statics

- **Use `std::sync::LazyLock`** (stable since Rust 1.80)
- **Never**: `once_cell`, `lazy_static`

### Smart Pointers & Ownership

- `Arc<str>` for immutable shared strings
- `Arc<T>` for config/state shared across async tasks
- `Cow<'_, str>` for parameters that might or might not allocate
- Accept `&str` not `&String` in function parameters

### Enums

- `#[non_exhaustive]` on all public enums/structs that may gain variants
- `#[default]` on the default variant of enums with `Default`

### Async & Networking

- **Reuse `reqwest::Client`** — create once, share via `Arc`. Never per-request.
- **Always set timeouts** — connect (10s) and request (30s).
- **Use `tokio::select!`** for cancellation and concurrent operations.

### File Size

- Soft limit: **1,000 lines per file** (split proactively)
- Hard limit: **1,500 lines per file**

### Compile-Time Guarantees

- `#[deny(unsafe_code)]` at crate level (except visor-vmm platform/ and boot/)
- `cargo clippy -- -D warnings` must pass — **zero warnings tolerated**
- `#[must_use]` on functions returning `Result` or important values

### Unsafe Code Policy

`visor-vmm` requires `unsafe` for KVM ioctls, mmap, and hardware interaction.
Unsafe code is restricted to:

- `crates/visor-vmm/src/platform/linux.rs` — KVM ioctls
- `crates/visor-vmm/src/platform/macos.rs` — Apple HVF calls
- `crates/visor-vmm/src/memory.rs` — mmap operations
- `crates/visor-vmm/src/boot/` — boot protocol setup
- `crates/visor-vmm/src/net/macos.rs` — vmnet `Send` wrapper (`SendableInterface`)

Every `unsafe` block must have a `// SAFETY:` comment explaining the invariants.
All other crates use `#[deny(unsafe_code)]`.

## Anti-Patterns (NEVER do these)

- Never use `unwrap()` / `expect()` in production code — only in `#[cfg(test)]`
- Never use `once_cell` or `lazy_static` — use `std::sync::LazyLock`
- Never use `Box<dyn Error>` — use `thiserror` (libs) or `anyhow` (bins)
- Never create `reqwest::Client` per-request — reuse shared client
- Never suppress Rust warnings — `cargo clippy -D warnings` must pass
- Never use `#[allow(clippy::*)]` in production code without documented exception
- Never assume crate versions — always check crates.io before adding dependencies
- Never skip `dprint fmt` after editing files
- Never skip `#[non_exhaustive]` on public enums/structs that may grow
- Never commit broken code — quality gates must pass before every commit
- Never look in the `maestro` codebase
- Never write inline `#[cfg(test)] mod tests {}` blocks — use companion `_test.rs` files
- Never trust the guest — validate all vsock messages host-side

## Directory Structure

```
visor/
+-- Cargo.toml                            # Workspace root (resolver = "3")
+-- dprint.json
+-- deny.toml                             # cargo-deny config (license + vuln)
+-- AGENTS.md                             # This file
+-- SECURITY.md                           # Vulnerability reporting policy
+-- crates/
|   +-- visor-vmm/                        # Library: VMM core (platform abstraction)
|   +-- visor-vmm/                        # Library: platform abstraction (replaces visor-machine)
|   |   +-- src/
|   |       +-- lib.rs
|   |       +-- memory.rs                  # Guest memory (generic over VmOps)
|   |       +-- vcpu.rs                    # vCPU run loop (cfg-gated per OS)
|   |       +-- snapshot.rs                # VM snapshots (CPU parts cfg-gated)
|   |       +-- cpu_template.rs            # CPU templates (apply cfg-gated)
|   |       +-- acpi.rs                    # ACPI table generation
|   |       +-- metrics.rs                 # VMM metrics
|   |       +-- migration.rs               # Live migration
|   |       +-- dirty_tracking.rs          # Memory dirty tracking
|   |       +-- seccomp.rs                 # Seccomp BPF filters
|   |       +-- platform/                  # Hypervisor traits + impls
|   |       |   +-- mod.rs                 # Platform, VmOps, VcpuOps, VmExit traits
|   |       |   +-- event.rs               # InterruptEvent trait + LinuxEventFd
|   |       |   +-- regs.rs                # Portable register types
|   |       |   +-- linux.rs               # KVM implementation
|   |       |   +-- macos.rs               # Apple HVF (stub)
|   |       |   +-- windows.rs             # Windows WHP (stub)
|   |       +-- devices/                   # Virtio device models (portable)
|   |       |   +-- mod.rs
|   |       |   +-- block.rs               # virtio-blk
|   |       |   +-- net.rs                 # virtio-net
|   |       |   +-- vsock.rs               # virtio-vsock
|   |       |   +-- gpu.rs                 # virtio-gpu (VFIO passthrough)
|   |       |   +-- balloon.rs             # virtio-balloon
|   |       |   +-- fs.rs                  # virtio-fs
|   |       |   +-- rng.rs                 # virtio-rng
|   |       |   +-- vfio.rs                # VFIO device passthrough
|   |       |   +-- serial.rs              # Serial console wrapper
|   |       |   +-- uart.rs                # UART 16550 emulator
|   |       |   +-- bus.rs                 # Device bus
|   |       +-- transport/                 # Virtio transport layers
|   |       |   +-- mod.rs
|   |       |   +-- mmio.rs                # virtio-mmio
|   |       |   +-- pci.rs                 # virtio-pci
|   |       |   +-- pci_bus.rs             # PCI bus management
|   |       +-- boot/                      # Boot protocol setup
|   |       |   +-- mod.rs
|   |       |   +-- x86_64.rs              # x86_64 boot (portable)
|   |       +-- net/                       # Networking (TAP / NAT / port-forward)
|   |       |   +-- mod.rs
|   |       |   +-- backend.rs             # NetworkBackend trait
|   |       |   +-- linux.rs               # TAP + iptables
|   |       |   +-- macos.rs               # vmnet (stub)
|   |       |   +-- windows.rs             # WinSock (stub)
|   |       +-- comms/                     # Guest communication (vsock / VZSocket)
|   |       |   +-- mod.rs
|   |       |   +-- backend.rs             # CommsBackend trait
|   |       |   +-- linux.rs               # AF_VSOCK
|   |       |   +-- macos.rs               # VZSocket (stub)
|   |       |   +-- windows.rs             # Hyper-V socket (stub)
|   |       +-- sandbox/                   # Sandboxing (seccomp / App Sandbox / Job Objects)
|   |       |   +-- mod.rs
|   |       |   +-- backend.rs             # SandboxBackend trait
|   |       |   +-- linux.rs               # seccomp BPF
|   |       |   +-- macos.rs               # App Sandbox (stub)
|   |       |   +-- windows.rs             # Job Objects (stub)
|   |       +-- rate_limit/                # I/O rate limiting
|   |           +-- mod.rs
|   |           +-- disk.rs                # Disk I/O rate limiting
|   |           +-- net.rs                 # Network rate limiting
|   +-- visor-runtime/                    # Binary: daemon + CLI
|   |   +-- src/
|   |       +-- lib.rs
|   |       +-- main.rs
|   |       +-- daemon.rs
|   |       +-- backend.rs
|   |       +-- oci/
|   |       +-- net/
|   |       +-- vsock/
|   |       +-- pool/
|   |       +-- api/
|   |       +-- cli/
|   +-- visor-init/                       # Binary: guest PID 1 (static musl)
|   +-- visor-kernel/                     # Library: kernel management
|   +-- visor-operator/                   # Binary: K8s operator (P2)
+-- tests/                                # Integration tests
+-- docs/plans/                           # 12 architecture docs (00-11)
```

## Commands

```bash
cargo check --workspace                   # Type check all crates
cargo test --workspace                    # ALL tests (we are on AX41)
cargo clippy --workspace -- -D warnings   # Lint (warnings = errors)
dprint fmt                                # Format markdown/toml/json
dprint check                              # Check formatting
cargo build -p visor-runtime --release    # Build visor binary
cargo build -p visor-init --release --target x86_64-unknown-linux-musl  # Guest init
cargo deny check                          # License + vulnerability audit
```

## Runtime & Manual Testing (macOS)

### Sudo Requirement

The visor daemon requires `sudo` for HVF access and `/var/run/visor`. **Agents cannot run sudo
non-interactively.** When testing requires starting/stopping the daemon, **ask the user to run
the commands** instead of attempting them directly.

### Codesigning (macOS only)

After every `cargo build -p visor-runtime --release`, the binary MUST be codesigned:

```bash
codesign --sign - --entitlements entitlements.plist --force ./target/release/visor
```

Without this, HVF calls will fail with `HV_DENIED`.

### Starting the Daemon

```bash
sudo ./target/release/visor start           # Background (default listen 0.0.0.0:7800)
sudo ./target/release/visor start --foreground  # Foreground (logs to stdout)
```

No need to pass `--listen` — default is already `0.0.0.0:7800`.

### Stopping the Daemon

```bash
./target/release/visor stop                  # Graceful shutdown (NOT pkill)
```

### Daemon Logs

- Background mode: `~/.visor/visor-daemon.log`
- Foreground mode: stdout/stderr

For debug-level logs (e.g. vsock diagnostics), use `RUST_LOG`:

```bash
sudo RUST_LOG=debug ./target/release/visor start
```

**Never change log levels in code for debugging.** Use `RUST_LOG=debug` or
`RUST_LOG=visor_vmm=debug` at runtime instead.

### Docker Testing

```bash
DOCKER_HOST=tcp://127.0.0.1:7800 docker run --rm alpine echo "hello from visor"
```

### Build + Test Cycle (Full)

```bash
# 1. Build + codesign (agent can do this)
cargo build -p visor-runtime --release
codesign --sign - --entitlements entitlements.plist --force ./target/release/visor

# 2. Ask user to restart daemon (agent CANNOT do this — requires sudo)
# User runs: ./target/release/visor stop && sudo RUST_LOG=debug ./target/release/visor start

# 3. Ask user to test (agent CANNOT do this — docker needs running daemon)
# User runs: DOCKER_HOST=tcp://127.0.0.1:7800 docker run --rm alpine echo "hello from visor"

# 4. Ask user to share logs
# User runs: cat ~/.visor/visor-daemon.log
```

## External Forks

Clone external repos outside the project tree. **Fork freely** — if you need a dependency's
source code for reference during implementation, clone it without asking. **Always report** what
you forked and why. Never clone into the project tree.

## Reference Material

- **Plan docs**: `docs/plans/00-overview.md` through `docs/plans/11-security-and-compliance.md`
- **rust-vmm crates**: https://github.com/rust-vmm
- **Firecracker source** (VMM reference): https://github.com/firecracker-microvm/firecracker
