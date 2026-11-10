#!/usr/bin/env node
// Verify that every relative markdown link points at an existing file or directory.
// Usage: node tools/check-links.mjs

import { readdirSync, readFileSync, statSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const skipDirs = new Set(['.git', 'target', 'node_modules', 'dist', '.tmp', '_probe'])

function walk(dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    if (entry.isDirectory()) {
      if (skipDirs.has(entry.name)) continue
      walk(join(dir, entry.name), out)
    } else if (entry.name.endsWith('.md')) {
      out.push(join(dir, entry.name))
    }
  }
  return out
}

const files = walk(repoRoot)
const problems = []
let checked = 0

for (const file of files) {
  const text = readFileSync(file, 'utf8')
  const links = text.matchAll(/\]\(([^)\s]+)\)/g)
  for (const [, rawTarget] of links) {
    if (/^(https?:|mailto:|#)/.test(rawTarget)) continue
    const target = rawTarget.split('#')[0]
    if (!target) continue
    checked += 1
    const resolved = resolve(dirname(file), decodeURI(target))
    try {
      statSync(resolved)
    } catch {
      problems.push(`${file.replace(repoRoot + '\\', '')} -> ${rawTarget}`)
    }
  }
}

for (const p of problems) console.error(`broken: ${p}`)
console.log(`\n${checked} link(s) checked, ${problems.length} broken, ${files.length} file(s) scanned`)
process.exit(problems.length === 0 ? 0 : 1)
