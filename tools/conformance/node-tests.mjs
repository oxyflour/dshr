import { readdirSync } from 'node:fs'
import { join } from 'node:path'

function sidecarTestFiles(root, directory) {
  return readdirSync(join(root, directory), { withFileTypes: true }).flatMap((entry) => {
    const path = `${directory}/${entry.name}`
    if (entry.isDirectory() && entry.name !== 'node_modules') return sidecarTestFiles(root, path)
    return entry.isFile() && /\.test\.(mjs|js|cjs)$/.test(entry.name) ? [path] : []
  })
}

export function nodeTestArgs(root) {
  const args = ['--test']
  // Avoid child-process isolation where supported, including restricted local environments.
  if (process.allowedNodeEnvironmentFlags.has('--test-isolation')) args.push('--test-isolation=none')
  const testFiles = sidecarTestFiles(root, 'sidecar').sort()
  if (!testFiles.length) throw new Error('No Sidecar test files were discovered')
  return [...args, ...testFiles]
}

// node:test reports spec (✔ name (1.2ms)) or TAP (ok 1 - name).
export function parseNodeTestOutput(output) {
  const okTests = []
  const failedTests = []
  for (const rawLine of output.split(/\r?\n/)) {
    const line = rawLine.replace(/\x1b\[[0-9;]*m/g, '').trimEnd()
    const specOk = line.match(/^\s*[✔✓]\s+(.+?)(?:\s+\(\d+(?:\.\d+)?ms\))?\s*$/)
    if (specOk) {
      okTests.push(specOk[1].trim())
      continue
    }
    const specFail = line.match(/^\s*[✖✗×]\s+(.+?)(?:\s+\(\d+(?:\.\d+)?ms\))?\s*$/)
    if (specFail) {
      failedTests.push(specFail[1].trim())
      continue
    }
    const tapOk = line.match(/^\s*ok\s+\d+\s+-\s+(.+)$/)
    if (tapOk) {
      okTests.push(tapOk[1].trim())
      continue
    }
    const tapFail = line.match(/^\s*not ok\s+\d+\s+-\s+(.+)$/)
    if (tapFail) failedTests.push(tapFail[1].trim())
  }
  const count = (label) => {
    const match = output.match(new RegExp(`(?:ℹ|#)\\s*${label}\\s+(\\d+)`))
    return match ? Number(match[1]) : null
  }
  return { okTests, failedTests, total: count('tests'), passed: count('pass'), failed: count('fail') }
}
