#!/usr/bin/env node
// G4 性能基线门禁：跑 ADC 构造预算测试，解析 ADC-PERF-JSON / ADC-PERF-MAX-QUOTA，
// 对照阈值并可选落盘基线。CI 与本地用同一条命令，避免"脚本是手动、CI 没跑"。
//
// 用法：
//   node tools/perf-gate.mjs
//   node tools/perf-gate.mjs --json docs/perf-baseline.json
//   node tools/perf-gate.mjs --limit 50 --max-quota-limit 16.7
//
// 退出码：0 = 全部阈值通过；1 = 测试失败/缺 JSON/超阈值。

import { spawnSync } from 'node:child_process'
import { mkdirSync, writeFileSync } from 'node:fs'
import { dirname, resolve } from 'node:path'
import process from 'node:process'

const ROOT = resolve(import.meta.dirname, '..')
const TEST_ARGS = [
  'test',
  '--offline',
  '-p',
  'dshr-ui-gpui',
  '--test',
  'adc_perf',
  '--',
  '--nocapture',
]

function parseArgs(argv) {
  const options = { json: null, limit: 50, maxQuotaLimit: 16.7, keepOutput: false }
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i]
    if (arg === '--json') options.json = argv[++i]
    else if (arg === '--limit') options.limit = Number(argv[++i])
    else if (arg === '--max-quota-limit') options.maxQuotaLimit = Number(argv[++i])
    else if (arg === '--keep-output') options.keepOutput = true
    else if (arg === '--help' || arg === '-h') {
      console.log(
        'usage: node tools/perf-gate.mjs [--json <path>] [--limit <ms>] [--max-quota-limit <ms>]',
      )
      process.exit(0)
    } else {
      console.error(`[perf-gate] 未知参数: ${arg}`)
      process.exit(2)
    }
  }
  if (!Number.isFinite(options.limit) || options.limit <= 0) {
    console.error('[perf-gate] --limit 必须是正数')
    process.exit(2)
  }
  if (!Number.isFinite(options.maxQuotaLimit) || options.maxQuotaLimit <= 0) {
    console.error('[perf-gate] --max-quota-limit 必须是正数')
    process.exit(2)
  }
  return options
}

function runTests() {
  const isWindows = process.platform === 'win32'
  const command = isWindows ? 'cmd.exe' : 'cargo'
  const args = isWindows
    ? ['/d', '/s', '/c', `tools\\build-env.cmd cargo ${TEST_ARGS.map(quoteIfNeeded).join(' ')}`]
    : TEST_ARGS
  const started = Date.now()
  const result = spawnSync(command, args, {
    cwd: ROOT,
    encoding: 'utf8',
    timeout: 15 * 60 * 1000,
    maxBuffer: 16 * 1024 * 1024,
  })
  const elapsedMs = Date.now() - started
  if (result.error) throw result.error
  const stdout = result.stdout ?? ''
  const stderr = result.stderr ?? ''
  return { status: result.status ?? 1, stdout, stderr, elapsedMs }
}

function quoteIfNeeded(value) {
  return /[\s"]/.test(value) ? `"${value.replaceAll('"', '""')}"` : value
}

function parseJsonLine(stdout, prefix) {
  const line = stdout
    .split(/\r?\n/)
    .find((candidate) => candidate.startsWith(prefix))
  if (!line) return null
  try {
    return JSON.parse(line.slice(prefix.length))
  } catch (error) {
    console.error(`[perf-gate] 解析 ${prefix.trim()} 失败: ${error.message}`)
    return null
  }
}

function main() {
  const options = parseArgs(process.argv.slice(2))
  const run = runTests()
  if (options.keepOutput) {
    console.log(run.stdout)
    if (run.stderr) console.error(run.stderr)
  }

  const perf = parseJsonLine(run.stdout, 'ADC-PERF-JSON ')
  const maxQuota = parseJsonLine(run.stdout, 'ADC-PERF-MAX-QUOTA ')
  const failures = []
  if (run.status !== 0) {
    failures.push(`cargo test 退出码 ${run.status}（${run.elapsedMs}ms）`)
  }
  if (!perf) failures.push('未找到 ADC-PERF-JSON 输出')
  if (!maxQuota) failures.push('未找到 ADC-PERF-MAX-QUOTA 输出')

  if (perf) {
    if (perf.worst_total_p95_ms > options.limit) {
      failures.push(
        `最差树 total_p95=${perf.worst_total_p95_ms.toFixed(3)}ms > ${options.limit}ms`,
      )
    }
    for (const tree of perf.trees ?? []) {
      if (tree.a11y_missing > 0) {
        failures.push(`${tree.tree} 有 ${tree.a11y_missing} 个 a11y 缺失`)
      }
    }
  }
  if (maxQuota && maxQuota.total_p95_ms > options.maxQuotaLimit) {
    failures.push(
      `满配额树 total_p95=${maxQuota.total_p95_ms.toFixed(3)}ms > ${options.maxQuotaLimit}ms`,
    )
  }

  const baseline = {
    schema_version: 1,
    tool: 'tools/perf-gate.mjs',
    generated_at: new Date().toISOString(),
    platform: process.platform,
    arch: process.arch,
    elapsed_ms: run.elapsedMs,
    thresholds: { worst_total_p95_ms: options.limit, max_quota_total_p95_ms: options.maxQuotaLimit },
    perf,
    max_quota: maxQuota,
    ok: failures.length === 0,
    failures,
  }
  if (options.json) {
    const target = resolve(ROOT, options.json)
    mkdirSync(dirname(target), { recursive: true })
    writeFileSync(target, `${JSON.stringify(baseline, null, 2)}\n`)
    console.log(`[perf-gate] 基线已写入 ${target}`)
  }

  if (perf) {
    console.log(
      `[perf-gate] worst_total_p95=${perf.worst_total_p95_ms.toFixed(3)}ms (limit ${options.limit}ms), trees=${perf.trees?.length ?? 0}`,
    )
  }
  if (maxQuota) {
    console.log(
      `[perf-gate] max_quota_total_p95=${maxQuota.total_p95_ms.toFixed(3)}ms (limit ${options.maxQuotaLimit}ms), nodes=${maxQuota.nodes}`,
    )
  }
  if (failures.length > 0) {
    for (const failure of failures) console.error(`[perf-gate] FAIL: ${failure}`)
    console.error('[perf-gate] FAIL')
    process.exit(1)
  }
  console.log('[perf-gate] PASS')
}

main()
