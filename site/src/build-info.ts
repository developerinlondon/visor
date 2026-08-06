import { GENERATED_BUILD_INFO } from './generated/build-info'

export interface SiteBindings {
  SITE_NAME?: string
  VISOR_VERSION?: string
  VISOR_COMMIT_SHA?: string
  VISOR_BUILD_TIMESTAMP?: string
  VISOR_DIRTY?: string
}

export interface SiteBuildInfo {
  siteName: string
  version: string
  versionLabel: string
  commitSha: string
  revisionLabel: string
  buildTimestamp: string
  buildTimestampLabel: string
}

export function normalizeVersion(version: string): string {
  const trimmed = version.trim().replace(/^v/i, '')
  return trimmed.length > 0 ? trimmed : GENERATED_BUILD_INFO.version
}

export function shortenCommitSha(commitSha: string): string {
  const trimmed = commitSha.trim().toLowerCase()
  return trimmed.length > 0 ? trimmed.slice(0, 7) : 'unknown'
}

export function parseDirtyFlag(dirtyFlag?: string): boolean {
  if (dirtyFlag === undefined) {
    return GENERATED_BUILD_INFO.dirty
  }

  return dirtyFlag.trim().toLowerCase() === 'true'
}

export function formatVersionLabel(version: string, dirty: boolean): string {
  return `Visor beta v${version}${dirty ? '-dev' : ''}`
}

export function formatRevisionLabel(commitSha: string, dirty: boolean): string {
  if (commitSha === 'unknown') {
    return 'Source revision unavailable'
  }

  return `Source ${commitSha}${dirty ? '-dirty' : ''}`
}

export function formatBuildTimestampLabel(timestamp: string): string {
  const date = new Date(timestamp)
  if (Number.isNaN(date.valueOf())) {
    return 'Build time unavailable'
  }

  const isoMinute = date.toISOString().slice(0, 16).replace('T', ' ')
  return `Built ${isoMinute} UTC`
}

export function createSiteBuildInfo(bindings: SiteBindings = {}): SiteBuildInfo {
  const version = normalizeVersion(bindings.VISOR_VERSION ?? GENERATED_BUILD_INFO.version)
  const commitSha = shortenCommitSha(
    bindings.VISOR_COMMIT_SHA ?? GENERATED_BUILD_INFO.commitSha
  )
  const buildTimestamp =
    bindings.VISOR_BUILD_TIMESTAMP?.trim() || GENERATED_BUILD_INFO.generatedAt
  const dirty = parseDirtyFlag(bindings.VISOR_DIRTY)

  return {
    siteName: bindings.SITE_NAME?.trim() || 'Visor',
    version,
    versionLabel: formatVersionLabel(version, dirty),
    commitSha,
    revisionLabel: formatRevisionLabel(commitSha, dirty),
    buildTimestamp,
    buildTimestampLabel: formatBuildTimestampLabel(buildTimestamp),
  }
}
