import { describe, expect, test } from 'bun:test'

import {
  createSiteBuildInfo,
  formatBuildTimestampLabel,
  formatRevisionLabel,
  formatVersionLabel,
  normalizeVersion,
  parseDirtyFlag,
  shortenCommitSha,
} from './build-info'

describe('build info helpers', () => {
  test('normalizeVersion strips a leading v prefix', () => {
    expect(normalizeVersion('v1.2.3')).toBe('1.2.3')
  })

  test('shortenCommitSha returns a short lowercase revision', () => {
    expect(shortenCommitSha('ABCDEF123456')).toBe('abcdef1')
  })

  test('parseDirtyFlag accepts explicit false', () => {
    expect(parseDirtyFlag('false')).toBe(false)
  })

  test('formatVersionLabel marks dirty builds as dev', () => {
    expect(formatVersionLabel('0.0.7', true)).toBe('Visor beta v0.0.7-dev')
  })

  test('formatRevisionLabel marks dirty revisions', () => {
    expect(formatRevisionLabel('6a2f44d', true)).toBe('Source 6a2f44d-dirty')
  })

  test('formatBuildTimestampLabel formats UTC timestamps', () => {
    expect(formatBuildTimestampLabel('2026-03-09T12:34:56.000Z')).toBe(
      'Built 2026-03-09 12:34 UTC'
    )
  })

  test('createSiteBuildInfo honors binding overrides', () => {
    expect(
      createSiteBuildInfo({
        SITE_NAME: 'Visor',
        VISOR_VERSION: 'v1.2.3',
        VISOR_COMMIT_SHA: 'ABCDEF123456',
        VISOR_BUILD_TIMESTAMP: '2026-03-09T12:34:56.000Z',
        VISOR_DIRTY: 'false',
      })
    ).toEqual({
      siteName: 'Visor',
      version: '1.2.3',
      versionLabel: 'Visor beta v1.2.3',
      commitSha: 'abcdef1',
      revisionLabel: 'Source abcdef1',
      buildTimestamp: '2026-03-09T12:34:56.000Z',
      buildTimestampLabel: 'Built 2026-03-09 12:34 UTC',
    })
  })
})
