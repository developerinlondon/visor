//! Vsock client for host↔guest communication over virtio-vsock.
//!
//! This module provides:
//! - [`protocol`] — JSON-RPC 2.0 message types matching visor-init's agent
//! - [`client`] — Async client with connect, ping, exec, kill, shutdown
//! - [`executor`] — [`BuildExecutor`](visor_build::BuildExecutor) implementation over vsock
//! - [`build_service`] — [`BuildService`](visor_types::BuildService) over ephemeral VMs

pub mod build_service;
pub mod client;
pub mod executor;
pub mod protocol;
