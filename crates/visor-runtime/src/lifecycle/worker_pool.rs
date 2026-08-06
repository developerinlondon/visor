//! Pre-fork worker pool for faster VM startup on macOS.
//!
//! [`WorkerPool`] maintains a pool of idle worker processes that have been
//! pre-forked but not yet assigned a VM. When a VM needs to boot, the pool
//! provides a ready-to-use worker, skipping the fork+exec+codesign-verify
//! overhead (~50-100ms per VM).
//!
//! # Architecture
//!
//! ```text
//! Parent (daemon)
//!   │
//!   ├─ spawn worker (pool mode) ──→ Worker 1 (idle, waiting for AssignVm)
//!   ├─ spawn worker (pool mode) ──→ Worker 2 (idle, waiting for AssignVm)
//!   └─ spawn worker (pool mode) ──→ Worker 3 (idle, waiting for AssignVm)
//!
//! When VM boots:
//!   pool.grab() ──→ Worker 1 (removed from idle list)
//!   send AssignVm(config) ──→ Worker 1 boots VM
//!   Worker 1 sends Ready ──→ Parent
//! ```
//!
//! # Pool Mode Protocol
//!
//! Workers spawned with `--pool` flag:
//! 1. Do NOT read `VmWorkerConfig` from stdin
//! 2. Connect to control socket
//! 3. Send `WorkerMessage::PoolReady { pid }`
//! 4. Wait for `ParentMessage::AssignVm(config)` on control socket
//! 5. Proceed with normal boot flow (same as non-pooled workers)

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use anyhow::Context;
use tokio::io::{AsyncBufReadExt, BufReader};

use super::worker_protocol::WorkerMessage;

#[cfg(test)]
#[path = "worker_pool_test.rs"]
mod tests;

/// A pooled worker process waiting for VM assignment.
///
/// Holds the control socket halves and child process handle.
/// When grabbed from the pool, the parent sends `AssignVm(config)` and
/// the worker proceeds with normal boot.
#[allow(dead_code)]
pub(crate) struct PooledWorker {
    /// Write half of the control socket (for sending `ParentMessage`).
    pub ctrl_write: tokio::net::unix::OwnedWriteHalf,
    /// Read half of the control socket (for receiving `WorkerMessage`).
    pub ctrl_read: BufReader<tokio::net::unix::OwnedReadHalf>,
    /// Child process handle (for kill/waitpid).
    pub child: tokio::process::Child,
    /// Worker PID (from `PoolReady` message).
    pub pid: u32,
    /// Socket path (for cleanup if needed).
    pub socket_path: PathBuf,
}

/// Pre-fork worker pool for faster VM startup.
///
/// Maintains a pool of idle worker processes. When a VM needs to boot,
/// `grab()` returns a ready-to-use worker, avoiding fork+exec overhead.
#[allow(dead_code)]
pub(crate) struct WorkerPool {
    /// Idle workers waiting for VM assignment.
    idle: tokio::sync::Mutex<Vec<PooledWorker>>,
    /// Target pool size (number of workers to maintain).
    target_size: usize,
    /// Shutdown flag (set when pool is being torn down).
    shutdown: Arc<AtomicBool>,
}

#[allow(dead_code)]
impl WorkerPool {
    /// Creates a new worker pool and spawns `target_size` idle workers.
    ///
    /// Each worker is spawned with `visor vm-worker --pool` and waits
    /// for VM assignment via the control socket.
    ///
    /// # Errors
    ///
    /// Returns an error if worker spawning fails.
    pub async fn new(target_size: usize) -> anyhow::Result<Self> {
        let pool = Self {
            idle: tokio::sync::Mutex::new(Vec::with_capacity(target_size)),
            target_size,
            shutdown: Arc::new(AtomicBool::new(false)),
        };

        // Spawn initial workers.
        for _ in 0..target_size {
            match pool.spawn_idle_worker().await {
                Ok(worker) => {
                    pool.idle.lock().await.push(worker);
                }
                Err(e) => {
                    tracing::warn!("failed to spawn pool worker: {e}");
                    // Continue spawning remaining workers even if one fails.
                }
            }
        }

        Ok(pool)
    }

    /// Grabs an idle worker from the pool, or returns `None` if empty.
    ///
    /// The returned worker is removed from the idle list and ready for
    /// VM assignment via `ParentMessage::AssignVm`.
    pub async fn grab(&self) -> Option<PooledWorker> {
        self.idle.lock().await.pop()
    }

    /// Returns the approximate number of idle workers available.
    ///
    /// This is a snapshot and may change immediately after the call.
    pub async fn available(&self) -> usize {
        self.idle.lock().await.len()
    }

    /// Shuts down the pool and kills all idle workers.
    ///
    /// Sets the shutdown flag and terminates all idle worker processes.
    pub async fn shutdown(&self) {
        self.shutdown
            .store(true, std::sync::atomic::Ordering::Release);

        let mut idle = self.idle.lock().await;
        for mut worker in idle.drain(..) {
            let _ = worker.child.kill().await;
            let _ = worker.child.wait().await;
            let _ = std::fs::remove_file(&worker.socket_path);
        }
    }

    /// Spawns a single idle worker process in pool mode.
    ///
    /// # Errors
    ///
    /// Returns an error if the worker cannot be spawned or the control
    /// socket cannot be created.
    async fn spawn_idle_worker(&self) -> anyhow::Result<PooledWorker> {
        use std::process::Stdio;

        let exe_path =
            std::env::current_exe().context("resolve current executable path for pool worker")?;

        // Create a unique control socket for this worker.
        let socket_dir = std::env::temp_dir().join(format!("visor-pool-worker-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&socket_dir).context("create pool worker socket directory")?;
        let socket_path = socket_dir.join("ctrl.sock");

        // Bind listener before spawning worker.
        let listener = tokio::net::UnixListener::bind(&socket_path)
            .context("bind pool worker control socket")?;

        // Spawn worker in pool mode (no stdin config).
        let child = tokio::process::Command::new(&exe_path)
            .arg("vm-worker")
            .arg("--pool")
            .arg(&socket_path)
            .stdin(Stdio::null())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
            .with_context(|| format!("spawn pool worker process: {}", exe_path.display()))?;

        // Wait for worker to connect to the control socket.
        let (stream, _) = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            listener.accept(),
        )
        .await
        .context("pool worker did not connect to control socket within 30s")?
        .context("accept pool worker connection")?;

        let (read_half, write_half) = stream.into_split();
        let mut ctrl_read = BufReader::new(read_half);

        // Read PoolReady message from worker.
        let mut line = String::new();
        let bytes_read = ctrl_read
            .read_line(&mut line)
            .await
            .context("read PoolReady from worker")?;

        if bytes_read == 0 {
            anyhow::bail!("pool worker closed connection before sending PoolReady");
        }

        let pool_ready: WorkerMessage = super::worker_protocol::decode_message(line.as_bytes())
            .context("decode PoolReady message")?;

        let WorkerMessage::PoolReady { pid } = pool_ready else {
            anyhow::bail!("expected PoolReady, got {pool_ready:?}");
        };

        tracing::debug!(pid, socket_path = %socket_path.display(), "spawned pool worker");

        Ok(PooledWorker {
            ctrl_write: write_half,
            ctrl_read,
            child,
            pid,
            socket_path,
        })
    }
}

impl Drop for WorkerPool {
    fn drop(&mut self) {
        // Best-effort cleanup: kill idle workers.
        // Use try_lock to avoid panicking if called inside a tokio runtime.
        let idle_workers = match self.idle.try_lock() {
            Ok(mut guard) => std::mem::take(&mut *guard),
            Err(_) => return,
        };
        for mut worker in idle_workers {
            let _ = worker.child.start_kill();
        }
    }
}
