# Security Policy

## Reporting a Vulnerability

If you discover a security vulnerability in visor, please report it responsibly.

**Email**: security@visor.rs

**Do NOT** file a public issue for security vulnerabilities.

## Supported Versions

| Version | Supported |
| ------- | --------- |
| latest  | Yes       |

## Disclosure Timeline

- **Acknowledgment**: Within 48 hours of report
- **Assessment**: Within 7 days
- **Fix (critical)**: Within 48 hours of confirmation
- **Fix (high)**: Within 7 days
- **Fix (medium)**: Within 30 days
- **Public disclosure**: 90 days after report (coordinated)

## Patch SLA

| Severity | Target       |
| -------- | ------------ |
| Critical | 48h          |
| High     | 7d           |
| Medium   | 30d          |
| Low      | Next release |

## Scope

The following are in scope:

- visor daemon (visor-runtime)
- visor-machine (VMM core)
- visor-init (guest PID 1)
- API authentication and authorization
- VM isolation boundary
- Host-guest communication (vsock)

## Self-Hosted CI Trust Boundary

Visor's KVM tests use a privileged, repository-scoped self-hosted runner. The hardware job accepts
only pushes to `main`, manual dispatches by repository collaborators, and pull requests whose head
repository is Visor itself. Pull requests from forks run the hosted checks but skip the hardware
job.

The hardware job inherits a read-only `GITHUB_TOKEN`, pins every action to an immutable commit, and
is guarded by `tests/ci_hardware_trust.sh`. That contract runs on a GitHub-hosted runner and rejects
mutable action references, `pull_request_target`, job-level permission overrides, or removal of the
same-repository check.

Do not register the KVM listener outside this repository or weaken these workflow controls. A
self-hosted runner is not an isolation boundary for untrusted pull-request code.

## Out of Scope

- Vulnerabilities in guest OS or user-provided container images
- Denial of service from within an already-authenticated session
- Issues requiring physical access to the host machine
