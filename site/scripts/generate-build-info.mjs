import { execFileSync } from 'node:child_process'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join } from 'node:path'
import { fileURLToPath } from 'node:url'

const scriptDir = dirname(fileURLToPath(import.meta.url))
const siteDir = dirname(scriptDir)
const repoRoot = dirname(siteDir)
const cargoTomlPath = join(repoRoot, 'Cargo.toml')
const generatedDir = join(siteDir, 'src', 'generated')
const generatedPath = join(generatedDir, 'build-info.ts')

function readWorkspaceVersion() {
  const cargoToml = readFileSync(cargoTomlPath, 'utf8')
  const versionMatch =
    cargoToml.match(/\[workspace\.package\][\s\S]*?version\s*=\s*"([^"]+)"/)

  if (!versionMatch) {
    throw new Error(`failed to find [workspace.package] version in ${cargoTomlPath}`)
  }

  return versionMatch[1]
}

function runGit(args, fallback) {
  try {
    return execFileSync('git', args, {
      cwd: repoRoot,
      encoding: 'utf8',
      stdio: ['ignore', 'pipe', 'ignore'],
    }).trim()
  } catch {
    return fallback
  }
}

const generatedBuildInfo = {
  version: readWorkspaceVersion(),
  commitSha: runGit(['rev-parse', '--short', 'HEAD'], 'unknown'),
  dirty: runGit(['status', '--porcelain'], '') !== '',
  generatedAt: new Date().toISOString(),
}

const output = `export const GENERATED_BUILD_INFO = ${JSON.stringify(
  generatedBuildInfo,
  null,
  2
)} as const;\n`

mkdirSync(generatedDir, { recursive: true })
writeFileSync(generatedPath, output)
