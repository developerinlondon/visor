//! Warm VM pool management: health checking, snapshot caching, and pool sizing.
//!
//! This module provides infrastructure for maintaining a pool of pre-warmed VM
//! instances:
//!
//! - [`health`] — VM health checking via vsock ping
//! - [`manager`] — warm pool manager with fast VM acquisition
//! - [`snapshot_cache`] — disk-based snapshot cache for VM images

pub mod health;
pub mod manager;
pub mod snapshot_cache;
