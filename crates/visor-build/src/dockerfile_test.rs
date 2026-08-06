use super::*;

// ── Helpers ─────────────────────────────────────────────────────────

fn parse(s: &str) -> ParsedDockerfile {
    DockerfileParser::parse(s).unwrap()
}

// ── Basic single-stage ──────────────────────────────────────────────

#[test]
fn simple_single_stage() {
    let df = parse("FROM ubuntu:22.04\nRUN apt-get update\nCMD [\"bash\"]\n");
    assert_eq!(df.stages.len(), 1);
    assert_eq!(df.stages[0].from.image, "ubuntu:22.04");
    assert!(df.stages[0].from.alias.is_none());
    // RUN + CMD = 2 instructions
    assert_eq!(df.stages[0].instructions.len(), 2);
}

// ── Multi-stage with COPY --from ────────────────────────────────────

#[test]
fn multi_stage_with_copy_from() {
    let input = "\
FROM golang:1.21 AS builder
RUN go build -o /app

FROM alpine:3.18
COPY --from=builder /app /app
CMD [\"/app\"]
";
    let df = parse(input);
    assert_eq!(df.stages.len(), 2);
    assert_eq!(df.stages[0].from.image, "golang:1.21");
    assert_eq!(df.stages[0].from.alias.as_deref(), Some("builder"));
    assert_eq!(df.stages[1].from.image, "alpine:3.18");

    // Second stage has COPY + CMD
    let copy = df.stages[1]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Copy(c) => Some(c),
            _ => None,
        })
        .expect("should have COPY");
    assert_eq!(copy.from.as_deref(), Some("builder"));
}

// ── FROM with AS alias ──────────────────────────────────────────────

#[test]
fn from_with_alias() {
    let df = parse("FROM node:20 AS frontend\nRUN npm ci\n");
    assert_eq!(df.stages[0].from.alias.as_deref(), Some("frontend"));
}

// ── FROM with --platform ────────────────────────────────────────────

#[test]
fn from_with_platform() {
    let df = parse("FROM --platform=linux/amd64 ubuntu:22.04\nRUN echo hi\n");
    assert_eq!(df.stages[0].from.platform.as_deref(), Some("linux/amd64"));
    assert_eq!(df.stages[0].from.image, "ubuntu:22.04");
}

// ── Global ARGs ─────────────────────────────────────────────────────

#[test]
fn global_args_before_from() {
    let input = "\
ARG BASE_IMAGE=ubuntu:22.04
ARG VERSION
FROM ${BASE_IMAGE}
RUN echo hi
";
    let df = parse(input);
    assert_eq!(df.global_args.len(), 2);
    assert_eq!(df.global_args[0].name, "BASE_IMAGE");
    assert_eq!(
        df.global_args[0].default_value.as_deref(),
        Some("ubuntu:22.04")
    );
    assert_eq!(df.global_args[1].name, "VERSION");
    assert!(df.global_args[1].default_value.is_none());
}

// ── ARG with default value ──────────────────────────────────────────

#[test]
fn arg_with_default() {
    let df = parse("FROM ubuntu\nARG GOVERSION=1.21\nRUN echo $GOVERSION\n");
    let arg = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Arg(a) => Some(a),
            _ => None,
        })
        .expect("should have ARG");
    assert_eq!(arg.name, "GOVERSION");
    assert_eq!(arg.default_value.as_deref(), Some("1.21"));
}

// ── RUN shell form ──────────────────────────────────────────────────

#[test]
fn run_shell_form() {
    let df = parse("FROM ubuntu\nRUN apt-get update && apt-get install -y curl\n");
    let run = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Run(r) => Some(r),
            _ => None,
        })
        .expect("should have RUN");
    match &run.command {
        CommandForm::Shell(s) => {
            assert!(s.contains("apt-get update"));
        }
        CommandForm::Exec(_) => panic!("expected shell form"),
    }
}

// ── RUN exec form ───────────────────────────────────────────────────

#[test]
fn run_exec_form() {
    let df = parse("FROM ubuntu\nRUN [\"echo\", \"hello\"]\n");
    let run = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Run(r) => Some(r),
            _ => None,
        })
        .expect("should have RUN");
    match &run.command {
        CommandForm::Exec(args) => {
            assert_eq!(args, &["echo", "hello"]);
        }
        CommandForm::Shell(_) => panic!("expected exec form"),
    }
}

// ── RUN with --mount=type=cache ─────────────────────────────────────

#[test]
fn run_with_mount_cache() {
    let df =
        parse("FROM ubuntu\nRUN --mount=type=cache,target=/root/.cache pip install -r req.txt\n");
    let run = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Run(r) => Some(r),
            _ => None,
        })
        .expect("should have RUN");
    assert_eq!(run.mounts.len(), 1);
    assert_eq!(run.mounts[0].mount_type, MountType::Cache);
    assert_eq!(run.mounts[0].target, "/root/.cache");
}

// ── RUN with --mount=type=secret ────────────────────────────────────

#[test]
fn run_with_mount_secret() {
    let df = parse("FROM ubuntu\nRUN --mount=type=secret,id=mysecret cat /run/secrets/mysecret\n");
    let run = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Run(r) => Some(r),
            _ => None,
        })
        .expect("should have RUN");
    assert_eq!(run.mounts.len(), 1);
    assert_eq!(run.mounts[0].mount_type, MountType::Secret);
    assert_eq!(run.mounts[0].id.as_deref(), Some("mysecret"));
}

// ── COPY with multiple sources ──────────────────────────────────────

#[test]
fn copy_multiple_sources() {
    let df = parse("FROM ubuntu\nCOPY file1.txt file2.txt /dest/\n");
    let copy = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Copy(c) => Some(c),
            _ => None,
        })
        .expect("should have COPY");
    assert_eq!(copy.sources.len(), 2);
    assert_eq!(copy.sources[0], "file1.txt");
    assert_eq!(copy.sources[1], "file2.txt");
    assert_eq!(copy.dest, "/dest/");
}

// ── ENV modern form ─────────────────────────────────────────────────

#[test]
fn env_modern_key_value() {
    let df = parse("FROM ubuntu\nENV FOO=bar BAZ=qux\n");
    let env = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Env(e) => Some(e),
            _ => None,
        })
        .expect("should have ENV");
    assert_eq!(env.vars.len(), 2);
    assert_eq!(env.vars[0], ("FOO".to_owned(), "bar".to_owned()));
    assert_eq!(env.vars[1], ("BAZ".to_owned(), "qux".to_owned()));
}

// ── EXPOSE with protocol ────────────────────────────────────────────

#[test]
fn expose_with_protocol() {
    let df = parse("FROM ubuntu\nEXPOSE 80/tcp 443/udp\n");
    let expose = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Expose(e) => Some(e),
            _ => None,
        })
        .expect("should have EXPOSE");
    assert_eq!(expose.ports.len(), 2);
    assert_eq!(expose.ports[0].port, 80);
    assert_eq!(expose.ports[0].protocol, "tcp");
    assert_eq!(expose.ports[1].port, 443);
    assert_eq!(expose.ports[1].protocol, "udp");
}

// ── LABEL key=value pairs ───────────────────────────────────────────

#[test]
fn label_key_value() {
    let df = parse("FROM ubuntu\nLABEL maintainer=\"test@example.com\" version=\"1.0\"\n");
    let label = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Label(l) => Some(l),
            _ => None,
        })
        .expect("should have LABEL");
    assert_eq!(label.labels.len(), 2);
    assert_eq!(
        label.labels[0],
        ("maintainer".to_owned(), "test@example.com".to_owned())
    );
}

// ── HEALTHCHECK CMD ─────────────────────────────────────────────────

#[test]
fn healthcheck_cmd() {
    let df = parse(
        "FROM ubuntu\nHEALTHCHECK --interval=30s --timeout=5s CMD curl -f http://localhost/\n",
    );
    let hc = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Healthcheck(h) => Some(h),
            _ => None,
        })
        .expect("should have HEALTHCHECK");
    assert!(!hc.disable);
    assert!(hc.command.is_some());
    assert_eq!(hc.interval.as_deref(), Some("30s"));
    assert_eq!(hc.timeout.as_deref(), Some("5s"));
}

// ── HEALTHCHECK NONE ────────────────────────────────────────────────

#[test]
fn healthcheck_none() {
    let df = parse("FROM ubuntu\nHEALTHCHECK NONE\n");
    let hc = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Healthcheck(h) => Some(h),
            _ => None,
        })
        .expect("should have HEALTHCHECK");
    assert!(hc.disable);
    assert!(hc.command.is_none());
}

// ── WORKDIR, USER, SHELL, STOPSIGNAL, VOLUME ────────────────────────

#[test]
fn workdir_instruction() {
    let df = parse("FROM ubuntu\nWORKDIR /app\n");
    let wd = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Workdir(w) => Some(w),
            _ => None,
        })
        .expect("should have WORKDIR");
    assert_eq!(wd.path, "/app");
}

#[test]
fn user_instruction() {
    let df = parse("FROM ubuntu\nUSER node:node\n");
    let u = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::User(u) => Some(u),
            _ => None,
        })
        .expect("should have USER");
    assert_eq!(u.user, "node");
    assert_eq!(u.group.as_deref(), Some("node"));
}

#[test]
fn shell_instruction() {
    let df = parse("FROM ubuntu\nSHELL [\"/bin/bash\", \"-c\"]\n");
    let s = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Shell(s) => Some(s),
            _ => None,
        })
        .expect("should have SHELL");
    assert_eq!(s.shell, vec!["/bin/bash", "-c"]);
}

#[test]
fn stopsignal_instruction() {
    let df = parse("FROM ubuntu\nSTOPSIGNAL SIGTERM\n");
    let ss = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Stopsignal(s) => Some(s),
            _ => None,
        })
        .expect("should have STOPSIGNAL");
    assert_eq!(ss.signal, "SIGTERM");
}

#[test]
fn volume_instruction() {
    let df = parse("FROM ubuntu\nVOLUME /data /logs\n");
    let v = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Volume(v) => Some(v),
            _ => None,
        })
        .expect("should have VOLUME");
    assert!(v.paths.contains(&"/data".to_owned()));
    assert!(v.paths.contains(&"/logs".to_owned()));
}

// ── Real-world multi-stage Go Dockerfile ────────────────────────────

#[test]
fn real_world_go_multistage() {
    let input = "\
FROM golang:1.21-alpine AS builder
WORKDIR /src
COPY go.mod go.sum ./
RUN go mod download
COPY . .
RUN CGO_ENABLED=0 go build -o /app ./cmd/server

FROM alpine:3.18
RUN apk add --no-cache ca-certificates
COPY --from=builder /app /usr/local/bin/app
EXPOSE 8080
ENTRYPOINT [\"app\"]
CMD [\"serve\"]
";
    let df = parse(input);
    assert_eq!(df.stages.len(), 2);
    assert_eq!(df.stages[0].from.alias.as_deref(), Some("builder"));
    // Builder stage: WORKDIR + COPY + RUN + COPY + RUN = 5
    assert_eq!(df.stages[0].instructions.len(), 5);
    // Runtime stage: RUN + COPY + EXPOSE + ENTRYPOINT + CMD = 5
    assert_eq!(df.stages[1].instructions.len(), 5);
}

// ── Real-world multi-stage Node.js Dockerfile ───────────────────────

#[test]
fn real_world_nodejs_multistage() {
    let input = "\
FROM node:20-alpine AS deps
WORKDIR /app
COPY package.json package-lock.json ./
RUN npm ci --production

FROM node:20-alpine AS build
WORKDIR /app
COPY --from=deps /app/node_modules ./node_modules
COPY . .
RUN npm run build

FROM node:20-alpine
WORKDIR /app
ENV NODE_ENV=production
COPY --from=build /app/dist ./dist
COPY --from=deps /app/node_modules ./node_modules
EXPOSE 3000
CMD [\"node\", \"dist/index.js\"]
";
    let df = parse(input);
    assert_eq!(df.stages.len(), 3);
    assert_eq!(df.stages[0].from.alias.as_deref(), Some("deps"));
    assert_eq!(df.stages[1].from.alias.as_deref(), Some("build"));
    assert!(df.stages[2].from.alias.is_none());
}

// ── EXPOSE default protocol ─────────────────────────────────────────

#[test]
fn expose_default_protocol() {
    let df = parse("FROM ubuntu\nEXPOSE 8080\n");
    let expose = df.stages[0]
        .instructions
        .iter()
        .find_map(|i| match i {
            BuildInstruction::Expose(e) => Some(e),
            _ => None,
        })
        .expect("should have EXPOSE");
    assert_eq!(expose.ports.len(), 1);
    assert_eq!(expose.ports[0].port, 8080);
    assert_eq!(expose.ports[0].protocol, "tcp");
}
