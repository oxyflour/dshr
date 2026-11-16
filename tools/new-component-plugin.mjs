#!/usr/bin/env node
// Generate a cpp-v1 WASM Component plugin project from
// templates/wasm-component-plugin.
//
// Usage:
//   node tools/new-component-plugin.mjs --out <dir> --id vendor.name \\
//     [--name "Display Name"] [--version 0.1.0]
import { existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const templateDir = join(repoRoot, 'templates', 'wasm-component-plugin')

function usage(code) {
  console.error('usage: node tools/new-component-plugin.mjs --out <dir> --id vendor.name [--name "Display Name"] [--version 0.1.0]')
  process.exit(code)
}

function parseArgs(argv) {
  const args = {}
  for (let i = 0; i < argv.length; i += 1) {
    const key = argv[i]
    if (!key.startsWith('--')) usage(1)
    const value = argv[i + 1]
    if (value === undefined) usage(1)
    args[key.slice(2)] = value
    i += 1
  }
  return args
}

const args = parseArgs(process.argv.slice(2))
if (!args.out || !args.id) usage(1)
if (!/^[a-z][a-z0-9_.-]{1,63}$/.test(args.id)) {
  console.error(`invalid --id: ${args.id}`)
  process.exit(1)
}
const name = args.name ?? args.id
const version = args.version ?? '0.1.0'
const crateName = args.id.replace(/[^A-Za-z0-9_]/g, '_')
const outDir = resolve(args.out)
if (existsSync(outDir) && readdirSync(outDir).length > 0) {
  console.error(`output directory is not empty: ${outDir}`)
  process.exit(1)
}

function render(text) {
  return text
    .replaceAll('__CRATE_NAME__', crateName)
    .replaceAll('__ID__', args.id)
    .replaceAll('__NAME__', name)
    .replaceAll('__VERSION__', version)
}

function copyTemplate(source, destination) {
  mkdirSync(destination, { recursive: true })
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    const from = join(source, entry.name)
    const to = join(destination, entry.name)
    if (entry.isDirectory()) {
      copyTemplate(from, to)
    } else if (entry.isFile()) {
      writeFileSync(to, render(readFileSync(from, 'utf8')), 'utf8')
    }
  }
}

if (!existsSync(templateDir)) {
  console.error(`template missing: ${templateDir}`)
  process.exit(1)
}
copyTemplate(templateDir, outDir)

console.log(`[new-component-plugin] ${outDir}`)
console.log('  rustup target add wasm32-wasip2')
console.log('  cargo build --release --target wasm32-wasip2')
console.log(`  cp target/wasm32-wasip2/release/${crateName}.wasm plugin.wasm`)
console.log('  dshr chat --install-plugin <this-dir>')
