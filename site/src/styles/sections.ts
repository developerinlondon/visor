export const SECTION_CSS = String.raw`.bridge {
  display: flex;
  flex-direction: column;
  align-items: center;
  margin-top: 48px;
}

.bridge-arrow {
  font-size: 28px;
  color: var(--accent-text);
  margin-bottom: 20px;
  animation: bounce 2.5s ease-in-out infinite;
  line-height: 1;
}

.bridge-card {
  border: 2px solid var(--accent);
  border-radius: 12px;
  padding: 28px 40px;
  background: var(--accent-dim);
  text-align: center;
  max-width: 620px;
}

.bridge-card p {
  font-size: 18px;
  font-weight: 500;
  color: var(--text-primary);
  line-height: 1.6;
}

/* --- APPROACH Section --- */
.approach-text {
  font-size: 18px;
  line-height: 1.7;
  color: var(--text-secondary);
  max-width: 720px;
  margin-bottom: 56px;
}

.arch-diagram {
  max-width: 620px;
  margin: 0 auto 64px;
}

.arch-outer {
  border: 2px solid var(--accent);
  border-radius: 14px;
  padding: 40px 32px 28px;
  position: relative;
  background: var(--bg-card);
}

.arch-label {
  position: absolute;
  top: -13px;
  left: 24px;
  background: var(--bg-primary);
  padding: 2px 14px;
  color: var(--accent-text);
  font-size: 14px;
  font-weight: 600;
  font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', Menlo, Consolas, monospace;
  border-radius: 4px;
}

section:nth-child(even) .arch-label {
  background: var(--bg-secondary);
}

.arch-vms-row {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 14px;
  margin-bottom: 20px;
}

.arch-vm-box {
  border: 1px dashed var(--border-color-strong);
  border-radius: 8px;
  padding: 18px 12px;
  text-align: center;
  background: var(--bg-tertiary);
}

.arch-vm-dot {
  width: 8px;
  height: 8px;
  background: var(--green);
  border-radius: 50%;
  margin: 0 auto 10px;
  animation: pulse 2s ease-in-out infinite;
}

.arch-vm-box strong {
  display: block;
  font-size: 15px;
  color: var(--text-primary);
  margin-bottom: 2px;
}

.arch-vm-box small {
  font-size: 12px;
  color: var(--text-tertiary);
  font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', Menlo, Consolas, monospace;
}

.arch-bar {
  border: 1px solid var(--border-color);
  border-radius: 6px;
  padding: 12px 16px;
  text-align: center;
  font-size: 13px;
  font-weight: 500;
  color: var(--text-secondary);
  margin-top: 10px;
  background: var(--bg-tertiary);
  font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', Menlo, Consolas, monospace;
}

.approach-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 24px;
}

/* --- FEATURES Section --- */
.features-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 20px;
  margin-top: 56px;
}

/* --- COMPARISON Section --- */
.table-wrapper {
  overflow-x: auto;
  margin-top: 48px;
  border: 1px solid var(--border-color);
  border-radius: 12px;
  background: var(--bg-card);
}

.comparison-table {
  width: 100%;
  border-collapse: collapse;
  font-size: 15px;
  min-width: 720px;
}

.comparison-table th,
.comparison-table td {
  text-align: left;
  padding: 15px 20px;
  border-bottom: 1px solid var(--border-color);
}

.comparison-table thead th {
  font-weight: 600;
  font-size: 13px;
  color: var(--text-tertiary);
  background: var(--table-header-bg);
  text-transform: uppercase;
  letter-spacing: 0.06em;
}

.comparison-table tbody td {
  color: var(--text-primary);
  font-size: 14px;
}

.comparison-table tbody tr:last-child td {
  border-bottom: none;
}

.comparison-table td:first-child {
  font-weight: 600;
  color: var(--text-secondary);
  font-size: 13px;
  text-transform: uppercase;
  letter-spacing: 0.04em;
  white-space: nowrap;
}

.visor-col {
  background: var(--accent-dim) !important;
}

.comparison-table th.visor-col {
  color: var(--accent-text) !important;
}

.comparison-table td.visor-col {
  color: var(--accent-text) !important;
  font-weight: 500;
}

.table-note {
  text-align: center;
  margin-top: 24px;
  font-size: 14px;
  color: var(--text-tertiary);
  font-style: italic;
}

/* --- USE CASES Section --- */
.usecase-grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 24px;
  margin-top: 56px;
}

.usecase-card {
  background: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: 12px;
  padding: 32px;
  transition: border-color 0.3s;
  position: relative;
}

.usecase-card:hover {
  border-color: var(--accent);
}

.usecase-card::before {
  content: '';
  display: block;
  width: 10px;
  height: 10px;
  background: var(--accent-text);
  border-radius: 50%;
  margin-bottom: 20px;
}

.usecase-card h3 {
  font-size: 20px;
  font-weight: 600;
  color: var(--text-primary);
  margin-bottom: 10px;
  letter-spacing: -0.01em;
}

.usecase-card p {
  font-size: 15px;
  line-height: 1.65;
  color: var(--text-secondary);
}

/* --- GET INVOLVED Section --- */
.involved-terminal {
  max-width: 560px;
  margin-top: 48px;
  margin-bottom: 40px;
}

.involved-ctas {
  margin-bottom: 24px;
}

.involved-note {
  font-size: 14px;
  color: var(--text-tertiary);
}

/* --- Footer --- */
.footer {
  border-top: 1px solid var(--border-color);
  padding: 52px 0;
  background: var(--bg-primary);
}

.footer-inner {
  display: flex;
  flex-direction: column;
  align-items: center;
  gap: 20px;
  text-align: center;
}

.footer-logo {
  font-size: 24px;
  font-weight: 700;
  color: var(--text-primary);
  letter-spacing: -0.03em;
}

.footer-links {
  display: flex;
  gap: 24px;
}

.footer-links a {
  color: var(--text-secondary);
  text-decoration: none;
  font-size: 14px;
  font-weight: 500;
  transition: color 0.2s;
}

.footer-links a:hover {
  color: var(--text-primary);
}

.footer-badges {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: 12px;
}

.footer-badge {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  padding: 10px 14px;
  border-radius: 999px;
  border: 1px solid var(--border-color);
  background: var(--bg-card);
  color: var(--text-secondary);
  font-size: 14px;
}

.footer-badge-icon {
  width: 18px;
  height: 18px;
  display: inline-flex;
  align-items: center;
  justify-content: center;
}

.footer-badge-icon svg {
  width: 18px;
  height: 18px;
  display: block;
}

.footer-release {
  display: flex;
  flex-wrap: wrap;
  justify-content: center;
  gap: 10px;
  font-size: 13px;
  color: var(--text-tertiary);
}

.footer-release-item {
  white-space: nowrap;
}

.footer-meta {
  font-size: 13px;
  color: var(--text-tertiary);
  line-height: 1.8;
}

.footer-sep {
  opacity: 0.4;
}

/* --- Animations --- */
@keyframes fadeInUp {
  from {
    opacity: 0;
    transform: translateY(28px);
  }

  to {
    opacity: 1;
    transform: translateY(0);
  }
}

@keyframes pulse {
  0%,
  100% {
    opacity: 1;
  }

  50% {
    opacity: 0.35;
  }
}

@keyframes bounce {
  0%,
  100% {
    transform: translateY(0);
  }

  50% {
    transform: translateY(7px);
  }
}

.hero-content > * {
  animation: fadeInUp 0.7s ease both;
}

.hero-content > *:nth-child(1) {
  animation-delay: 0.05s;
}

.hero-content > *:nth-child(2) {
  animation-delay: 0.15s;
}

.hero-content > *:nth-child(3) {
  animation-delay: 0.25s;
}

.hero-content > *:nth-child(4) {
  animation-delay: 0.35s;
}

.hero-content > *:nth-child(5) {
  animation-delay: 0.45s;
}

/* Scroll reveal */
.reveal {
  opacity: 0;
  transform: translateY(28px);
  transition: opacity 0.65s ease, transform 0.65s ease;
}

.reveal.revealed {
  opacity: 1;
  transform: translateY(0);
}

.reveal-d1 {
  transition-delay: 0.08s;
}

.reveal-d2 {
  transition-delay: 0.16s;
}

.reveal-d3 {
  transition-delay: 0.24s;
}

.reveal-d4 {
  transition-delay: 0.32s;
}

.reveal-d5 {
  transition-delay: 0.4s;
}

.reveal-d6 {
  transition-delay: 0.48s;
}

/* --- Responsive --- */
@media (max-width: 1024px) {
  .features-grid,
  .approach-grid {
    grid-template-columns: repeat(2, 1fr);
  }
}

@media (max-width: 768px) {
  section {
    padding: 76px 0;
  }

  .hero {
    padding: 120px 0 76px;
    min-height: auto;
  }

  .hero h1 {
    font-size: clamp(36px, 9vw, 52px);
  }

  .features-grid,
  .approach-grid,
  .usecase-grid {
    grid-template-columns: 1fr;
  }

  .tradeoff-grid {
    grid-template-columns: 1fr;
  }

  .hero-ctas,
  .cta-group {
    flex-direction: column;
  }

  .btn {
    text-align: center;
    justify-content: center;
  }

  .arch-vms-row {
    grid-template-columns: repeat(3, 1fr);
    gap: 8px;
  }

  .arch-outer {
    padding: 36px 16px 20px;
  }

  .arch-vm-box {
    padding: 12px 6px;
  }

  .arch-vm-box strong {
    font-size: 13px;
  }

  .nav-hamburger {
    display: flex;
  }

  .nav-right {
    gap: 12px;
  }

  .nav-links {
    display: none;
    position: absolute;
    top: 64px;
    left: 0;
    right: 0;
    flex-direction: column;
    padding: 20px 24px;
    background: var(--nav-bg);
    backdrop-filter: blur(14px);
    -webkit-backdrop-filter: blur(14px);
    border-bottom: 1px solid var(--border-color);
    gap: 14px;
  }

  .nav-links.open {
    display: flex;
  }

  .bridge-card {
    padding: 24px 20px;
  }

  .footer-release {
    flex-direction: column;
    gap: 6px;
  }

  .footer-sep {
    display: none;
  }
}

@media (max-width: 480px) {
  .container {
    padding: 0 16px;
  }

  .hero h1 {
    font-size: 34px;
  }

  .section-title {
    font-size: 28px;
  }

  .terminal-body {
    font-size: 12px;
    padding: 16px;
  }

  .card {
    padding: 24px;
  }

  .arch-vms-row {
    grid-template-columns: 1fr;
  }
}`
