use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;

use super::*;
use crate::dockerfile::DockerfileParser;

// ── Mock Executor ───────────────────────────────────────────────────────

struct MockBuildExecutor {
    exec_results: Mutex<Vec<(i32, String, String)>>,
    snapshot_results: Mutex<Vec<LayerSnapshot>>,
    calls: Mutex<Vec<String>>,
}

impl MockBuildExecutor {
    fn new() -> Self {
        Self {
            exec_results: Mutex::new(Vec::new()),
            snapshot_results: Mutex::new(Vec::new()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn with_exec(self, results: Vec<(i32, String, String)>) -> Self {
        *self.exec_results.lock().unwrap() = results;
        self
    }

    fn with_snapshots(self, results: Vec<LayerSnapshot>) -> Self {
        *self.snapshot_results.lock().unwrap() = results;
        self
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }
}

fn snap(n: usize) -> LayerSnapshot {
    LayerSnapshot {
        data: format!("layer_{n}_data"),
        compressed_digest: format!("sha256:compressed_{n}"),
        uncompressed_digest: format!("sha256:uncompressed_{n}"),
        compressed_size: 1024 * (n as u64 + 1),
    }
}

#[async_trait]
impl BuildExecutor for MockBuildExecutor {
    async fn overlay_init(&self, lower_dir: Option<String>) -> anyhow::Result<()> {
        let arg = lower_dir.as_deref().unwrap_or("none");
        self.calls
            .lock()
            .unwrap()
            .push(format!("overlay_init({arg})"));
        Ok(())
    }

    async fn exec(
        &self,
        cmd: &[String],
        _env: &[String],
        workdir: &str,
    ) -> anyhow::Result<(i32, String, String)> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("exec({cmd:?}, {workdir})"));
        let mut results = self.exec_results.lock().unwrap();
        if results.is_empty() {
            Ok((0, String::new(), String::new()))
        } else {
            Ok(results.remove(0))
        }
    }

    async fn snapshot_layer(&self) -> anyhow::Result<LayerSnapshot> {
        self.calls.lock().unwrap().push("snapshot_layer".to_owned());
        let mut results = self.snapshot_results.lock().unwrap();
        if results.is_empty() {
            Ok(snap(0))
        } else {
            Ok(results.remove(0))
        }
    }

    async fn flatten_overlay(&self) -> anyhow::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push("flatten_overlay".to_owned());
        Ok(())
    }

    async fn copy_to_guest(&self, host_paths: &[PathBuf], dest: &str) -> anyhow::Result<()> {
        let paths: Vec<String> = host_paths.iter().map(|p| p.display().to_string()).collect();
        self.calls
            .lock()
            .unwrap()
            .push(format!("copy_to_guest({paths:?}, {dest})"));
        Ok(())
    }

    async fn setup_mount(&self, mount: &ResolvedMount) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!(
            "setup_mount({:?}, {})",
            mount.mount_type, mount.target
        ));
        Ok(())
    }

    async fn teardown_mount(&self, mount: &ResolvedMount) -> anyhow::Result<()> {
        self.calls.lock().unwrap().push(format!(
            "teardown_mount({:?}, {})",
            mount.mount_type, mount.target
        ));
        Ok(())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn parse(content: &str) -> crate::dockerfile::ParsedDockerfile {
    DockerfileParser::parse(content).unwrap()
}

fn cfg(dockerfile: crate::dockerfile::ParsedDockerfile) -> BuildConfig {
    BuildConfig {
        dockerfile,
        build_args: HashMap::new(),
        target: None,
        no_cache: false,
        context_dir: PathBuf::from("/context"),
        tag: None,
    }
}

// ── 1. Single stage: FROM + RUN + CMD ───────────────────────────────────

#[tokio::test]
async fn single_stage_from_run_cmd() {
    let df = parse("FROM alpine\nRUN echo hello\nCMD [\"echo\", \"hi\"]");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.base_image.as_deref(), Some("alpine"));
    assert_eq!(result.layers.len(), 1);
    assert!(!result.layers[0].empty);
    assert_eq!(result.layers[0].compressed_digest, "sha256:compressed_1");
    assert_eq!(
        result.config.cmd,
        Some(vec!["echo".to_owned(), "hi".to_owned()])
    );
}

// ── 2. Multiple RUN → multiple layers ───────────────────────────────────

#[tokio::test]
async fn multiple_run_produces_multiple_layers() {
    let df = parse("FROM alpine\nRUN echo one\nRUN echo two\nRUN echo three");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1), snap(2), snap(3)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.layers.len(), 3);
    assert_eq!(result.layers[0].compressed_digest, "sha256:compressed_1");
    assert_eq!(result.layers[1].compressed_digest, "sha256:compressed_2");
    assert_eq!(result.layers[2].compressed_digest, "sha256:compressed_3");
}

// ── 3. ENV is metadata-only (no snapshot) ───────────────────────────────

#[tokio::test]
async fn env_is_metadata_only() {
    let df = parse("FROM alpine\nENV FOO=bar");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.layers.len(), 0);
    assert_eq!(
        result.config.env,
        vec![("FOO".to_owned(), "bar".to_owned())]
    );
    // No snapshot calls.
    let calls = engine.executor.calls();
    assert!(!calls.iter().any(|c| c == "snapshot_layer"));
}

// ── 4. WORKDIR is metadata-only ─────────────────────────────────────────

#[tokio::test]
async fn workdir_is_metadata_only() {
    let df = parse("FROM alpine\nWORKDIR /app");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.layers.len(), 0);
    assert_eq!(result.config.working_dir, Some("/app".to_owned()));
}

// ── 5. CMD exec form metadata ───────────────────────────────────────────

#[tokio::test]
async fn cmd_exec_form() {
    let df = parse("FROM alpine\nCMD [\"node\", \"server.js\"]");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(
        result.config.cmd,
        Some(vec!["node".to_owned(), "server.js".to_owned()])
    );
}

// ── 6. ENTRYPOINT metadata ──────────────────────────────────────────────

#[tokio::test]
async fn entrypoint_metadata() {
    let df = parse("FROM alpine\nENTRYPOINT [\"/entrypoint.sh\"]");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(
        result.config.entrypoint,
        Some(vec!["/entrypoint.sh".to_owned()])
    );
}

// ── 7. EXPOSE metadata ──────────────────────────────────────────────────

#[tokio::test]
async fn expose_metadata() {
    let df = parse("FROM alpine\nEXPOSE 8080/tcp 9090/udp");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.config.exposed_ports.len(), 2);
    assert_eq!(result.config.exposed_ports[0], (8080, "tcp".to_owned()));
    assert_eq!(result.config.exposed_ports[1], (9090, "udp".to_owned()));
}

// ── 8. LABEL metadata ───────────────────────────────────────────────────

#[tokio::test]
async fn label_metadata() {
    let df = parse("FROM alpine\nLABEL maintainer=alice version=1.0");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.config.labels.len(), 2);
    assert!(
        result
            .config
            .labels
            .contains(&("maintainer".to_owned(), "alice".to_owned()))
    );
    assert!(
        result
            .config
            .labels
            .contains(&("version".to_owned(), "1.0".to_owned()))
    );
}

// ── 9. USER metadata ────────────────────────────────────────────────────

#[tokio::test]
async fn user_metadata() {
    let df = parse("FROM alpine\nUSER nobody");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.config.user, Some("nobody".to_owned()));
}

// ── 10. SHELL metadata ──────────────────────────────────────────────────

#[tokio::test]
async fn shell_metadata() {
    let df = parse("FROM alpine\nSHELL [\"/bin/bash\", \"-c\"]");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(
        result.config.shell,
        Some(vec!["/bin/bash".to_owned(), "-c".to_owned()])
    );
}

// ── 11. Multi-stage with COPY --from ────────────────────────────────────

#[tokio::test]
async fn multi_stage_copy_from() {
    let df = parse(
        "FROM golang:1.21 AS builder\nRUN go build -o /app\n\
         FROM alpine\nCOPY --from=builder /app /app\nCMD [\"/app\"]",
    );
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1), snap(2)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    // Final stage (alpine) has 1 layer from COPY.
    assert_eq!(result.layers.len(), 1);
    assert_eq!(result.config.cmd, Some(vec!["/app".to_owned()]));
}

// ── 12. --target stops at named stage ───────────────────────────────────

#[tokio::test]
async fn target_stops_at_named_stage() {
    let df = parse(
        "FROM golang:1.21 AS builder\nRUN go build\n\
         FROM alpine AS runtime\nRUN echo runtime\n\
         FROM debian AS extra\nRUN echo extra",
    );
    let mut c = cfg(df);
    c.target = Some("runtime".to_owned());
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, c);
    let result = engine.build().await.unwrap();

    // runtime has 1 RUN, no dependencies → only runtime built.
    assert_eq!(result.layers.len(), 1);
    // Only 1 overlay_init (for runtime).
    let calls = engine.executor.calls();
    let oi: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("overlay_init"))
        .collect();
    assert_eq!(oi.len(), 1);
}

// ── 13. --build-arg substitution in RUN ─────────────────────────────────

#[tokio::test]
async fn build_arg_substitution_in_run() {
    let df = parse("FROM alpine\nARG GREETING\nRUN echo $GREETING");
    let mut c = cfg(df);
    c.build_args
        .insert("GREETING".to_owned(), "hello_world".to_owned());

    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, c);
    let result = engine.build().await.unwrap();

    assert_eq!(result.layers.len(), 1);
    let calls = engine.executor.calls();
    let exec_call = calls.iter().find(|c| c.starts_with("exec(")).unwrap();
    assert!(
        exec_call.contains("hello_world"),
        "expected substituted arg in exec call: {exec_call}"
    );
}

// ── 14. RUN failure returns error ───────────────────────────────────────

#[tokio::test]
async fn run_failure_returns_error() {
    let df = parse("FROM alpine\nRUN false");
    let mock = MockBuildExecutor::new().with_exec(vec![(1, "out".to_owned(), "err".to_owned())]);
    let engine = BuildEngine::new(mock, cfg(df));
    let err = engine.build().await.unwrap_err();

    let msg = err.to_string();
    assert!(
        msg.contains("exit code 1"),
        "error should mention exit code: {msg}"
    );
}

// ── 15. ENV variable substitution in RUN ────────────────────────────────

#[tokio::test]
async fn env_substitution_in_run() {
    let df = parse("FROM alpine\nENV NAME=world\nRUN echo $NAME");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.layers.len(), 1);
    let calls = engine.executor.calls();
    let exec_call = calls.iter().find(|c| c.starts_with("exec(")).unwrap();
    assert!(
        exec_call.contains("echo world"),
        "expected substituted env in exec: {exec_call}"
    );
}

// ── 16. Variable substitution in WORKDIR ────────────────────────────────

#[tokio::test]
async fn env_substitution_in_workdir() {
    let df = parse("FROM alpine\nENV APP_DIR=/myapp\nWORKDIR $APP_DIR");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.config.working_dir, Some("/myapp".to_owned()));
}

// ── 17. ENV accumulates across instructions ─────────────────────────────

#[tokio::test]
async fn env_accumulates() {
    let df = parse("FROM alpine\nENV FOO=bar\nENV BAZ=qux");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.config.env.len(), 2);
    assert!(
        result
            .config
            .env
            .contains(&("FOO".to_owned(), "bar".to_owned()))
    );
    assert!(
        result
            .config
            .env
            .contains(&("BAZ".to_owned(), "qux".to_owned()))
    );
}

// ── 18. COPY from context triggers copy_to_guest + snapshot ─────────────

#[tokio::test]
async fn copy_from_context() {
    let df = parse("FROM alpine\nCOPY src/ /app/");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.layers.len(), 1);
    let calls = engine.executor.calls();
    assert!(calls.iter().any(|c| c.starts_with("copy_to_guest(")));
    assert!(calls.iter().any(|c| c == "snapshot_layer"));
    assert!(calls.iter().any(|c| c == "flatten_overlay"));
}

// ── 19. Steps numbered correctly ────────────────────────────────────────

#[tokio::test]
async fn steps_numbered_correctly() {
    let df = parse("FROM alpine\nRUN echo one\nENV FOO=bar\nRUN echo two");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1), snap(2)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    // 4 steps: FROM, RUN, ENV, RUN.
    assert_eq!(result.steps.len(), 4);
    assert_eq!(result.steps[0].number, 1);
    assert_eq!(result.steps[0].total, 4);
    assert_eq!(result.steps[1].number, 2);
    assert_eq!(result.steps[1].total, 4);
    assert_eq!(result.steps[2].number, 3);
    assert_eq!(result.steps[2].total, 4);
    assert_eq!(result.steps[3].number, 4);
    assert_eq!(result.steps[3].total, 4);
    assert!(result.steps[0].instruction.starts_with("FROM"));
    assert!(result.steps[1].instruction.starts_with("RUN"));
    assert!(result.steps[2].instruction.starts_with("ENV"));
    assert!(result.steps[3].instruction.starts_with("RUN"));
}

// ── 20. Unreferenced stages skipped with --target ───────────────────────

#[tokio::test]
async fn unreferenced_stages_skipped() {
    let df = parse(
        "FROM golang:1.21 AS builder\nRUN go build\n\
         FROM node:18 AS frontend\nRUN npm build\n\
         FROM alpine AS runtime\nCOPY --from=builder /app /app",
    );
    let mut c = cfg(df);
    c.target = Some("runtime".to_owned());

    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1), snap(2)]);
    let engine = BuildEngine::new(mock, c);
    let result = engine.build().await.unwrap();

    // builder + runtime needed. frontend skipped.
    // Final result = runtime's layers (1 COPY layer).
    assert_eq!(result.layers.len(), 1);
    let calls = engine.executor.calls();
    let oi: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("overlay_init"))
        .collect();
    // 2 overlay_init calls: builder + runtime.
    assert_eq!(oi.len(), 2);
}

// ── 21. Global ARGs available for FROM substitution ─────────────────────

#[tokio::test]
async fn global_args_in_from() {
    let df = parse("ARG BASE=alpine\nFROM ${BASE}");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert!(result.steps[0].instruction.contains("alpine"));
}

// ── 22. Multiple COPY --from different stages ───────────────────────────

#[tokio::test]
async fn multiple_copy_from_different_stages() {
    let df = parse(
        "FROM golang:1.21 AS backend\nRUN go build -o /backend\n\
         FROM node:18 AS frontend\nRUN npm run build\n\
         FROM alpine\nCOPY --from=backend /backend /usr/bin/backend\n\
         COPY --from=frontend /dist /var/www",
    );
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1), snap(2), snap(3), snap(4)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    // Final stage has 2 COPY layers.
    assert_eq!(result.layers.len(), 2);
    let calls = engine.executor.calls();
    let copy_calls: Vec<_> = calls
        .iter()
        .filter(|c| c.starts_with("copy_to_guest("))
        .collect();
    assert_eq!(copy_calls.len(), 2);
}

// ── 23. STOPSIGNAL metadata ─────────────────────────────────────────────

#[tokio::test]
async fn stopsignal_metadata() {
    let df = parse("FROM alpine\nSTOPSIGNAL SIGTERM");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.config.stop_signal, Some("SIGTERM".to_owned()));
}

// ── 24. VOLUME metadata ─────────────────────────────────────────────────

#[tokio::test]
async fn volume_metadata() {
    let df = parse("FROM alpine\nVOLUME /data /logs");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.config.volumes.len(), 2);
    assert!(result.config.volumes.contains(&"/data".to_owned()));
    assert!(result.config.volumes.contains(&"/logs".to_owned()));
}

// ── 25. Global ARG overridden by --build-arg ────────────────────────────

#[tokio::test]
async fn global_arg_overridden_by_build_arg() {
    let df = parse("ARG BASE=alpine\nFROM ${BASE}");
    let mut c = cfg(df);
    c.build_args.insert("BASE".to_owned(), "debian".to_owned());

    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, c);
    let result = engine.build().await.unwrap();

    assert!(
        result.steps[0].instruction.contains("debian"),
        "FROM should use --build-arg override: {}",
        result.steps[0].instruction
    );
}

// ── 26. SHELL changes RUN execution shell ───────────────────────────────

#[tokio::test]
async fn shell_changes_run_execution() {
    let df = parse("FROM alpine\nSHELL [\"/bin/bash\", \"-c\"]\nRUN echo hello");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let _result = engine.build().await.unwrap();

    let calls = engine.executor.calls();
    let exec_call = calls.iter().find(|c| c.starts_with("exec(")).unwrap();
    assert!(
        exec_call.contains("/bin/bash"),
        "RUN should use custom shell: {exec_call}"
    );
}

// ── 27. WORKDIR relative path appended ──────────────────────────────────

#[tokio::test]
async fn workdir_relative_path_appended() {
    let df = parse("FROM alpine\nWORKDIR /app\nWORKDIR src");
    let mock = MockBuildExecutor::new();
    let engine = BuildEngine::new(mock, cfg(df));
    let result = engine.build().await.unwrap();

    assert_eq!(result.config.working_dir, Some("/app/src".to_owned()));
}

// ── 28. RUN uses current WORKDIR ────────────────────────────────────────

#[tokio::test]
async fn run_uses_current_workdir() {
    let df = parse("FROM alpine\nWORKDIR /app\nRUN make");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let _result = engine.build().await.unwrap();

    let calls = engine.executor.calls();
    let exec_call = calls.iter().find(|c| c.starts_with("exec(")).unwrap();
    assert!(
        exec_call.contains("/app"),
        "exec should use WORKDIR: {exec_call}"
    );
}

// ── 29. RUN with cache mount ────────────────────────────────────────────

#[tokio::test]
async fn run_with_cache_mount() {
    let df = parse("FROM alpine\nRUN --mount=type=cache,target=/go/pkg/mod go build");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let _result = engine.build().await.unwrap();

    let calls = engine.executor.calls();
    let setup_idx = calls
        .iter()
        .position(|c| c.contains("setup_mount(Cache"))
        .unwrap();
    let exec_idx = calls.iter().position(|c| c.starts_with("exec(")).unwrap();
    let teardown_idx = calls
        .iter()
        .position(|c| c.contains("teardown_mount(Cache"))
        .unwrap();

    assert!(setup_idx < exec_idx, "setup_mount must come before exec");
    assert!(
        exec_idx < teardown_idx,
        "teardown_mount must come after exec"
    );
    assert!(calls[setup_idx].contains("/go/pkg/mod"));
}

// ── 30. RUN with secret mount ───────────────────────────────────────────

#[tokio::test]
async fn run_with_secret_mount() {
    let df = parse(
        "FROM alpine\nRUN --mount=type=secret,id=mykey,target=/run/secrets/mykey cat /run/secrets/mykey",
    );
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let _result = engine.build().await.unwrap();

    let calls = engine.executor.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("setup_mount(Secret") && c.contains("/run/secrets/mykey"))
    );
    assert!(calls.iter().any(|c| c.contains("teardown_mount(Secret")));
}

// ── 31. RUN with bind mount ─────────────────────────────────────────────

#[tokio::test]
async fn run_with_bind_mount() {
    let df = parse("FROM alpine\nRUN --mount=type=bind,source=.,target=/src ls /src");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let _result = engine.build().await.unwrap();

    let calls = engine.executor.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("setup_mount(Bind") && c.contains("/src"))
    );
    assert!(calls.iter().any(|c| c.contains("teardown_mount(Bind")));
}

// ── 32. RUN with tmpfs mount ────────────────────────────────────────────

#[tokio::test]
async fn run_with_tmpfs_mount() {
    let df = parse("FROM alpine\nRUN --mount=type=tmpfs,target=/tmp/build echo fast");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let _result = engine.build().await.unwrap();

    let calls = engine.executor.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("setup_mount(Tmpfs") && c.contains("/tmp/build"))
    );
    assert!(calls.iter().any(|c| c.contains("teardown_mount(Tmpfs")));
}

// ── 33. RUN with multiple mounts (order matters) ────────────────────────

#[tokio::test]
async fn run_with_multiple_mounts() {
    let df = parse(
        "FROM alpine\nRUN --mount=type=cache,target=/cache --mount=type=tmpfs,target=/tmp echo hi",
    );
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let _result = engine.build().await.unwrap();

    let calls = engine.executor.calls();
    let setup_cache = calls
        .iter()
        .position(|c| c.contains("setup_mount(Cache"))
        .unwrap();
    let setup_tmpfs = calls
        .iter()
        .position(|c| c.contains("setup_mount(Tmpfs"))
        .unwrap();
    let exec_idx = calls.iter().position(|c| c.starts_with("exec(")).unwrap();
    let teardown_tmpfs = calls
        .iter()
        .position(|c| c.contains("teardown_mount(Tmpfs"))
        .unwrap();
    let teardown_cache = calls
        .iter()
        .position(|c| c.contains("teardown_mount(Cache"))
        .unwrap();

    // Setup: forward order (cache then tmpfs).
    assert!(setup_cache < setup_tmpfs);
    // Both setups before exec.
    assert!(setup_tmpfs < exec_idx);
    // Teardown: reverse order (tmpfs then cache).
    assert!(exec_idx < teardown_tmpfs);
    assert!(teardown_tmpfs < teardown_cache);
}

// ── 34. Mount teardown on RUN failure ───────────────────────────────────

#[tokio::test]
async fn mount_teardown_on_run_failure() {
    let df = parse("FROM alpine\nRUN --mount=type=cache,target=/cache false");
    let mock = MockBuildExecutor::new().with_exec(vec![(1, String::new(), "error".to_owned())]);
    let engine = BuildEngine::new(mock, cfg(df));
    let err = engine.build().await.unwrap_err();

    assert!(err.to_string().contains("exit code 1"));

    // Mounts must still be torn down even on failure.
    let calls = engine.executor.calls();
    assert!(calls.iter().any(|c| c.contains("setup_mount(Cache")));
    assert!(calls.iter().any(|c| c.contains("teardown_mount(Cache")));
}

// ── 35. Mount target variable substitution ──────────────────────────────

#[tokio::test]
async fn mount_target_variable_substitution() {
    let df = parse(
        "FROM alpine\nARG CACHE_DIR=/cache\nRUN --mount=type=cache,target=$CACHE_DIR echo hi",
    );
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let _result = engine.build().await.unwrap();

    let calls = engine.executor.calls();
    // The resolved target should be /cache, not $CACHE_DIR.
    assert!(
        calls
            .iter()
            .any(|c| c.contains("setup_mount(Cache") && c.contains("/cache")),
        "expected substituted path in setup_mount: {calls:?}"
    );
}

// ── 36. RUN with readonly mount ─────────────────────────────────────────

#[tokio::test]
async fn run_with_readonly_mount() {
    let df = parse("FROM alpine\nRUN --mount=type=bind,source=.,target=/src,readonly=true ls /src");
    let mock = MockBuildExecutor::new().with_snapshots(vec![snap(1)]);
    let engine = BuildEngine::new(mock, cfg(df));
    let _result = engine.build().await.unwrap();

    let calls = engine.executor.calls();
    assert!(
        calls
            .iter()
            .any(|c| c.contains("setup_mount(Bind") && c.contains("/src"))
    );
}
