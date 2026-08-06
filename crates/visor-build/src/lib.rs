//! Native Rust OCI image builder for visor.
//!
//! This crate provides Dockerfile parsing, variable substitution, and
//! `.dockerignore` filtering — the foundation for building OCI images
//! without any Go dependencies.
//!
//! # Modules
//!
//! - [`dockerfile`] — Parse Dockerfiles into typed [`BuildInstruction`]s
//! - [`substitute`] — `${VAR}`, `${VAR:-default}`, `${VAR:+alt}` expansion
//! - [`ignore`] — `.dockerignore` file parsing and path filtering
//! - [`engine`] — Multi-stage build orchestration with [`BuildExecutor`] abstraction
//! - [`layer`] — OCI layer creation with whiteout conversion and digest computation
//! - [`assembly`] — OCI image assembly and tag store
//! - [`cache`] — Content-addressable build cache for incremental rebuilds
//! - [`push`] — Push OCI images to container registries

pub mod assembly;
pub mod cache;
pub mod dockerfile;
pub mod engine;
pub mod ignore;
pub mod layer;
pub mod push;
pub mod substitute;

#[cfg(test)]
pub(crate) mod testutil;

pub use assembly::{ImageAssembler, ImageStore, StoredImage};
pub use cache::{BuildCache, CacheEntry, CacheKey};
pub use dockerfile::{
    AddInstr, ArgInstr, BuildInstruction, CmdInstr, CommandForm, CopyInstr, DockerfileParser,
    EntrypointInstr, EnvInstr, ExposeInstr, ExposedPort, FromInstr, HealthcheckInstr, LabelInstr,
    MountFlag, MountType, ParsedDockerfile, RunInstr, ShellInstr, Stage, StopsignalInstr,
    UserInstr, VolumeInstr, WorkdirInstr,
};
pub use engine::{
    BuildConfig, BuildEngine, BuildExecutor, BuildResult, BuildStep, BuiltLayer, ImageMetadata,
    LayerSnapshot, ResolvedMount,
};
pub use ignore::DockerIgnore;
pub use layer::{LayerCreator, ProcessedLayer};
pub use push::{ImageReference, PushResult, RegistryAuth, RegistryPusher};
pub use substitute::substitute_vars;
