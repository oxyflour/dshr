#!/usr/bin/env node
// Generate an external-process CPP-v1 plugin project from templates/external-tool-plugin.
//
// Usage:
//   node tools/new-plugin.mjs --out <dir> [--id vendor.name] [--name "Display Name"]
//                             [--version 0.1.0] [--repo <repo-root>]
//
// - Validates the plugin id (same rule as schema/cpp-v1/manifest.schema.json) and version.
// - Refuses to write into an existing non-empty directory.
// - Inside this repository, only `.tmp/` targets are allowed (keeps the repo clean).
// - Replaces `__PLUGIN_ID__`, `__PLUGIN_NAME__`, `__PLUGIN_VERSION__`,
//   `__CRATE_NAME__`, `__BIN_NAME__`, `__TOOL_NAME__` placeholders in the copied template.
// - Copies `crates/plugin-sdk` into `<out>/sdk/` (self-contained, see P1-4) and appends
//   `[workspace]` to the copied SDK manifest; the generated Cargo.toml only references
//   the relative path `sdk` and **never the dshr repository path**.
// - Prints the exact build / pack / install commands for the generated project.
//
// Zero npm dependencies.

import {
  copyFileSync,
  existsSync,
  mkdirSync,
  readFileSync,
  readdirSync,
  statSync,
  writeFileSync,
} from 'node:fs'
import { dirname, join, relative, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

const repoRoot = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const PLUGIN_ID_RE = /^[a-z0-9][a-z0-9._-]{1,63}$/
const VERSION_RE = /^[0-9]+\.[0-9]+\.[0-9]+([-+][0-9A-Za-z.-]+)?$/
const TEXT_EXTENSIONS = new Set(['.json', '.toml', '.rs', '.md', '.txt', '.yml', '.yaml', '.ps1', '.sh', '.cmd'])

function usage(exitCode = 0) {
  const out = exitCode === 0 ? console.log : console.error
  out(`用法:
  node tools/new-plugin.mjs --out <dir> [--id vendor.name] [--name "Display Name"]
                            [--version 0.1.0]

示例:
  node tools/new-plugin.mjs --out .tmp/authoring/sample --id example.echo --name "Echo Panel"

--out     生成目录（必须是空目录或不存在；仓库内只允许 .tmp/ 下）
--id      插件 ID / 包名，形如 vendor.name（默认 example.echo）
--name    人类可读名称（默认由 id 推导，如 "Example Echo"）
--version SemVer（默认 0.1.0）

生成物内含自包含 SDK（<out>/sdk/），Cargo.toml 只用相对路径 "sdk"，不引用 dshr 仓库。
`)
  process.exit(exitCode)
}

function fail(message) {
  console.error(`[new-plugin] 失败: ${message}`)
  process.exit(1)
}

function parseArgs(argv) {
  const args = { _: [] }
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i]
    if (arg === '--help' || arg === '-h') usage(0)
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

function titleCase(id) {
  return id
    .split(/[._-]+/)
    .filter(Boolean)
    .map((part) => part.charAt(0).toUpperCase() + part.slice(1))
    .join(' ')
}

// `example.echo` -> `example-echo`（Cargo 包名/二进制名；不以数字开头）
function crateNameFor(id) {
  let name = id
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, '-')
    .replace(/^-+|-+$/g, '')
  if (name === '') name = 'plugin'
  if (/^[0-9]/.test(name)) name = `plugin-${name}`
  return name
}

function checkOutPath(out) {
  const relativeToRepo = relative(repoRoot, out)
  const insideRepo = relativeToRepo !== '' && !relativeToRepo.startsWith('..') && !relativeToRepo.startsWith('\\..')
  if (insideRepo) {
    const tmpRoot = join(repoRoot, '.tmp')
    const relTmp = relative(tmpRoot, out)
    const underTmp = relTmp !== '' && !relTmp.startsWith('..')
    if (!underTmp) {
      fail(`拒绝写到仓库内已有目录: ${out}（仓库内只允许生成到 .tmp/ 下）`)
    }
  }
  if (existsSync(out)) {
    if (!statSync(out).isDirectory()) fail(`--out 已存在且不是目录: ${out}`)
    if (readdirSync(out).length > 0) fail(`--out 已存在且非空: ${out}`)
  }
}

const TEMPLATE_DIR = join(repoRoot, 'templates', 'external-tool-plugin')
if (!existsSync(TEMPLATE_DIR)) fail(`模板不存在: ${TEMPLATE_DIR}`)

const args = parseArgs(process.argv.slice(2))
if (!args.out) usage(1)

const pluginId = args.id ?? 'example.echo'
if (!PLUGIN_ID_RE.test(pluginId)) {
  fail(`插件 ID 非法: ${JSON.stringify(pluginId)}（规则: ^[a-z0-9][a-z0-9._-]{1,63}$，见 schema/cpp-v1/manifest.schema.json）`)
}
const version = args.version ?? '0.1.0'
if (!VERSION_RE.test(version)) fail(`版本非法: ${JSON.stringify(version)}（SemVer，如 0.1.0）`)
const displayName = args.name ?? titleCase(pluginId)
if (displayName.trim() === '') fail('--name 不能为空')
if (/["\\\r\n]/.test(displayName)) {
  fail('--name 不能包含双引号、反斜杠或换行（会破坏生成的 plugin.toml / 源码字符串）')
}
const crateName = crateNameFor(pluginId)
const binName = crateName
// 工具名必须匹配 [a-z][a-z0-9_]*（见 cpp-v1 §7.1），所以从 crate 名派生；
// 加插件前缀可避免多个插件都用 `echo` 时全局冲突。
const toolName = `${crateName.replace(/-/g, '_')}_echo`
if (toolName.length > 64) {
  fail(`派生的工具名过长（${toolName.length} > 64）：请用更短的 --id`)
}

const outDir = resolve(args.out)
checkOutPath(outDir)

const replacements = new Map([
  ['__PLUGIN_ID__', pluginId],
  ['__PLUGIN_NAME__', displayName],
  ['__PLUGIN_VERSION__', version],
  ['__CRATE_NAME__', crateName],
  ['__BIN_NAME__', binName],
  ['__TOOL_NAME__', toolName],
])

function render(text) {
  let result = text
  for (const [placeholder, value] of replacements) result = result.split(placeholder).join(value)
  return result
}

function isTextFile(name) {
  if (name === '.gitignore' || name === '.gitattributes') return true
  const dot = name.lastIndexOf('.')
  if (dot === -1) return false
  return TEXT_EXTENSIONS.has(name.slice(dot).toLowerCase())
}

function copyTemplate(source, destination) {
  mkdirSync(destination, { recursive: true })
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    const from = join(source, entry.name)
    const to = join(destination, entry.name)
    if (entry.isDirectory()) {
      if (entry.name === '.git' || entry.name === 'target' || entry.name === 'node_modules') continue
      copyTemplate(from, to)
    } else if (entry.isFile()) {
      if (isTextFile(entry.name)) {
        writeFileSync(to, render(readFileSync(from, 'utf8')), 'utf8')
      } else {
        copyFileSync(from, to)
      }
    }
  }
}

copyTemplate(TEMPLATE_DIR, outDir)
// 保证 bin/ 存在（构建后把可执行文件复制到这里）
mkdirSync(join(outDir, 'bin'), { recursive: true })

// ---------------------------------------------------------------------------
// 自包含 SDK（P1-4）：SDK 是插件作者唯一依赖，必须随生成物拷贝，模板 Cargo.toml
// 只写 `path = "sdk"`，绝不出现 dshr 仓库绝对路径。
// ---------------------------------------------------------------------------
function copyTree(source, destination) {
  mkdirSync(destination, { recursive: true })
  for (const entry of readdirSync(source, { withFileTypes: true })) {
    const from = join(source, entry.name)
    const to = join(destination, entry.name)
    if (entry.isDirectory()) {
      if (entry.name === 'target' || entry.name === '.git' || entry.name === 'node_modules') continue
      copyTree(from, to)
    } else if (entry.isFile()) {
      copyFileSync(from, to)
    }
  }
}

const sdkSource = join(repoRoot, 'crates', 'plugin-sdk')
if (!existsSync(join(sdkSource, 'Cargo.toml'))) fail(`SDK 不存在: ${sdkSource}`)
const sdkOut = join(outDir, 'sdk')
copyTree(sdkSource, sdkOut)

// SDK 的 Cargo.toml 必须自包含：不能有 `*.workspace = true`（拷到仓库外无 workspace 可继承）。
// 不额外写 `[workspace]`：作为样例 workspace 的 path 依赖时它就是成员；单独拷出时
// 没有上层 workspace，Cargo 会把它当作独立 crate 处理。
const sdkCargoPath = join(sdkOut, 'Cargo.toml')
const sdkCargo = readFileSync(sdkCargoPath, 'utf8')
if (/\.workspace\s*=\s*true/.test(sdkCargo)) {
  fail('SDK Cargo.toml 仍继承 workspace 字段；请改成具体版本/edition 后再生成')
}
const sampleCargo = readFileSync(join(outDir, 'Cargo.toml'), 'utf8')
if (!sampleCargo.includes('path = "sdk"')) {
  fail('生成的 Cargo.toml 没有引用相对路径 sdk')
}
if (sampleCargo.includes(repoRoot.replace(/\\/g, '/')) || sampleCargo.includes(repoRoot)) {
  fail('生成的 Cargo.toml 仍包含 dshr 仓库绝对路径（禁止：包必须可离开仓库独立构建）')
}

// 兜底检查：模板里不允许残留未替换的占位符（.gitkeep 之类不动）。
function walkFiles(dir, out = []) {
  for (const entry of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, entry.name)
    if (entry.isDirectory()) walkFiles(full, out)
    else if (entry.isFile()) out.push(full)
  }
  return out
}

for (const full of walkFiles(outDir)) {
  if (!isTextFile(full.slice(full.lastIndexOf(sep) + 1))) continue
  const text = readFileSync(full, 'utf8')
  if (/__[A-Z][A-Z0-9_]*__/.test(text)) {
    fail(`生成结果残留占位符: ${relative(outDir, full)}`)
  }
}

console.log(`[new-plugin] created ${outDir}`)
console.log(`[new-plugin] plugin_id=${pluginId} crate=${crateName} bin=${binName}.exe version=${version}`)
console.log('[new-plugin] 后续命令（Windows PowerShell）：')
console.log(`  1) cd "${outDir}"`)
console.log(
  `  2) $env:CARGO_TARGET_DIR = "${join(repoRoot, '.tmp', 'authoring-target', crateName)}"; & "${join(repoRoot, 'tools', 'build-env.cmd')}" cargo build --release`,
)
console.log(`  3) Copy-Item "$env:CARGO_TARGET_DIR\\release\\${binName}.exe" "bin\\${binName}.exe"`)
console.log(
  `  4) node "${join(repoRoot, 'tools', 'plugin-pack.mjs')}" --plugin "${outDir}" --out "${join(outDir, '..', `pkg-${crateName}`)}"`,
)
console.log('  5) 本地开发免信任：把包目录传给宿主直接加载；正式安装见 docs/plugin-authoring.md §6')
console.log('[new-plugin] 下一步阅读: docs/plugin-authoring.md')
