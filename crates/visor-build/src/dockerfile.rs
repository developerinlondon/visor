//! Dockerfile parsing into typed build instructions.
//!
//! Wraps the [`parse_dockerfile`] crate to produce owned instruction types
//! that the build engine can execute. Variable substitution (ARG/ENV) is
//! **not** performed here — the raw strings come through as-is, and the
//! caller is responsible for running [`crate::substitute::substitute_vars`].

// ── Types ────────────────────────────────────────────────────────────────

/// A fully parsed Dockerfile.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ParsedDockerfile {
    /// ARG instructions that appear **before** the first `FROM`.
    pub global_args: Vec<ArgInstr>,
    /// Ordered list of build stages (one per `FROM`).
    pub stages: Vec<Stage>,
}

/// A single build stage starting with a `FROM` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Stage {
    /// The `FROM` instruction that opens this stage.
    pub from: FromInstr,
    /// Instructions within the stage, in order.
    pub instructions: Vec<BuildInstruction>,
}

/// Every Dockerfile instruction we recognise.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum BuildInstruction {
    /// `RUN` instruction.
    Run(RunInstr),
    /// `COPY` instruction.
    Copy(CopyInstr),
    /// `ADD` instruction.
    Add(AddInstr),
    /// `CMD` instruction.
    Cmd(CmdInstr),
    /// `ENTRYPOINT` instruction.
    Entrypoint(EntrypointInstr),
    /// `ENV` instruction.
    Env(EnvInstr),
    /// `ARG` instruction (within a stage).
    Arg(ArgInstr),
    /// `WORKDIR` instruction.
    Workdir(WorkdirInstr),
    /// `USER` instruction.
    User(UserInstr),
    /// `EXPOSE` instruction.
    Expose(ExposeInstr),
    /// `LABEL` instruction.
    Label(LabelInstr),
    /// `SHELL` instruction.
    Shell(ShellInstr),
    /// `STOPSIGNAL` instruction.
    Stopsignal(StopsignalInstr),
    /// `HEALTHCHECK` instruction.
    Healthcheck(HealthcheckInstr),
    /// `VOLUME` instruction.
    Volume(VolumeInstr),
}

/// `FROM` instruction — opens a new build stage.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct FromInstr {
    /// Raw image reference (may contain `${VAR}`).
    pub image: String,
    /// `FROM ... AS name`.
    pub alias: Option<String>,
    /// `--platform=linux/amd64`.
    pub platform: Option<String>,
}

/// Shell form vs exec form for commands.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum CommandForm {
    /// Shell form: `"apt-get update"`.
    Shell(String),
    /// Exec form: `["executable", "arg1"]`.
    Exec(Vec<String>),
}

/// `RUN` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct RunInstr {
    /// The command to execute.
    pub command: CommandForm,
    /// `--mount` flags (e.g. `type=cache,target=/tmp`).
    pub mounts: Vec<MountFlag>,
    /// `--network=none` etc.
    pub network: Option<String>,
}

/// A parsed `--mount` flag.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct MountFlag {
    /// Mount type (bind, cache, tmpfs, secret, ssh).
    pub mount_type: MountType,
    /// Target path inside the container.
    pub target: String,
    /// Source path or identifier.
    pub source: Option<String>,
    /// `from=builder` for bind mounts from another stage.
    pub from: Option<String>,
    /// Cache identifier.
    pub id: Option<String>,
    /// Whether the mount is read-only.
    pub read_only: bool,
    /// Sharing mode (`locked`, `shared`, `private`).
    pub sharing: Option<String>,
}

/// Type of a `--mount` flag.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum MountType {
    /// `type=bind`.
    Bind,
    /// `type=cache`.
    Cache,
    /// `type=tmpfs`.
    Tmpfs,
    /// `type=secret`.
    Secret,
    /// `type=ssh`.
    Ssh,
}

/// `COPY` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CopyInstr {
    /// Source paths.
    pub sources: Vec<String>,
    /// Destination path.
    pub dest: String,
    /// `--from=builder`.
    pub from: Option<String>,
    /// `--chown=user:group`.
    pub chown: Option<String>,
    /// `--chmod=755`.
    pub chmod: Option<String>,
    /// `--link`.
    pub link: bool,
}

/// `ADD` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AddInstr {
    /// Source paths/URLs.
    pub sources: Vec<String>,
    /// Destination path.
    pub dest: String,
    /// `--chown=user:group`.
    pub chown: Option<String>,
    /// `--chmod=755`.
    pub chmod: Option<String>,
    /// `--link`.
    pub link: bool,
    /// `--checksum=sha256:...`.
    pub checksum: Option<String>,
}

/// `ARG` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ArgInstr {
    /// Variable name.
    pub name: String,
    /// Default value (after `=`).
    pub default_value: Option<String>,
}

/// `ENV` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EnvInstr {
    /// Parsed `KEY=VALUE` pairs.
    pub vars: Vec<(String, String)>,
}

/// `CMD` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct CmdInstr {
    /// The command.
    pub command: CommandForm,
}

/// `ENTRYPOINT` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct EntrypointInstr {
    /// The command.
    pub command: CommandForm,
}

/// `WORKDIR` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct WorkdirInstr {
    /// Working directory path.
    pub path: String,
}

/// `USER` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct UserInstr {
    /// User name or UID.
    pub user: String,
    /// Optional group name or GID.
    pub group: Option<String>,
}

/// `EXPOSE` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExposeInstr {
    /// List of exposed ports.
    pub ports: Vec<ExposedPort>,
}

/// A single exposed port.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ExposedPort {
    /// Port number.
    pub port: u16,
    /// Protocol (`"tcp"` or `"udp"`).
    pub protocol: String,
}

/// `LABEL` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct LabelInstr {
    /// Parsed `KEY=VALUE` pairs.
    pub labels: Vec<(String, String)>,
}

/// `SHELL` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct ShellInstr {
    /// Shell command components.
    pub shell: Vec<String>,
}

/// `STOPSIGNAL` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StopsignalInstr {
    /// Signal name or number.
    pub signal: String,
}

/// `HEALTHCHECK` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct HealthcheckInstr {
    /// `true` when `HEALTHCHECK NONE`.
    pub disable: bool,
    /// The health-check command (absent when `disable` is `true`).
    pub command: Option<CommandForm>,
    /// `--interval=...`.
    pub interval: Option<String>,
    /// `--timeout=...`.
    pub timeout: Option<String>,
    /// `--retries=...`.
    pub retries: Option<String>,
    /// `--start-period=...`.
    pub start_period: Option<String>,
}

/// `VOLUME` instruction.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct VolumeInstr {
    /// Volume mount paths.
    pub paths: Vec<String>,
}

// ── Parser ───────────────────────────────────────────────────────────────

use parse_dockerfile::Instruction;

/// Parses raw Dockerfile text into a [`ParsedDockerfile`].
pub struct DockerfileParser;

impl DockerfileParser {
    /// Parse the given Dockerfile content string.
    ///
    /// The parser wraps [`parse_dockerfile::parse`] and converts the
    /// borrowed types into fully owned structures.
    ///
    /// # Errors
    ///
    /// Returns an error when the Dockerfile is syntactically invalid
    /// (e.g. missing `FROM` instruction).
    pub fn parse(content: &str) -> anyhow::Result<ParsedDockerfile> {
        let raw = parse_dockerfile::parse(content)
            .map_err(|e| anyhow::anyhow!("dockerfile parse error: {e}"))?;

        let global_args = raw
            .global_args()
            .map(|a| parse_arg_value(&a.arguments.value))
            .collect();

        let stages = raw.stages().map(|s| convert_stage(&s)).collect();

        Ok(ParsedDockerfile {
            global_args,
            stages,
        })
    }
}

// ── Conversion helpers ──────────────────────────────────────────────────

fn convert_stage(stage: &parse_dockerfile::Stage<'_, '_>) -> Stage {
    let from = convert_from(stage.from);
    let instructions = stage
        .instructions
        .iter()
        .filter_map(convert_instruction)
        .collect();
    Stage { from, instructions }
}

fn convert_from(from: &parse_dockerfile::FromInstruction<'_>) -> FromInstr {
    let image = from.image.value.to_string();
    let alias = from.as_.as_ref().map(|(_, name)| name.value.to_string());
    let platform = find_flag_value(&from.options, "platform");
    FromInstr {
        image,
        alias,
        platform,
    }
}

fn convert_instruction(instr: &Instruction<'_>) -> Option<BuildInstruction> {
    let bi = match instr {
        Instruction::Run(r) => BuildInstruction::Run(convert_run(r)),
        Instruction::Copy(c) => BuildInstruction::Copy(convert_copy(c)),
        Instruction::Add(a) => BuildInstruction::Add(convert_add(a)),
        Instruction::Cmd(c) => BuildInstruction::Cmd(CmdInstr {
            command: convert_command(&c.arguments),
        }),
        Instruction::Entrypoint(e) => BuildInstruction::Entrypoint(EntrypointInstr {
            command: convert_command(&e.arguments),
        }),
        Instruction::Env(e) => BuildInstruction::Env(EnvInstr {
            vars: parse_key_value_pairs(&e.arguments.value),
        }),
        Instruction::Arg(a) => BuildInstruction::Arg(parse_arg_value(&a.arguments.value)),
        Instruction::Workdir(w) => BuildInstruction::Workdir(WorkdirInstr {
            path: w.arguments.value.to_string(),
        }),
        Instruction::User(u) => BuildInstruction::User(parse_user_value(&u.arguments.value)),
        Instruction::Expose(e) => BuildInstruction::Expose(convert_expose(e)),
        Instruction::Label(l) => BuildInstruction::Label(LabelInstr {
            labels: parse_key_value_pairs(&l.arguments.value),
        }),
        Instruction::Shell(s) => BuildInstruction::Shell(ShellInstr {
            shell: s.arguments.iter().map(|a| a.value.to_string()).collect(),
        }),
        Instruction::Stopsignal(s) => BuildInstruction::Stopsignal(StopsignalInstr {
            signal: s.arguments.value.to_string(),
        }),
        Instruction::Healthcheck(h) => BuildInstruction::Healthcheck(convert_healthcheck(h)),
        Instruction::Volume(v) => BuildInstruction::Volume(convert_volume(v)),
        // Maintainer, Onbuild, From, and any future variants — skip.
        _ => return None,
    };
    Some(bi)
}

// ── Command conversion ──────────────────────────────────────────────────

fn convert_command(cmd: &parse_dockerfile::Command<'_>) -> CommandForm {
    match cmd {
        parse_dockerfile::Command::Shell(s) => CommandForm::Shell(s.value.to_string()),
        parse_dockerfile::Command::Exec(e) => {
            CommandForm::Exec(e.value.iter().map(|s| s.value.to_string()).collect())
        }
        // Future command forms — fall back to shell representation.
        _ => CommandForm::Shell(String::new()),
    }
}
// ── RUN conversion ──────────────────────────────────────────────────────

fn convert_run(run: &parse_dockerfile::RunInstruction<'_>) -> RunInstr {
    let command = convert_command(&run.arguments);
    let mut mounts = Vec::new();
    let mut network = None;

    for flag in &run.options {
        let name = flag.name.value.as_ref();
        let value = flag.value.as_ref().map(|v| v.value.as_ref());
        match name {
            "mount" => {
                if let Some(v) = value {
                    mounts.push(parse_mount_flag(v));
                }
            }
            "network" => {
                network = value.map(std::string::ToString::to_string);
            }
            _ => {}
        }
    }

    RunInstr {
        command,
        mounts,
        network,
    }
}

fn parse_mount_flag(raw: &str) -> MountFlag {
    let mut mount_type = MountType::Bind; // Docker default
    let mut target = String::new();
    let mut source = None;
    let mut from = None;
    let mut id = None;
    let mut read_only = false;
    let mut sharing = None;

    for part in raw.split(',') {
        if let Some((k, v)) = part.split_once('=') {
            match k {
                "type" => {
                    mount_type = match v {
                        "cache" => MountType::Cache,
                        "tmpfs" => MountType::Tmpfs,
                        "secret" => MountType::Secret,
                        "ssh" => MountType::Ssh,
                        _ => MountType::Bind,
                    };
                }
                "target" | "dst" | "destination" => target = v.to_string(),
                "source" | "src" => source = Some(v.to_string()),
                "from" => from = Some(v.to_string()),
                "id" => id = Some(v.to_string()),
                "readonly" | "ro" => read_only = v == "true" || v == "1",
                "sharing" => sharing = Some(v.to_string()),
                _ => {}
            }
        } else {
            // Bare flags like "readonly"
            match part {
                "readonly" | "ro" => read_only = true,
                _ => {}
            }
        }
    }

    MountFlag {
        mount_type,
        target,
        source,
        from,
        id,
        read_only,
        sharing,
    }
}

// ── COPY / ADD conversion ───────────────────────────────────────────────

fn convert_copy(copy: &parse_dockerfile::CopyInstruction<'_>) -> CopyInstr {
    let sources: Vec<String> = copy
        .src
        .iter()
        .filter_map(|s| match s {
            parse_dockerfile::Source::Path(p) => Some(p.value.to_string()),
            parse_dockerfile::Source::HereDoc(_) | _ => None,
        })
        .collect();
    let dest = copy.dest.value.to_string();
    let from = find_flag_value_small(&copy.options, "from");
    let chown = find_flag_value_small(&copy.options, "chown");
    let chmod = find_flag_value_small(&copy.options, "chmod");
    let link = has_flag_small(&copy.options, "link");

    CopyInstr {
        sources,
        dest,
        from,
        chown,
        chmod,
        link,
    }
}

fn convert_add(add: &parse_dockerfile::AddInstruction<'_>) -> AddInstr {
    let sources: Vec<String> = add
        .src
        .iter()
        .filter_map(|s| match s {
            parse_dockerfile::Source::Path(p) => Some(p.value.to_string()),
            parse_dockerfile::Source::HereDoc(_) | _ => None,
        })
        .collect();
    let dest = add.dest.value.to_string();
    let chown = find_flag_value_small(&add.options, "chown");
    let chmod = find_flag_value_small(&add.options, "chmod");
    let link = has_flag_small(&add.options, "link");
    let checksum = find_flag_value_small(&add.options, "checksum");

    AddInstr {
        sources,
        dest,
        chown,
        chmod,
        link,
        checksum,
    }
}

// ── EXPOSE conversion ───────────────────────────────────────────────────

fn convert_expose(expose: &parse_dockerfile::ExposeInstruction<'_>) -> ExposeInstr {
    let mut ports = Vec::new();
    for arg in &expose.arguments {
        let raw = arg.value.as_ref();
        for part in raw.split_whitespace() {
            if let Some(ep) = parse_exposed_port(part) {
                ports.push(ep);
            }
        }
    }
    ExposeInstr { ports }
}

fn parse_exposed_port(raw: &str) -> Option<ExposedPort> {
    let (port_str, proto) = if let Some((p, pr)) = raw.split_once('/') {
        (p, pr.to_string())
    } else {
        (raw, "tcp".to_string())
    };
    let port = port_str.parse::<u16>().ok()?;
    Some(ExposedPort {
        port,
        protocol: proto,
    })
}

// ── HEALTHCHECK conversion ──────────────────────────────────────────────

fn convert_healthcheck(hc: &parse_dockerfile::HealthcheckInstruction<'_>) -> HealthcheckInstr {
    let mut interval = None;
    let mut timeout = None;
    let mut retries = None;
    let mut start_period = None;

    for flag in &hc.options {
        let name = flag.name.value.as_ref();
        let value = flag.value.as_ref().map(|v| v.value.to_string());
        match name {
            "interval" => interval = value,
            "timeout" => timeout = value,
            "retries" => retries = value,
            "start-period" => start_period = value,
            _ => {}
        }
    }

    match &hc.arguments {
        parse_dockerfile::HealthcheckArguments::None { .. } => HealthcheckInstr {
            disable: true,
            command: None,
            interval,
            timeout,
            retries,
            start_period,
        },
        parse_dockerfile::HealthcheckArguments::Cmd { arguments, .. } => HealthcheckInstr {
            disable: false,
            command: Some(convert_command(arguments)),
            interval,
            timeout,
            retries,
            start_period,
        },
        _ => HealthcheckInstr {
            disable: false,
            command: None,
            interval,
            timeout,
            retries,
            start_period,
        },
    }
}

// ── VOLUME conversion ───────────────────────────────────────────────────

fn convert_volume(vol: &parse_dockerfile::VolumeInstruction<'_>) -> VolumeInstr {
    let paths = match &vol.arguments {
        parse_dockerfile::JsonOrStringArray::Json(j) => {
            j.value.iter().map(|s| s.value.to_string()).collect()
        }
        parse_dockerfile::JsonOrStringArray::String(parts) => {
            parts.iter().map(|s| s.value.to_string()).collect()
        }
    };
    VolumeInstr { paths }
}

// ── ARG / ENV / USER parsing helpers ────────────────────────────────────

/// Parse `NAME=default` or `NAME` from an ARG arguments string.
fn parse_arg_value(raw: &str) -> ArgInstr {
    if let Some((name, default)) = raw.split_once('=') {
        ArgInstr {
            name: name.to_string(),
            default_value: Some(default.to_string()),
        }
    } else {
        ArgInstr {
            name: raw.to_string(),
            default_value: None,
        }
    }
}

/// Parse `user:group` or `user` from a USER arguments string.
fn parse_user_value(raw: &str) -> UserInstr {
    if let Some((user, group)) = raw.split_once(':') {
        UserInstr {
            user: user.to_string(),
            group: Some(group.to_string()),
        }
    } else {
        UserInstr {
            user: raw.to_string(),
            group: None,
        }
    }
}

/// Parse KEY=VALUE pairs from an ENV or LABEL arguments string.
///
/// Handles both modern form (`KEY=val KEY2=val2`) and legacy form
/// (`KEY value with spaces`).
fn parse_key_value_pairs(raw: &str) -> Vec<(String, String)> {
    let trimmed = raw.trim();
    // Detect modern form: contains '=' somewhere
    if !trimmed.contains('=') {
        // Legacy form: `ENV KEY value with spaces`
        if let Some((key, value)) = trimmed.split_once(char::is_whitespace) {
            return vec![(key.to_string(), value.trim_start().to_string())];
        }
        return vec![(trimmed.to_string(), String::new())];
    }

    // Modern form: split into KEY=VALUE tokens
    let mut pairs = Vec::new();
    let mut chars = trimmed.chars().peekable();

    while chars.peek().is_some() {
        // Skip whitespace between pairs
        skip_whitespace(&mut chars);
        if chars.peek().is_none() {
            break;
        }

        // Read key (up to '=')
        let key = read_until(&mut chars, '=');
        // consume '='
        if chars.peek() == Some(&'=') {
            chars.next();
        }

        // Read value (possibly quoted)
        let value = read_value(&mut chars);
        pairs.push((key, value));
    }

    pairs
}

fn skip_whitespace(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) {
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else {
            break;
        }
    }
}

fn read_until(chars: &mut std::iter::Peekable<std::str::Chars<'_>>, stop: char) -> String {
    let mut buf = String::new();
    while let Some(&c) = chars.peek() {
        if c == stop {
            break;
        }
        buf.push(c);
        chars.next();
    }
    buf
}

fn read_value(chars: &mut std::iter::Peekable<std::str::Chars<'_>>) -> String {
    match chars.peek() {
        Some(&'"') => {
            chars.next(); // consume opening quote
            let mut buf = String::new();
            for c in chars.by_ref() {
                if c == '"' {
                    break;
                }
                buf.push(c);
            }
            buf
        }
        Some(&'\'') => {
            chars.next(); // consume opening quote
            let mut buf = String::new();
            for c in chars.by_ref() {
                if c == '\'' {
                    break;
                }
                buf.push(c);
            }
            buf
        }
        _ => {
            // Unquoted: read until whitespace or end
            let mut buf = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_whitespace() {
                    break;
                }
                buf.push(c);
                chars.next();
            }
            buf
        }
    }
}

// ── Flag helpers ────────────────────────────────────────────────────────

fn find_flag_value(flags: &[parse_dockerfile::Flag<'_>], name: &str) -> Option<String> {
    flags.iter().find_map(|f| {
        if f.name.value.as_ref() == name {
            f.value.as_ref().map(|v| v.value.to_string())
        } else {
            None
        }
    })
}

fn find_flag_value_small(flags: &[parse_dockerfile::Flag<'_>], name: &str) -> Option<String> {
    flags.iter().find_map(|f| {
        if f.name.value.as_ref() == name {
            f.value.as_ref().map(|v| v.value.to_string())
        } else {
            None
        }
    })
}

fn has_flag_small(flags: &[parse_dockerfile::Flag<'_>], name: &str) -> bool {
    flags.iter().any(|f| f.name.value.as_ref() == name)
}

#[cfg(test)]
#[path = "dockerfile_test.rs"]
mod tests;
