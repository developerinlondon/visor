export const BASE_CSS = String.raw`/* =============================================
   VISOR — Marketing Landing Page
   Precision-industrial aesthetic. Mission control.
   ============================================= */

/* --- Reset & Base --- */
*, *::before, *::after {
  box-sizing: border-box;
  margin: 0;
  padding: 0;
}

html {
  scroll-behavior: smooth;
  -webkit-text-size-adjust: 100%;
}

body {
  font-family:
    -apple-system, BlinkMacSystemFont, 'Segoe UI', 'Noto Sans', Helvetica, Arial,
    sans-serif, 'Apple Color Emoji', 'Segoe UI Emoji';
  font-size: 17px;
  line-height: 1.6;
  color: var(--text-primary);
  background-color: var(--bg-primary);
  background-image: radial-gradient(circle, var(--dot-color) 0.8px, transparent 0.8px);
  background-size: 28px 28px;
  -webkit-font-smoothing: antialiased;
  -moz-osx-font-smoothing: grayscale;
  overflow-x: hidden;
}

a {
  color: inherit;
}

img {
  max-width: 100%;
  display: block;
}

code {
  font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', Menlo, Consolas, monospace;
}

/* --- Custom Properties: Dark (Default) --- */
:root,
[data-theme='dark'] {
  color-scheme: dark;
  --bg-primary: #0a0a0f;
  --bg-secondary: #0c0c14;
  --bg-tertiary: #131320;
  --bg-card: rgba(255, 255, 255, 0.025);
  --bg-card-hover: rgba(255, 255, 255, 0.05);
  --border-color: rgba(255, 255, 255, 0.07);
  --border-color-strong: rgba(255, 255, 255, 0.14);
  --text-primary: #ededf0;
  --text-secondary: #8e8ea0;
  --text-tertiary: #5a5a70;
  --accent: #06d6a0;
  --accent-dim: rgba(6, 214, 160, 0.1);
  --accent-glow: rgba(6, 214, 160, 0.25);
  --accent-text: #06d6a0;
  --red: #ff6b6b;
  --green: #06d6a0;
  --yellow: #ffd93d;
  --cta: #06d6a0;
  --cta-glow: rgba(6, 214, 160, 0.3);
  --cta-text: #08131f;
  --code-bg: #08080d;
  --code-border: rgba(255, 255, 255, 0.05);
  --table-header-bg: rgba(255, 255, 255, 0.03);
  --table-row-alt: rgba(255, 255, 255, 0.015);
  --dot-color: rgba(255, 255, 255, 0.035);
  --shadow-sm: 0 1px 3px rgba(0, 0, 0, 0.4);
  --shadow-md: 0 4px 16px rgba(0, 0, 0, 0.5);
  --shadow-lg: 0 12px 48px rgba(0, 0, 0, 0.6);
  --nav-bg: rgba(10, 10, 15, 0.82);
}

/* --- Custom Properties: Light --- */
[data-theme='light'] {
  color-scheme: light;
  --bg-primary: #fafafa;
  --bg-secondary: #f3f3f6;
  --bg-tertiary: #e8e8ee;
  --bg-card: rgba(0, 0, 0, 0.02);
  --bg-card-hover: rgba(0, 0, 0, 0.04);
  --border-color: rgba(0, 0, 0, 0.08);
  --border-color-strong: rgba(0, 0, 0, 0.15);
  --text-primary: #1a1a2e;
  --text-secondary: #555568;
  --text-tertiary: #8888a0;
  --accent: #059669;
  --accent-dim: rgba(5, 150, 105, 0.08);
  --accent-glow: rgba(5, 150, 105, 0.18);
  --accent-text: #047857;
  --red: #dc2626;
  --green: #059669;
  --yellow: #d97706;
  --cta: #059669;
  --cta-glow: rgba(5, 150, 105, 0.22);
  --cta-text: #f4fffb;
  --code-bg: #1a1a2e;
  --code-border: rgba(0, 0, 0, 0.08);
  --table-header-bg: rgba(0, 0, 0, 0.025);
  --table-row-alt: rgba(0, 0, 0, 0.015);
  --dot-color: rgba(0, 0, 0, 0.04);
  --shadow-sm: 0 1px 3px rgba(0, 0, 0, 0.06);
  --shadow-md: 0 4px 16px rgba(0, 0, 0, 0.08);
  --shadow-lg: 0 12px 48px rgba(0, 0, 0, 0.12);
  --nav-bg: rgba(250, 250, 250, 0.82);
}

/* --- Layout --- */
.container {
  max-width: 1200px;
  margin: 0 auto;
  padding: 0 24px;
}

section {
  padding: 108px 0;
  position: relative;
}

section:nth-child(even) {
  background-color: var(--bg-secondary);
}

/* --- Typography --- */
.section-label {
  font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', 'Cascadia Code', Menlo, Consolas,
    monospace;
  font-size: 13px;
  font-weight: 600;
  color: var(--accent-text);
  text-transform: uppercase;
  letter-spacing: 0.1em;
  margin-bottom: 16px;
}

.section-title {
  font-size: clamp(30px, 5vw, 46px);
  font-weight: 700;
  line-height: 1.15;
  color: var(--text-primary);
  margin-bottom: 20px;
  letter-spacing: -0.025em;
}

.section-desc {
  font-size: 18px;
  line-height: 1.7;
  color: var(--text-secondary);
  max-width: 680px;
}

/* --- Navigation --- */
.nav {
  position: fixed;
  top: 0;
  left: 0;
  right: 0;
  z-index: 100;
  background: var(--nav-bg);
  backdrop-filter: blur(14px);
  -webkit-backdrop-filter: blur(14px);
  border-bottom: 1px solid var(--border-color);
}

.nav-inner {
  display: flex;
  align-items: center;
  justify-content: space-between;
  height: 64px;
  position: relative;
}

.nav-logo {
  font-size: 22px;
  font-weight: 700;
  color: var(--text-primary);
  text-decoration: none;
  letter-spacing: -0.03em;
}

.nav-right {
  display: flex;
  align-items: center;
  gap: 28px;
}

.nav-links {
  display: flex;
  align-items: center;
  gap: 28px;
}

.nav-links a {
  color: var(--text-secondary);
  text-decoration: none;
  font-size: 15px;
  font-weight: 500;
  transition: color 0.2s;
}

.nav-links a:hover {
  color: var(--text-primary);
}

.nav-sep {
  width: 1px;
  height: 20px;
  background: var(--border-color-strong);
}

.theme-toggle {
  display: flex;
  align-items: center;
  justify-content: center;
  width: 36px;
  height: 36px;
  border: 1px solid var(--border-color);
  border-radius: 8px;
  background: transparent;
  color: var(--text-secondary);
  cursor: pointer;
  transition: color 0.2s, border-color 0.2s;
}

.theme-toggle:hover {
  color: var(--text-primary);
  border-color: var(--border-color-strong);
}

.theme-toggle svg {
  width: 18px;
  height: 18px;
}

[data-theme='dark'] .icon-sun {
  display: block;
}

[data-theme='dark'] .icon-moon {
  display: none;
}

[data-theme='light'] .icon-sun {
  display: none;
}

[data-theme='light'] .icon-moon {
  display: block;
}

.nav-hamburger {
  display: none;
  flex-direction: column;
  justify-content: center;
  gap: 5px;
  width: 36px;
  height: 36px;
  border: none;
  background: transparent;
  cursor: pointer;
  padding: 8px;
}

.nav-hamburger span {
  display: block;
  width: 100%;
  height: 2px;
  background: var(--text-secondary);
  border-radius: 1px;
  transition: transform 0.3s, opacity 0.3s;
}

/* --- Buttons --- */
.btn {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  padding: 13px 26px;
  border-radius: 8px;
  font-size: 15px;
  font-weight: 600;
  text-decoration: none;
  cursor: pointer;
  transition: all 0.25s;
  border: none;
  font-family: inherit;
  white-space: nowrap;
}

.btn-primary {
  background: var(--cta);
  color: var(--cta-text);
}

.btn-primary:hover {
  box-shadow: 0 0 24px var(--cta-glow);
  transform: translateY(-1px);
}

.cta-group {
  display: flex;
  gap: 16px;
  flex-wrap: wrap;
}

/* --- Terminal Component --- */
.terminal {
  background: var(--code-bg);
  border: 1px solid var(--code-border);
  border-radius: 12px;
  overflow: hidden;
  box-shadow: var(--shadow-lg);
}

.terminal-header {
  display: flex;
  align-items: center;
  gap: 8px;
  padding: 14px 18px;
  border-bottom: 1px solid var(--code-border);
  background: rgba(255, 255, 255, 0.015);
}

.terminal-dot {
  width: 12px;
  height: 12px;
  border-radius: 50%;
}

.dot-red {
  background: #ff5f56;
}

.dot-yellow {
  background: #ffbd2e;
}

.dot-green {
  background: #27c93f;
}

.terminal-title {
  margin-left: 8px;
  font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', Menlo, Consolas, monospace;
  font-size: 12px;
  color: #69789c;
}

.terminal-body {
  padding: 20px 24px;
  font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', Menlo, Consolas, monospace;
  font-size: 14px;
  line-height: 1.8;
  color: #d7def1;
  overflow-x: auto;
}

.terminal-body .prompt {
  color: var(--accent-text);
}

.terminal-body .cmd {
  color: #e0e8f8;
}

.terminal-body .output {
  color: #a7b3cd;
}

.terminal-body .comment {
  color: #5f6e92;
}

.terminal-body .string {
  color: var(--yellow);
}

/* --- Badge --- */
.badge {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  padding: 7px 18px;
  background: var(--accent-dim);
  border: 1px solid var(--border-color-strong);
  border-radius: 100px;
  font-size: 13px;
  font-weight: 600;
  color: var(--accent-text);
  letter-spacing: 0.02em;
}

.badge-dot {
  width: 7px;
  height: 7px;
  background: var(--green);
  border-radius: 50%;
  animation: pulse 2s ease-in-out infinite;
}

/* --- Cards --- */
.card {
  background: var(--bg-card);
  border: 1px solid var(--border-color);
  border-radius: 12px;
  padding: 32px;
  transition: border-color 0.3s, transform 0.3s, box-shadow 0.3s;
}

.card:hover {
  border-color: var(--border-color-strong);
  transform: translateY(-2px);
  box-shadow: var(--shadow-md);
}

.card::before {
  content: '';
  display: block;
  width: 36px;
  height: 3px;
  background: var(--accent-text);
  border-radius: 2px;
  margin-bottom: 20px;
}

.card h3 {
  font-size: 19px;
  font-weight: 600;
  color: var(--text-primary);
  margin-bottom: 10px;
  letter-spacing: -0.01em;
}

.card p {
  font-size: 15px;
  line-height: 1.65;
  color: var(--text-secondary);
}

/* --- HERO Section --- */
.hero {
  position: relative;
  padding: 160px 0 108px;
  overflow: hidden;
  min-height: 100vh;
  display: flex;
  align-items: center;
}

.hero::before {
  content: '';
  position: absolute;
  top: -35%;
  left: 50%;
  transform: translateX(-50%);
  width: 150%;
  height: 75%;
  background: radial-gradient(ellipse at center, var(--accent-dim) 0%, transparent 55%);
  pointer-events: none;
  z-index: 0;
}

.hero-content {
  position: relative;
  z-index: 1;
}

.hero .badge {
  margin-bottom: 36px;
}

.hero h1 {
  font-size: clamp(42px, 7vw, 76px);
  font-weight: 700;
  line-height: 1.06;
  letter-spacing: -0.04em;
  color: var(--text-primary);
  margin-bottom: 24px;
  max-width: 900px;
}

.hero-sub {
  font-size: clamp(17px, 2vw, 20px);
  line-height: 1.65;
  color: var(--text-secondary);
  margin-bottom: 44px;
  max-width: 640px;
}

.hero-ctas {
  margin-bottom: 64px;
}

.hero .terminal {
  max-width: 580px;
  box-shadow:
    0 0 0 1px var(--code-border),
    0 24px 64px -12px rgba(0, 0, 0, 0.55),
    0 0 48px var(--accent-dim);
}

/* --- PROBLEM Section --- */
.tradeoff-grid {
  display: grid;
  grid-template-columns: 1fr 1fr;
  gap: 24px;
  margin: 48px 0;
}

.tradeoff-card {
  border: 1px solid var(--border-color);
  border-radius: 12px;
  overflow: hidden;
  background: var(--bg-card);
}

.tradeoff-header {
  padding: 18px 24px;
  font-weight: 600;
  font-size: 17px;
  border-bottom: 1px solid var(--border-color);
  color: var(--text-primary);
}

.tradeoff-body {
  padding: 24px;
}

.tradeoff-item {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 7px 0;
  font-size: 15px;
  color: var(--text-secondary);
}

.tradeoff-icon {
  flex-shrink: 0;
  width: 20px;
  font-weight: 700;
  font-size: 15px;
}

.tradeoff-item.good .tradeoff-icon {
  color: var(--green);
}

.tradeoff-item.bad .tradeoff-icon {
  color: var(--red);
}
`
