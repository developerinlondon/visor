//! VM state persistence for daemon restart recovery.
//!
//! Saves and restores VM metadata to `~/.visor/state/<vm_id>/` so the daemon
//! can reconstruct its VM table after a restart or crash.

pub mod persistence;
