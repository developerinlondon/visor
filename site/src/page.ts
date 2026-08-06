import { html, raw } from 'hono/html'

import { INTERACTION_SCRIPT, THEME_BOOTSTRAP_SCRIPT } from './client-scripts'
import type { SiteBuildInfo } from './build-info'
import {
  CLOUDFLARE_ICON,
  RUST_ICON,
  THEME_MOON_ICON,
  THEME_SUN_ICON,
} from './icons'
import { SITE_CSS } from './styles'

const APPROACH_CARDS = [
  {
    title: 'In-process VMM',
    description:
      'Single host-side daemon with an embedded VMM, native CLI, HTTP API, and Docker API shim.',
  },
  {
    title: 'Snapshots + warm pool',
    description:
      'Linux snapshot restore and warm-pool foundations are in place for faster repeated acquisition.',
  },
  {
    title: 'Single binary',
    description:
      'One visor binary for daemon, CLI, API, Docker adapter, and terminal dashboard on Linux.',
  },
] as const

const FEATURE_CARDS = [
  {
    title: 'OCI + Docker workflows',
    description:
      'Run OCI images and drive Visor through <code>docker run</code>, Compose, and build flows that work today on Linux.',
  },
  {
    title: 'Per-workload kernels',
    description:
      'Each workload gets its own Linux kernel and virtual hardware on Linux/KVM instead of sharing the host kernel.',
  },
  {
    title: 'Compose support',
    description:
      'Run realistic multi-service stacks with Compose on Linux, with lifecycle and networking edges being actively tightened.',
  },
  {
    title: 'Networking baseline',
    description:
      'TAP-backed guest networking, embedded DNS, and forwarded ports work on Linux today. Advanced network parity is still being tightened.',
  },
  {
    title: 'Native control surface',
    description:
      'Use the <code>visor</code> CLI, HTTP API, Docker-compatible API, shell, exec, console, and terminal dashboard against the same daemon.',
  },
  {
    title: 'Build + image flows',
    description:
      'Pull, inspect, build, and run images through native and Docker-compatible interfaces, with remaining build parity gaps being closed.',
  },
] as const

const COMPARISON_ROWS = [
  [
    'Isolation',
    'Namespaces',
    'Separate VM process',
    'Separate VM process',
    'In-process VM threads',
  ],
  ['Kernel', 'Shared host kernel', 'Dedicated per VM', 'Dedicated per VM', 'Dedicated per VM'],
  [
    'Product focus',
    'Containers',
    'MicroVM engine',
    'Container runtime',
    'OCI workloads as microVMs',
  ],
  ['Process Model', '1 per container', '1 per VM', '3+ per VM', 'Single host daemon'],
  ['Platform', 'Linux, macOS, Win', 'Linux only', 'Linux only', 'Linux today, macOS planned'],
  [
    'Docker parity',
    'Native',
    'Manual tooling',
    'containerd/K8s focused',
    'Core flows working, full parity in progress',
  ],
] as const

const USE_CASES = [
  {
    title: 'AI Agent Execution',
    description:
      'Isolate untrusted AI-generated code in individual VMs. No shared kernel. No container escape risk. Clean VM per execution.',
  },
  {
    title: 'Multi-tenant SaaS',
    description:
      'Per-workload microVM boundaries with a compact control plane for systems that need more isolation than shared-kernel containers provide.',
  },
  {
    title: 'Secure CI/CD',
    description:
      'Clean VM per build. No shared filesystem between jobs. No leftover state from previous runs.',
  },
] as const

const FAVICON = `data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 32 32'><rect width='32' height='32' rx='6' fill='%2306d6a0'/><text x='50%25' y='50%25' text-anchor='middle' dy='.35em' font-family='system-ui' font-size='20' font-weight='700' fill='%23080d12'>V</text></svg>`

function renderCards(
  cards: ReadonlyArray<{ title: string; description: string }>,
  cardClass = 'card'
) {
  return raw(
    cards
      .map((card, index) =>
        html`<div class="${cardClass} reveal reveal-d${index + 1}">
          <h3>${card.title}</h3>
          <p>${raw(card.description)}</p>
        </div>`.toString()
      )
      .join('')
  )
}

function renderComparisonRows() {
  return raw(
    COMPARISON_ROWS.map(
      ([label, docker, firecracker, kata, visor]) =>
        html`<tr>
          <td>${label}</td>
          <td>${docker}</td>
          <td>${firecracker}</td>
          <td>${kata}</td>
          <td class="visor-col">${visor}</td>
        </tr>`.toString()
    ).join('')
  )
}

function renderFooter(buildInfo: SiteBuildInfo) {
  return html`<footer class="footer">
    <div class="container footer-inner">
      <div class="footer-logo">${buildInfo.siteName.toLowerCase()}</div>
      <div class="footer-badges">
        <div class="footer-badge">
          <span class="footer-badge-icon">${raw(RUST_ICON)}</span>
          <span>Built with Rust</span>
        </div>
        <div class="footer-badge">
          <span class="footer-badge-icon">${raw(CLOUDFLARE_ICON)}</span>
          <span>Hosted on Cloudflare</span>
        </div>
      </div>
      <div class="footer-release">
        <span class="footer-release-item">${buildInfo.versionLabel}</span>
        <span class="footer-sep">&bull;</span>
        <span class="footer-release-item">${buildInfo.revisionLabel}</span>
        <span class="footer-sep">&bull;</span>
        <time class="footer-release-item" datetime="${buildInfo.buildTimestamp}">
          ${buildInfo.buildTimestampLabel}
        </time>
      </div>
      <div class="footer-meta">Apache-2.0 + MIT</div>
    </div>
  </footer>`
}

export function renderLandingPage(buildInfo: SiteBuildInfo) {
  return html`<!DOCTYPE html>
    <html lang="en" data-theme="dark">
      <head>
        <meta charset="UTF-8" />
        <meta name="viewport" content="width=device-width, initial-scale=1.0" />
        <title>${buildInfo.siteName} - microVM runtime for OCI workloads</title>
        <meta
          name="description"
          content="Beta microVM runtime for OCI workloads, with native visor commands and Docker-compatible workflows. Runs on Linux today, with macOS planned."
        />
        <link rel="icon" href="${FAVICON}" />
        <script>
          ${raw(THEME_BOOTSTRAP_SCRIPT)}
        </script>
        <style>
          ${raw(SITE_CSS)}
        </style>
      </head>
      <body>
        <nav class="nav">
          <div class="container nav-inner">
            <a href="/" class="nav-logo">${buildInfo.siteName.toLowerCase()}</a>
            <div class="nav-right">
              <div class="nav-links" id="nav-links"></div>
              <div class="nav-sep"></div>
              <button class="theme-toggle" id="theme-toggle" aria-label="Toggle theme">
                ${raw(THEME_SUN_ICON)} ${raw(THEME_MOON_ICON)}
              </button>
              <button class="nav-hamburger" id="nav-hamburger" aria-label="Toggle menu">
                <span></span><span></span><span></span>
              </button>
            </div>
          </div>
        </nav>

        <div class="hero">
          <div class="container hero-content">
            <div class="badge">
              <span class="badge-dot"></span> Beta - Linux Available Now
            </div>
            <h1>Run OCI workloads inside Linux microVMs.</h1>
            <p class="hero-sub">
              A beta KVM runtime that runs OCI workloads in microVMs, with native visor
              commands and Docker-compatible workflows available today on Linux. macOS is
              planned.
            </p>
            <div class="hero-ctas">
              <div class="cta-group">
                <a href="#" class="btn btn-primary">Join the Waitlist -&gt;</a>
              </div>
            </div>
            <div class="terminal">
              <div class="terminal-header">
                <div class="terminal-dot dot-red"></div>
                <div class="terminal-dot dot-yellow"></div>
                <div class="terminal-dot dot-green"></div>
                <span class="terminal-title">visor</span>
              </div>
              <div class="terminal-body">
                <span class="prompt">$ </span
                ><span class="cmd"
                  >DOCKER_HOST=tcp://127.0.0.1:7800 docker run --rm alpine:latest echo </span
                ><span class="string">"hello from a microVM"</span>
                <span class="output">hello from a microVM</span>
              </div>
            </div>
          </div>
        </div>

        <section>
          <div class="container">
            <div class="reveal">
              <div class="section-label">The Problem</div>
              <h2 class="section-title">Containers share a kernel. That's the problem.</h2>
              <p class="section-desc">
                Containers are fast and developer-friendly, but they share the host kernel.
                Virtual machines solve that, but they are usually heavier and harder to
                operate. Visor closes that gap with microVM-backed OCI workloads, available
                on Linux today.
              </p>
            </div>

            <div class="tradeoff-grid reveal">
              <div class="tradeoff-card">
                <div class="tradeoff-header">Containers</div>
                <div class="tradeoff-body">
                  <div class="tradeoff-item good">
                    <span class="tradeoff-icon">✓</span> Fast startup
                  </div>
                  <div class="tradeoff-item good">
                    <span class="tradeoff-icon">✓</span> Simple workflow
                  </div>
                  <div class="tradeoff-item good">
                    <span class="tradeoff-icon">✓</span> Docker ecosystem
                  </div>
                  <div class="tradeoff-item bad">
                    <span class="tradeoff-icon">✗</span> Shared kernel
                  </div>
                  <div class="tradeoff-item bad">
                    <span class="tradeoff-icon">✗</span> Namespace escape risk
                  </div>
                  <div class="tradeoff-item bad">
                    <span class="tradeoff-icon">✗</span> No real isolation
                  </div>
                </div>
              </div>
              <div class="tradeoff-card">
                <div class="tradeoff-header">Traditional VMs</div>
                <div class="tradeoff-body">
                  <div class="tradeoff-item good">
                    <span class="tradeoff-icon">✓</span> Hardware isolation
                  </div>
                  <div class="tradeoff-item good">
                    <span class="tradeoff-icon">✓</span> Separate kernel
                  </div>
                  <div class="tradeoff-item good">
                    <span class="tradeoff-icon">✓</span> Strong security
                  </div>
                  <div class="tradeoff-item bad">
                    <span class="tradeoff-icon">✗</span> Slow boot times
                  </div>
                  <div class="tradeoff-item bad">
                    <span class="tradeoff-icon">✗</span> Complex management
                  </div>
                  <div class="tradeoff-item bad">
                    <span class="tradeoff-icon">✗</span> Heavy resource usage
                  </div>
                </div>
              </div>
            </div>

            <div class="bridge reveal">
              <div class="bridge-arrow">↓</div>
              <div class="bridge-card">
                <p>
                  Visor's approach: the workflow of containers, with the isolation of virtual
                  machines.
                </p>
              </div>
            </div>
          </div>
        </section>

        <section>
          <div class="container">
            <div class="reveal">
              <div class="section-label">The Approach</div>
              <h2 class="section-title">One daemon. VM-backed workloads.</h2>
              <p class="approach-text">
                On Linux today, Visor runs its VMM, API, CLI, and workload lifecycle in a
                single host-side daemon. That keeps the control plane compact while giving
                each workload its own kernel and virtual hardware.
              </p>
            </div>

            <div class="arch-diagram reveal">
              <div class="arch-outer">
                <div class="arch-label">visor daemon (single process)</div>
                <div class="arch-vms-row">
                  <div class="arch-vm-box">
                    <div class="arch-vm-dot"></div>
                    <strong>VM-1</strong>
                    <small>alpine</small>
                  </div>
                  <div class="arch-vm-box">
                    <div class="arch-vm-dot"></div>
                    <strong>VM-2</strong>
                    <small>nginx</small>
                  </div>
                  <div class="arch-vm-box">
                    <div class="arch-vm-dot"></div>
                    <strong>VM-3</strong>
                    <small>python</small>
                  </div>
                </div>
                <div class="arch-bar">virtual switch</div>
                <div class="arch-bar">REST + SSE API</div>
              </div>
            </div>

            <div class="approach-grid">${renderCards(APPROACH_CARDS)}</div>
          </div>
        </section>

        <section>
          <div class="container">
            <div class="reveal">
              <div class="section-label">Capabilities</div>
              <h2 class="section-title">Beta workflows that already exist.</h2>
              <p class="section-desc">
                The beta story is about real, test-backed flows on Linux today, while being
                explicit about the parity work still in flight and macOS support still to
                come.
              </p>
            </div>

            <div class="features-grid">${renderCards(FEATURE_CARDS)}</div>
          </div>
        </section>

        <section>
          <div class="container">
            <div class="reveal">
              <div class="section-label">Comparison</div>
              <h2 class="section-title">Where Visor fits.</h2>
            </div>

            <div class="table-wrapper reveal">
              <table class="comparison-table">
                <thead>
                  <tr>
                    <th>Aspect</th>
                    <th>Docker</th>
                    <th>Firecracker</th>
                    <th>Kata Containers</th>
                    <th class="visor-col">Visor</th>
                  </tr>
                </thead>
                <tbody>
                  ${renderComparisonRows()}
                </tbody>
              </table>
            </div>

            <p class="table-note reveal">
              Visor is currently in beta, runs on Linux today, and is not a libvirt
              replacement or a finished cross-platform VM platform.
            </p>
          </div>
        </section>

        <section>
          <div class="container">
            <div class="reveal">
              <div class="section-label">Use Cases</div>
              <h2 class="section-title">Built for workloads that need isolation.</h2>
            </div>

            <div class="usecase-grid">${renderCards(USE_CASES, 'usecase-card')}</div>
          </div>
        </section>

        <section>
          <div class="container">
            <div class="reveal">
              <div class="section-label">Get Involved</div>
              <h2 class="section-title">Get involved early.</h2>
            </div>

            <div class="involved-terminal reveal">
              <div class="terminal">
                <div class="terminal-header">
                  <div class="terminal-dot dot-red"></div>
                  <div class="terminal-dot dot-yellow"></div>
                  <div class="terminal-dot dot-green"></div>
                  <span class="terminal-title">terminal</span>
                </div>
                <div class="terminal-body">
                  <span class="comment"># Start the Linux daemon</span>
                  <span class="prompt">$ </span
                  ><span class="cmd">visor start --listen 127.0.0.1:7800</span>

                  <span class="comment"># Point Docker at visor</span>
                  <span class="prompt">$ </span
                  ><span class="cmd"
                    >DOCKER_HOST=tcp://127.0.0.1:7800 docker run --rm alpine:latest echo "hello
                    from a microVM"</span
                  >
                </div>
              </div>
            </div>

            <div class="involved-ctas reveal">
              <div class="cta-group">
                <a href="#" class="btn btn-primary">Join the Waitlist -&gt;</a>
              </div>
            </div>

            <p class="involved-note reveal">
              Visor is in beta and runs on Linux today. Core workflows are real now; macOS
              and broader parity are still being built.
            </p>
          </div>
        </section>

        ${renderFooter(buildInfo)}

        <script>
          ${raw(INTERACTION_SCRIPT)}
        </script>
      </body>
    </html>`
}
