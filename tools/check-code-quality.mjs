#!/usr/bin/env node

import { spawnSync } from 'node:child_process'
import { existsSync, readFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const root = fileURLToPath(new URL('../', import.meta.url))
const baselinePath = 'tools/code-quality-baseline.json'
const rustPath = /^(apps|crates)\/[^/]+\/(src|tests)\/.+\.rs$/
const nodePath = /^(sidecar|tools|schema)\/.+\.(cjs|mjs|js)$/
const desktopRuntime = /^apps\/desktop-gpui\/src\/bin\/chat\/runtime\/.*\.rs$/
const windowReferences = /\b(?:gpui|gpui_platform|dshr_ui_gpui)\s*(?:::|as\b|[,};])|\b(?:ChatApp|DesktopTurnUpdate|ComposerAttachment|RenderImage)\b|^\s*use\s+crate::\*/m
const testScript = /\.test\.(cjs|mjs|js)$/
const implementationLimit = 1000
const testLimit = 1500
const reviewThreshold = 500

function run(command, args, stdio = 'pipe') {
  const result = spawnSync(command, args, { cwd: root, encoding: 'utf8', stdio })
  if (result.error) throw result.error
  if (result.status !== 0) {
    throw new Error(`${command} ${args.join(' ')} exited with ${result.status ?? result.signal}${result.stderr ? `\n${result.stderr.trim()}` : ''}`)
  }
  return result.stdout
}

function git(...args) {
  return run('git', args)
}

function gitPaths(...args) {
  return git(...args).split('\0').filter(Boolean)
}

function isFirstParty(file) {
  return !file.split('/').some(part => ['node_modules', 'target', 'dist', '.tmp'].includes(part))
    && (rustPath.test(file) || nodePath.test(file))
}

function sourceLimit(file, source) {
  const integrationTest = /^(apps|crates)\/[^/]+\/tests\//.test(file)
  const testModule = rustPath.test(file) && /^#!\[cfg\(test\)\][ \t]*(?:\r?\n|$)/.test(source)
  return integrationTest || testModule || testScript.test(file) ? testLimit : implementationLimit
}

function physicalLines(source) {
  return source ? source.split('\n').length - Number(source.endsWith('\n')) : 0
}

function baselineAt(base) {
  if (!git('ls-tree', '--full-tree', '--name-only', base, '--', baselinePath).trim()) return null
  return JSON.parse(git('show', `${base}:${baselinePath}`))
}

function checkFiles(files, changed, baseline, previous, report) {
  const { limits, exceptions } = baseline
  const today = new Date().toISOString().slice(0, 10)
  let warnings = 0

  for (const [file, ceiling] of Object.entries(limits)) {
    if (!files.has(file) || !Number.isSafeInteger(ceiling) || ceiling <= files.get(file).limit) {
      report('file length', `${file}: remove the unused or invalid baseline limit`)
    }
    if (previous && (!Object.hasOwn(previous.limits, file) || ceiling > previous.limits[file])) {
      report('file length', `${file}: the baseline can only shrink; record an expiring exception for temporary growth`)
    }
  }

  const allowedExceptions = new Map()
  for (const [file, exception] of Object.entries(exceptions)) {
    const source = files.get(file)
    const ceiling = limits[file] ?? source?.limit
    const { maxLines, owner, reason, expires } = exception
    const date = new Date(expires)
    const validDate = /^\d{4}-\d{2}-\d{2}$/.test(expires)
      && Number.isFinite(date.valueOf()) && expires === date.toISOString().slice(0, 10)
    if (!source || !Number.isSafeInteger(maxLines) || maxLines <= ceiling
      || typeof owner !== 'string' || !owner.trim() || typeof reason !== 'string' || !reason.trim()
      || !validDate) {
      report('file length', `${file}: invalid exception; supply maxLines, owner, reason and a valid UTC expiry date`)
    } else if (expires <= today) {
      report('file length', `${file}: exception expired on ${expires}`)
    } else if (source.lines <= ceiling) {
      report('file length', `${file}: remove the unused exception`)
    } else {
      allowedExceptions.set(file, maxLines)
    }
  }

  for (const [file, { lines, limit }] of files) {
    const recorded = limits[file]
    const ceiling = allowedExceptions.get(file) ?? recorded ?? limit
    if (lines > ceiling) {
      report('file length', `${file}: ${lines} lines exceeds its limit of ${ceiling}`)
    } else if (recorded !== undefined && lines < recorded) {
      report('file length', `${file}: reduce its baseline to ${lines} or remove it at ${limit} lines or fewer`)
    }
    if (limit === implementationLimit && lines > reviewThreshold && changed.has(file)) {
      console.log(`[file length] ${file}: review module responsibilities (${lines} lines)`)
      warnings++
    }
  }
  console.log(`[file length] Checked ${files.size} files, ${warnings} changed files need a responsibility review`)
}

function checkDependencies(metadata, report) {
  const workspaceLocations = new Set(metadata.packages.map(pkg => resolve(dirname(pkg.manifest_path))))
  const gpuiOwners = new Set(['dshr-desktop-gpui', 'dshr-ui-gpui'])
  const leaves = new Set(['dshr-compat-dsh-protocol', 'dshr-ui-contributions'])
  const pluginImplementations = new Set(['dshr-plugin-host', 'dshr-plugin-wasm'])
  let devDependencies = 0

  for (const pkg of metadata.packages) {
    for (const dependency of pkg.dependencies) {
      const { name, kind, path, target } = dependency
      if (kind === 'dev') {
        devDependencies++
        continue
      }
      const location = `${pkg.name} -> ${name} (${kind ?? 'normal'}${target ? `, ${target}` : ''})`
      if (/^gpui(?:$|[-_])/.test(name) && !gpuiOwners.has(pkg.name)) {
        report('dependency direction', `${location}: GPUI is owned by desktop-gpui and ui-gpui`)
      }
      if (leaves.has(pkg.name) && path && workspaceLocations.has(resolve(path))) {
        report('dependency direction', `${location}: this crate cannot depend on other workspace crates`)
      }
      if (pkg.name === 'dshr-plugin-api' && pluginImplementations.has(name)) {
        report('dependency direction', `${location}: plugin implementations depend on the API`)
      }
    }
  }
  console.log(`[dependency direction] Checked ${metadata.packages.length} workspace packages, ${devDependencies} test-only declarations excluded`)
}

function main(args) {
  if (args.length !== 0 && (args.length !== 2 || args[0] !== '--base')) {
    throw new Error('usage: node tools/check-code-quality.mjs [--base <git-ref>]')
  }
  const base = git('rev-parse', '--verify', '--end-of-options', `${args[1] ?? 'HEAD'}^{commit}`).trim()
  const changed = new Set(gitPaths('diff', '--name-only', '-z', base, '--'))
  const paths = gitPaths('ls-files', '--cached', '-z')
    .filter(file => isFirstParty(file) && existsSync(resolve(root, file)))
  let failures = 0
  const report = (rule, message) => {
    console.error(`[${rule}] ${message}`)
    failures++
  }
  const files = new Map(paths.map(file => {
    const source = readFileSync(resolve(root, file), 'utf8')
    if (desktopRuntime.test(file) && windowReferences.test(source)) {
      report('dependency direction', `${file}: the shared runtime cannot import the desktop view or GPUI`)
    }
    return [file, { lines: physicalLines(source), limit: sourceLimit(file, source) }]
  }))

  const baseline = JSON.parse(readFileSync(resolve(root, baselinePath), 'utf8'))
  const previous = baselineAt(base)
  if (!previous) console.log(`[file length] Initial baseline; ${base.slice(0, 7)} has no existing policy`)
  checkFiles(files, changed, baseline, previous, report)

  let scriptCount = 0
  for (const file of files.keys()) {
    if (!nodePath.test(file)) continue
    scriptCount++
    try {
      run(process.execPath, ['--check', file])
    } catch (error) {
      report('Node syntax', error.message)
    }
  }
  console.log(`[Node syntax] Checked ${scriptCount} first-party scripts`)

  const metadata = JSON.parse(run('cargo', ['metadata', '--locked', '--no-deps', '--format-version', '1']))
  checkDependencies(metadata, report)
  try {
    run('cargo', ['fmt', '--all', '--', '--check'], 'inherit')
  } catch (error) {
    report('Rust formatting', error.message)
  }
  console.log(`[code quality] ${failures ? `${failures} violations` : 'PASS'}`)
  return failures ? 1 : 0
}

try {
  process.exitCode = main(process.argv.slice(2))
} catch (error) {
  console.error(`[code quality] ${error.message}`)
  process.exitCode = 1
}
