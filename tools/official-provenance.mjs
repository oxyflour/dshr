#!/usr/bin/env node
// 官方包来源/摘要校验（review N4）：gate 不能只看目录存在。
//
// lock 文件记录官方包的版本与关键文件 SHA-256；gate 先校验，再跑官方包测试。
// 升级官方包时必须显式 `--update` 重写 lock，并一起 review。
//
// 用法：
//   node tools/official-provenance.mjs --check
//   node tools/official-provenance.mjs --update
//   node tools/official-provenance.mjs --check --rc5-dir <sidecar directory>
//
// 退出码：0 = 全部匹配；1 = 缺包/缺 lock/内容变化。

import { createHash } from 'node:crypto'
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import process from 'node:process'
import { mountOfficialArchive } from '../sidecar/dsh-official/lib/official-archive.mjs'

const ROOT = resolve(import.meta.dirname, '..')
const RC1_PACKAGES = [
  {
    name: '@deepseek-ai/dsh-tool-working-directory',
    source: '.tmp/dsh-tools-all/dsh-tool-working-directory',
    files: ['package.json', 'lib/index.js'],
  },
  {
    name: '@deepseek-ai/dsh-tool-todo',
    source: '.tmp/dsh-tools-all/dsh-tool-todo',
    files: ['package.json', 'lib/index.js'],
  },
  {
    name: '@deepseek-ai/dsh-storage-domain',
    source: 'sidecar/dsh-official/node_modules/@deepseek-ai/dsh-storage-domain',
    files: ['package.json', 'lib/index.js'],
  },
]
const RC5_TOOLS = [
  'agent-team',
  'ask-user',
  'bash',
  'bash-persistent',
  'call-timeout-policy',
  'cordis',
  'executor-native',
  'executor-ptc',
  'fs',
  'fs-search',
  'goal',
  'jobs',
  'office',
  'present',
  'pwsh',
  'pwsh-persistent',
  'ralph',
  'schedule',
  'session-query',
  'skill',
  'str-replace-editor',
  'subagent',
  'subagent-control',
  'terminal',
  'todo',
  'web',
  'working-directory',
  'workflow',
  'workspace-dependencies',
]
const RC5_PACKAGES = [
  'dsh-agent-team', 'dsh-cordis-host-runner', 'dsh-goal', 'dsh-office-kit', 'dsh-ptc-runtime-node', 'dsh-sandbox-local',
  'dsh-sandbox-windows-acl', 'dsh-schedule', 'dsh-session-query', 'dsh-skill-filesystem', 'dsh-skill-office', 'dsh-subagent',
  'dsh-subprocess-local', 'dsh-terminal', 'dsh-terminal-bash', 'dsh-util-time',
  ...RC5_TOOLS.map(tool => `dsh-tool-${tool}`),
  'dsh-web', 'dsh-web-deepseek-api', 'dsh-web-fetch-http', 'dsh-workflow', 'dsh-workflow-ptc',
].map(pkg => {
  const name = `@deepseek-ai/${pkg}`
  return {
    name,
    source: `node_modules/${name}`,
    files: ['package.json', 'lib/index.js'],
    version: '0.2.2-rc.5',
  }
})
const LINUX_ONLY_TOOLS = new Set(['@deepseek-ai/dsh-tool-bash', '@deepseek-ai/dsh-tool-bash-persistent'])

function sha256(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex')
}

function collect(packageRoots, packages) {
  const entries = []
  for (const pkg of packages) {
    const roots = packageRoots.map(base => resolve(base, pkg.source)).filter(existsSync)
    if (roots.length !== 1) {
      throw new Error(`官方包目录 ${pkg.source} 应只出现一次，实际为 ${roots.length}`)
    }
    const [root] = roots
    const packageJson = JSON.parse(
      readFileSync(resolve(root, 'package.json'), 'utf8'),
    )
    if (packageJson.name !== pkg.name) {
      throw new Error(`${pkg.source} 的 package name 是 ${packageJson.name}，期望 ${pkg.name}`)
    }
    if (pkg.version && packageJson.version !== pkg.version) {
      throw new Error(`${pkg.name} 的版本为 ${packageJson.version}，期望 ${pkg.version}`)
    }
    const files = {}
    for (const file of pkg.files) {
      const path = resolve(root, file)
      if (!existsSync(path)) throw new Error(`缺少 ${pkg.source}/${file}`)
      files[file] = sha256(path)
    }
    entries.push({
      name: pkg.name,
      version: packageJson.version,
      source: pkg.source,
      files,
    })
  }
  return { schema_version: 1, tool: 'tools/official-provenance.mjs', packages: entries }
}

function main() {
  const update = process.argv.includes('--update')
  const rc5Option = process.argv.indexOf('--rc5-dir')
  const rc5Directory = rc5Option === -1 ? null : process.argv[rc5Option + 1]
  if (rc5Option !== -1 && (!rc5Directory || rc5Directory.startsWith('--'))) {
    throw new Error('--rc5-dir 需要隔离 Sidecar 目录')
  }
  const windowsDebug = process.argv.includes('--windows-debug')
  if (windowsDebug && (!rc5Directory || update)) {
    throw new Error('--windows-debug 只支持通过 --check --rc5-dir 验证 Windows 调试包')
  }
  const inScope = pkg => !windowsDebug || !LINUX_ONLY_TOOLS.has(pkg.name)
  const lockName = rc5Directory ? 'official-rc5-packages.lock.json' : 'official-packages.lock.json'
  const lock = resolve(ROOT, 'sidecar/dsh-official', lockName)
  const archive = rc5Directory && existsSync(resolve(rc5Directory, 'official.zip')) && mountOfficialArchive(rc5Directory)
  const packageRoots = archive ? [archive.directory, resolve(rc5Directory, 'native')] : [rc5Directory ?? ROOT]
  let actual
  try {
    actual = collect(packageRoots, rc5Directory ? RC5_PACKAGES.filter(inScope) : RC1_PACKAGES)
  } finally {
    if (archive) archive.close()
  }
  if (update) {
    writeFileSync(lock, `${JSON.stringify(actual, null, 2)}\n`)
    console.log(`[official-provenance] 已写入 ${lock}`)
    return
  }
  if (!existsSync(lock)) {
    console.error(`[official-provenance] 缺少 ${lock}；先运行 --update 并提交`)
    process.exit(1)
  }
  const expected = JSON.parse(readFileSync(lock, 'utf8'))
  const problems = []
  for (const got of actual.packages) {
    const wanted = expected.packages?.filter(pkg => pkg.name === got.name) ?? []
    if (wanted.length !== 1 || Object.keys(got.files).some(file => !wanted[0].files?.[file])) {
      problems.push(`lock 缺少 ${got.name} 的唯一条目或文件摘要`)
    }
  }
  for (const wanted of (expected.packages ?? []).filter(inScope)) {
    const got = actual.packages.find((pkg) => pkg.name === wanted.name)
    if (!got) {
      problems.push(`lock 中的 ${wanted.name} 在磁盘上不存在`)
      continue
    }
    if (got.version !== wanted.version) {
      problems.push(`${wanted.name} 版本 ${got.version} != lock ${wanted.version}`)
    }
    for (const [file, hash] of Object.entries(wanted.files ?? {})) {
      const npmHash = windowsDebug ? wanted.npm_files?.[file] ?? null : null
      if (got.files[file] !== hash && got.files[file] !== npmHash) {
        problems.push(`${wanted.name}/${file} SHA-256 变化（期望 ${hash}，实际 ${got.files[file]}）`)
      }
    }
  }
  if (problems.length > 0) {
    for (const problem of problems) console.error(`[official-provenance] FAIL: ${problem}`)
    console.error('[official-provenance] FAIL（官方包来源/摘要与 lock 不一致）')
    process.exit(1)
  }
  console.log(
    `[official-provenance] PASS（${actual.packages.length} 个官方包，版本+SHA-256 匹配）`,
  )
}

main()
