# 12 — Kernel and Boot Architecture

How the guest kernel, kernel config, and visor-machine boot code fit together.

## The Big Idea

A VM is a fake computer. visor-machine builds that fake computer — it creates virtual CPU, RAM,
and devices using Linux's KVM API. But a fake computer with no software is useless. It needs an
operating system kernel, just like a real computer. That's what the ~30 MB `vmlinux` binary is — a
Linux kernel, compiled specifically for visor's virtual hardware.

```
+-----------------------------------------------------------+
|  Host Machine (real hardware, real Linux)                  |
|                                                           |
|  +-------------------------------------------------------+
|  |  visor-runtime (our Rust binary)                      |
|  |                                                       |
|  |  +---------------------------------------------------+
|  |  |  visor-machine (VMM library)                      |
|  |  |                                                   |
|  |  |  - Asks KVM to create a VM                        |
|  |  |  - Allocates RAM (mmap)                           |
|  |  |  - Loads vmlinux into that RAM                    |
|  |  |  - Sets up CPU registers                          |
|  |  |  - Runs the vCPU in a loop                        |
|  |  |  - Emulates devices (serial, virtio-blk, ...)     |
|  |  +---------------------------------------------------+
|  |                  |                                     |
|  |                  | KVM ioctls (/dev/kvm)               |
|  |                  v                                     |
|  |  +---------------------------------------------------+
|  |  |  KVM (kernel module in the HOST kernel)           |
|  |  |  Uses Intel VT-x / AMD-V hardware                 |
|  |  |  to run guest code at near-native speed           |
|  |  +---------------------------------------------------+
|  |                  |                                     |
|  |                  | hardware virtualization             |
|  |                  v                                     |
|  |  +===================================================+
|  |  ||  GUEST VM (the fake computer)                   ||
|  |  ||                                                 ||
|  |  ||  RAM: the mmap'd region from visor-machine      ||
|  |  ||  CPU: virtual CPU running in KVM                ||
|  |  ||                                                 ||
|  |  ||  +---------------------------------------------+||
|  |  ||  |  vmlinux (our ~30 MB custom kernel)         |||
|  |  ||  |  - boots, initializes memory/devices        |||
|  |  ||  |  - starts visor-init (PID 1)                |||
|  |  ||  |  - runs the container workload              |||
|  |  ||  +---------------------------------------------+||
|  |  +===================================================+
|  +-------------------------------------------------------+
+-----------------------------------------------------------+
```

## What Is the Kernel Config?

The Linux kernel is not a single fixed program. It's a massive menu of features — networking
protocols, filesystems, CPU drivers, security modules, device drivers, etc. The
`config/visor-kernel.config` file (3,243 lines) says which features to include.

Think of it like ordering a car. You don't want every possible option — you want exactly what
fits the use case:

```
CONFIG_VIRTIO_MMIO=y          "Yes, include the virtio-MMIO driver"
                               (because that's how visor exposes devices)

CONFIG_VIRTIO_BLK=y           "Yes, include virtio block device support"
                               (because visor-machine emulates virtio-blk disks)

CONFIG_VIRTIO_NET=y           "Yes, include virtio network support"

# CONFIG_ETHERNET is not set  "No real Ethernet hardware in our VM"

# CONFIG_USB_SUPPORT is not set  "No USB — it's a microVM, not a laptop"

# CONFIG_SOUND is not set     "No sound card"

CONFIG_EXT4_FS=y              "Yes, we need ext4 filesystem for the rootfs"

CONFIG_ACPI_REDUCED_HARDWARE_ONLY=y  "Simplified ACPI — we're a VM, not real HW"
```

`=y` means "compile into the kernel binary". There's also `=m` (loadable module, loaded later)
but visor uses `CONFIG_MODULES=n` — everything is baked into the single `vmlinux` binary.
No module loading infrastructure, no `/lib/modules`, just one file.

**Why custom?** A stock Ubuntu/Fedora kernel is ~100+ MB with thousands of drivers for hardware
that will never exist inside a microVM. Our custom kernel is ~30 MB because it only includes what
a microVM needs. Faster boot, smaller attack surface, less memory.

## How the Boot Sequence Works

### Step 1: visor-machine creates the "hardware"

```
platform/linux.rs — open /dev/kvm

  kvm = Kvm::new()                 get KVM handle
  vm_fd = kvm.create_vm()          create empty VM

memory.rs — allocate guest RAM

  memory = GuestMemory::new(
      256 * 1024 * 1024,           256 MiB
      0,                           starting at guest physical address 0
  )
  memory.register(&vm_fd, 0)       tell KVM "this mmap region IS the guest's RAM"
```

At this point there is an empty computer with RAM but nothing in it.

### Step 2: visor-machine loads the kernel into RAM

`boot/x86_64.rs` — `configure_boot()` does several things to the guest's RAM:

```
Guest Physical Memory Map (after boot setup):

0x0000_0500   GDT — tells the CPU about memory segments
0x0000_0520   IDT — interrupt descriptor table (empty)
0x0000_7000   boot_params / "zero page" — metadata for the kernel
                 - e820 memory map ("you have RAM from 0 to 256M")
                 - command line pointer
                 - boot flags
0x0000_8FF0   Stack pointer (where RSP starts)
0x0000_9000   PML4 page table ---+
0x0000_A000   PDPT               +-- identity maps first 1 GiB
0x0000_B000   PD (512 entries) --+   so addr X maps to phys addr X
0x0002_0000   Kernel command line ("console=ttyS0 reboot=k ...")
0x000A_0000   ACPI tables (RSDP, FADT, MADT, DSDT, XSDT)
0x0010_0000   <-- KERNEL STARTS HERE (loaded from vmlinux ELF segments)
   ...           (the ~30 MB kernel binary lives here)
0x0FFF_FFFF   <-- end of 256 MiB RAM
```

The code in `x86_64.rs` copies bytes from the vmlinux ELF file into the guest's mmap'd memory
using `memory.write_bytes()`.

### Step 3: visor-machine sets up the virtual CPU

`vcpu.rs` — `configure_regs()` tells KVM what state the CPU should be in when it starts:

| Register | Value                                | Purpose                                                    |
| -------- | ------------------------------------ | ---------------------------------------------------------- |
| RIP      | kernel entry point (from ELF header) | "start executing here"                                     |
| RSP      | `0x8FF0`                             | "stack lives here"                                         |
| RSI      | `0x7000`                             | "boot_params struct is at this address"                    |
| CR3      | `0x9000`                             | "page tables are here"                                     |
| CR0      | PE + PG                              | protected mode + paging enabled                            |
| CR4      | PAE                                  | physical address extension                                 |
| EFER     | LME + LMA                            | 64-bit long mode                                           |
| CPUID    | host's real CPUID                    | "pretend you're the same CPU as the host"                  |
| GDT      | code/data/tss segments               | x86 segmentation (required but mostly vestigial in 64-bit) |

The Linux boot protocol specifies this contract: the kernel expects RSI to point to `boot_params`,
RIP to be the entry point, and the CPU to already be in 64-bit long mode with paging enabled.

### Step 4: visor-machine runs the vCPU

`vcpu.rs` — `run_loop()`:

```
loop {
    match vcpu.fd.run() {        // KVM_RUN ioctl
        //
        // The CPU executes the kernel's machine code at full speed
        // using Intel VT-x / AMD-V. This is NOT emulation — it's real
        // execution on real hardware.
        //
        // KVM only returns ("VM exits") when the guest does something
        // that needs VMM help:
        //

        IoOut(port, data) =>
            // Guest wrote to an I/O port.
            // e.g. kernel prints "Hello" to serial port 0x3F8
            //   -> visor captures this and forwards to the terminal
            handler.handle_exit(VmExit::IoOut { port, data })

        MmioWrite(addr, data) =>
            // Guest wrote to a memory address that isn't RAM.
            // e.g. kernel writes to virtio-mmio device at 0xD000_0000
            //   -> visor's MmioTransport handles the virtio protocol
            handler.handle_exit(VmExit::MmioWrite { addr, data })

        Hlt =>
            // Guest CPU is idle (executed HLT instruction)
            // -> continue, KVM will wake it on next interrupt

        Shutdown =>
            // Guest requested poweroff
            break
    }
}
```

### Step 5: What the kernel does once it starts running

From the kernel's perspective, it just woke up on a computer:

1. Entry point runs (RIP)
2. Reads `boot_params` from RSI (`0x7000`):
   - "I have 256 MiB of RAM" (from the e820 map visor wrote)
   - "My command line is at `0x20000`" → `console=ttyS0 reboot=k panic=1`
   - "ACPI tables are at `0xA0000`"
3. Initializes memory management, interrupts, scheduler
4. Finds ACPI tables:
   - MADT → "I have 1 CPU with Local APIC ID 0, and an I/O APIC"
   - FADT → "I'm a HW_REDUCED system" (no legacy PIC/PIT/etc.)
5. Probes for devices via virtio-mmio (from kernel cmdline)
   - Finds virtio-blk → that's the rootfs
   - Finds virtio-vsock → communication channel with host
   - Finds virtio-net → network interface
6. Mounts the root filesystem
7. Executes `/init` (visor-init binary, PID 1)
8. visor-init starts the container workload

## How the Config and the Code Match Up

Every feature has two halves — one in the kernel, one in visor. Both halves must agree.

```
+---------------+
|  GUEST        |
|  (kernel)     |
|               |
|  virtio-blk   |  <- kernel CONFIG_VIRTIO_BLK=y
|  driver       |     (knows the virtio protocol)
|      |        |
|      | MMIO   |  <- reads/writes to magic addresses
|      | r/w    |
+------+--------+
       | VM EXIT (KVM returns to visor)
+------+--------+
|      v        |
|  MmioTransp   |  <- transport/mmio.rs
|      |        |     (speaks virtio register protocol)
|      v        |
|  BlockDevice  |  <- devices/block.rs
|      |        |     (reads/writes actual disk image)
|      v        |
|  HOST         |
|  (visor)      |
+---------------+
```

If the kernel doesn't have `CONFIG_VIRTIO_BLK=y`, it won't probe for the device — even though
visor perfectly emulates it. If visor doesn't emulate the device, the kernel driver gets no
response and the device never appears.

### Config ↔ Code Mapping Table

| Kernel Config                            | visor Code                  | Relationship                                                         |
| ---------------------------------------- | --------------------------- | -------------------------------------------------------------------- |
| `CONFIG_KVM_GUEST=y`                     | `platform/linux.rs`         | Kernel knows it's under KVM; visor IS the KVM host                   |
| `CONFIG_VIRTIO_MMIO=y`                   | `transport/mmio.rs`         | Kernel has the virtio-mmio driver; visor emulates the registers      |
| `CONFIG_VIRTIO_BLK=y`                    | `devices/block.rs`          | Kernel has virtio-blk driver; visor provides disk backend            |
| `CONFIG_VIRTIO_VSOCKETS=y`               | `devices/vsock.rs`          | Kernel has vsock driver; visor handles host↔guest comms              |
| `CONFIG_SERIAL_8250=y`                   | `devices/serial.rs`         | Kernel has UART driver; visor emulates 16550 on port `0x3F8`         |
| `CONFIG_ACPI=y`                          | `acpi.rs`                   | Kernel expects ACPI tables; visor generates RSDP/FADT/MADT/DSDT/XSDT |
| `CONFIG_ACPI_REDUCED_HARDWARE_ONLY=y`    | `acpi.rs` (HW_REDUCED flag) | Kernel won't look for legacy hardware (PIC/PIT)                      |
| `CONFIG_SMP=y`                           | `acpi.rs` (MADT entries)    | Kernel supports multiple CPUs; visor tells it how many via MADT      |
| `CONFIG_PARAVIRT=y` / `PARAVIRT_CLOCK=y` | (automatic via KVM)         | Guest uses KVM paravirt clock instead of hardware timers             |
| `CONFIG_VIRTIO_NET=y`                    | `devices/net.rs`            | Kernel has virtio-net driver; visor provides network backend         |
| `CONFIG_EXT4_FS=y`                       | visor-runtime OCI rootfs    | Kernel mounts ext4; runtime prepares ext4 image via virtio-blk       |
| `CONFIG_MODULES=n`                       | (simplicity decision)       | Single vmlinux binary, no module loading infrastructure              |
| `CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`   | (kernel cmdline)            | Kernel discovers MMIO devices from boot cmdline parameters           |

## Why the Config Is 3,243 Lines

Linux has ~15,000 config options. Our config resolves all of them — either `=y` (include),
`# ... is not set` (exclude), or a specific value. Most lines say "no" to things we don't need:

```
# CONFIG_SOUND is not set          no audio hardware in a VM
# CONFIG_USB_SUPPORT is not set    no USB
# CONFIG_WIRELESS is not set       no WiFi
# CONFIG_DRM is not set            no GPU
# CONFIG_INPUT_KEYBOARD is not set no keyboard (serial console only)
# CONFIG_ETHERNET is not set       no real NICs (virtio-net only)
```

The remaining `=y` lines define our kernel's identity: a minimal, KVM-aware, virtio-equipped,
container-optimized kernel with no hardware drivers, no modules, and a small footprint.

## Kernel Resolution at Build Time

`visor-kernel` crate's `build.rs` resolves the kernel binary through a 4-step chain:

1. `OUT_DIR` cache — already downloaded from a previous build
2. `VISOR_KERNEL_PATH` env var — explicit path to a vmlinux binary
3. Local cache at `/var/lib/visor/kernel/vmlinux-x86_64`
4. GitLab release download (requires `GITLAB_TOKEN`)

At runtime, `visor_kernel::kernel_path()` returns the path baked in at compile time.

The kernel config (`config/visor-kernel.config`) is stored alongside the crate for reproducible
builds from source. See `crates/visor-kernel/scripts/build-kernel.sh` for manual rebuilds.

## Key Design Decisions

| Decision                                   | Rationale                                                              |
| ------------------------------------------ | ---------------------------------------------------------------------- |
| `CONFIG_MODULES=n`                         | Single binary simplicity. No `/lib/modules` in guest. Faster boot.     |
| `CONFIG_ACPI_REDUCED_HARDWARE_ONLY=y`      | VM has no legacy hardware. Avoids emulating PIC/PIT/RTC.               |
| Direct boot (not UEFI)                     | No firmware needed. `boot/x86_64.rs` sets up everything. ~50ms faster. |
| Identity-mapped 1 GiB page tables          | Enough for kernel to start. Kernel extends mapping during early boot.  |
| Virtio-MMIO (not PCI) transport            | Simpler to emulate (flat register space vs. PCI config space + BARs).  |
| `CONFIG_VIRTIO_MMIO_CMDLINE_DEVICES=y`     | Device discovery via kernel cmdline, no device tree on x86_64.         |
| Custom kernel (~30 MB) vs stock (~100+ MB) | Smaller attack surface, faster boot, less memory.                      |
