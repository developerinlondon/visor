//! Guest PID 1 for visor microVMs.
//!
//! `visor-init` boots as the first process inside the guest VM and handles:
//!
//! - Parsing the run configuration received from the host
//! - Mounting essential filesystems (`/proc`, `/sys`, `/dev`)
//! - Configuring guest networking (IP, mask, gateway)
//! - Running a vsock agent for host↔guest JSON-RPC communication
//! - Executing the user's command with signal forwarding and zombie reaping
//! - Mounting volumes from the host
//! - Providing shell access

pub mod agent;
pub mod config;
pub mod shell;

#[cfg(test)]
pub(crate) mod testutil;

// These modules use Linux-only APIs (nix, libc ioctls) and only compile on Linux,
// where visor-init actually runs as the guest PID 1.
#[cfg(target_os = "linux")]
pub mod entrypoint;
#[cfg(target_os = "linux")]
pub mod listener;
#[cfg(target_os = "linux")]
pub mod mount;
#[cfg(target_os = "linux")]
pub mod network;
#[cfg(target_os = "linux")]
pub mod overlay;
#[cfg(target_os = "linux")]
pub mod volume;
