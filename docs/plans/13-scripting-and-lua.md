# 13 — Scripting and Lua

> Batteries-included scripting for agent VMs via assay-lua. One crate dependency,
> zero protocol changes.

## Overview

This plan covers embedding Lua scripting in visor through the `assay-lua` crate.
The goal: replace the bash+curl+jq+python toolchains that agent VMs currently
need with a single, capable scripting runtime baked into visor-init.

The key insight is that `visor exec -- lua <script>` works through existing
infrastructure. No new protocol, no new API endpoints, no new transport. The
daemon sends the command over vsock, visor-init detects `lua` as the first arg,
and runs it in-process via the assay VM instead of fork/exec. From the outside,
it looks like any other `visor exec` call.

Why this matters:

- Agent VMs need scripting. Today that means fat images with bash, curl, jq,
  python, vault CLI, kubectl. That's 100-200 MB of tools per image.
- assay-lua provides HTTP, JSON, YAML, crypto, Vault, K8s, and 20+ other
  modules in ~5-7 MB of static binary. All running inside KVM isolation.
- No image rebuild to iterate on scripts. Lua source is just text sent over
  vsock.

## Architecture

### How Lua Fits Into visor

```
visor exec <vm> -- lua /scripts/setup.lua

+---CLI---+     +---daemon---+     +---guest VM---+
|         |     |            |     |              |
| visor   +---->| HTTP API   +---->| visor-init   |
| exec    | HTTP| /v1/vms/   |vsock| (PID 1)      |
|         |     | {id}/exec  |     |              |
+---------+     +------------+     +------+-------+
                                          |
                                   detect first arg
                                          |
                                   +------+-------+
                                   |  "lua" ?     |
                                   +--+--------+--+
                                      |        |
                                   YES|        |NO
                                      v        v
                              +-------+--+  +--+--------+
                              | assay VM |  | fork/exec |
                              | in-proc  |  | (normal)  |
                              | Lua 5.5  |  |           |
                              +----------+  +-----------+
```

visor-init already handles `visor exec` commands by forking a child process.
For Lua, it skips the fork. When the first argument is `lua`, visor-init
creates an assay Lua VM in-process and runs the script directly. Same vsock
channel, same stdout/stderr streaming, same exit code handling.

Two integration points exist:

| Integration Point | Where         | Phase | What It Does                            |
| ----------------- | ------------- | ----- | --------------------------------------- |
| Guest-side        | visor-init    | P0    | Lua scripts inside VMs via `visor exec` |
| Host-side         | visor-runtime | P2    | Lua compose files, health checks, hooks |

`assay-lua` is pulled as a regular cargo dependency of visor-init. It compiles
to a static musl binary alongside everything else. No dynamic linking, no
runtime downloads, no separate Lua installation.

## Distribution

### The Current Problem

visor's distribution story is broken. Three separate files that must be
co-located:

```
visor              ~26 MB    the daemon + CLI binary
vmlinux            ~31 MB    guest kernel (separate file)
visor-init         ~865 KB   guest PID 1 (separate file)
```

The `kernel_path()` function returns a hardcoded build directory path via
`env!("OUT_DIR")`. This works on the dev machine. Nowhere else. Users can't
just download visor and run it. They need to know where to put the kernel, how
to build visor-init, and hope the paths line up.

### Single Binary Fix

Embed everything in one binary:

```
visor binary (proposed):
  +-- visor daemon + CLI code            ~26 MB
  +-- vmlinux (include_bytes!())         ~31 MB (compressed)
  +-- visor-init (include_bytes!())      ~5-7 MB (with assay-lua)
  = total                                ~62-68 MB
```

On first run, `visor start` extracts both files to `~/.visor/bin/`:

```
~/.visor/
  bin/
    vmlinux              extracted kernel
    visor-init           extracted init binary
  cache/                 OCI image cache, snapshots
  config.toml            daemon config
```

Version detection: visor embeds its own version string. On startup, it checks
`~/.visor/bin/.version`. If the versions differ, it re-extracts. Users upgrade
by downloading a new visor binary. Next `visor start` handles the rest.

### Size Comparison

~65 MB for a complete VM runtime is competitive:

| Runtime              | Install Size | What You Get                                   |
| -------------------- | ------------ | ---------------------------------------------- |
| Docker Engine (full) | ~200 MB      | dockerd + containerd + runc + CLI              |
| Kata Containers      | ~150 MB      | runtime + agent + kernel + QEMU                |
| gVisor (runsc)       | ~50 MB       | Sandbox runtime                                |
| QEMU system          | ~50 MB       | Emulator only (no management)                  |
| visor (proposed)     | ~65 MB       | VMM + kernel + init + Lua + OCI + CLI + daemon |
| Firecracker          | ~3 MB        | VMM only (no kernel, no init, no OCI, no CLI)  |

Firecracker's 3 MB looks small until you realize it ships nothing usable. You
still need a kernel, a rootfs, an init system, an OCI pipeline, and a
management layer. Add those up and you're well past visor's total.

## User Experience

### Install

One curl, one binary, done:

```bash
curl -fsSL https://visor.dev/install.sh | sh
# or:
curl -LO https://github.com/agentx-rs/visor/releases/download/v0.1.0/visor-linux-x86_64
chmod +x visor-linux-x86_64
mv visor-linux-x86_64 /usr/local/bin/visor
```

### First Run

```
$ visor start
[visor] First run detected
[visor] Extracting vmlinux to ~/.visor/bin/vmlinux (31 MB)
[visor] Extracting visor-init to ~/.visor/bin/visor-init (5.2 MB)
[visor] Writing version marker v0.1.0
[visor] Daemon listening on /run/visor.sock
```

### Upgrade

Download the new binary. Next start re-extracts:

```
$ visor start
[visor] Version changed: v0.1.0 -> v0.2.0
[visor] Re-extracting vmlinux to ~/.visor/bin/vmlinux
[visor] Re-extracting visor-init to ~/.visor/bin/visor-init
[visor] Daemon listening on /run/visor.sock
```

### Lua in Action

```bash
# Inline Lua
visor exec <vm> -- lua -e 'log.info(http.get("http://api/health").status)'

# Script file (mounted volume or already in image)
visor exec <vm> -- lua /scripts/setup.lua

# Lua as the entrypoint for a VM
visor run alpine:3.20 lua /app/init.lua

# Pass env vars (existing visor mechanism)
visor run -e VAULT_TOKEN=hvs.xxx -e DB_URL=postgres://... alpine lua /app/deploy.lua
```

No special flags. No "enable Lua" toggle. If you say `lua`, visor-init runs
it in-process. If you say `bash`, it forks as usual.

## Environment Variables and Secrets

### Env Var Flow

Env vars already flow end-to-end through visor. No new plumbing needed for Lua.

```
+-----+     +--------+     +--------+     +------------+     +----------+
| CLI |---->| HTTP   |---->| daemon |---->| visor-init |---->| Lua VM   |
|     | -e  | API    | env | vsock  | env | setenv()   | env | env.get()|
|     | KEY | {"env":| arr | Run    | arr |            | vars|          |
|     | =   |  [...]}| ay  | Config | ay  |            |     |          |
|     | VAL |        |     |        |     |            |     |          |
+-----+     +--------+     +--------+     +------------+     +----------+
```

Three ways to set env vars:

| Method        | Example                                      |
| ------------- | -------------------------------------------- |
| CLI flag      | `visor run -e KEY=VALUE alpine lua script`   |
| CLI exec flag | `visor exec --env KEY=VALUE <vm> lua script` |
| API body      | `POST /v1/vms {"env": ["KEY=VALUE"]}`        |

Inside Lua, `env.get("KEY")` returns the value. Same environment the process
would see via `std::env::var` if it were a normal fork/exec.

### Secrets

Env vars work for non-sensitive config. For actual secrets, three options:

| Approach                   | Phase | Tradeoffs                                     |
| -------------------------- | ----- | --------------------------------------------- |
| Env vars                   | P0    | Simple, works now. Visible in /proc but guest |
|                            |       | is KVM-isolated, so only the VM sees it       |
| Vault stdlib (assay.vault) | P0    | Scripts fetch secrets directly from Vault.    |
|                            |       | Secret never in process env                   |
| visor-managed secrets      | P2    | Daemon fetches secrets, injects via vsock.    |
|                            |       | Never in process env, never on CLI            |

Recommendation: env vars for connection strings and non-sensitive config.
Vault stdlib for passwords, tokens, and credentials.

Vault usage from inside a VM:

```lua
local vault = require("assay.vault")
local v = vault.client("http://vault:8200", env.get("VAULT_TOKEN"))
local creds = v:kv_get("secret", "db/prod")

local conn = db.connect(
    "postgres://" .. creds.data.username
    .. ":" .. creds.data.password
    .. "@db:5432/app"
)
```

The VAULT_TOKEN arrives as an env var (acceptable, it's a short-lived token).
The actual database credentials never touch the process environment.

## SSH

### Not Needed

visor already provides three access methods that make SSH unnecessary:

| Method          | What It Does                    | Works On           |
| --------------- | ------------------------------- | ------------------ |
| `visor exec`    | Run commands (now with Lua too) | Images with shell  |
| `visor shell`   | Interactive shell via toybox    | ANY image, even    |
|                 |                                 | scratch/distroless |
| `visor attach`  | Serial console access           | Everything         |
| `visor console` | Alias for attach (boot logs)    | Everything         |

All four work over vsock. No network exposure. No SSH daemon running in the
guest. No SSH keys to manage, rotate, or leak.

This is a security advantage, not a limitation. No SSH means:

- No SSHD attack surface (CVEs, brute force, key theft)
- No port 22 exposed to the network
- No authorized_keys management
- No PAM configuration
- Smaller images (no openssh-server package)

If users truly need SSH for existing tooling compatibility, they install
openssh-server in their image and map port 22 like any other port. visor
doesn't block this. But for new workloads, especially agent VMs, there's no
reason to drag SSH along.

## What Lua Builtins Provide Inside VMs

### assay Core Builtins

These are compiled into the Lua VM. Always available, zero imports needed:

| Builtin     | What It Does                          | Replaces             |
| ----------- | ------------------------------------- | -------------------- |
| `http`      | HTTP client (GET, POST, PUT, etc.)    | curl, wget           |
| `json`      | JSON encode/decode                    | jq, python json      |
| `yaml`      | YAML encode/decode                    | yq, python yaml      |
| `toml`      | TOML encode/decode                    | toml CLI tools       |
| `crypto`    | SHA256, HMAC, AES, RSA                | openssl CLI          |
| `fs`        | Read/write/list files                 | cat, ls, tee, dd     |
| `base64`    | Encode/decode                         | base64 CLI           |
| `regex`     | Pattern matching                      | grep, sed, awk       |
| `db`        | SQL queries (postgres, mysql, sqlite) | psql, mysql CLI      |
| `websocket` | WebSocket client                      | wscat, websocat      |
| `template`  | Text templating                       | envsubst, sed        |
| `assert`    | Test assertions                       | test, bash `[[ ]]`   |
| `log`       | Structured logging                    | echo, printf, logger |
| `env`       | Environment variable access           | $VAR, printenv       |
| `sleep`     | Pause execution                       | sleep CLI            |
| `time`      | Timestamps, durations                 | date CLI             |
| `async`     | Concurrent execution                  | bash &, wait         |

### assay Stdlib Modules

These are Lua files embedded via `include_dir!()`. Import with `require()`:

| Module             | What It Does                         |
| ------------------ | ------------------------------------ |
| `assay.prometheus` | Query Prometheus metrics             |
| `assay.vault`      | HashiCorp Vault client (KV, transit) |
| `assay.k8s`        | Kubernetes API client                |
| `assay.argocd`     | ArgoCD application management        |
| `assay.kargo`      | Kargo stage and freight operations   |
| `assay.aws`        | AWS API (S3, EC2, IAM, STS)          |
| `assay.gcp`        | GCP API (GCS, GCE, IAM)              |
| `assay.azure`      | Azure API (Blob, VM, AAD)            |
| `assay.docker`     | Docker Engine API client             |
| `assay.git`        | Git operations                       |
| `assay.helm`       | Helm chart operations                |
| `assay.terraform`  | Terraform state and plan parsing     |
| `assay.datadog`    | Datadog metrics and events           |
| `assay.pagerduty`  | PagerDuty incident management        |
| `assay.slack`      | Slack messaging                      |
| `assay.jira`       | Jira issue operations                |
| `assay.github`     | GitHub API (repos, PRs, actions)     |
| `assay.gitlab`     | GitLab API (repos, MRs, pipelines)   |
| `assay.dns`        | DNS resolution and record queries    |
| `assay.cert`       | TLS certificate inspection           |
| `assay.jwt`        | JWT encode/decode/verify             |
| `assay.ssh`        | SSH client operations                |
| `assay.redis`      | Redis client                         |

23 modules covering the K8s-native infrastructure stack. All pure Lua built on
top of the core builtins (mostly http + json).

### Before and After

Health check (is the API up?):

```bash
# Before: bash + curl + jq
STATUS=$(curl -s http://api/health | jq -r '.status')
if [ "$STATUS" != "ok" ]; then echo "FAIL"; exit 1; fi
```

```lua
-- After: Lua
assert.equal(http.get("http://api/health").json().status, "ok")
```

Read a secret from Vault:

```bash
# Before: vault CLI (50+ MB binary)
export VAULT_ADDR=http://vault:8200
DB_PASS=$(vault kv get -field=password secret/db/prod)
```

```lua
-- After: Lua
local vault = require("assay.vault")
local v = vault.client("http://vault:8200", env.get("VAULT_TOKEN"))
local db_pass = v:kv_get("secret", "db/prod").data.password
```

Check a Kubernetes deployment:

```bash
# Before: kubectl (45+ MB binary)
READY=$(kubectl get deploy myapp -o jsonpath='{.status.readyReplicas}')
DESIRED=$(kubectl get deploy myapp -o jsonpath='{.spec.replicas}')
if [ "$READY" != "$DESIRED" ]; then echo "NOT READY"; exit 1; fi
```

```lua
-- After: Lua
local k8s = require("assay.k8s")
local client = k8s.client()
local deploy = client:get_deployment("default", "myapp")
assert.equal(deploy.status.readyReplicas, deploy.spec.replicas)
```

Query a database:

```bash
# Before: psql CLI (needs libpq, ~20 MB)
RESULT=$(PGPASSWORD=$DB_PASS psql -h db -U app -d mydb -t -c "SELECT count(*) FROM users")
```

```lua
-- After: Lua
local conn = db.connect(env.get("DB_URL"))
local count = conn:query_one("SELECT count(*) FROM users")
log.info("user count: " .. count)
```

## Binary Size Impact

### visor-init Growth

| Configuration         | Binary Size | Notes                                        |
| --------------------- | ----------- | -------------------------------------------- |
| Current visor-init    | ~865 KB     | anyhow, base64, libc, nix, serde, serde_json |
| + assay-lua (no db)   | ~4-5 MB     | adds mlua, lua-src, reqwest, rustls, crypto  |
| + assay-lua (with db) | ~5-7 MB     | adds sqlx with postgres/mysql/sqlite drivers |

### Dependency Overlap

visor and assay share a large dependency tree:

```
visor workspace:  ~400 crates
assay workspace:  ~357 crates
shared:           ~260 crates (73% overlap)

Net new crates (no db):   ~70
Net new crates (with db):  ~95
```

The overlap matters for compile time more than binary size. visor-init is a
separate static musl binary, so it pulls its own copies regardless. But the
shared crate versions mean fewer surprises in dependency resolution.

### Where the Size Lives

The ~5-7 MB increase is on disk in the init drive image. Per-VM memory cost
is near zero thanks to two mechanisms:

```
+-- Init drive (ext4 on /dev/vda) --+
|                                    |
|  /visor-init         5.2 MB       |  <-- on disk, in golden snapshot
|  /toybox             200 KB       |
|  /run.json           200 B        |
|                                    |
+------------------------------------+

                    |
                    | Golden snapshot CoW
                    v

VM-1: reads /visor-init pages --> page cache (shared)
VM-2: reads /visor-init pages --> page cache (shared, same physical pages)
VM-3: reads /visor-init pages --> page cache (shared, same physical pages)
```

Demand paging: Lua code pages load into RAM only when Lua is actually invoked.
A VM running `echo hello` never touches the Lua code paths. Those pages stay
on disk. 100 VMs that don't use Lua pay zero memory cost for it.

## Agent VM Use Case

This is the primary motivation for Lua integration.

### The Problem

Agent VMs today need a fat toolchain to be useful. An AI agent that manages
infrastructure needs to call APIs, parse JSON, read secrets, check deployments.
The typical image includes:

```
bash           ~1 MB     shell scripting
curl           ~3 MB     HTTP calls
jq             ~1 MB     JSON parsing
python3        ~50 MB    "real" scripting
vault CLI      ~50 MB    secret management
kubectl        ~45 MB    K8s operations
psql           ~20 MB    database queries
                --------
Total:         ~170 MB   of tools per image
```

Every tool call from the agent spawns a process: fork, exec, wait, parse
stdout. Slow and heavyweight for what amounts to "make an HTTP request and
read the JSON."

### The Fix

With Lua in visor-init, the agent sends `visor exec -- lua -e '...'` for each
tool call:

```
Agent                  visor daemon              VM (visor-init)
  |                        |                          |
  |  exec lua -e '...'    |                          |
  +----------------------->|                          |
  |                        | vsock: run "lua -e ..." |
  |                        +------------------------->|
  |                        |                          | in-process Lua VM
  |                        |                          | no fork, no exec
  |                        |         stdout/stderr    |
  |                        |<-------------------------+
  |   result               |                          |
  |<-----------------------+                          |
```

What changes:

| Before (fat image)                | After (Lua in visor-init)            |
| --------------------------------- | ------------------------------------ |
| 100-200 MB of CLI tools per image | ~5-7 MB in visor-init (shared CoW)   |
| fork/exec per tool call           | In-process Lua VM, no fork           |
| Image rebuild to change scripts   | Scripts are text over vsock          |
| Each tool has its own auth method | Unified Lua stdlib (same patterns)   |
| bash string parsing               | Proper types, tables, error handling |

### Faster Agent Iteration

Scripts are just text. The agent (or developer) can iterate on Lua scripts
without rebuilding the image:

```bash
# Write a script, run it immediately
visor exec <vm> -- lua -e '
    local vault = require("assay.vault")
    local k8s = require("assay.k8s")
    
    local v = vault.client("http://vault:8200", env.get("VAULT_TOKEN"))
    local creds = v:kv_get("secret", "db/prod")
    
    local client = k8s.client()
    local secret = client:create_secret("default", "db-creds", {
        username = creds.data.username,
        password = creds.data.password,
    })
    log.info("created k8s secret: " .. secret.metadata.name)
'
```

The K8s stdlib gives agents infrastructure awareness: read secrets, check
deployment status, query Prometheus metrics, trigger ArgoCD syncs. All from
inside a hardware-isolated VM.

## Upgrade Path and Versioning

### How It Works

`assay-lua` is a cargo dependency pinned in `visor-init/Cargo.toml`:

```toml
[dependencies]
assay-lua = { version = "0.5", features = ["db"] }
```

Upgrading assay means bumping the version and rebuilding visor. Users get the
new assay when they download a new visor binary. They never think about assay
versions. They think about visor versions.

```
visor v0.2.0 changelog:
  - Updated assay-lua to 0.6 (adds assay.redis module, fixes Vault KV v2)
  - ...
```

Lua stdlib modules (the `.lua` files for assay.vault, assay.k8s, etc.) are
embedded in visor-init via `include_dir!()`. They update with the binary. No
separate module installation, no package manager, no version conflicts.

### What Users See

```
visor version
visor v0.2.0 (assay-lua 0.6.0, lua 5.5)
```

That's it. One version to track.

## Implementation Roadmap

### P0: Core (Must-Have)

- [ ] Embed visor-init in visor binary via `include_bytes!()`
- [ ] Embed vmlinux kernel in visor binary (fix broken distribution)
- [ ] First-run extraction to `~/.visor/bin/` with version detection
- [ ] Add `assay-lua` dependency to visor-init `Cargo.toml`
- [ ] visor-init: detect `lua` as first arg, run in-process via assay VM
- [ ] Pass env vars through to Lua VM (existing flow, just wire it up)
- [ ] Lua stdout/stderr streaming over vsock (same as fork/exec path)
- [ ] Lua exit code propagation

### P1: Quality of Life

- [ ] visor-managed secrets (daemon fetches, injects via vsock, not in env)
- [ ] Host-side Lua in visor-runtime (Lua compose files, Lua health checks)
- [ ] `visor lua <vm>` for interactive Lua REPL over vsock
- [ ] Lua script timeout (configurable, default 5 minutes)
- [ ] Structured Lua output (JSON mode for agent consumption)

### P2: Advanced

- [ ] Lua event hooks (on VM create, destroy, exec, health check fail)
- [ ] Custom Lua modules directory (`~/.visor/lua/` for user-defined modules)
- [ ] Lua profiling and debugging (execution time, memory usage)
- [ ] Lua script caching (hash-based, skip re-send for repeated scripts)
- [ ] Host-side Lua plugins (extend visor CLI with Lua commands)

## Security

### Lua Sandbox

assay strips dangerous Lua globals before any user code runs:

| Removed Global   | Why                                          |
| ---------------- | -------------------------------------------- |
| `load`           | Arbitrary bytecode execution                 |
| `loadfile`       | Load code from filesystem                    |
| `dofile`         | Execute file as Lua code                     |
| `collectgarbage` | GC manipulation can cause DoS                |
| `print`          | Replaced by `log.*` for structured output    |
| `string.dump`    | Dump function bytecode (reverse engineering) |

### Resource Limits

| Limit             | Default | Configurable | Notes                      |
| ----------------- | ------- | ------------ | -------------------------- |
| Memory per Lua VM | 64 MB   | Yes          | OOM kills the Lua script   |
| Execution timeout | 5 min   | Yes (P1)     | Prevents infinite loops    |
| Open file handles | 256     | No           | Guest kernel limit applies |
| Network           | Guest   | No           | KVM networking isolation   |

### Isolation Layers

Lua in visor runs behind three isolation boundaries:

```
+-- Layer 1: KVM hardware isolation ----------------------------------+
|                                                                      |
|  The VM is a separate machine. Separate kernel, separate memory,    |
|  separate network. Even a complete Lua sandbox escape stays inside  |
|  the VM.                                                             |
|                                                                      |
|  +-- Layer 2: Guest kernel process isolation ----------------------+  |
|  |                                                                  |  |
|  |  visor-init runs as PID 1. Lua runs inside visor-init's         |  |
|  |  process. Standard Linux process isolation applies.              |  |
|  |                                                                  |  |
|  |  +-- Layer 3: Lua sandbox ------------------------------------+  |  |
|  |  |                                                            |  |  |
|  |  |  Dangerous globals removed. Memory capped at 64 MB.       |  |  |
|  |  |  No raw bytecode loading. No GC manipulation.              |  |  |
|  |  |  fs.read/write scoped to guest filesystem.                 |  |  |
|  |  |                                                            |  |  |
|  |  +------------------------------------------------------------+  |  |
|  +------------------------------------------------------------------+  |
+----------------------------------------------------------------------+
```

Compare this to running Lua (or Python, or Node) in a container: one kernel
compromise and the host is exposed. visor's KVM isolation means even a
worst-case Lua vulnerability, combined with a guest kernel exploit, still has
to break out of hardware virtualization. That's the same bar as escaping EC2.

### Filesystem Scope

Lua's `fs.read()` and `fs.write()` operate on the guest filesystem only.
There is no path to the host filesystem from inside the VM. The guest sees
its own rootfs (the OCI image mounted at `/`), its own `/proc`, its own
`/dev`. The host's files don't exist in the guest's universe.

## Related Plans

| Doc                                                         | Relevance                                 |
| ----------------------------------------------------------- | ----------------------------------------- |
| [00-overview](00-overview.md)                               | Single binary distribution, project goals |
| [01-architecture](01-architecture.md)                       | vsock protocol, visor-init, exec flow     |
| [03-visor-runtime](03-visor-runtime.md)                     | Daemon API, exec command handling         |
| [05-disks-and-volumes](05-disks-and-volumes.md)             | Init drive layout, volume mounting        |
| [08-roadmap](08-roadmap.md)                                 | P0/P1/P2 phasing                          |
| [09-dependencies](09-dependencies.md)                       | Cargo workspace, dependency management    |
| [11-security-and-compliance](11-security-and-compliance.md) | Security model, isolation guarantees      |
| [12-kernel-and-boot](12-kernel-and-boot.md)                 | Kernel embedding, boot sequence           |
