//! Platform abstraction layer and VMM core for visor.
//!
//! `visor-vmm` is the single home for ALL platform-specific code: hypervisor
//! abstraction (KVM/HVF/WHP), OS networking (TAP/vmnet), guest communication
//! (vsock/VZSocket), sandboxing (seccomp/App Sandbox), device emulation,
//! and guest memory / boot / vCPU management.
//!
//! `visor-runtime` depends on this crate via traits only — no platform-specific
//! imports leak into orchestration code.
//!
//! # Architecture
//!
//! ```text
//! visor-vmm
//! ├── platform/      — Hypervisor abstraction (KVM / HVF / WHP)
//! ├── net/           — Network backend abstraction (TAP / NAT / port-forward)
//! ├── comms/         — Guest communication backend (vsock / VZSocket)
//! ├── sandbox/       — Process-level security (seccomp / App Sandbox / Job Objects)
//! ├── devices/       — Device models (UART 16550, virtio-blk, virtio-net, virtio-vsock, bus)
//! ├── transport/     — Virtio transports (MMIO, PCI)
//! ├── boot/          — Kernel loading, CPU setup (x86_64 / aarch64)
//! ├── rate_limit/    — Token-bucket rate limiters (disk, net)
//! ├── memory.rs      — Guest physical memory (mmap, demand-paged)
//! ├── vm.rs          — Portable VM boot facade + exit handling types
//! ├── acpi.rs        — ACPI table generation
//! ├── metrics.rs     — VM metrics collection
//! ├── seccomp.rs     — Syscall filtering
//! ├── snapshot.rs    — VM snapshots
//! ├── cpu_template.rs — CPU feature templates
//! ├── dirty_tracking.rs — Dirty page tracking
//! └── migration.rs   — Live migration
//! ```

pub mod comms;
pub mod net;
pub mod platform;
pub mod sandbox;

// All modules — no separation between "old" and "new".
#[cfg(target_arch = "x86_64")]
pub mod acpi;
pub mod boot;
pub mod cpu_template;
pub mod devices;
pub mod dirty_tracking;
pub mod guest_virtualization;
pub mod memory;
pub mod metrics;
pub mod migration;
pub mod rate_limit;
pub mod shared_memory;
#[cfg(target_os = "linux")]
pub mod seccomp;
pub mod snapshot;
pub mod transport;
#[cfg(target_os = "linux")]
pub mod vcpu;
pub mod vm;

#[cfg(test)]
pub(crate) mod testutil;
