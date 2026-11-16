#!/usr/bin/env node
// 官方包来源/摘要校验（review N4）：gate 不能只看目录存在。
//
// lock 文件记录官方包的版本与关键文件 SHA-256；gate 先校验，再跑官方包测试。
// 升级官方包时必须显式 `--update` 重写 lock，并一起 review。
//
// 用法：
//   node tools/official-provenance.mjs --check
//   node tools/official-provenance.mjs --update
//
// 退出码：0 = 全部匹配；1 = 缺包/缺 lock/内容变化。

import { createHash } from 'node:crypto'
import { existsSync, readFileSync, writeFileSync } from 'node:fs'
import { resolve } from 'node:path'
import process from 'node:process'

const ROOT = resolve(import.meta.dirname, '..')
const LOCK = resolve(ROOT, 'sidecar/dsh-official/official-packages.lock.json')
const PACKAGES = [
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

function sha256(path) {
  return createHash('sha256').update(readFileSync(path)).digest('hex')
}

function collect() {
  const entries = []
  for (const pkg of PACKAGES) {
    const root = resolve(ROOT, pkg.source)
    if (!existsSync(root)) {
      throw new Error(`缺少官方包目录 ${pkg.source}`)
    }
    const packageJson = JSON.parse(
      readFileSync(resolve(root, 'package.json'), 'utf8'),
    )
    if (packageJson.name !== pkg.name) {
      throw new Error(`${pkg.source} 的 package name 是 ${packageJson.name}，期望 ${pkg.name}`)
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
  const check = process.argv.includes('--check') || !update
  const actual = collect()
  if (update) {
    writeFileSync(LOCK, `${JSON.stringify(actual, null, 2)}\n`)
    console.log(`[official-provenance] 已写入 ${LOCK}`)
    return
  }
  if (!check) {
    console.error('usage: node tools/official-provenance.mjs [--check|--update]')
    process.exit(2)
  }
  if (!existsSync(LOCK)) {
    console.error(`[official-provenance] 缺少 ${LOCK}；先运行 --update 并提交`)
    process.exit(1)
  }
  const expected = JSON.parse(readFileSync(LOCK, 'utf8'))
  const problems = []
  for (const wanted of expected.packages ?? []) {
    const got = actual.packages.find((pkg) => pkg.name === wanted.name)
    if (!got) {
      problems.push(`lock 中的 ${wanted.name} 在磁盘上不存在`)
      continue
    }
    if (got.version !== wanted.version) {
      problems.push(`${wanted.name} 版本 ${got.version} != lock ${wanted.version}`)
    }
    for (const [file, hash] of Object.entries(wanted.files ?? {})) {
      if (got.files[file] !== hash) {
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
