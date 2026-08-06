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

## Out of Scope

- Vulnerabilities in guest OS or user-provided container images
- Denial of service from within an already-authenticated session
- Issues requiring physical access to the host machine
