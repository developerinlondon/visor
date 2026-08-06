# Linux Beta Operations

Updated: 2026-03-09

This document defines the operator-facing runtime contract for the current
Linux beta. It is intentionally narrower than the broader architecture plans:
it describes what a Linux operator should expect from `visor start`,
`visor stop`, restart recovery, and the currently supported control surfaces.

## Runtime Flow

```text
+---------------- visor start ----------------+
| bind API | start DNS | start health/pool   |
| restore persisted VM metadata as stopped    |
+---------------------+-----------------------+
                      |
                      v
+-------------- steady state -----------------+
| run workloads | manage with CLI/API/TUI     |
| Docker/Compose supported subset             |
+---------------------+-----------------------+
                      |
          +-----------+-----------+
          |                       |
          v                       v
+-------- clean stop -------+ +-- crash / abrupt exit ---------------+
| persist running VM meta   | | next start cleans incomplete state   |
| stop live VMs             | | next start removes orphan `vsr*` TAP |
| next start restores meta  | | valid persisted VM meta restores as  |
| as stopped VMs            | | stopped VMs                          |
+---------------------------+ +--------------------------------------+
```

## Linux Beta Support Matrix

| Area                   | Beta status         | Expectations and notes                                                                                                                                                                                                   |
| ---------------------- | ------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| Host OS                | Supported           | Linux is the only host OS in the current beta promise                                                                                                                                                                    |
| Virtualization         | Required            | The beta promise assumes the current KVM-backed runtime path is usable on the host                                                                                                                                       |
| Host networking tools  | Required            | `ip` and `iptables` must be available because the Linux network backend uses TAP devices and iptables NAT/port forwarding                                                                                                |
| Daemon listener        | Supported           | `visor start` listens on `0.0.0.0:7800` by default; use `--listen` to override                                                                                                                                           |
| Background daemon logs | Supported           | Background `visor start` writes logs to `$VISOR_HOME/visor-daemon.log`, or `$HOME/.visor/visor-daemon.log` when `VISOR_HOME` is unset                                                                                    |
| Restart metadata       | Supported           | VM metadata for restart recovery lives under `$VISOR_HOME/state/<vm-id>/vm_meta.json`, or `$HOME/.visor/state/<vm-id>/vm_meta.json`                                                                                      |
| Seccomp sandbox        | Not active in beta  | The daemon reports seccomp as disabled today; sandbox code exists, but it is not applied in the shipped Linux beta                                                                                                       |
| Native lifecycle CLI   | Supported           | `start`, `stop`, `run`, `ps`, `info`, `inspect`, `logs`, `exec`, `shell`, and `console` are first-class beta surfaces                                                                                                    |
| Interactive shell      | Supported           | `visor shell` is a line-oriented REPL that runs each line through `/bin/sh -lc`; use `visor exec` for one-shot commands                                                                                                  |
| Serial console         | Partial             | `visor console` is a live serial-output surface today; do not treat it as a full bidirectional serial-console contract yet                                                                                               |
| TUI                    | Supported subset    | The TUI can inspect VMs, create them, start stopped VMs again, stop/kill/delete them, surface warm-pool status, show lifecycle events, show selected-VM logs, and launch shell or console; use CLI/API for broader flows |
| Docker / Compose       | Supported subset    | The supported subset is defined in [`docs/beta-compatibility.md`](../docs/beta-compatibility.md)                                                                                                                         |
| macOS host support     | Not in beta promise | Planned separately; not part of the current Linux beta contract                                                                                                                                                          |

## Startup

1. Run `visor start` to daemonize in the background, or `visor start --foreground` to keep the daemon in the current terminal.
2. Wait for readiness on `GET /v1/health` or by running `visor info`.
3. Use `visor ps` to inspect known VMs, including restored stopped VMs from earlier daemon runs.

Operational notes:

- On startup, the daemon attempts to remove orphan Linux TAP interfaces whose
  names start with `vsr`.
- On startup, the daemon also attempts best-effort cleanup of stale
  `visor-*` iptables rules left behind by abrupt exits.
- Persistent runtime state now resolves under `VISOR_HOME` when it is set. If
  `VISOR_HOME` is unset, the daemon uses `$HOME/.visor`. Only scratch staging
  uses `VISOR_TMPDIR` or the system temp directory.
- On startup, the daemon restores previously persisted VM metadata into the
  backend as `Stopped` entries.
- Background startup prints the daemon PID, the Swagger URL, and the log path.

## Clean Shutdown

Run `visor stop` with no VM ID to stop the daemon itself.

Current clean-shutdown behavior:

1. the daemon persists metadata for currently running VMs into
   `$VISOR_HOME/state` or `$HOME/.visor/state`
2. the daemon force-stops live VMs so TAP devices, NAT rules, and other
   host-side resources are released
3. the next `visor start` restores those persisted VMs as `Stopped` entries

This is restart recovery for VM metadata, not live-memory restore. A cleanly
stopped VM will not resume execution automatically on the next daemon start.

## Crash Recovery and Cleanup

Current crash-recovery behavior on the next daemon start:

- incomplete state directories under `$VISOR_HOME/state` or `$HOME/.visor/state`
  are removed
- valid persisted VM metadata is restored as `Stopped` entries
- orphan `vsr*` TAP interfaces are removed before the daemon starts serving
- stale Visor-tagged `iptables` rules are removed on Linux before the daemon
  starts serving

## Supported Operator Checks

Use these checks as the default operator workflow:

- `visor info`
  Confirms daemon version, uptime, pool state, health monitoring, and current
  observability capabilities.
- `visor ps`
  Shows VM state, per-VM health, CID, published ports, and creation time.
- `visor tui`
  Provides an interactive dashboard for core VM lifecycle work, logs, shell,
  and console access.
- `curl http://127.0.0.1:7800/v1/health`
  Confirms the daemon is serving requests.
- `curl http://127.0.0.1:7800/v1/metrics`
  Exposes truthful fleet-level metrics. Per-VM runtime CPU, memory, disk, and
  network metrics are not yet exported as real values.

## Known Limits

- The Linux beta promise is about Linux hosts, not cross-platform parity.
- The Linux daemon does not run under an active seccomp filter today. The code
  for sandboxing exists, but enabling it now would conflict with current
  host-tool dependencies such as `ip`, `iptables`, `truncate`, and `mke2fs`
  that are still used in normal runtime paths.
- `visor console` should be treated as a serial-output inspection surface, not
  a full serial-management contract.
- `visor shell` runs each entered line through `/bin/sh -lc` inside the guest.
  Shell syntax works, but it is still a line-oriented REPL rather than a
  persistent login-shell session.
- Native `visor run` now enables guest networking by default. Use
  `visor run --no-network ...` for hermetic or offline workloads.
- The TUI now covers the core per-VM lifecycle and interactive surfaces,
  including restarting stopped VMs and viewing selected-VM logs, but image
  management, automation, and Compose project workflows still live primarily in
  the CLI, API, Docker, and Compose surfaces.
- Per-VM runtime Prometheus counters are not yet part of the truthful beta
  observability contract.
