//! Daemon, CLI, API, OCI pipeline, and networking for visor.
//!
//! `visor-runtime` is the host-side orchestrator. It manages the full lifecycle
//! of microVMs: pulling OCI images, building rootfs, creating VMs via
//! `visor-vmm`, and exposing everything through a REST API on a unix socket.
//!
//! # Architecture
//!
//! ```text
//! visor-runtime
//! ├── cli/     — clap-based subcommands (start, run, exec, ps, stop, shell)
//! ├── daemon   — HTTP server on unix socket, graceful shutdown
//! ├── api/     — REST routes, SSE events, audit logging
//! ├── backend  — ExecutionBackend trait (VMM, Container)
//! ├── oci/     — Registry client, layer cache, rootfs builder
//! ├── net/     — Virtual switch, DNS, NAT, port forwarding
//! └── pool/    — Warm VM pool, snapshot management
//! ```

pub mod api;
pub mod audit;
pub mod backend;
pub(crate) mod codesign;
pub mod cli;
pub mod compose;
pub mod container;
pub mod daemon;
pub mod ext4;
pub mod image_manager;
pub(crate) mod lifecycle;
pub mod names;
pub mod net;
pub mod oci;
pub mod paths;
pub mod pool;
pub mod state;
pub mod timeutil;
pub mod tls;
pub mod tui;
pub mod vm;
pub mod volume;
pub mod vsock;

#[cfg(test)]
pub(crate) mod testutil;
