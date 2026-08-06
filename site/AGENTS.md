# AGENTS.md — visor-site

## Purpose

`visor-site` is the marketing and positioning site for Visor.

Its job is to describe the product truthfully and clearly. It must stay aligned
with the real runtime in the repository root, especially:

- `docs/plans/19-beta-hardening-and-positioning.md`
- the current Linux-first beta scope
- the actual Docker, Compose, CLI, API, and TUI behavior shipped in code

Do not treat this site as an aspirational landing page disconnected from the
product.

## Product Truth Rules

When editing copy, always verify claims against the codebase first.

### Safe Claims Today

- Linux-first KVM microVM runtime
- OCI workload support
- Docker-compatible core workflows in beta
- Compose support for realistic Linux stacks
- native CLI, HTTP API, and TUI
- warm-pool and snapshot baseline
- real Linux networking and port forwarding

### Claims That Must Stay Qualified

- full Docker parity
- full Docker network parity
- polished multi-tenant production hardening
- buildx parity beyond the tested paths
- Kubernetes operator support
- generic VM or libvirt support
- macOS support

If a feature is incomplete, say so directly. Do not imply parity or GA maturity
that the runtime does not currently have.

## Architecture

This site is a Cloudflare Worker app built with Hono.

Current structure:

- `src/index.tsx`: main request handler and page markup
- `src/style.css`: extra CSS asset if the page is later split out
- `wrangler.toml`: Worker entry configuration
- `package.json`: local development, build, and deploy scripts

Keep the implementation simple. Prefer editing the existing single-page flow
instead of adding framework churn.

## Commands

Run from this directory.

```bash
bun install
bun run dev
bun run build
bun run deploy
```

Expected meaning:

- `bun install`: install site dependencies and sync `bun.lock`
- `bun run dev`: local Wrangler Worker dev server on port `3000`
- `bun run build`: dry-run Worker bundle build into `dist/`
- `bun run deploy`: real Wrangler deploy

## Maintenance Workflow

1. Confirm the product claim against the repository root.
2. Run `bun install` if dependencies changed or the lockfile is absent.
3. Update copy or layout in the smallest sensible change.
4. Run `bun run build`.
5. If the site copy changed meaningfully, verify that it still matches:
   - current product scope
   - current plan docs
   - current CLI/API/runtime behavior
6. Call out any gap between site claims and shipped behavior immediately.

## Editing Rules

- Prefer accurate, concrete language over broad category claims.
- Keep the Linux-first beta framing unless the product reality changes.
- Do not advertise libvirt, generic VM management, or Kubernetes operator
  support unless those are actually implemented.
- Do not reintroduce claims like FIPS, full virtual-switch maturity, or full
  Docker parity without code and validation to support them.
- Preserve the current visual language unless there is a deliberate design task.
- Avoid adding unnecessary dependencies or frontend tooling.
- Prefer Bun for package management in this repo. Do not reintroduce `npm` lock
  files unless there is a concrete compatibility reason.

## Validation

A copy or site change is only done when:

- `npm run build` passes
- `bun run build` passes
- the copy matches the shipped product surface
- no major product claim contradicts the repository root

## Coordination With The Main Repo

`visor-site` is not the source of product truth. The runtime repo is.

Before changing positioning, compare against:

- `../docs/plans/19-beta-hardening-and-positioning.md`
- `../crates/visor-runtime`
- `../crates/visor-docker`
- `../crates/visor-vmm`

If the site and product diverge, fix the site or open a plan in the main repo.
