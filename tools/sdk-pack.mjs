#!/usr/bin/env node
// Package the self-contained CPP-v1 plugin SDK as an offline distribution channel.
//
// Usage:
//   node tools/sdk-pack.mjs --out <dir> [--sdk <dir>] [--force]
//   node tools/sdk-pack.mjs --verify <dir>
//
// Output (default --out dist/sdk):
//   <out>/sdk-manifest.json             版本、文件列表、SHA-256
//   <out>/SHA256SUMS                    sha256sum 兼容的逐文件校验和
//   <out>/dshr-plugin-sdk-<version>/    SDK 源码（作者直接复制或压缩分发）
//
// 作者消费方式：
//   node tools/new-plugin.mjs --out my-plugin --sdk <out>/dshr-plugin-sdk-0.1.0
//   # 生成器仍把 SDK 拷到 <plugin>/sdk/，Cargo.toml 只引用相对路径 "sdk"
//
// 没有 registry、没有 npm：这是 M4 的"离线渠道"；registry/证书体系仍在 W7。
// Zero npm dependencies (node:crypto / node:fs only).

import { createHash } from 'node:crypto'
import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  rmSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { dirname, isAbsolute, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const MANIFEST = 'sdk-manifest.json'
const CHECKSUMS = 'SHA256SUMS'
const SKIPPED_DIRS = new Set(['.git', 'node_modules', 'target'])

function usage(exitCode = 0) {
  const out = exitCode === 0 ? console.log : console.error
  out(`用法:
  node tools/sdk-pack.mjs --out <dir> [--sdk <dir>] [--force]
  node tools/sdk-pack.mjs --verify <dir>

示例:
  node tools/sdk-pack.mjs --out .tmp/sdk-dist
  node tools/sdk-pack.mjs --verify .tmp/sdk-dist

--out     输出目录（默认 dist/sdk；已存在同版本 SDK 时需 --force）
--sdk     SDK 源码目录（默认 crates/plugin-sdk；用于发布前检查）
--force   覆盖已存在的 <out>/dshr-plugin-sdk-<version>/
--verify  只校验输出目录里的 sdk-manifest.json 与文件哈希
`)
  process.exit(exitCode)
}

function fail(message) {
  console.error(`[sdk] 失败: ${message}`)
  process.exit(1)
}

function parseArgs(argv) {
  const args = { _: [] }
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i]
    if (arg === '--help' || arg === '-h') usage(0)
    if (arg === '--force') {
      args.force = true
      continue
    }
    if (arg.startsWith('--')) {
      const key = arg.slice(2)
      const value = argv[i + 1]
      if (value === undefined || value.startsWith('--')) fail(`参数 ${arg} 缺少值`)
      args[key] = value
      i += 1
      continue
    }
    args._.push(arg)
  }
  return args
}

function sha256File(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex')
}

function packageVersion(cargoText, label) {
  const section = cargoText.match(/\[package\]([\s\S]*?)(?=\n\[|$)/)
  if (!section) fail(`${label} 缺少 [package] 段`)
  const name = section[1].match(/^\s*name\s*=\s*"([^"]+)"/m)
  const version = section[1].match(/^\s*version\s*=\s*"([^"]+)"/m)
  if (!name) fail(`${label} [package].name 缺失`)
  if (!version) fail(`${label} [package].version 缺失（离线发布包必须有具体版本）`)
  return { name: name[1], version: version[1] }
}

function collectFiles(root) {
  const files = []
  const walk = (dir, prefix) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const absolute = join(dir, entry.name)
      const rel = prefix === '' ? entry.name : `${prefix}/${entry.name}`
      if (entry.isSymbolicLink()) continue
      if (entry.isDirectory()) {
        if (SKIPPED_DIRS.has(entry.name) || entry.name.startsWith('.')) continue
        walk(absolute, rel)
      } else if (entry.isFile()) {
        files.push({ rel, absolute, size: statSync(absolute).size, sha256: sha256File(absolute) })
      }
    }
  }
  walk(root, '')
  files.sort((a, b) => Buffer.compare(Buffer.from(a.rel, 'utf8'), Buffer.from(b.rel, 'utf8')))
  return files
}

function copyTree(source, destination) {
  mkdirSync(destination, { recursive: true })
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    const from = join(source, entry.name)
    const to = join(destination, entry.name)
    if (entry.isSymbolicLink()) continue
    if (entry.isDirectory()) {
      if (SKIPPED_DIRS.has(entry.name) || entry.name.startsWith('.')) continue
      copyTree(from, to)
    } else if (entry.isFile()) {
      copyFileSync(from, to)
    }
  }
}

function readManifest(outDir) {
  const path = join(outDir, MANIFEST)
  if (!existsSync(path)) fail(`缺少 ${MANIFEST}: ${path}（先运行 --out 生成）`)
  try {
    return JSON.parse(readFileSync(path, 'utf8').replace(/^\uFEFF/, ''))
  } catch (err) {
    fail(`${MANIFEST} 不是合法 JSON: ${err.message}`)
  }
}

function runPack(args) {
  const sdkSource = resolve(args.sdk ?? join(repoRoot, 'crates', 'plugin-sdk'))
  if (!existsSync(sdkSource) || !statSync(sdkSource).isDirectory()) {
    fail(`--sdk 不是目录: ${sdkSource}`)
  }
  const cargoPath = join(sdkSource, 'Cargo.toml')
  if (!existsSync(cargoPath)) fail(`SDK 缺少 Cargo.toml: ${cargoPath}`)
  const info = packageVersion(readFileSync(cargoPath, 'utf8'), 'SDK Cargo.toml')
  if (info.name !== 'dshr-plugin-sdk') {
    fail(`SDK 包名应为 dshr-plugin-sdk，实际: ${info.name}`)
  }

  const outDir = resolve(args.out ?? join(repoRoot, 'dist', 'sdk'))
  const relOut = relative(sdkSource, outDir)
  if (relOut === '' || (!relOut.startsWith('..') && !isAbsolute(relOut))) {
    fail(`--out 不能位于 --sdk 内部（会复制递归）: ${outDir}`)
  }
  const packageName = `${info.name}-${info.version}`
  const packageDir = join(outDir, packageName)
  if (existsSync(packageDir)) {
    if (!args.force) fail(`${packageDir} 已存在（加 --force 覆盖）`)
    rmSync(packageDir, { recursive: true, force: true })
  }

  copyTree(sdkSource, packageDir)
  const files = collectFiles(packageDir)
  if (files.length === 0) fail('SDK 目录为空')
  const manifest = {
    schema_version: 1,
    name: info.name,
    version: info.version,
    package_dir: packageName,
    generated_by: 'tools/sdk-pack.mjs',
    files: files.map((file) => ({ path: file.rel, size: file.size, sha256: file.sha256 })),
  }
  mkdirSync(outDir, { recursive: true })
  writeFileSync(join(outDir, MANIFEST), `${JSON.stringify(manifest, null, 2)}\n`, 'utf8')
  writeFileSync(
    join(outDir, CHECKSUMS),
    files.map((file) => `${file.sha256}  ${file.rel}`).join('\n') + '\n',
    'utf8',
  )

  console.log(`[sdk] sdk=${sdkSource}`)
  console.log(`[sdk] out=${outDir}`)
  console.log(`[sdk] package=${packageName} files=${files.length}`)
  console.log(`[sdk] manifest=${join(outDir, MANIFEST)}`)
  console.log(`[sdk] checksums=${join(outDir, CHECKSUMS)}`)
  console.log('[sdk] 生成插件:')
  console.log(`  node tools/new-plugin.mjs --out <dir> --sdk "${packageDir}"`)
  return 0
}

function runVerify(args) {
  const outDir = resolve(args.verify)
  if (!existsSync(outDir) || !statSync(outDir).isDirectory()) {
    fail(`--verify 不是目录: ${outDir}`)
  }
  const manifest = readManifest(outDir)
  if (manifest.schema_version !== 1) fail(`不支持 sdk-manifest schema_version=${manifest.schema_version}`)
  if (typeof manifest.package_dir !== 'string' || manifest.package_dir === '') {
    fail('sdk-manifest.package_dir 缺失')
  }
  const packageDir = join(outDir, manifest.package_dir)
  if (!existsSync(packageDir)) fail(`manifest 指向的 SDK 目录不存在: ${packageDir}`)
  const actual = new Map(collectFiles(packageDir).map((file) => [file.rel, file]))
  const expected = Array.isArray(manifest.files) ? manifest.files : []
  if (expected.length === 0) fail('sdk-manifest.files 为空')

  let failures = 0
  for (const file of expected) {
    if (typeof file.path !== 'string' || typeof file.sha256 !== 'string') {
      console.error('[sdk] manifest 文件条目缺 path/sha256')
      failures += 1
      continue
    }
    const found = actual.get(file.path)
    if (!found) {
      console.error(`[sdk] 缺少文件: ${file.path}`)
      failures += 1
      continue
    }
    if (found.sha256 !== file.sha256) {
      console.error(`[sdk] 哈希不匹配: ${file.path}`)
      failures += 1
    }
    if (file.size !== undefined && found.size !== file.size) {
      console.error(`[sdk] 大小不匹配: ${file.path}`)
      failures += 1
    }
    actual.delete(file.path)
  }
  for (const extra of actual.keys()) {
    console.error(`[sdk] manifest 未登记的文件: ${extra}`)
    failures += 1
  }
  if (failures > 0) {
    console.error(`[sdk] verify FAIL（${failures} 项）: ${outDir}`)
    return 1
  }
  console.log(
    `[sdk] verify OK name=${manifest.name} version=${manifest.version} files=${expected.length} dir=${outDir}`,
  )
  return 0
}

const args = parseArgs(process.argv.slice(2))
if (args.verify) {
  process.exit(runVerify(args))
} else {
  process.exit(runPack(args))
}
