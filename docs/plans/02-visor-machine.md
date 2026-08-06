# 02 — visor-machine (VMM Core)

The VMM engine. Handles KVM/HVF, vCPUs, memory, devices, snapshots.

## Platform Abstraction

~90% of visor-machine is platform-agnostic Rust. Platform-specific code is
isolated to thin shims:

```
platform/
    linux.rs       # KVM API — create VM, create vCPU, set memory region, KVM_RUN
    macos.rs       # Apple HVF API — same operations, different syscalls

boot/
    x86_64.rs      # Intel/AMD: GDT, MSRs, CPUID leaves, initial page tables
    aarch64.rs     # ARM/Apple Silicon: FDT, system regs, GICv3 interrupt controller
```

Everything else is shared: virtio devices, snapshot logic, memory management,
the vCPU run loop structure, rate limiting, metrics.

### What Each Platform File Does

**`platform/linux.rs`** (~300-500 lines):

```rust
// Wraps kvm-ioctls crate
pub fn create_vm(kvm: &Kvm) -> Result<VmFd>
pub fn create_vcpu(vm: &VmFd, id: u8) -> Result<VcpuFd>
pub fn set_memory_region(vm: &VmFd, slot: u32, addr: u64, size: usize, host: *mut u8) -> Result<()>
pub fn run_vcpu(vcpu: &VcpuFd) -> Result<VcpuExit>
pub fn set_regs(vcpu: &VcpuFd, regs: &Registers) -> Result<()>
pub fn get_regs(vcpu: &VcpuFd) -> Result<Registers>
```

**`platform/macos.rs`** (~300-500 lines):

```rust
// Wraps applevisor crate
pub fn create_vm() -> Result<VmHandle>           // hv_vm_create()
pub fn create_vcpu(vm: &VmHandle) -> Result<VcpuHandle>  // hv_vcpu_create()
pub fn set_memory_region(...) -> Result<()>       // hv_vm_map()
pub fn run_vcpu(vcpu: &VcpuHandle) -> Result<VcpuExit>   // hv_vcpu_run()
// ... same shape, different syscalls
```

### What Each Boot File Does

**`boot/x86_64.rs`** (~400-600 lines):

- Build GDT (Global Descriptor Table) with code/data segments
- Set MSRs (Model-Specific Registers) — EFER, STAR, LSTAR for syscalls
- Configure CPUID leaves (what the guest sees as CPU features)
- Set up initial page tables (identity-mapped for kernel boot)
- Set initial register state (RIP = kernel entry, RSP = stack top)

**`boot/aarch64.rs`** (~400-600 lines):

- Build FDT (Flattened Device Tree) — memory, CPU, devices
- Configure system registers (SCTLR_EL1, TCR_EL1, MAIR_EL1)
- Set up GICv3 interrupt controller (distributor + redistributor)
- Set initial register state (PC = kernel entry, X0 = FDT address)

## Crate Layout

```
visor-machine/
+-- src/
    +-- lib.rs                  # Public API: Vmm, VmConfig, VmHandle
    +-- vm.rs                   # VM lifecycle (create, start, pause, destroy)
    +-- vcpu.rs                 # vCPU thread, KVM_RUN/HVF_RUN loop, exit handling
    +-- memory.rs               # Guest memory (mmap, huge pages, CoW, ballooning)
    +-- snapshot.rs             # Custom <5ms save/restore
    +-- config.rs               # VmConfig, CPU templates
    +-- metrics.rs              # Per-VM CPU/memory/disk/net counters
    +-- platform/
    |   +-- mod.rs              # Platform trait
    |   +-- linux.rs            # KVM (kvm-ioctls)
    |   +-- macos.rs            # Apple HVF (applevisor)
    +-- devices/
    |   +-- mod.rs              # Device registry
    |   +-- block.rs            # virtio-blk (host file → guest /dev/vdX)
    |   +-- net.rs              # virtio-net (connects to virtual switch)
    |   +-- vsock.rs            # virtio-vsock (host↔guest communication)
    |   +-- serial.rs           # UART 16550 / virtio-console
    |   +-- rng.rs              # virtio-rng (entropy)
    |   +-- balloon.rs          # virtio-balloon (memory reclaim)
    |   +-- gpu.rs              # VFIO GPU passthrough
    |   +-- fs.rs               # virtio-fs (host dir passthrough)
    +-- transport/
    |   +-- mmio.rs             # virtio-mmio (P0)
    |   +-- pci.rs              # virtio-pci (P2, for VFIO)
    +-- rate_limit/
    |   +-- disk.rs             # Per-drive I/O throttling
    |   +-- net.rs              # Per-NIC bandwidth throttling
    +-- seccomp.rs              # Seccomp BPF filters
    +-- boot/
        +-- x86_64.rs           # x86_64 boot setup
        +-- aarch64.rs          # ARM64 boot setup
```

## Public API

```rust
/// Create and manage virtual machines.
pub struct Vmm { /* platform handle, device registry */ }

impl Vmm {
    pub fn new(config: VmmConfig) -> Result<Self>;
    pub fn create_vm(&self, config: VmConfig) -> Result<VmHandle>;
}

/// Handle to a running VM.
pub struct VmHandle { /* vm_fd, vcpu threads, memory, devices */ }

impl VmHandle {
    pub fn start(&self) -> Result<()>;
    pub fn pause(&self) -> Result<()>;
    pub fn resume(&self) -> Result<()>;
    pub fn snapshot(&self) -> Result<SnapshotData>;
    pub fn destroy(self) -> Result<()>;  // consumes self — RAII
    pub fn metrics(&self) -> VmMetrics;
}

/// Restore a VM from a snapshot (the <5ms path).
pub fn restore_from_snapshot(vmm: &Vmm, snap: &SnapshotData) -> Result<VmHandle>;
```

## Snapshot Format

Three files per golden snapshot:

```
~/.visor/cache/<image_digest>/golden/
    memory.bin       # Guest RAM (sparse file, only touched pages non-zero)
    cpu_state.bin    # vCPU registers (~500 bytes per vCPU)
    device_state.bin # Virtio queue positions, serial/vsock state (~1KB)
```

Restore path:

1. `mmap(MAP_PRIVATE, memory.bin)` — O(1), zero data copied
2. `KVM_SET_USER_MEMORY_REGION` — point KVM at mmap'd region
3. `KVM_SET_REGS` + `KVM_SET_SREGS` — restore ~100 bytes per vCPU
4. `bincode::deserialize(device_state.bin)` — ~1KB
5. `KVM_RUN` — resume

Total: <5ms regardless of VM memory size.
