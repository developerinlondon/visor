# visor-kernel

Guest kernel management for visor microVMs.

This crate does **not** compile the kernel. It resolves a pre-built kernel binary at
`cargo build` time and exposes `kernel_path()` for the runtime to load into VM memory.

The kernel itself is compiled separately via `scripts/build-kernel.sh` using the
[mainline Linux](https://github.com/torvalds/linux) (`v7.0-rc1`).

## Architecture Support

| Architecture | Binary           | Format                 | Console             | Interrupt Controller |
| ------------ | ---------------- | ---------------------- | ------------------- | -------------------- |
| x86_64       | `vmlinux-x86_64` | Uncompressed ELF       | `ttyS0` (8250 UART) | APIC                 |
| aarch64      | `Image-aarch64`  | ARM64 Linux Image (PE) | `ttyAMA0` (PL011)   | GICv3                |

## Directory Layout

```
visor-kernel/
+-- build.rs                        # Resolves pre-built kernel at cargo build time
+-- src/
|   +-- lib.rs                      # Exports kernel_path() -> PathBuf
|   +-- lib_test.rs                 # Tests: file exists, magic bytes, minimum size
+-- config/
|   +-- fragments/                  # Human-edited source of truth
|   |   +-- x86_64/                 # x86_64-specific config fragments
|   |   |   +-- base.config         # Architecture, identity, namespaces, filesystems
|   |   |   +-- security.config     # Stack hardening, MCE, mitigations, LSM
|   |   |   +-- devices.config      # VirtIO enables, 8250 UART, dead driver disables
|   |   |   +-- perf.config         # CPU freq, hotplug, debug flags
|   |   |   +-- intentional.config  # Documented tradeoffs (IPv6, BPF, TIME_NS)
|   |   |   +-- rust.config         # CONFIG_RUST, XFS, VFS features
|   |   +-- aarch64/                # aarch64-specific config fragments
|   |       +-- base.config         # ARM64 arch, FDT, PSCI, namespaces, filesystems
|   |       +-- security.config     # Stack hardening, ARM mitigations, LSM
|   |       +-- devices.config      # VirtIO enables, PL011 UART, GICv3
|   |       +-- perf.config         # CPU freq, hotplug, debug flags
|   |       +-- intentional.config  # Documented tradeoffs (IPv6, BPF, TIME_NS)
|   |       +-- rust.config         # XFS, VFS features (CONFIG_RUST disabled for now)
|   +-- visor-kernel.config         # GENERATED lockfile (x86_64) — do not hand-edit
+-- scripts/
    +-- build-kernel.sh             # Clone + resolve + compile + install
    +-- resolve-config.sh           # Merge fragments into lockfile via kernel kconfig
```

## How It Works

### Overview: From Config to Running VM

```
 Source of truth              Generated                Compiled             Runtime
 (human-edited)               (lockfile)               (binary)            (in memory)
+------------------+     +------------------+     +----------------+     +-----------+
| fragments/{arch}/| --> | visor-kernel     | --> | vmlinux-x86_64 | --> | VM guest  |
| (~110 lines)     |     | .config          |     | or Image-arm64 |     |           |
+------------------+     | (~3,200 lines)   |     +----------------+     +-----------+
        |                +------------------+            |                     ^
        |                        |                       |                     |
 resolve-config.sh        build-kernel.sh          build.rs             visor-runtime
 (merge + olddefconfig)   (make vmlinux/Image)     (copy to OUT_DIR)   (load into memory)
```

### Step 1: Config Resolution (fragments → lockfile)

The kernel config uses a **fragment-based system**, analogous to `Cargo.toml` → `Cargo.lock`.
Fragments are organized per architecture:

```
fragments/x86_64/                       visor-kernel.config
+------------------+                    +-------------------+
| base.config      |--+                 |                   |
| (76 lines)       |  |                 | # Automatically   |
+------------------+  |   merge_config  | # generated from  |
| security.config  |--+-- .sh + make -->| # fragments.      |
| (48 lines)       |  |   olddefconfig  | # Do not edit.    |
+------------------+  |                 |                   |
| devices.config   |--+                 | CONFIG_64BIT=y    |
| perf.config      |--+                 | CONFIG_SMP=y      |
| intentional      |--+                 | ...               |
| rust.config      |--+                 | (~3,200 lines)    |
+------------------+                    +-------------------+
```

**Why fragments?** The full kernel config is ~3,200 lines of auto-generated defaults. Only ~110 lines
are actual decisions. Fragments capture _only_ the decisions, with comments explaining each one.
The lockfile is checked into git for reproducibility — anyone can rebuild the exact same kernel
without re-resolving.

### Step 2: Kernel Compilation (lockfile → binary)

The output varies by architecture:

- **x86_64**: `make vmlinux` → `vmlinux-x86_64` (~30 MB ELF)
- **aarch64**: `make Image` → `Image-aarch64` (~30 MB PE)

`scripts/build-kernel.sh` handles this: auto-detects architecture, shallow-clones the kernel
source (if not cached), resolves fragments, and compiles the target.

### Step 3: Binary Resolution at `cargo build` (build.rs)

`build.rs` does **not** compile the kernel. It finds an already-built binary through
a 4-step fallback chain:

```
build.rs resolution chain
+------------------------------------------------------------------+
|                                                                  |
|  1. OUT_DIR cache                                                |
|     +-- Already in target/? -----> YES --> done                  |
|     +-- NO                                                       |
|         |                                                        |
|  2. VISOR_KERNEL_PATH env var                                    |
|     +-- Set and valid? ---------> YES --> copy to OUT_DIR, done  |
|     +-- NO                                                       |
|         |                                                        |
|  3. /var/lib/visor/kernel/{filename}                             |
|     +-- Exists and >= 1 MB? ----> YES --> copy to OUT_DIR, done  |
|     +-- NO                                                       |
|         |                                                        |
|  4. GitLab release download                                      |
|     +-- GITLAB_TOKEN set? ------> YES --> download, done         |
|     +-- NO --> panic! with help message                          |
|                                                                  |
+------------------------------------------------------------------+
```

The kernel filename is determined by `CARGO_CFG_TARGET_ARCH` and emitted as
`VISOR_KERNEL_FILENAME` for `lib.rs` to use.

### Step 4: Runtime Loading

```
visor-runtime (binary)                visor-kernel (library)
+-------------------------+          +-------------------------+
|                         |          |                         |
| let kernel =            |          | pub fn kernel_path()    |
|   visor_kernel::        | -------> |   -> PathBuf            |
|   kernel_path();        |          |                         |
|                         |          | Returns path baked in   |
| // Load kernel into VM  |          | at compile time via     |
| // guest memory         |          | env!("OUT_DIR")         |
|                         |          |                         |
+-------------------------+          +-------------------------+
```

`kernel_path()` is infallible — if no kernel was found, `build.rs` already panicked during
compilation. At runtime, the path always points to a valid kernel binary.

## Common Tasks

### Modify the kernel config

1. Edit the relevant fragment in `config/fragments/{arch}/`:

   | Fragment             | What it controls                                |
   | -------------------- | ----------------------------------------------- |
   | `base.config`        | Architecture, identity, namespaces, cgroups, FS |
   | `security.config`    | Stack init, mitigations, LSM, seccomp           |
   | `devices.config`     | VirtIO enables, UART, dead driver disables      |
   | `perf.config`        | CPU freq, hotplug, debug, memory hotplug        |
   | `intentional.config` | Documented tradeoffs (IPv6, BPF, TIME_NS)       |
   | `rust.config`        | CONFIG_RUST, XFS, VFS mount API                 |

2. Re-resolve the lockfile:

   ```bash
   ./crates/visor-kernel/scripts/resolve-config.sh
   ```

3. Review changes:

   ```bash
   git diff crates/visor-kernel/config/visor-kernel.config
   ```

4. Commit **both** `fragments/` and the lockfile together.

### Rebuild the kernel from source

```bash
./crates/visor-kernel/scripts/build-kernel.sh
```

This auto-detects the host architecture, clones the kernel source (if not cached),
resolves fragments, compiles the kernel, and installs to `/var/lib/visor/kernel/`.

Override the architecture:

```bash
VISOR_KERNEL_ARCH=aarch64 ./crates/visor-kernel/scripts/build-kernel.sh
```

Custom output directory:

```bash
./crates/visor-kernel/scripts/build-kernel.sh /tmp/my-kernel
```

### Build visor with a custom kernel

```bash
VISOR_KERNEL_PATH=/path/to/my/kernel cargo build -p visor-runtime
```

### Build visor via GitLab download (CI)

```bash
GITLAB_TOKEN=glpat-xxx cargo build -p visor-runtime
```

## Fragment Format

Fragments use standard Linux kconfig syntax:

```kconfig
# Comment explaining the decision
CONFIG_FEATURE=y              # Enable a feature
# CONFIG_OTHER is not set     # Explicitly disable (NOT deletion — kconfig requires this form)
CONFIG_STRING="value"         # String option
CONFIG_NUMBER=4               # Numeric option
```

**Important**: To disable a kernel option, use `# CONFIG_X is not set`. Simply omitting the line
lets `olddefconfig` choose the default (which may be `=y`).

## Kernel Details

| Property         | x86_64                           | aarch64                 |
| ---------------- | -------------------------------- | ----------------------- |
| Source           | Mainline Linux (torvalds/linux)  | Same                    |
| Tag              | `v7.0-rc1`                       | Same                    |
| Binary format    | Uncompressed ELF (`vmlinux`)     | ARM64 Image (PE format) |
| Modules          | None (`CONFIG_MODULES` not set)  | Same                    |
| Rust             | `CONFIG_RUST=y` (LLVM toolchain) | Not yet (planned)       |
| Console          | `ttyS0` (8250 UART)              | `ttyAMA0` (PL011)       |
| Interrupts       | APIC                             | GICv3                   |
| Power mgmt       | ACPI                             | PSCI                    |
| Device discovery | ACPI tables                      | Device Tree (FDT)       |
| Size             | ~30 MB                           | ~30 MB                  |

## Tests

```bash
cargo test -p visor-kernel
```

Tests verify the resolved kernel binary:

1. **`kernel_path_returns_existing_file`** — `build.rs` successfully resolved a kernel
2. **`kernel_file_has_correct_magic_bytes`** — x86_64: ELF magic (`\x7fELF`), aarch64: ARM64 magic (`ARM\x64` at offset 0x38)
3. **`kernel_file_is_at_least_1mb`** — not a truncated download or error page
4. **`kernel_version_starts_with_linux_version`** — version string extracted
5. **`kernel_size_matches_file`** — compile-time size matches runtime
6. **`kernel_sha256_is_valid_hex`** — valid 64-char hex hash
