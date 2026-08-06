//! OCI image pipeline: pull, cache, merge, build rootfs.
//!
//! This module handles the complete lifecycle of OCI container images:
//!
//! - [`reference`] — Parse image references (`alpine:3.20`, digests)
//! - [`config`] — Extract image configuration (CMD, ENV, WORKDIR)
//! - [`registry`] — Pull manifests and layers from OCI registries
//! - [`cache`] — Local layer cache by content digest
//! - [`layers`] — Merge layers with whiteout handling
//! - [`rootfs`] — Build ext4 filesystem from merged layer tree

pub mod cache;
pub mod config;
pub mod layers;
pub mod reference;
pub mod registry;
pub mod rootfs;
