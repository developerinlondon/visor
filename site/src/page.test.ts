import { expect, test } from 'bun:test'

import { renderLandingPage } from './page'

test('renderLandingPage includes footer version metadata and provider badges', () => {
  const markup = renderLandingPage({
    siteName: 'Visor',
    version: '0.0.7',
    versionLabel: 'Visor beta v0.0.7',
    commitSha: '6a2f44d',
    revisionLabel: 'Source 6a2f44d',
    buildTimestamp: '2026-03-09T12:34:56.000Z',
    buildTimestampLabel: 'Built 2026-03-09 12:34 UTC',
  }).toString()

  expect(markup).toContain('Visor beta v0.0.7')
  expect(markup).toContain('Source 6a2f44d')
  expect(markup).toContain('Built with Rust')
  expect(markup).toContain('Hosted on Cloudflare')
  expect(markup).toContain('footer-badge-icon')
  expect(markup).toContain('macOS planned')
  expect(markup).not.toContain('Linux-First')
})
