# Cross-Platform Refactor Plan

| Field        | Value                                                |
| ------------ | ---------------------------------------------------- |
| Status       | Draft                                                |
| Created      | 2026-03-01                                           |
| Author       | AI Agent                                             |
| Dependencies | Phase 0 (Core VMM) substantially complete            |
| Crates       | `visor-machine`, `visor-runtime`                     |
| Targets      | Linux (KVM), macOS (Hypervisor.framework), Win (WHP) |

## 1. The Problem

Visor's codebase is shot through with Linux-only assumptions. The `Platform` trait
exists as an abstraction point, but its actual surface is too thin. KVM types bleed
through it everywhere: into the runtime's VM boot code, into device interrupt wiring,
into memory registration, and into the vCPU run loop.

Below is every coupling point, mapped to source files.

### 1.1 Linux Leakage Inventory

| Category           | File(s)                                    | What leaks                                                                                           |
| ------------------ | ------------------------------------------ | ---------------------------------------------------------------------------------------------------- |
| **Hypervisor**     | `visor-machine/src/platform/linux.rs`      | `kvm_ioctls::Kvm`, `kvm_ioctls::VmFd`                                                                |
| **Platform trait** | `visor-machine/src/platform/mod.rs`        | `type Vm = VmFd` exposes KVM's fd directly                                                           |
| **vCPU**           | `visor-machine/src/vcpu.rs`                | `kvm_ioctls::VcpuFd`, `kvm_bindings::*`, `VcpuExit` enum                                             |
| **Memory**         | `visor-machine/src/memory.rs`              | `kvm_bindings::kvm_userspace_memory_region`, `VmFd` in `register()`                                  |
| **VM Boot**        | `visor-runtime/src/vm.rs`                  | `KvmPlatform` hard-imported, `VmFd.create_irq_chip()`, `VmFd.create_pit2()`, `VmFd.register_irqfd()` |
| **Devices (irq)**  | `visor-runtime/src/vm.rs` (`wire_devices`) | `vmm_sys_util::eventfd::EventFd` for irqfd, `VmFd.register_irqfd()`                                  |
| **Serial**         | `visor-machine/src/devices/serial.rs`      | `vm_superio::Serial` (depends on `vmm-sys-util`), `EventFd`, `libc::EFD_NONBLOCK`                    |
| **Seccomp**        | `visor-machine/src/seccomp.rs`             | `seccompiler` (Linux-only BPF), `libc::SYS_*` constants                                              |
| **Networking**     | `visor-runtime/src/net/tap.rs`             | `/dev/net/tun`, `ip tuntap` shell commands                                                           |
| **NAT**            | `visor-runtime/src/net/nat.rs`             | `iptables` shell commands for MASQUERADE                                                             |
| **Port Fwd**       | `visor-runtime/src/net/port_forward.rs`    | `iptables` DNAT rules                                                                                |
| **Vsock client**   | `visor-runtime/src/vsock/client.rs`        | `nix::sys::socket::AddressFamily::Vsock`, `AF_VSOCK`                                                 |
| **Huge pages**     | `visor-machine/src/memory.rs`              | `/sys/kernel/mm/hugepages`, `MAP_HUGETLB`                                                            |
| **Deps**           | `visor-machine/Cargo.toml`                 | `kvm-bindings`, `kvm-ioctls`, `vmm-sys-util`, `vm-superio`, `seccompiler`                            |
| **Deps**           | `visor-runtime/Cargo.toml`                 | `kvm-bindings`, `kvm-ioctls`, `vmm-sys-util`, `nix`                                                  |

### 1.2 Why This Hurts

```
visor-runtime/src/vm.rs (binary crate)
     |
     | hard-codes KvmPlatform, VmFd, kvm_bindings,
     | eventfd, irqfd registration
     |
     v
visor-machine/src/platform/mod.rs
     |
     | Platform trait returns kvm_ioctls::VmFd
     | via `type Vm`
     |
     v
visor-machine/src/vcpu.rs
     |
     | Vcpu wraps VcpuFd, matches on VcpuExit,
     | uses kvm_bindings for regs/sregs/MSRs
     v
     NOTHING COMPILES ON macOS OR WINDOWS
```

The `Platform` trait was designed with the right idea (see the `macos.rs` placeholder
in the directory listing), but the abstraction boundary stopped at `create_vm()`.
Everything after that call, including memory registration, IRQ chip setup, vCPU creation,
the run loop, and interrupt delivery, uses raw KVM types directly.

## 2. The Goal

A clean architectural seam between `visor-runtime` (orchestration) and `visor-machine`
(hardware), where **zero platform-specific types cross the crate boundary**.

```
+---------------------------------------------------------------+
|                       visor-runtime                           |
|  vm.rs  |  net/*  |  vsock/*  |  pool/*  |  daemon.rs        |
|         |         |           |          |                    |
|  Uses only visor_machine::{Platform, Vm, Vcpu, ...} traits   |
|  Uses only NetworkBackend / CommsBackend traits               |
+---------------------------+-----------------------------------+
                            | trait objects / generics
                            | (no kvm_*, no eventfd, no iptables)
+---------------------------v-----------------------------------+
|                       visor-machine                           |
|                                                               |
|  +------------------+  +------------------+  +--------------+ |
|  | platform/        |  | devices/         |  | transport/   | |
|  |  mod.rs (traits) |  |  serial.rs       |  |  mmio.rs     | |
|  |  linux.rs  (KVM) |  |  block.rs        |  |  pci.rs      | |
|  |  macos.rs  (HVF) |  |  vsock.rs        |  +--------------+ |
|  |  windows.rs(WHP) |  |  net.rs          |                   |
|  +------------------+  +------------------+                   |
+---------------------------------------------------------------+
```

### 2.1 Design Principles

1. **Platform types stay inside `visor-machine/src/platform/`**. The trait surface
   returns opaque handles, not raw fds.
2. **No `kvm-*` crate imports in `visor-runtime`**. Period.
3. **Event signaling is abstracted**. Replace `EventFd` with a cross-platform
   `InterruptEvent` trait backed by `mio`/`polling` on all OSes.
4. **Networking is a trait**. TAP/iptables become one implementation of
   `NetworkBackend`. macOS gets `vmnet.framework`, Windows gets Hyper-V vSwitch.
5. **Vsock becomes `CommsBackend`**. `AF_VSOCK` on Linux, named pipes or TCP
   loopback on macOS/Windows where vsock isn't available.
6. **Seccomp is `cfg(target_os = "linux")`**. macOS uses `sandbox-exec` (App Sandbox),
   Windows uses Job Objects. Each behind a `SandboxBackend` trait.

## 3. Phase 1: Hypervisor Abstraction

**Goal**: `visor-runtime` compiles without importing any `kvm-*` crate.

### 3.1 Expand the `Platform` Trait

The current `Platform` trait in `visor-machine/src/platform/mod.rs` is too narrow:

```rust
// CURRENT (too thin)
pub trait Platform: Sized {
    type Vm;
    fn new() -> Result<Self, PlatformError>;
    fn api_version(&self) -> i32;
    fn create_vm(&self) -> Result<Self::Vm, PlatformError>;
}
```

It needs to absorb every operation that `visor-runtime/src/vm.rs` currently does
with raw `VmFd`:

```rust
// TARGET
pub trait Platform: Sized + Send + Sync {
    type Vm: VmOps;
    type Vcpu: VcpuOps;

    fn new() -> Result<Self, PlatformError>;
    fn create_vm(&self) -> Result<Self::Vm, PlatformError>;
}

pub trait VmOps: Send {
    type Vcpu: VcpuOps;

    fn create_irq_chip(&self) -> Result<(), PlatformError>;
    fn create_pit(&self) -> Result<(), PlatformError>;
    fn register_memory(
        &self, slot: u32, guest_addr: u64,
        size: u64, host_addr: *mut u8,
    ) -> Result<(), PlatformError>;
    fn register_irqfd(
        &self, event: &dyn InterruptEvent, gsi: u32,
    ) -> Result<(), PlatformError>;
    fn create_vcpu(&self, index: u64) -> Result<Self::Vcpu, PlatformError>;
}

pub trait VcpuOps: Send {
    fn set_regs(&self, regs: &StandardRegs) -> Result<(), PlatformError>;
    fn set_sregs(&self, sregs: &SpecialRegs) -> Result<(), PlatformError>;
    fn run(&mut self) -> Result<VmExit, PlatformError>;
}
```

### 3.2 Portable Register Types

`vcpu.rs` currently uses `kvm_bindings::kvm_regs`, `kvm_sregs`, `kvm_fpu`, etc.
These must become visor-owned structs:

```rust
// visor-machine/src/platform/regs.rs (NEW)
pub struct StandardRegs {
    pub rip: u64, pub rsp: u64, pub rbp: u64,
    pub rsi: u64, pub rflags: u64,
    // ... remaining GPRs
}
pub struct SpecialRegs {
    pub cr0: u64, pub cr3: u64, pub cr4: u64,
    pub efer: u64,
    pub gdt: TableReg, pub idt: TableReg,
    pub cs: SegmentReg, pub ds: SegmentReg,
    // ... remaining segment regs
}
```

The Linux implementation converts these to/from `kvm_bindings` types internally.
macOS/Windows implementations do likewise for their native structs.

### 3.3 Abstract the vCPU Run Loop

`Vcpu::run_loop` in `vcpu.rs` currently calls `self.fd.run()` and matches on
`kvm_ioctls::VcpuExit`. The visor-owned `VmExit` enum already exists and is
portable. The fix:

- `VcpuOps::run()` returns `Result<VmExit, PlatformError>`, not `VcpuExit`.
- Each platform's `run()` translates its native exit reason to `VmExit`.
- The `run_loop` function moves to generic code that calls `VcpuOps::run()`.

### 3.4 Decouple `GuestMemory::register()`

`memory.rs` takes a `&VmFd` parameter. Replace with `&dyn VmOps`:

```rust
// BEFORE
pub fn register(&self, vm_fd: &VmFd, slot: u32) -> Result<(), MemoryError>

// AFTER
pub fn register(&self, vm: &dyn VmOps, slot: u32) -> Result<(), MemoryError>
```

The `VmOps::register_memory()` method encapsulates the KVM ioctl (or HVF/WHP
equivalent). `GuestMemory` no longer needs to know what hypervisor backs it.

### 3.5 File-by-File Checklist

- [ ] `visor-machine/src/platform/mod.rs` -- expand `Platform`, add `VmOps`,
      `VcpuOps` traits
- [ ] `visor-machine/src/platform/regs.rs` -- new file, portable register structs
- [ ] `visor-machine/src/platform/linux.rs` -- implement expanded traits, wrap
      all `kvm_ioctls`/`kvm_bindings` usage
- [ ] `visor-machine/src/platform/macos.rs` -- stub `HvfPlatform` implementing traits
- [ ] `visor-machine/src/vcpu.rs` -- remove `kvm_ioctls` imports, use `VcpuOps`
- [ ] `visor-machine/src/memory.rs` -- replace `VmFd` with `&dyn VmOps`
- [ ] `visor-runtime/src/vm.rs` -- replace `KvmPlatform` with generic `P: Platform`,
      remove `kvm-bindings`/`kvm-ioctls` from imports
- [ ] `visor-runtime/Cargo.toml` -- remove `kvm-bindings`, `kvm-ioctls` from
      `[dependencies]`
- [ ] `visor-machine/Cargo.toml` -- gate `kvm-bindings`, `kvm-ioctls` behind
      `cfg(target_os = "linux")`

## 4. Phase 2: Eventing and Devices

**Goal**: Replace Linux `EventFd` with a cross-platform interrupt mechanism.

### 4.1 The `EventFd` Problem

Three places create `EventFd` instances:

| Location                              | Purpose                        |
| ------------------------------------- | ------------------------------ |
| `devices/serial.rs` (`SerialIrq`)     | Serial TX-empty interrupt      |
| `vm.rs` (`wire_devices`, blk irqfd)   | Block device virtio interrupts |
| `vm.rs` (`wire_devices`, vsock irqfd) | Vsock device virtio interrupts |

`EventFd` is a Linux-only concept (the `eventfd2` syscall). On macOS, the
equivalent is `kqueue` with `EVFILT_USER`. On Windows, it's `CreateEvent`.

### 4.2 `InterruptEvent` Trait

```rust
// visor-machine/src/platform/event.rs (NEW)
pub trait InterruptEvent: Send + Sync {
    /// Signal the event (write side).
    fn trigger(&self) -> Result<(), std::io::Error>;

    /// Get a waitable token for polling integration.
    fn as_raw(&self) -> RawEventHandle;
}

/// Platform-specific raw handle.
#[cfg(target_os = "linux")]
pub type RawEventHandle = std::os::fd::RawFd;
#[cfg(target_os = "macos")]
pub type RawEventHandle = std::os::fd::RawFd; // kqueue fd
#[cfg(target_os = "windows")]
pub type RawEventHandle = std::os::windows::io::RawHandle;
```

### 4.3 Replace `vm-superio` with Visor-Owned 16550 Emulator

**This is not optional.** The serial console is the primary I/O channel for
guest shell access. `vm-superio` depends on `vmm-sys-util` internally, which
means it cannot compile on macOS or Windows. We cannot gate it behind
`cfg(target_os = "linux")` and still have a working VMM on other platforms.

The fix: replace `vm-superio` entirely with a visor-owned, pure-Rust UART
16550 emulator that uses `InterruptEvent` for its interrupt trigger. This
eliminates both the `vm-superio` and `vmm-sys-util` dependencies from the
serial device path.

Current dependency chain that breaks cross-platform:

```
SerialDevice
  +-- vm_superio::Serial<SerialIrq, NoEvents, Box<dyn Write>>
  |     +-- vm_superio::Trigger (trait, implemented by SerialIrq)
  |     +-- vmm-sys-util (transitive, Linux-only)
  +-- SerialIrq
        +-- vmm_sys_util::eventfd::EventFd (Linux-only)
```

Target dependency chain (fully portable):

```
SerialDevice
  +-- visor Uart16550<Box<dyn Write>>
  |     +-- InterruptEvent (trait, platform-provided)
  |     +-- no external deps
  +-- Box<dyn InterruptEvent> (injected by caller)
```

### 4.4 16550 Emulator Implementation Blueprint

The UART 16550 is a simple, well-documented device. `vm-superio`'s
implementation is ~400 lines. Ours needs to cover the same register set
but use `InterruptEvent` instead of the `Trigger` trait.

#### 4.4.1 Register Map

The 16550 occupies 8 I/O ports. All accesses are single-byte.

| Offset | Read           | Write            | Abbrev  |
| ------ | -------------- | ---------------- | ------- |
| 0      | Receive Buffer | Transmit Holding | RBR/THR |
| 1      | Interrupt En.  | Interrupt En.    | IER     |
| 2      | Interrupt ID   | FIFO Control     | IIR/FCR |
| 3      | Line Control   | Line Control     | LCR     |
| 4      | Modem Control  | Modem Control    | MCR     |
| 5      | Line Status    | (factory test)   | LSR     |
| 6      | Modem Status   | (not used)       | MSR     |
| 7      | Scratch        | Scratch          | SCR     |

When DLAB (bit 7 of LCR) is set, offsets 0 and 1 become the divisor
latch (DLL/DLM). In a VMM we don't emulate real baud rates, but we must
accept and store the divisor writes so the guest driver doesn't fault.

#### 4.4.2 Struct Layout

```rust
// visor-machine/src/devices/uart.rs (NEW)
use std::io::Write;
use std::collections::VecDeque;
use crate::platform::event::InterruptEvent;

/// Pure-Rust UART 16550 emulator.
///
/// Replaces `vm-superio::Serial`. Uses [`InterruptEvent`] for guest
/// interrupt delivery -- no Linux-specific dependencies.
pub struct Uart16550 {
    /// Interrupt trigger (platform-provided).
    irq: Box<dyn InterruptEvent>,
    /// Output sink (serial console capture).
    output: Box<dyn Write + Send>,
    /// Receive FIFO (host -> guest).
    rx_fifo: VecDeque<u8>,

    // Registers
    ier: u8,     // Interrupt Enable Register
    iir: u8,     // Interrupt Identification Register
    lcr: u8,     // Line Control Register
    mcr: u8,     // Modem Control Register
    lsr: u8,     // Line Status Register
    msr: u8,     // Modem Status Register
    scr: u8,     // Scratch Register
    dll: u8,     // Divisor Latch Low
    dlm: u8,     // Divisor Latch High
    fcr: u8,     // FIFO Control Register
}
```

#### 4.4.3 Key Behaviors to Implement

1. **THR write (offset 0, DLAB=0)**: Write byte to `self.output`. Set
   LSR bit 5 (THR empty) and bit 6 (transmitter empty). If IER bit 1
   (THRE interrupt enable) is set, fire `self.irq.trigger()`.
2. **RBR read (offset 0, DLAB=0)**: Pop from `rx_fifo`. If FIFO becomes
   empty, clear LSR bit 0 (data ready).
3. **IER write (offset 1, DLAB=0)**: Store value. Re-evaluate pending
   interrupts and update IIR. Fire interrupt if any enabled condition
   is active.
4. **IIR read (offset 2)**: Return highest-priority pending interrupt.
   Priority order: Line Status > RX Data > THR Empty > Modem Status.
   Bit 0 = 0 means interrupt pending, = 1 means no interrupt.
5. **LSR read (offset 5)**: Return line status. Bit 0 = data ready
   (rx_fifo non-empty), bit 5 = THR empty (always, since we flush
   immediately), bit 6 = transmitter empty.
6. **DLAB writes**: When LCR bit 7 is set, offsets 0/1 write DLL/DLM.
   Store but don't change behavior (no real baud rate in a VMM).

#### 4.4.4 What We Can Skip

- **Modem signals**: DSR/CTS/RI/DCD changes. Return static MSR (DSR +
  CTS asserted). No real modem in a VMM.
- **Break/parity/framing errors**: No real wire. LSR error bits stay 0.
- **Hardware flow control**: RTS/CTS is a no-op.
- **DMA mode**: Not applicable to virtualized I/O.

#### 4.4.5 Testing Strategy

1. **Register read/write round-trips**: Write IER, LCR, MCR, SCR, read
   back. Verify DLAB switching between data/divisor modes.
2. **THR -> output**: Write byte to offset 0, verify output sink received
   it. Verify LSR reports THR empty. Verify interrupt fires if IER
   enables THRE.
3. **RX path**: Enqueue bytes via `rx_fifo`, verify RBR reads return them.
   Verify LSR bit 0 set when data available, cleared when empty.
4. **IIR priority**: Set up multiple pending conditions, verify IIR
   reports the highest-priority one.
5. **Integration**: Use `MockInterruptEvent` (returns Ok from trigger,
   counts calls) in all tests. Verify interrupt count matches expected.

The existing `serial_test.rs` tests should be adapted to test `Uart16550`
directly -- same behavioral expectations, different struct.

#### 4.4.6 Migration Path

1. Create `visor-machine/src/devices/uart.rs` with `Uart16550`.
2. Create `visor-machine/src/devices/uart_test.rs` with full test suite.
3. Update `SerialDevice` in `serial.rs` to wrap `Uart16550` instead of
   `vm_superio::Serial`. The `BusDevice` impl stays identical -- it still
   delegates single-byte reads/writes. The constructor changes to accept
   a `Box<dyn InterruptEvent>` instead of creating an `EventFd` internally.
4. Update `serial_test.rs` to use a `MockInterruptEvent`.
5. Remove `vm-superio` from `visor-machine/Cargo.toml`.

### 4.5 Rewire MMIO Transport Interrupt Delivery

`MmioTransport` currently stores an `Arc<vmm_sys_util::eventfd::EventFd>` via
`set_irq_evt()`. Replace with `Arc<dyn InterruptEvent>`.

### 4.6 Remove `vmm-sys-util` and `vm-superio` from the Crate Graph

After Phase 2, neither `visor-runtime` nor `visor-machine` imports
`vmm_sys_util` or `vm-superio` on non-Linux targets. On Linux,
`vmm-sys-util` may remain as an internal dependency of `kvm-ioctls`
(which pulls it transitively), but visor code never imports it directly.

### 4.7 File-by-File Checklist

- [ ] `visor-machine/src/platform/event.rs` -- new file, `InterruptEvent` trait
- [ ] `visor-machine/src/platform/linux.rs` -- `LinuxEventFd` implementing
      `InterruptEvent`
- [ ] `visor-machine/src/platform/macos.rs` -- `KqueueEvent` implementing
      `InterruptEvent`
- [ ] `visor-machine/src/devices/uart.rs` -- new file, pure-Rust `Uart16550`
      emulator using `InterruptEvent`
- [ ] `visor-machine/src/devices/uart_test.rs` -- new file, full test suite
      with `MockInterruptEvent`
- [ ] `visor-machine/src/devices/serial.rs` -- replace `vm_superio::Serial`
      with `Uart16550`, accept `Box<dyn InterruptEvent>` in constructor
- [ ] `visor-machine/src/devices/serial_test.rs` -- update tests to use
      `MockInterruptEvent`
- [ ] `visor-machine/src/transport/mmio.rs` -- `set_irq_evt` accepts
      `Arc<dyn InterruptEvent>`
- [ ] `visor-runtime/src/vm.rs` -- create events via `Platform`, not `EventFd::new()`
- [ ] `visor-machine/Cargo.toml` -- remove `vm-superio` from
      `[dependencies]`
- [ ] `visor-runtime/Cargo.toml` -- remove `vmm-sys-util`

## 5. Phase 3: Networking and Comms

**Goal**: Abstract TAP/iptables and AF_VSOCK behind traits.

### 5.1 Current Architecture (Linux-only)

```
+--visor-runtime/src/net/------------------+
|                                          |
|  tap.rs ---- ip tuntap add (Linux)       |
|  nat.rs ---- iptables MASQUERADE         |
|  port_forward.rs -- iptables DNAT        |
|  switch.rs ----- MAC forwarding table    |
|  ip_alloc.rs --- subnet allocation       |
|  dns.rs -------- hickory-server          |
|  packet.rs ----- packet inspection       |
|                                          |
+------------------------------------------+
```

`switch.rs`, `ip_alloc.rs`, `dns.rs`, and `packet.rs` are already portable.
The Linux coupling is isolated to three files: `tap.rs`, `nat.rs`,
`port_forward.rs`.

### 5.2 `NetworkBackend` Trait

```rust
// visor-runtime/src/net/backend.rs (NEW)
#[async_trait]
pub trait NetworkBackend: Send + Sync {
    /// Create a virtual network interface for a VM.
    async fn create_interface(
        &self, config: &InterfaceConfig,
    ) -> anyhow::Result<Box<dyn NetworkInterface>>;

    /// Set up NAT/masquerade for outbound traffic.
    async fn setup_nat(
        &self, config: &NatConfig,
    ) -> anyhow::Result<Box<dyn NatHandle>>;

    /// Forward a host port to a guest port.
    async fn forward_port(
        &self, mapping: &PortMapping,
    ) -> anyhow::Result<Box<dyn PortForwardHandle>>;
}

pub trait NetworkInterface: Send + Sync {
    fn name(&self) -> &str;
    // Drop cleans up the interface
}

pub trait NatHandle: Send + Sync {
    // Drop removes NAT rules
}

pub trait PortForwardHandle: Send + Sync {
    // Drop removes port-forward rules
}
```

### 5.3 Platform Implementations

| Platform | Interface            | NAT / Routing          | Port Forwarding   |
| -------- | -------------------- | ---------------------- | ----------------- |
| Linux    | `ip tuntap` (as now) | `iptables` (as now)    | `iptables` DNAT   |
| macOS    | `vmnet.framework`    | `pfctl` or `vmnet` NAT | `pfctl` rdr rules |
| Windows  | Hyper-V vSwitch      | `netsh` or WinNAT      | `netsh portproxy` |

Each platform module lives under `visor-runtime/src/net/`:

```
visor-runtime/src/net/
  mod.rs
  backend.rs        <-- trait definitions
  linux.rs          <-- LinuxNetworkBackend (tap + iptables)
  macos.rs          <-- MacOsNetworkBackend (vmnet + pfctl)
  windows.rs        <-- WindowsNetworkBackend (vswitch + netsh)
  switch.rs         <-- portable, unchanged
  ip_alloc.rs       <-- portable, unchanged
  dns.rs            <-- portable, unchanged
  packet.rs         <-- portable, unchanged
```

### 5.4 Restructure Existing Networking Files

The current `tap.rs`, `nat.rs`, and `port_forward.rs` become the body of
`linux.rs`. Specifically:

1. Move `TapDevice`, `TapConfig` into `linux.rs` and have them implement
   `NetworkInterface` / associated builder.
2. Move `NatManager`, `NatConfig`, `IptablesRule` into `linux.rs` and have
   `NatManager` implement `NatHandle`.
3. Move `PortForwardManager`, `PortMapping` into `linux.rs`.
4. Gate `linux.rs` with `#[cfg(target_os = "linux")]`.

The types `NatConfig` and `PortMapping` that describe _what_ to do (not _how_)
stay in `backend.rs` as portable config structs. The `IptablesRule` type is
internal to the Linux implementation.

### 5.5 Abstract `AF_VSOCK`

`visor-runtime/src/vsock/client.rs` uses `nix::sys::socket` to create an
`AF_VSOCK` socket. This is Linux-only. The client is already generic over
`AsyncRead + AsyncWrite`, which is the right shape.

The fix is a `CommsBackend` trait:

```rust
// visor-runtime/src/vsock/backend.rs (NEW)
#[async_trait]
pub trait CommsBackend: Send + Sync {
    type Stream: AsyncRead + AsyncWrite + Unpin + Send;

    /// Connect to the guest agent.
    async fn connect(
        &self, vm_id: &str, port: u32,
    ) -> Result<Self::Stream, VsockError>;
}
```

| Platform | Implementation                                  |
| -------- | ----------------------------------------------- |
| Linux    | `AF_VSOCK` socket (current code)                |
| macOS    | Virtualization.framework `VZVirtioSocketDevice` |
| Windows  | Hyper-V sockets (`AF_HYPERV` / `hvsocket`)      |

### 5.6 File-by-File Checklist

- [ ] `visor-runtime/src/net/backend.rs` -- new, `NetworkBackend` trait + config
      structs
- [ ] `visor-runtime/src/net/linux.rs` -- new, absorbs `tap.rs`, `nat.rs`,
      `port_forward.rs`
- [ ] `visor-runtime/src/net/macos.rs` -- new stub for `vmnet.framework`
- [ ] `visor-runtime/src/net/windows.rs` -- new stub for vSwitch
- [ ] `visor-runtime/src/net/tap.rs` -- delete (moved to `linux.rs`)
- [ ] `visor-runtime/src/net/nat.rs` -- delete (moved to `linux.rs`)
- [ ] `visor-runtime/src/net/port_forward.rs` -- delete (moved to `linux.rs`)
- [ ] `visor-runtime/src/net/mod.rs` -- re-export `backend::NetworkBackend`,
      conditionally use platform module
- [ ] `visor-runtime/src/vsock/backend.rs` -- new, `CommsBackend` trait
- [ ] `visor-runtime/src/vsock/client.rs` -- make `connect()` use `CommsBackend`
- [ ] `visor-runtime/src/vsock/linux.rs` -- new, current `AF_VSOCK` code
- [ ] `visor-runtime/Cargo.toml` -- gate `nix` behind `cfg(target_os = "linux")`

## 6. Supplementary: Seccomp and Sandbox

`visor-machine/src/seccomp.rs` uses `seccompiler` (Linux BPF). This doesn't
block compilation on other platforms because it's only called at runtime, but
the imports won't resolve.

### 6.1 `SandboxBackend` Trait

```rust
// visor-machine/src/sandbox.rs (NEW, replaces seccomp.rs role)
pub trait SandboxBackend: Send + Sync {
    fn apply(&self) -> Result<(), SandboxError>;
}
```

| Platform | Implementation                   |
| -------- | -------------------------------- |
| Linux    | `seccompiler` BPF (current code) |
| macOS    | `sandbox-exec` / App Sandbox     |
| Windows  | Job Objects / AppContainer       |

### 6.2 Checklist

- [ ] `visor-machine/src/seccomp.rs` -- gate with `#[cfg(target_os = "linux")]`
- [ ] `visor-machine/src/sandbox.rs` -- new, `SandboxBackend` trait
- [ ] `visor-machine/src/sandbox/linux.rs` -- wraps current seccomp code
- [ ] `visor-machine/src/sandbox/macos.rs` -- stub
- [ ] `visor-machine/src/sandbox/windows.rs` -- stub

## 7. Dependency Gating Strategy

### 7.1 `visor-machine/Cargo.toml` Changes

```toml
[dependencies]
# Always
thiserror = { workspace = true }
serde = { workspace = true }
libc = { workspace = true }

# Linux only
[target.'cfg(target_os = "linux")'.dependencies]
kvm-bindings = { workspace = true }
kvm-ioctls = { workspace = true }
seccompiler = { workspace = true }
```

`vm-superio` and `vmm-sys-util` are **removed entirely** (not gated behind
`cfg`). The visor-owned `Uart16550` in `devices/uart.rs` replaces `vm-superio`.
Serial console access -- the primary guest shell I/O channel -- must work
natively on all platforms from day one, so there is no acceptable path that
keeps `vm-superio` even behind a cfg gate. On Linux, `vmm-sys-util` may
still appear as a transitive dependency of `kvm-ioctls`, but visor code
never imports it directly.

### 7.2 `visor-runtime/Cargo.toml` Changes

```toml
[dependencies]
# Remove entirely:
# kvm-bindings   (moved behind visor-machine)
# kvm-ioctls     (moved behind visor-machine)
# vmm-sys-util   (moved behind visor-machine)

# Gate:
[target.'cfg(target_os = "linux")'.dependencies]
nix = { workspace = true }
```

## 8. Execution Strategy

This plan is designed for an AI coding agent, not a human. Each phase is
structured as a sequence of atomic commits with clear inputs and outputs.

### 8.1 Phase 1 Execution (Hypervisor Abstraction)

```
Step 1 ─── Create portable register types
           File: visor-machine/src/platform/regs.rs
           Test: Unit tests for struct construction and Default

Step 2 ─── Expand Platform/VmOps/VcpuOps traits
           File: visor-machine/src/platform/mod.rs
           Test: Trait compiles, doc tests pass

Step 3 ─── Implement traits in linux.rs
           File: visor-machine/src/platform/linux.rs
           Test: Existing platform tests still pass

Step 4 ─── Refactor Vcpu to use VcpuOps
           File: visor-machine/src/vcpu.rs
           Test: vcpu_test.rs passes

Step 5 ─── Refactor GuestMemory::register to use VmOps
           File: visor-machine/src/memory.rs
           Test: memory_test.rs passes

Step 6 ─── Refactor visor-runtime/src/vm.rs
           Remove: direct kvm-* imports
           Use: P: Platform generic (or trait object)
           Test: vm_test.rs passes, integration tests pass

Step 7 ─── Remove kvm-* from visor-runtime/Cargo.toml
           Test: cargo check -p visor-runtime compiles
           Gate: kvm-* in visor-machine/Cargo.toml

Step 8 ─── Stub macos.rs platform
           File: visor-machine/src/platform/macos.rs
           Test: cfg(target_os = "macos") compiles (stub errors)
```

**Validation gate**: `cargo check --workspace` and `cargo test --workspace`
pass on Linux. `cargo check -p visor-machine --target aarch64-apple-darwin`
succeeds (with feature stubs).

### 8.2 Phase 2 Execution (Eventing)

```
Step 1 ─── Create InterruptEvent trait
           File: visor-machine/src/platform/event.rs
           Test: Trait compiles, mock impl for testing

Step 2 ─── LinuxEventFd implementation
           File: visor-machine/src/platform/linux.rs
           Test: LinuxEventFd trigger/as_raw round-trip

Step 3 ─── Build pure-Rust Uart16550 emulator
           File: visor-machine/src/devices/uart.rs (NEW)
           File: visor-machine/src/devices/uart_test.rs (NEW)
           Test: Register read/write, THR -> output, RX FIFO,
                 IIR priority, interrupt trigger counting
           Ref:  vm-superio Serial (~400 lines), 16550 datasheet

Step 4 ─── Replace vm-superio in SerialDevice
           File: visor-machine/src/devices/serial.rs
           File: visor-machine/src/devices/serial_test.rs
           What: SerialDevice wraps Uart16550 instead of
                 vm_superio::Serial. Constructor takes
                 Box<dyn InterruptEvent>. Remove SerialIrq.
           Test: serial_test.rs passes with MockInterruptEvent

Step 5 ─── Remove vm-superio from visor-machine/Cargo.toml
           Test: cargo check -p visor-machine compiles
                 cargo test -p visor-machine passes

Step 6 ─── Refactor MmioTransport irq
           File: visor-machine/src/transport/mmio.rs
           What: set_irq_evt accepts Arc<dyn InterruptEvent>

Step 7 ─── Update wire_devices in vm.rs
           File: visor-runtime/src/vm.rs
           What: Create events via Platform, not EventFd::new()

Step 8 ─── Remove vmm-sys-util from visor-runtime
           File: visor-runtime/Cargo.toml
           Test: cargo tree -p visor-runtime shows no vmm-sys-util
```

**Validation gate**: Full test suite passes. Neither `vmm-sys-util` nor
`vm-superio` appear in `cargo tree -p visor-runtime` or as direct imports
in `visor-machine`. Serial console works identically (verified by existing
integration tests that boot a VM and capture serial output).

### 8.3 Phase 3 Execution (Networking and Comms)

```
Step 1 ─── Define NetworkBackend trait
           File: visor-runtime/src/net/backend.rs

Step 2 ─── Move tap/nat/port_forward into linux.rs
           Files: visor-runtime/src/net/linux.rs (new)
                  visor-runtime/src/net/tap.rs (delete)
                  visor-runtime/src/net/nat.rs (delete)
                  visor-runtime/src/net/port_forward.rs (delete)

Step 3 ─── Implement LinuxNetworkBackend
           Test: Existing net tests pass

Step 4 ─── Define CommsBackend trait
           File: visor-runtime/src/vsock/backend.rs

Step 5 ─── Move AF_VSOCK connect into linux vsock backend
           File: visor-runtime/src/vsock/linux.rs

Step 6 ─── Gate nix behind cfg(linux)
           File: visor-runtime/Cargo.toml

Step 7 ─── Stub macOS / Windows backends
           Files: visor-runtime/src/net/macos.rs
                  visor-runtime/src/vsock/macos.rs
```

**Validation gate**: Full test suite. `nix` doesn't appear in non-Linux
`cargo tree`.

### 8.4 Ordering Constraints

```
Phase 1 ──> Phase 2 ──> Phase 3
  |              |           |
  |              |           +── networking is independent of
  |              |               hypervisor, but traits in Phase 1
  |              |               establish the pattern
  |              |
  |              +── InterruptEvent must exist before
  |                  devices can be refactored
  |
  +── Platform traits must exist before
      anything else can decouple from KVM
```

Phase 3 (networking) has no hard dependency on Phase 2 (eventing), but
doing them in order keeps the codebase in a consistently buildable state
at each step.

### 8.5 Agent Instructions

Each step above is one atomic commit. The agent should:

1. **Read the target file(s)** to understand current state.
2. **Write tests first** (TDD, per AGENTS.md). Put them in companion `_test.rs`
   files.
3. **Implement the change**. Keep unsafe code in `platform/linux.rs` only.
4. **Run quality gates** after every commit:
   - `cargo check --workspace`
   - `cargo test --workspace`
   - `cargo clippy --workspace -- -D warnings`
   - `dprint fmt && dprint check`
5. **Never break existing tests.** If a refactor changes a function signature,
   update all callers in the same commit.
6. **Use conventional commits**: `refactor(machine): extract VmOps trait from Platform`.

### 8.6 What Not To Do

- Don't implement full macOS/Windows hypervisor backends yet. Stubs that return
  `PlatformError::System(io::Error::new(Unsupported, "not yet"))` are fine.
- Don't rewrite the virtio device models. They're already portable (pure Rust
  data structures). Only their interrupt signaling needs abstraction.
- Don't touch `visor-init` or `visor-kernel`. They run inside the guest or manage
  kernel binaries; neither has platform coupling.
- Don't refactor `boot/x86_64.rs`. Boot protocol setup is inherently
  architecture-specific. It stays as-is, called from the Linux platform path.
  An `aarch64.rs` path already exists for ARM.

## 9. Success Criteria

When this plan is fully executed:

| Criterion                                        | How to verify                                  |
| ------------------------------------------------ | ---------------------------------------------- |
| `visor-runtime` has zero `kvm-*` imports         | `grep -r "kvm_" crates/visor-runtime/src/`     |
| `visor-runtime` has zero `vmm_sys_util` imports  | `cargo tree -p visor-runtime \| grep vmm`      |
| `vm-superio` fully removed from workspace        | `grep -r "vm.superio" crates/`                 |
| Serial console works on all platforms            | `Uart16550` unit tests + integration boot test |
| `visor-machine` compiles on macOS (stubs)        | `cargo check -p visor-machine --target ...`    |
| All Linux tests pass unchanged                   | `cargo test --workspace` on AX41               |
| No Linux-specific shell commands in generic code | `grep -r "iptables\|ip tuntap" src/net/mod.rs` |
| Platform trait covers VM lifecycle completely    | Code review of `platform/mod.rs`               |
