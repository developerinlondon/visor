# 19 — Beta Hardening and Product Positioning

| Field        | Value                                                                                               |
| ------------ | --------------------------------------------------------------------------------------------------- |
| Status       | Beta-cut execution complete; WS1-WS5 complete                                                       |
| Created      | 2026-03-08                                                                                          |
| Dependencies | Plans 18, 17, 08, 04, 03, 01                                                                        |
| Scope        | Linux-first beta, Docker/Compose/build parity, control-surface polish, truthful product messaging   |
| Goal         | Turn the current Linux-first baseline into a defensible beta product and align marketing to reality |

## Problem Statement

visor now has a real Linux KVM baseline:

- OCI images boot as microVM-backed workloads.
- Docker CLI compatibility works for core flows.
- Compose works for realistic multi-service stacks.
- Native `visor` CLI, shell, exec, and console are usable.

That is enough for a serious beta, but not enough for a polished broad-market
launch.

The current gap is not only technical. It is product-definition drift:

- the runtime delivers a Linux-first microVM container engine,
- some roadmap items are still future work,
- the marketing site currently describes a broader and smoother product than the
  branch actually ships.

This plan closes that gap by pairing engineering hardening with product
positioning.

## Product Truth

### What Visor Is Today

- Linux-first KVM microVM runtime for OCI-style workloads
- single host-side daemon embedding the VMM
- native CLI, HTTP API, Docker API shim, and TUI
- warm-pool and snapshot baseline
- real Linux guest networking, DNS, and forwarded ports

### What Visor Is Not Yet

- libvirt-compatible VM manager
- general-purpose desktop/server VM platform
- full Docker Engine parity
- full Docker network parity
- production-hardened multi-tenant control plane
- Kubernetes operator product

## Beta Definition

visor beta should mean:

- a Linux user can install it, start the daemon, and run daily Docker-style
  workflows against it
- the major advertised flows are verified end to end
- the site, docs, and CLI all describe the same product
- known limitations are explicit rather than implied away

It should not mean:

- broad hypervisor-platform parity
- libvirt replacement
- complete Kubernetes integration
- finished macOS support

## Architecture Frame

```text
+------------------- Shippable Beta --------------------+
| Linux KVM | OCI | Docker CLI | Compose | shell/exec  |
| build     | warm pool | API | TUI | port forwarding |
+--------------------------+----------------------------+
                           |
                           v
+------------------- Follow-on Expansion ---------------+
| Docker network parity | buildx load | daemon sandbox |
| operator | generic VM workflows | broader platform   |
+-------------------------------------------------------+
```

## Process Model Decision

We should weigh the current single-daemon design against a process-per-VM
design explicitly.

### Current Direction

Recommendation for beta:

- keep the current single host-side daemon and embedded VMM
- harden it rather than split it prematurely
- preserve clean boundaries so per-VM worker processes remain possible later

### Tradeoff Summary

| Model              | Pros                                                                  | Cons                                                               |
| ------------------ | --------------------------------------------------------------------- | ------------------------------------------------------------------ |
| Single host daemon | simpler architecture, lower overhead, fewer moving parts, faster IPC  | larger blast radius, weaker isolation, daemon sandboxing is harder |
| Process per VM     | stronger fault isolation, cleaner security boundaries, easier jailing | higher orchestration complexity, more IPC, more resource overhead  |

### Beta Call

For the Linux-first beta, the right move is to keep the current process model.
The biggest gaps today are Docker parity, networking parity, operational
hardening, and product polish. Splitting the runtime into per-VM workers before
those are stable would slow down delivery and make the product harder to
reason about.

### Follow-On Trigger

Revisit process-per-VM once the beta baseline is stable and one of these is
true:

- daemon sandboxing remains too weak for the target customer profile
- operational blast radius becomes a real support problem
- multi-tenant requirements demand stronger host-side fault isolation

## Execution Status

| Workstream | Status   | Spent  | Remaining | Notes                                                                                                         |
| ---------- | -------- | ------ | --------- | ------------------------------------------------------------------------------------------------------------- |
| WS1        | Complete | ~8-10h | 0h        | Docker beta-cut flows and support-boundary docs are in place                                                  |
| WS2        | Complete | ~5-6h  | 0h        | Compose isolation, reachability, lifecycle coverage, and cleanup are in place                                 |
| WS3        | Complete | ~5-6h  | 0h        | CLI visibility, shell/console ergonomics, and first-class TUI VM workflows landed                             |
| WS4        | Complete | ~4-5h  | 0h        | truthful metrics, clean-stop recovery, ops docs, seccomp decision, and explicit persistent-path policy landed |
| WS5        | Complete | ~3-4h  | 0h        | Site and messaging were aligned and deployed                                                                  |

## Current Focus

1. treat WS1-WS5 as the current beta baseline unless a new regression appears
2. move new scope into the follow-on plans instead of re-opening the beta cut implicitly

## Latest Validation

- `cargo test -p visor-docker --quiet`
- `cargo test -p visor-vmm muxer_drop_removes_listener_socket_path --quiet`
- `cargo test --test e2e_docker docker_smoke_matrix_covers_run_exec_logs_stop_rm_and_build -- --exact --quiet`
- `cargo test --test e2e_docker docker_compose_projects_are_isolated_and_reachable -- --exact --quiet`
- `cargo test --test e2e_docker docker_compose_lifecycle_covers_logs_stop_and_start -- --exact --quiet`
- `cargo test --test e2e_docker docker_buildx_load_imports_image_into_visor -- --exact --quiet`
- `cargo test -p visor-runtime cli:: --quiet`
- `cargo test -p visor-runtime get_info_returns_system_info --quiet`
- `cargo test -p visor-runtime includes_all_metric_help_and_type_headers --quiet`
- `cargo test -p visor-runtime runtime_vm_metrics_availability_is_explicit_and_placeholder_metrics_are_absent --quiet`
- `cargo test -p visor-runtime clean_shutdown_sequence_persists_metadata_before_vm_teardown --quiet`
- `cargo test -p visor-runtime console_ --quiet`
- `cargo check -p visor-runtime --quiet`
- `cargo test -p visor-runtime --quiet`
- focused reruns leave no `vsr*` interfaces, no `visor-*` iptables rules, and no stale `/var/run/visor/vsock/*.sock`

## Workstreams

### WS1 — Docker Parity Hardening

Objective: make the advertised Docker workflows reliable enough for beta.

- [x] fix `docker buildx build --load` so built images are imported back into
      Visor reliably
- [x] fix interactive TTY behavior for `docker exec -it`
- [x] add a Docker smoke matrix that covers:
  - `docker run`
  - `docker exec`
  - `docker logs`
  - `docker stop`
  - `docker rm`
  - `docker build`
  - `docker buildx build`
- [x] document supported versus unsupported Docker API areas

Acceptance:

- the supported Docker commands behave consistently on a clean Linux host
- unsupported commands fail clearly rather than hanging or pretending to work

### WS2 — Compose and Networking Beta Readiness

Objective: make the Compose story credible for real developer workflows.

- [x] add realistic Compose end-to-end coverage beyond the current smoke file
- [x] tighten Compose lifecycle coverage for:
  - `logs`
  - `stop`
  - `start`
- [x] validate project-scoped `up`, `ps`, `exec`, and `down`
- [x] finish the most important Docker network parity gaps
- [x] define the beta contract for multi-project isolation and service
      discovery
- [x] publish known networking limits explicitly

Acceptance:

- two independent Compose projects can run concurrently on Linux
- service-name resolution and host port reachability are both test-covered

### WS3 — Native Control Surface Polish

Objective: make `visor` feel like a complete product, not only an engine.

- [x] audit native CLI coverage against the product story
- [x] tighten `visor shell`, `visor exec`, and `visor console` ergonomics
- [x] keep TUI and CLI both first-class for core VM workflows
- [x] expose network, pool, and health state clearly in CLI and API output
- [x] decide what must be first-class in TUI for beta versus later

Acceptance:

- native `visor` commands cover the core Linux-first workflows cleanly
- operators can inspect current state without dropping into ad hoc debug steps

### WS4 — Daemon Hardening and Operational Credibility

Objective: reduce the gap between a working engine and a marketable runtime.

- [x] revisit seccomp/self-sandboxing for the single-process daemon
- [x] remove or isolate remaining long-lived temp-root and shutdown edge cases
- [x] ensure pool, health, and metrics reflect real state
- [x] add operational documentation for startup, shutdown, recovery, and cleanup
- [x] define the beta support matrix:
  - kernel/KVM expectations
  - supported CLI surfaces
  - known limitations

Acceptance:

- the daemon starts, runs, and shuts down predictably on Linux
- the operational story is documented, not tribal knowledge

### WS5 — Product Positioning and Site Alignment

Objective: make public messaging match the shipped product.

- [x] update the marketing site to describe the current beta scope
- [x] remove or soften claims that imply:
  - full Docker parity
  - full virtual switch maturity
  - generic VM/libvirt support
  - shipped macOS support
- [x] emphasize the real differentiators:
  - microVM isolation
  - single binary
  - Docker/Compose compatibility baseline
  - native CLI/API/TUI
  - warm-pool foundation
- [x] add an explicit "current beta scope" message

Acceptance:

- a technical buyer reading the site will form the same product picture that an
  engineer gets from the codebase

## Follow-On Plans After Beta

These items are needed, but they are not prerequisites for the initial
Linux-first beta claim:

### FP1 — Kubernetes Operator

Use the existing HTTP API as the control-plane substrate for:

- CRDs for VM-backed workloads
- reconciliation against daemon state
- status/health propagation
- image, network, and lifecycle orchestration

Exit condition:

- operator exists as a real workspace crate and deployable control plane

### FP2 — Builder Guests, Then Broader VM Expansion

Split the post-beta VM work into explicit tracks so we do not conflate them:

- FP2A: nested-KVM builder guests for image-building workloads inside selected
  Visor VMs, matching the practical Cloud Hypervisor-style `/dev/kvm` builder
  use case
- FP2B: generic VM workflows using explicit kernels, disks, and machine configs
- FP2C: libvirt integration or compatibility shims if the product boundary
  widens that far

Current recommendation:

- treat FP2A as the primary near-term goal because it directly supports
  builder workloads inside Visor VMs without requiring host-side libvirt
  compatibility
- keep FP2B and FP2C separate until the product boundary is intentionally
  widened

Current status:

- FP2A is now complete on Linux `x86_64`: selected Visor VMs can be launched
  with nested virtualization enabled, the guest kernel includes KVM support,
  automated tests cover the config/plumbing, and a real smoke test confirmed
  `/dev/kvm` is present inside a nested builder guest and can be used for a
  package-mirror-backed `qemu-img` workflow inside the VM
- FP2B and FP2C remain follow-on expansion work, not part of this completed
  slice

## Sequencing

1. harden Docker parity and interactive control paths
2. deepen Compose and networking coverage
3. polish native CLI/TUI/API control surfaces
4. harden daemon operations and publish support boundaries
5. align the site and outward messaging
6. only then spin out operator and broader VM expansion plans

## Exit Criteria

- Linux-first beta scope is clearly defined and honestly marketable
- core Docker, Compose, and native `visor` workflows are stable and test-backed
- site and docs reflect the shipped product accurately
- the next expansion items are planned without being conflated with beta

## Execution Status

Updated: 2026-03-10

Tracking note:

- elapsed effort below is rough active AI-agent time on this branch, not a
  human-team estimate
- we did not keep per-workstream timers from the start, so completed time is
  approximate
- remaining estimates are for this repo and current failing tests, not generic
  industry estimates

### Current Summary

| WS  | Status   | Approx time spent | Approx remaining | Notes                                                                                       |
| --- | -------- | ----------------- | ---------------- | ------------------------------------------------------------------------------------------- |
| WS1 | Complete | 8-10h             | 0h               | Docker beta-cut flows landed and are regression-covered                                     |
| WS2 | Complete | 5-6h              | 0h               | Compose isolation, lifecycle, and cleanup coverage landed                                   |
| WS3 | Complete | 5-6h              | 0h               | `visor info`, `visor ps`, shell/console ergonomics, and first-class TUI VM workflows landed |
| WS4 | Complete | 4-5h              | 0h               | truthful metrics, clean-stop recovery, ops docs, seccomp decision, and path policy landed   |
| WS5 | Complete | 3-4h              | 0h               | Site aligned and deployed                                                                   |

### Current Working Set

- WS3 local changes now make `visor info` expose warm-pool, health, and
  runtime-metrics state clearly.
- WS3 local changes now make `visor ps` show per-VM health, CID, and published
  ports directly in the default table output.
- WS3 local changes now make `visor console --escape-key ...` honor the
  configured detach sequence on a real terminal.
- WS3 local changes now run each `visor shell` line through `/bin/sh -lc`, so
  quoting, pipes, redirects, and conditionals work predictably.
- WS3 local changes now let the TUI launch shell and console directly while
  keeping create, stop, kill, delete, and logs in the same primary surface.
- WS4 local changes now remove zero-filled placeholder Prometheus series and
  replace them with an explicit `visor_vm_runtime_metrics_available 0` gauge,
  plus matching `/v1/info` capability reporting.
- WS4 local changes now persist running VM metadata before clean daemon
  shutdown tears live VMs down, so restart recovery matches the documented
  operator story.
- WS4 local changes now keep durable runtime state under `VISOR_HOME` or
  `$HOME/.visor` instead of silently drifting into `/tmp`; only scratch staging
  continues to use `VISOR_TMPDIR` or the system temp directory.
- WS4 documentation now lives in
  [`docs/linux-beta-operations.md`](../../docs/linux-beta-operations.md)
  and defines the current Linux support matrix and operator workflow.
- WS4 review confirmed that seccomp should remain a follow-on item for this
  daemon design until runtime paths stop depending on host helper commands like
  `ip`, `iptables`, `truncate`, and `mke2fs`.
- All beta-cut workstreams are now complete. New work should be tracked as
  follow-on expansion or as regressions against the documented beta contract.

### WS1 - Docker Parity Hardening

Status: complete

Completed:

- [x] `docker buildx build --load` image import path landed
- [x] `docker exec -it` TTY path landed
- [x] stdin-attached non-TTY exec now returns stdout correctly
- [x] Docker smoke matrix covers `run`, `exec`, `logs`, `stop`, `rm`, `build`
- [x] pulled-image boot path fixed
- [x] document supported versus unsupported Docker API areas

Shipped commits:

- `d769c02` `feat: support docker buildx load workflows`
- `92859ea` `feat: support docker exec tty sessions`
- `ba207be` `feat: harden docker buildx load and exec flows`
- `9f1de21` `fix: boot pulled images from cached metadata`

Current remaining work:

- keep [`docs/beta-compatibility.md`](../../docs/beta-compatibility.md)
  aligned if the Docker beta contract changes later

### WS2 - Compose and Networking Beta Readiness

Status: complete

Completed:

- [x] Compose project-scoped service discovery landed
- [x] shorter generated VM names landed
- [x] stronger Compose end-to-end coverage has been added locally
- [x] host/service-discovery beta contract is written down
- [x] known networking limits are published

Shipped commits:

- `01106a3` `feat: scope docker compose service discovery by project`
- `9a13a8d` `feat: shorten generated VM names`

Current remaining work:

- keep [`docs/beta-compatibility.md`](../../docs/beta-compatibility.md)
  aligned if the Compose beta contract changes later

### WS3 - Native Control Surface Polish

Status: complete for the beta cut

Completed locally:

- [x] `visor info` now surfaces warm-pool, health, and observability capability
      state in a human-readable form
- [x] `visor ps` now surfaces VM health, CID, and published ports by default
- [x] `visor console` now detaches on the configured escape key instead of only
      relying on Ctrl-C
- [x] `visor shell` now runs each entered line through `/bin/sh -lc`, so shell
      syntax works predictably
- [x] the TUI now keeps create, stop, kill, delete, logs, shell, and console
      together as first-class VM workflows

Current remaining work:

- keep the runtime docs aligned if the native control-surface contract changes
  later

### WS4 - Daemon Hardening and Operational Credibility

Status: complete for the beta cut

Completed locally:

- [x] `/v1/metrics` now exports only truthful daemon and fleet-level metrics
- [x] `/v1/info` now explicitly reports that per-VM runtime metrics are not yet
      available
- [x] clean daemon shutdown now persists restart metadata before live VM
      teardown
- [x] Linux beta operations and support-matrix documentation now exist
- [x] seccomp follow-on decision is documented against current host-tool
      dependencies
- [x] durable runtime state now resolves under `VISOR_HOME` or `$HOME/.visor`
      instead of silently drifting into `/tmp`

Current remaining work:

- no beta-cut implementation work remains here; treat new daemon-ops bugs as
  regressions against the documented Linux beta contract

### WS5 - Product Positioning and Site Alignment

Status: complete for the beta cut

Completed:

- [x] site copy aligned to beta reality
- [x] version/build stamp added
- [x] footer and icon cleanup shipped
- [x] oversized site entrypoint modularized
- [x] final deployed color theme restored to green

Shipped commits:

- `870aba5` `feat: align beta positioning and visor-init discovery`
- `6a2f44d` `feat: vendor marketing site into monorepo`
- `267e8e5` `feat(site): refresh beta landing page`

## Execution Order Correction

What happened:

- WS5 was pulled forward because the live site was user-visible and you asked
  for immediate copy, theme, and deploy fixes
- WS1 and WS2 then overlapped because Compose failures initially looked like
  networking/runtime failures and needed isolation before changing code
- the beta-cut workstreams are now closed; new work should move into follow-on
  plans instead of re-opening this execution track implicitly

What we should do now:

1. treat WS1-WS5 as complete for the beta cut
2. use FP1, FP2, or a new follow-on plan for broader product expansion
3. keep the operator docs aligned if the support boundary changes

## Next Beta-Cut Scope

No immediate beta-cut items remain.

Immediate next items:

- [x] finish the next WS3 shell, exec, or console ergonomics slice
- [x] decide whether temp-root cleanup needs beta-cut implementation
- [x] audit remaining shutdown-edge behavior beyond clean-stop metadata persistence
- [x] keep [`docs/linux-beta-operations.md`](../../docs/linux-beta-operations.md) aligned with code truth

Defer for now:

- [ ] broad WS3 CLI/TUI polish
- [ ] broad WS4 daemon hardening beyond cleanup and accurate docs

Recent post-beta follow-on progress:

- [x] wire the TUI events pane to the daemon SSE stream instead of leaving it
      test-only
- [x] wire the TUI pool metric to `/v1/pool` so warm-pool counts reflect the
      running daemon rather than staying at `0/0`
- [x] add a real backend/API start path for stopped VMs and surface it through
      the TUI lifecycle key
- [x] split the TUI full-screen logs surface away from daemon events so `l`
      shows selected-VM stdout/stderr instead of relabeling the SSE event
      buffer
- [x] scrub stale `visor-*` iptables rules on Linux daemon startup so abrupt
      exits do not leave tagged firewall state behind
- [x] execute the true multi-network Compose dataplane follow-on in
      [`20-compose-multi-network-dataplane.md`](./20-compose-multi-network-dataplane.md)
