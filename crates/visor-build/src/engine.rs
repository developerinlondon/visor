//! Multi-stage Dockerfile build engine.
//!
//! Orchestrates instruction execution, layer creation, and stage
//! management.  The [`BuildExecutor`] trait abstracts guest-VM
//! communication so the engine can be tested without a real VM.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use anyhow::Context as _;
use async_trait::async_trait;

use crate::dockerfile::{
    AddInstr, ArgInstr, BuildInstruction, CmdInstr, CommandForm, CopyInstr, EntrypointInstr,
    EnvInstr, ExposeInstr, LabelInstr, MountType, ParsedDockerfile, RunInstr, ShellInstr, Stage,
    StopsignalInstr, UserInstr, VolumeInstr, WorkdirInstr,
};
use crate::substitute::substitute_vars;

// ── Executor Trait ──────────────────────────────────────────────────────

/// Abstracts guest VM communication for the build engine.
///
/// The real implementation uses vsock to communicate with visor-init.
/// Tests use a mock that records calls and returns predetermined results.
#[async_trait]
pub trait BuildExecutor: Send + Sync {
    /// Initialize overlay filesystem in the guest.
    ///
    /// # Errors
    ///
    /// Returns an error if the overlay cannot be initialized.
    async fn overlay_init(&self, lower_dir: Option<String>) -> anyhow::Result<()>;

    /// Execute a command in the guest (inside the overlay merged view).
    ///
    /// Returns `(exit_code, stdout, stderr)`.
    ///
    /// # Errors
    ///
    /// Returns an error if the command cannot be dispatched.
    async fn exec(
        &self,
        cmd: &[String],
        env: &[String],
        workdir: &str,
    ) -> anyhow::Result<(i32, String, String)>;

    /// Snapshot the overlay upper as a compressed layer.
    ///
    /// # Errors
    ///
    /// Returns an error if the snapshot cannot be created.
    async fn snapshot_layer(&self) -> anyhow::Result<LayerSnapshot>;

    /// Flatten overlay and reset for next instruction.
    ///
    /// # Errors
    ///
    /// Returns an error if the overlay cannot be flattened.
    async fn flatten_overlay(&self) -> anyhow::Result<()>;

    /// Copy files into the guest at `dest`.
    ///
    /// Used for `COPY`/`ADD` from build context or between stages.
    ///
    /// # Errors
    ///
    /// Returns an error if files cannot be copied.
    async fn copy_to_guest(&self, host_paths: &[PathBuf], dest: &str) -> anyhow::Result<()>;

    /// Set up a mount before `RUN` execution.
    ///
    /// # Errors
    ///
    /// Returns an error if the mount cannot be set up.
    async fn setup_mount(&self, mount: &ResolvedMount) -> anyhow::Result<()>;

    /// Tear down a mount after `RUN` execution.
    ///
    /// # Errors
    ///
    /// Returns an error if the mount cannot be torn down.
    async fn teardown_mount(&self, mount: &ResolvedMount) -> anyhow::Result<()>;
}

// ── Data Types ──────────────────────────────────────────────────────────

/// Data returned from a layer snapshot.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LayerSnapshot {
    /// Base64-encoded tar.gz of the layer.
    pub data: String,
    /// SHA-256 digest of compressed layer (`sha256:...`).
    pub compressed_digest: String,
    /// SHA-256 digest of uncompressed layer (`sha256:...`).
    pub uncompressed_digest: String,
    /// Size of compressed layer in bytes.
    pub compressed_size: u64,
}

impl LayerSnapshot {
    /// Create a new layer snapshot.
    #[must_use]
    pub fn new(
        data: String,
        compressed_digest: String,
        uncompressed_digest: String,
        compressed_size: u64,
    ) -> Self {
        Self {
            data,
            compressed_digest,
            uncompressed_digest,
            compressed_size,
        }
    }
}

/// A resolved mount for a `RUN --mount` instruction.
///
/// Created by resolving variable substitutions in [`MountFlag`](crate::dockerfile::MountFlag)
/// fields before passing to [`BuildExecutor::setup_mount`].
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ResolvedMount {
    /// The type of mount (bind, cache, tmpfs, secret, ssh).
    pub mount_type: MountType,
    /// Target path inside the container.
    pub target: String,
    /// Source path or identifier.
    pub source: Option<String>,
    /// Whether the mount is read-only.
    pub read_only: bool,
    /// Cache or secret identifier.
    pub id: Option<String>,
}

impl ResolvedMount {
    /// Create a new resolved mount.
    #[must_use]
    pub fn new(
        mount_type: MountType,
        target: String,
        source: Option<String>,
        read_only: bool,
        id: Option<String>,
    ) -> Self {
        Self {
            mount_type,
            target,
            source,
            read_only,
            id,
        }
    }
}

/// Configuration for a build operation.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BuildConfig {
    /// Parsed Dockerfile.
    pub dockerfile: ParsedDockerfile,
    /// Build arguments (`--build-arg KEY=VAL`).
    pub build_args: HashMap<String, String>,
    /// Target stage name (`--target`).  `None` = build all stages.
    pub target: Option<String>,
    /// Whether to skip cache.
    pub no_cache: bool,
    /// Build context directory path.
    pub context_dir: PathBuf,
    /// Image tag to apply.
    pub tag: Option<String>,
}

impl BuildConfig {
    /// Create a new build configuration.
    #[must_use]
    pub fn new(dockerfile: ParsedDockerfile, context_dir: PathBuf) -> Self {
        Self {
            dockerfile,
            build_args: HashMap::new(),
            target: None,
            no_cache: false,
            context_dir,
            tag: None,
        }
    }
}

/// The result of a successful build.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BuildResult {
    /// External base image reference for the final stage, if any.
    pub base_image: Option<String>,
    /// All layers produced by the build, in order.
    pub layers: Vec<BuiltLayer>,
    /// Final image configuration metadata.
    pub config: ImageMetadata,
    /// Build steps executed (for progress reporting).
    pub steps: Vec<BuildStep>,
}

/// A single built layer.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BuiltLayer {
    /// Base64-encoded compressed layer data.
    pub data: String,
    /// Compressed digest.
    pub compressed_digest: String,
    /// Uncompressed digest (`DiffID`).
    pub uncompressed_digest: String,
    /// Compressed size in bytes.
    pub compressed_size: u64,
    /// Whether this is an empty layer (metadata-only instruction).
    pub empty: bool,
}

impl BuiltLayer {
    /// Create a new built layer.
    #[must_use]
    pub fn new(
        data: String,
        compressed_digest: String,
        uncompressed_digest: String,
        compressed_size: u64,
        empty: bool,
    ) -> Self {
        Self {
            data,
            compressed_digest,
            uncompressed_digest,
            compressed_size,
            empty,
        }
    }
}

/// Accumulated image metadata from Dockerfile instructions.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct ImageMetadata {
    /// `CMD` instruction.
    pub cmd: Option<Vec<String>>,
    /// `ENTRYPOINT` instruction.
    pub entrypoint: Option<Vec<String>>,
    /// `ENV` key-value pairs.
    pub env: Vec<(String, String)>,
    /// `WORKDIR` value.
    pub working_dir: Option<String>,
    /// `USER` value.
    pub user: Option<String>,
    /// `EXPOSE`d ports.
    pub exposed_ports: Vec<(u16, String)>,
    /// `LABEL`s.
    pub labels: Vec<(String, String)>,
    /// `SHELL` override.
    pub shell: Option<Vec<String>>,
    /// `STOPSIGNAL` value.
    pub stop_signal: Option<String>,
    /// `VOLUME` mount points.
    pub volumes: Vec<String>,
}

/// A record of a build step for progress reporting.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct BuildStep {
    /// Step number (1-indexed).
    pub number: usize,
    /// Total steps in the build.
    pub total: usize,
    /// The instruction text.
    pub instruction: String,
    /// Whether the step was cached.
    pub cached: bool,
}

// ── Build Engine ────────────────────────────────────────────────────────

/// Orchestrates multi-stage Dockerfile builds.
///
/// The engine processes each stage sequentially, executing instructions
/// and creating layers.  It uses a [`BuildExecutor`] trait to abstract
/// the actual guest VM communication, making the engine fully testable.
pub struct BuildEngine<E: BuildExecutor> {
    executor: E,
    config: BuildConfig,
}

impl<E: BuildExecutor> BuildEngine<E> {
    /// Create a new build engine.
    #[must_use]
    pub fn new(executor: E, config: BuildConfig) -> Self {
        Self { executor, config }
    }

    /// Execute the build, returning the built image layers and metadata.
    ///
    /// # Errors
    ///
    /// Returns an error if any build instruction fails (e.g. a `RUN`
    /// command exits non-zero, or the executor cannot snapshot a layer).
    pub async fn build(&self) -> anyhow::Result<BuildResult> {
        let needed = self.resolve_needed_stages()?;
        let global_vars = self.resolve_global_args();
        let total_steps = self.count_steps(&needed);
        let final_stage_idx = *needed.last().context("no stages were selected")?;

        let mut completed: Vec<StageState> = Vec::new();
        let mut all_steps: Vec<BuildStep> = Vec::new();
        let mut step_num: usize = 0;
        let mut final_base_image = None;

        for &stage_idx in &needed {
            let stage = &self.config.dockerfile.stages[stage_idx];

            // Substitute global ARGs in FROM image reference.
            let from_image = substitute_vars(&stage.from.image, &global_vars)
                .context("variable substitution in FROM image")?;

            // FROM step.
            step_num += 1;
            all_steps.push(BuildStep {
                number: step_num,
                total: total_steps,
                instruction: format_from(&from_image, stage.from.alias.as_deref()),
                cached: false,
            });

            // Determine overlay lower dir (another stage or external image).
            let lower = resolve_stage_ref(&from_image, &self.config.dockerfile.stages)
                .map(|_| from_image.clone());
            if stage_idx == final_stage_idx && lower.is_none() {
                final_base_image = Some(from_image.clone());
            }

            self.executor
                .overlay_init(lower)
                .await
                .context("failed to initialize overlay")?;

            let mut build_st = StageState {
                layers: Vec::new(),
                vars: HashMap::new(),
                env_vars: HashMap::new(),
                working_dir: "/".to_owned(),
                shell: vec!["/bin/sh".to_owned(), "-c".to_owned()],
                metadata: ImageMetadata::default(),
                context_dir: self.config.context_dir.clone(),
            };

            for instr in &stage.instructions {
                step_num += 1;
                all_steps.push(BuildStep {
                    number: step_num,
                    total: total_steps,
                    instruction: format_instruction(instr),
                    cached: false,
                });

                self.process_instruction(instr, &mut build_st, &completed)
                    .await?;
            }

            completed.push(build_st);
        }

        let final_state = completed.last().context("no stages were built")?;

        Ok(BuildResult {
            base_image: final_base_image,
            layers: final_state.layers.clone(),
            config: final_state.metadata.clone(),
            steps: all_steps,
        })
    }

    // ── Private helpers ─────────────────────────────────────────────────

    /// Determine which stage indices must be built.
    fn resolve_needed_stages(&self) -> anyhow::Result<Vec<usize>> {
        let stages = &self.config.dockerfile.stages;

        let Some(target) = &self.config.target else {
            return Ok((0..stages.len()).collect());
        };

        let target_idx = stages
            .iter()
            .position(|s| s.from.alias.as_deref() == Some(target.as_str()))
            .ok_or_else(|| anyhow::anyhow!("target stage '{target}' not found"))?;

        let mut needed = HashSet::new();
        let mut queue = vec![target_idx];

        while let Some(idx) = queue.pop() {
            if !needed.insert(idx) {
                continue;
            }
            let stage = &stages[idx];

            // FROM dependency.
            if let Some(dep) = resolve_stage_ref(&stage.from.image, stages) {
                queue.push(dep);
            }

            // COPY --from dependencies.
            for instr in &stage.instructions {
                if let BuildInstruction::Copy(c) = instr {
                    if let Some(from) = &c.from {
                        if let Some(dep) = resolve_stage_ref(from, stages) {
                            queue.push(dep);
                        }
                    }
                }
            }
        }

        let mut result: Vec<usize> = needed.into_iter().collect();
        result.sort_unstable();
        Ok(result)
    }

    /// Merge global `ARG`s with `--build-arg` values.
    fn resolve_global_args(&self) -> HashMap<String, String> {
        let mut vars = HashMap::new();
        for arg in &self.config.dockerfile.global_args {
            if let Some(val) = self.config.build_args.get(&arg.name) {
                vars.insert(arg.name.clone(), val.clone());
            } else if let Some(default) = &arg.default_value {
                vars.insert(arg.name.clone(), default.clone());
            }
        }
        vars
    }

    /// Count total build steps across all needed stages (FROM + instructions).
    fn count_steps(&self, needed: &[usize]) -> usize {
        needed
            .iter()
            .map(|&i| 1 + self.config.dockerfile.stages[i].instructions.len())
            .sum()
    }

    /// Dispatch a single instruction to the appropriate handler.
    async fn process_instruction(
        &self,
        instr: &BuildInstruction,
        state: &mut StageState,
        completed: &[StageState],
    ) -> anyhow::Result<()> {
        match instr {
            BuildInstruction::Run(r) => self.process_run(r, state).await,
            BuildInstruction::Copy(c) => self.process_copy(c, state, completed).await,
            BuildInstruction::Add(a) => self.process_add(a, state).await,
            BuildInstruction::Env(e) => Self::apply_env(e, state),
            BuildInstruction::Arg(a) => {
                self.apply_arg(a, state);
                Ok(())
            }
            BuildInstruction::Workdir(w) => Self::apply_workdir(w, state),
            BuildInstruction::User(u) => {
                Self::apply_user(u, state);
                Ok(())
            }
            BuildInstruction::Cmd(c) => {
                Self::apply_cmd(c, state);
                Ok(())
            }
            BuildInstruction::Entrypoint(e) => {
                Self::apply_entrypoint(e, state);
                Ok(())
            }
            BuildInstruction::Expose(e) => {
                Self::apply_expose(e, state);
                Ok(())
            }
            BuildInstruction::Label(l) => {
                Self::apply_label(l, state);
                Ok(())
            }
            BuildInstruction::Shell(s) => {
                Self::apply_shell(s, state);
                Ok(())
            }
            BuildInstruction::Stopsignal(s) => {
                Self::apply_stopsignal(s, state);
                Ok(())
            }
            BuildInstruction::Healthcheck(_) => Ok(()),
            BuildInstruction::Volume(v) => {
                Self::apply_volume(v, state);
                Ok(())
            }
        }
    }

    // ── Layer-producing instructions ────────────────────────────────────

    async fn process_run(&self, run: &RunInstr, state: &mut StageState) -> anyhow::Result<()> {
        let cmd = build_run_command(run, &state.vars, &state.shell)?;

        let env: Vec<String> = {
            let mut pairs: Vec<_> = state
                .env_vars
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            pairs.sort();
            pairs
        };

        // Resolve mounts with variable substitution.
        let resolved_mounts = resolve_mounts(&run.mounts, &state.vars)?;

        // Set up mounts in forward order.
        for mount in &resolved_mounts {
            self.executor
                .setup_mount(mount)
                .await
                .context("failed to set up mount")?;
        }

        // Execute the command.
        let exec_result = self
            .executor
            .exec(&cmd, &env, &state.working_dir)
            .await
            .context("failed to execute RUN command");

        // Tear down mounts in reverse order (even on failure).
        for mount in resolved_mounts.iter().rev() {
            self.executor
                .teardown_mount(mount)
                .await
                .context("failed to tear down mount")?;
        }

        // Log secret mount exclusions (actual exclusion in WS3.1).
        for mount in &resolved_mounts {
            if mount.mount_type == MountType::Secret {
                tracing::debug!(
                    target = %mount.target,
                    "secret mount path excluded from snapshot"
                );
            }
        }

        let (exit_code, stdout, stderr) = exec_result?;

        if exit_code != 0 {
            anyhow::bail!(
                "RUN command failed (exit code {exit_code}): {}\nstdout: {stdout}\nstderr: {stderr}",
                format_command(&run.command),
            );
        }

        snapshot_and_flatten(&self.executor, &mut state.layers).await
    }

    async fn process_copy(
        &self,
        copy: &CopyInstr,
        state: &mut StageState,
        _completed: &[StageState],
    ) -> anyhow::Result<()> {
        let dest = substitute_vars(&copy.dest, &state.vars)
            .context("variable substitution in COPY dest")?;

        let sources: Vec<PathBuf> = copy
            .sources
            .iter()
            .map(|s| {
                let expanded = substitute_vars(s, &state.vars)
                    .context("variable substitution in COPY source")?;
                if copy.from.is_some() {
                    Ok(PathBuf::from(expanded))
                } else {
                    Ok(state.context_dir.join(expanded))
                }
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        self.executor
            .copy_to_guest(&sources, &dest)
            .await
            .context("failed to copy files to guest")?;

        snapshot_and_flatten(&self.executor, &mut state.layers).await
    }

    async fn process_add(&self, add: &AddInstr, state: &mut StageState) -> anyhow::Result<()> {
        let dest =
            substitute_vars(&add.dest, &state.vars).context("variable substitution in ADD dest")?;

        let sources: Vec<PathBuf> = add
            .sources
            .iter()
            .map(|s| {
                let expanded = substitute_vars(s, &state.vars)
                    .context("variable substitution in ADD source")?;
                Ok(state.context_dir.join(expanded))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        self.executor
            .copy_to_guest(&sources, &dest)
            .await
            .context("failed to ADD files to guest")?;

        snapshot_and_flatten(&self.executor, &mut state.layers).await
    }

    // ── Metadata-only instructions ──────────────────────────────────────

    fn apply_env(env: &EnvInstr, state: &mut StageState) -> anyhow::Result<()> {
        for (key, val) in &env.vars {
            let expanded =
                substitute_vars(val, &state.vars).context("variable substitution in ENV value")?;
            state.vars.insert(key.clone(), expanded.clone());
            state.env_vars.insert(key.clone(), expanded.clone());
            state.metadata.env.push((key.clone(), expanded));
        }
        Ok(())
    }

    fn apply_arg(&self, arg: &ArgInstr, state: &mut StageState) {
        let val = self
            .config
            .build_args
            .get(&arg.name)
            .cloned()
            .or_else(|| arg.default_value.clone())
            .or_else(|| {
                // Re-declared global ARG: inherit its default.
                self.config
                    .dockerfile
                    .global_args
                    .iter()
                    .find(|g| g.name == arg.name)
                    .and_then(|g| g.default_value.clone())
            })
            .unwrap_or_default();
        state.vars.insert(arg.name.clone(), val);
    }

    fn apply_workdir(wd: &WorkdirInstr, state: &mut StageState) -> anyhow::Result<()> {
        let path =
            substitute_vars(&wd.path, &state.vars).context("variable substitution in WORKDIR")?;
        if path.starts_with('/') {
            state.working_dir.clone_from(&path);
        } else if state.working_dir.ends_with('/') {
            state.working_dir.push_str(&path);
        } else {
            state.working_dir.push('/');
            state.working_dir.push_str(&path);
        }
        state.metadata.working_dir = Some(state.working_dir.clone());
        Ok(())
    }

    fn apply_user(user: &UserInstr, state: &mut StageState) {
        state.metadata.user = Some(user.user.clone());
    }

    fn apply_cmd(cmd: &CmdInstr, state: &mut StageState) {
        state.metadata.cmd = Some(command_to_vec(&cmd.command));
    }

    fn apply_entrypoint(ep: &EntrypointInstr, state: &mut StageState) {
        state.metadata.entrypoint = Some(command_to_vec(&ep.command));
    }

    fn apply_expose(exp: &ExposeInstr, state: &mut StageState) {
        for port in &exp.ports {
            state
                .metadata
                .exposed_ports
                .push((port.port, port.protocol.clone()));
        }
    }

    fn apply_label(label: &LabelInstr, state: &mut StageState) {
        for (k, v) in &label.labels {
            state.metadata.labels.push((k.clone(), v.clone()));
        }
    }

    fn apply_shell(shell: &ShellInstr, state: &mut StageState) {
        state.shell.clone_from(&shell.shell);
        state.metadata.shell = Some(shell.shell.clone());
    }

    fn apply_stopsignal(sig: &StopsignalInstr, state: &mut StageState) {
        state.metadata.stop_signal = Some(sig.signal.clone());
    }

    fn apply_volume(vol: &VolumeInstr, state: &mut StageState) {
        state.metadata.volumes.extend(vol.paths.iter().cloned());
    }
}

// ── Internal State ──────────────────────────────────────────────────────

/// Internal state for a single build stage.
/// Fields `from_image` and `alias` are used during image assembly (WS3.2).
#[allow(dead_code)]
struct StageState {
    /// Layers built in this stage.
    layers: Vec<BuiltLayer>,
    /// Combined ARG + ENV vars for variable substitution.
    vars: HashMap<String, String>,
    /// ENV-only vars passed to `executor.exec()`.
    env_vars: HashMap<String, String>,
    /// Current working directory.
    working_dir: String,
    /// Current shell (`["/bin/sh", "-c"]` by default).
    shell: Vec<String>,
    /// Image metadata accumulated from instructions.
    metadata: ImageMetadata,
    /// Build context directory for `COPY`/`ADD` from host.
    context_dir: PathBuf,
}

// ── Free Functions ──────────────────────────────────────────────────────

/// Resolve a stage reference (alias name or numeric index).
fn resolve_stage_ref(reference: &str, stages: &[Stage]) -> Option<usize> {
    if let Ok(idx) = reference.parse::<usize>() {
        if idx < stages.len() {
            return Some(idx);
        }
    }
    stages
        .iter()
        .position(|s| s.from.alias.as_deref() == Some(reference))
}

/// Snapshot the current overlay and flatten for the next instruction.
async fn snapshot_and_flatten<E: BuildExecutor>(
    executor: &E,
    layers: &mut Vec<BuiltLayer>,
) -> anyhow::Result<()> {
    let snap = executor
        .snapshot_layer()
        .await
        .context("failed to snapshot layer")?;

    layers.push(BuiltLayer {
        data: snap.data,
        compressed_digest: snap.compressed_digest,
        uncompressed_digest: snap.uncompressed_digest,
        compressed_size: snap.compressed_size,
        empty: false,
    });

    executor
        .flatten_overlay()
        .await
        .context("failed to flatten overlay")?;

    Ok(())
}

/// Resolve [`MountFlag`]s into [`ResolvedMount`]s with variable substitution.
fn resolve_mounts(
    mounts: &[crate::dockerfile::MountFlag],
    vars: &HashMap<String, String>,
) -> anyhow::Result<Vec<ResolvedMount>> {
    mounts
        .iter()
        .map(|m| {
            let target = substitute_vars(&m.target, vars)
                .context("variable substitution in mount target")?;
            let source = m
                .source
                .as_ref()
                .map(|s| substitute_vars(s, vars))
                .transpose()
                .context("variable substitution in mount source")?;
            Ok(ResolvedMount {
                mount_type: m.mount_type.clone(),
                target,
                source,
                read_only: m.read_only,
                id: m.id.clone(),
            })
        })
        .collect()
}

/// Build the command array for a `RUN` instruction.
fn build_run_command(
    run: &RunInstr,
    vars: &HashMap<String, String>,
    shell: &[String],
) -> anyhow::Result<Vec<String>> {
    match &run.command {
        CommandForm::Shell(s) => {
            let expanded =
                substitute_vars(s, vars).context("variable substitution in RUN command")?;
            let mut cmd = shell.to_vec();
            cmd.push(expanded);
            Ok(cmd)
        }
        CommandForm::Exec(args) => args
            .iter()
            .map(|a| substitute_vars(a, vars).context("variable substitution in RUN exec arg"))
            .collect(),
    }
}

/// Convert a [`CommandForm`] to a flat `Vec<String>`.
fn command_to_vec(cmd: &CommandForm) -> Vec<String> {
    match cmd {
        CommandForm::Shell(s) => vec![s.clone()],
        CommandForm::Exec(args) => args.clone(),
    }
}

/// Format a `FROM` instruction for step reporting.
fn format_from(image: &str, alias: Option<&str>) -> String {
    match alias {
        Some(a) => format!("FROM {image} AS {a}"),
        None => format!("FROM {image}"),
    }
}

/// Format a [`BuildInstruction`] for step reporting.
fn format_instruction(instr: &BuildInstruction) -> String {
    match instr {
        BuildInstruction::Run(r) => format!("RUN {}", format_command(&r.command)),
        BuildInstruction::Copy(c) => {
            let from = c
                .from
                .as_ref()
                .map_or(String::new(), |f| format!("--from={f} "));
            format!("COPY {from}{} {}", c.sources.join(" "), c.dest)
        }
        BuildInstruction::Add(a) => {
            format!("ADD {} {}", a.sources.join(" "), a.dest)
        }
        BuildInstruction::Env(e) => {
            let pairs: Vec<String> = e.vars.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("ENV {}", pairs.join(" "))
        }
        BuildInstruction::Arg(a) => match &a.default_value {
            Some(d) => format!("ARG {}={d}", a.name),
            None => format!("ARG {}", a.name),
        },
        BuildInstruction::Workdir(w) => format!("WORKDIR {}", w.path),
        BuildInstruction::User(u) => format!("USER {}", u.user),
        BuildInstruction::Cmd(c) => format!("CMD {}", format_command(&c.command)),
        BuildInstruction::Entrypoint(e) => {
            format!("ENTRYPOINT {}", format_command(&e.command))
        }
        BuildInstruction::Expose(e) => {
            let ports: Vec<String> = e
                .ports
                .iter()
                .map(|p| format!("{}/{}", p.port, p.protocol))
                .collect();
            format!("EXPOSE {}", ports.join(" "))
        }
        BuildInstruction::Label(l) => {
            let pairs: Vec<String> = l.labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
            format!("LABEL {}", pairs.join(" "))
        }
        BuildInstruction::Shell(s) => format!("SHELL {:?}", s.shell),
        BuildInstruction::Stopsignal(s) => format!("STOPSIGNAL {}", s.signal),
        BuildInstruction::Healthcheck(_) => "HEALTHCHECK".to_owned(),
        BuildInstruction::Volume(v) => format!("VOLUME {}", v.paths.join(" ")),
    }
}

/// Format a [`CommandForm`] as a human-readable string.
fn format_command(cmd: &CommandForm) -> String {
    match cmd {
        CommandForm::Shell(s) => s.clone(),
        CommandForm::Exec(args) => format!("{args:?}"),
    }
}

#[cfg(test)]
#[path = "engine_test.rs"]
mod tests;
