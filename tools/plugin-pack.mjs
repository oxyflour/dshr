#!/usr/bin/env node
// Pack an external-process CPP-v1 plugin and write its `dshr-trust.json`.
//
// Usage:
//   node tools/plugin-pack.mjs --plugin <dir> --out <pkgdir> [--note <text>] [--force]
//   node tools/plugin-pack.mjs --verify <dir> [--digest <hex>]
//
// What it does:
//   1. Validate `package.json` (name/version) and `plugin.toml` (plugin.id/version/api,
//      runtime.kind/entry) exist and agree; entry must be a regular file inside the package.
//   2. Copy the package content to --out (skipping dshr-trust.json/.git/node_modules/dot dirs).
//   3. Compute the dshr-trust/v1 package digest exactly as docs/spec/plugin-trust.md §3:
//        - regular files only, symlinks skipped
//        - skip **package-root** `dshr-trust.json` and `.dshr-install.json` only
//          (nested same-name files are normal package content and are hashed)
//        - skip `.git/`, `node_modules/`, and any dot-prefixed directory
//        - relative paths use `/`, sorted by UTF-8 bytes
//        - per file: path_bytes, 0x00, decimal size bytes, 0x00, file bytes
//        - SHA-256, lowercase hex
//   4. Write an unsigned `dshr-trust.json` (schema_version/plugin_id/version/package.digest_sha256).
//
// The digest is byte-compatible with Rust `dshr_profile::package_digest`; the strongest
// cross-check is a successful `chat --install-plugin` (host recomputes the digest and
// rejects mismatches with E_INTEGRITY_MISMATCH). `--verify` recomputes for an existing
// directory and can compare against a digest printed earlier.
//
// Zero npm dependencies (node:crypto / node:fs only).

import { createHash } from 'node:crypto'
import {
  copyFileSync,
  existsSync,
  lstatSync,
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
const TRUST_FILE = 'dshr-trust.json'
// 本机安装元数据：只在**包根**排除，嵌套同名文件参与摘要（与 Rust `package_digest` 一致）。
const INSTALL_META_FILE = '.dshr-install.json'
const PLUGIN_ID_RE = /^[a-z0-9][a-z0-9._-]{1,63}$/
const VERSION_RE = /^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$/
const SKIPPED_DIRS = new Set(['.git', 'node_modules'])

function usage(exitCode = 0) {
  const out = exitCode === 0 ? console.log : console.error
  out(`用法:
  node tools/plugin-pack.mjs --plugin <dir> --out <pkgdir> [--note <text>] [--force]
  node tools/plugin-pack.mjs --verify <dir> [--digest <hex>]

示例:
  node tools/plugin-pack.mjs --plugin .tmp/authoring/sample --out .tmp/authoring/pkg
  node tools/plugin-pack.mjs --verify .tmp/authoring/pkg

--plugin  插件项目目录（含 package.json + plugin.toml + 已复制到 bin/ 的可执行文件）
--out     打包输出目录（会先复制内容，再写入 dshr-trust.json；已存在非空目录需 --force）
--note    只打印在摘要输出里，不写进 dshr-trust.json（trust schema 拒绝未知字段）
--verify  只计算并打印目录摘要，用于和 Rust package_digest / 安装输出对照
--digest  配合 --verify：命中返回 0，不命中返回 1
`)
  process.exit(exitCode)
}

function fail(message) {
  console.error(`[pack] 失败: ${message}`)
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

function readJson(path, label) {
  let text
  try {
    text = readFileSync(path, 'utf8')
  } catch (err) {
    fail(`读取 ${label} 失败（${path}）: ${err.message}`)
  }
  try {
    return JSON.parse(text.replace(/^\uFEFF/, ''))
  } catch (err) {
    fail(`解析 ${label} 失败（${path}）: ${err.message}`)
  }
}

// 极简 TOML 读取：只支持 `[section]` + `key = "string" | number | bool | array`，
// 足以校验 plugin.toml 的 plugin/runtime 段；未知值当字符串处理。
function parseToml(text) {
  const root = {}
  let current = root
  let currentName = ''
  for (const rawLine of text.split(/\r?\n/)) {
    let line = rawLine.trim()
    if (line === '' || line.startsWith('#')) continue
    // 去掉行尾注释（引号外）
    let inString = false
    for (let i = 0; i < line.length; i += 1) {
      if (line[i] === '"') inString = !inString
      if (line[i] === '#' && !inString) {
        line = line.slice(0, i).trim()
        break
      }
    }
    if (line === '') continue
    const section = line.match(/^\[([A-Za-z0-9_.-]+)\]$/)
    if (section) {
      currentName = section[1]
      root[currentName] ??= {}
      current = root[currentName]
      continue
    }
    const pair = line.match(/^([A-Za-z0-9_.-]+)\s*=\s*(.+)$/)
    if (!pair) fail(`plugin.toml 无法解析的行（${currentName || 'top'}）: ${rawLine}`)
    const [, key, rawValue] = pair
    let value = rawValue.trim()
    if (value.startsWith('"') && value.endsWith('"')) {
      value = value
        .slice(1, -1)
        .replace(/\\n/g, '\n')
        .replace(/\\"/g, '"')
        .replace(/\\\\/g, '\\')
    } else if (value.startsWith('[')) {
      try {
        value = JSON.parse(value.replace(/'/g, '"'))
      } catch {
        value = value
      }
    } else if (value === 'true' || value === 'false') {
      value = value === 'true'
    } else if (/^-?[0-9]+$/.test(value)) {
      value = Number.parseInt(value, 10)
    }
    current[key] = value
  }
  return root
}

// 与 Rust `collect_files` 等价：普通文件、只跳过**包根**的 dshr-trust.json 与
// .dshr-install.json、跳过 .git/node_modules/点目录、不跟随符号链接。
// 返回按相对路径 UTF-8 字节序升序排列。
function collectFiles(root) {
  const files = []
  const walk = (dir, prefix) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const name = entry.name
      const absolute = join(dir, name)
      const rel = prefix === '' ? name : `${prefix}/${name}`
      if (entry.isSymbolicLink()) continue
      if (entry.isDirectory()) {
        if (SKIPPED_DIRS.has(name) || name.startsWith('.')) continue
        walk(absolute, rel)
      } else if (entry.isFile()) {
        if (prefix === '' && (name === TRUST_FILE || name === INSTALL_META_FILE)) continue
        files.push({ rel, absolute })
      }
    }
  }
  walk(root, '')
  files.sort((a, b) => Buffer.compare(Buffer.from(a.rel, 'utf8'), Buffer.from(b.rel, 'utf8')))
  return files
}

function packageDigest(root) {
  const files = collectFiles(root)
  const hash = createHash('sha256')
  let bytes = 0
  for (const file of files) {
    const data = readFileSync(file.absolute)
    hash.update(Buffer.from(file.rel, 'utf8'))
    hash.update(Buffer.from([0]))
    hash.update(Buffer.from(String(data.length), 'utf8'))
    hash.update(Buffer.from([0]))
    hash.update(data)
    bytes += data.length
  }
  return { digest: hash.digest('hex'), files: files.length, bytes }
}

function assertSafeRelativePath(entry, label) {
  const normalized = entry.replace(/\\/g, '/')
  if (normalized.startsWith('/') || /^[A-Za-z]:/.test(normalized)) {
    fail(`${label} 必须是包内相对路径（拒绝绝对路径）: ${entry}`)
  }
  const parts = normalized.split('/')
  if (parts.some((part) => part === '..' || part === '')) {
    fail(`${label} 必须是包内相对路径（拒绝 .. / 空段）: ${entry}`)
  }
  return normalized
}

function validatePackage(pluginDir) {
  const packageJsonPath = join(pluginDir, 'package.json')
  const pluginTomlPath = join(pluginDir, 'plugin.toml')
  if (!existsSync(packageJsonPath)) fail(`缺少 package.json: ${packageJsonPath}`)
  if (!existsSync(pluginTomlPath)) fail(`缺少 plugin.toml: ${pluginTomlPath}`)

  const packageJson = readJson(packageJsonPath, 'package.json')
  const pluginTomlBytes = readFileSync(pluginTomlPath)
  if (pluginTomlBytes.length >= 3 && pluginTomlBytes[0] === 0xef && pluginTomlBytes[1] === 0xbb && pluginTomlBytes[2] === 0xbf) {
    fail('plugin.toml 含 UTF-8 BOM；请另存为 UTF-8 无 BOM（TOML 解析器可能拒绝 BOM）')
  }
  const manifest = parseToml(pluginTomlBytes.toString('utf8'))
  const plugin = manifest.plugin ?? {}
  const runtime = manifest.runtime ?? {}

  if (typeof packageJson.name !== 'string' || packageJson.name === '') {
    fail('package.json 缺少 name')
  }
  if (typeof packageJson.version !== 'string' || packageJson.version === '') {
    fail('package.json 缺少 version')
  }
  if (typeof plugin.id !== 'string' || !PLUGIN_ID_RE.test(plugin.id)) {
    fail(`plugin.toml [plugin].id 非法: ${JSON.stringify(plugin.id)}`)
  }
  if (typeof plugin.version !== 'string' || !VERSION_RE.test(plugin.version)) {
    fail(`plugin.toml [plugin].version 非法: ${JSON.stringify(plugin.version)}`)
  }
  if (plugin.api !== 'cordis-portable/v1') {
    fail(`plugin.toml [plugin].api 必须等于 "cordis-portable/v1"，实际: ${JSON.stringify(plugin.api)}`)
  }
  if (packageJson.name !== plugin.id) {
    fail(`package.json name (${packageJson.name}) 与 plugin.toml id (${plugin.id}) 不一致`)
  }
  if (packageJson.version !== plugin.version) {
    fail(`package.json version (${packageJson.version}) 与 plugin.toml version (${plugin.version}) 不一致`)
  }
  if (typeof runtime.kind !== 'string' || runtime.kind === '') {
    fail('plugin.toml 缺少 [runtime].kind')
  }
  if (!['external-process', 'wasm-component', 'rust-static'].includes(runtime.kind)) {
    fail(`[runtime].kind 未知: ${runtime.kind}`)
  }

  let entry = null
  if (runtime.kind === 'external-process') {
    if (typeof runtime.entry !== 'string' || runtime.entry === '') {
      fail('external-process 插件必须在 [runtime].entry 声明包内可执行文件')
    }
    entry = assertSafeRelativePath(runtime.entry, '[runtime].entry')
    const entryPath = join(pluginDir, entry)
    if (!existsSync(entryPath)) {
      fail(`[runtime].entry 指向的文件不存在: ${entry}（先构建并把可执行文件复制到 bin/）`)
    }
    const entryStat = lstatSync(entryPath)
    if (entryStat.isSymbolicLink()) fail(`[runtime].entry 不允许是符号链接: ${entry}`)
    if (!entryStat.isFile()) fail(`[runtime].entry 不是普通文件: ${entry}`)
  }

  return {
    packageJson,
    plugin,
    runtime,
    entry,
    files: collectFiles(pluginDir),
  }
}

function copyPackage(sourceDir, outDir, files) {
  if (existsSync(outDir)) {
    const stat = statSync(outDir)
    if (!stat.isDirectory()) fail(`--out 已存在且不是目录: ${outDir}`)
    const existing = readdirSync(outDir)
    if (existing.length > 0) fail(`--out 已存在且非空: ${outDir}（加 --force 覆盖）`)
  }
  mkdirSync(outDir, { recursive: true })
  for (const file of files) {
    const destination = join(outDir, ...file.rel.split('/'))
    mkdirSync(dirname(destination), { recursive: true })
    copyFileSync(file.absolute, destination)
  }
}

function runPack(args) {
  if (!args.plugin) usage(1)
  if (!args.out) usage(1)
  const pluginDir = resolve(args.plugin)
  if (!existsSync(pluginDir) || !statSync(pluginDir).isDirectory()) {
    fail(`--plugin 不是目录: ${pluginDir}`)
  }
  const outDir = resolve(args.out)
  const relOut = relative(pluginDir, outDir)
  if (relOut === '') fail('--out 不能与 --plugin 相同')
  if (!relOut.startsWith('..') && !isAbsolute(relOut)) {
    fail(`--out 不能位于 --plugin 内部（会导致复制递归）: ${outDir}`)
  }

  const info = validatePackage(pluginDir)
  if (info.runtime.kind !== 'external-process') {
    console.warn(`[pack] 注意: runtime.kind=${info.runtime.kind}；本工具按通用包打包，entry 校验只对 external-process 生效`)
  }
  const targetDir = join(pluginDir, 'target')
  if (existsSync(targetDir)) {
    console.warn(
      `[pack] 警告: ${targetDir} 会作为包内容计入摘要；建议用 CARGO_TARGET_DIR 指到插件目录外，或打包前清理`,
    )
  }

  if (args.force && existsSync(outDir)) rmSync(outDir, { recursive: true, force: true })
  copyPackage(pluginDir, outDir, info.files)

  const report = packageDigest(outDir)
  const trust = {
    schema_version: 1,
    plugin_id: info.plugin.id,
    version: info.plugin.version,
    package: { digest_sha256: report.digest },
  }
  writeFileSync(join(outDir, TRUST_FILE), `${JSON.stringify(trust, null, 2)}\n`, 'utf8')

  console.log(`[pack] plugin=${info.plugin.id} version=${info.plugin.version} kind=${info.runtime.kind}`)
  if (info.entry) console.log(`[pack] entry=${info.entry}`)
  console.log(`[pack] files=${report.files} bytes=${report.bytes}`)
  console.log(`[pack] digest_sha256=${report.digest}`)
  console.log(`[pack] trust=${join(outDir, TRUST_FILE)}`)
  if (args.note) console.log(`[pack] note=${args.note}`)
  console.log(`[pack] 安装: target\\debug\\chat.exe --install-plugin "${outDir}" --yes-install`)
  console.log(`[pack] 校验: node tools/plugin-pack.mjs --verify "${outDir}" --digest ${report.digest}`)
  console.log('[pack] 自校验: 用 chat --install-plugin 安装成功即证明 JS 摘要 == Rust package_digest')
}

function runVerify(args) {
  const target = args.verify
  if (!target) usage(1)
  const dir = resolve(target)
  if (!existsSync(dir) || !statSync(dir).isDirectory()) fail(`--verify 不是目录: ${dir}`)
  const report = packageDigest(dir)
  console.log(`[verify] dir=${dir}`)
  console.log(`[verify] files=${report.files} bytes=${report.bytes} digest_sha256=${report.digest}`)
  if (args.digest !== undefined) {
    if (args.digest === report.digest) {
      console.log('[verify] digest 匹配')
      return 0
    }
    console.error(`[verify] digest 不匹配（期望 ${args.digest}）`)
    return 1
  }
  return 0
}

const args = parseArgs(process.argv.slice(2))
if (args.verify) {
  process.exit(runVerify(args))
} else {
  runPack(args)
  process.exit(0)
}
