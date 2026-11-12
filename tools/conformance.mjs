#!/usr/bin/env node
// tools/conformance.mjs — 把 M3 gate 证据变成可复现命令。
//
// 设计要点（见 docs/conformance.md）：
//   * 测试 ID 注册表 docs/conformance.d/registry.json 是唯一事实源；
//     本 harness 不硬编码测试清单，只负责校验注册表、执行 suite、按 selector 判定。
//   * 零 npm 依赖；Node >= 20；Windows / macOS / Linux 均可运行。
//   * --quick 只跑 schema + links（秒级）；cargo 只有 --workspace / --full 才跑。
//
// 用法：
//   node tools/conformance.mjs --quick
//   node tools/conformance.mjs --quick --json docs/conformance-matrix.json --markdown docs/conformance-matrix.md
//   node tools/conformance.mjs --list
//   node tools/conformance.mjs --suite schema --suite links
//   node tools/conformance.mjs --workspace            # 额外跑 cargo test --offline --workspace
//   node tools/conformance.mjs --full                 # schema + links + cargo
//   node tools/conformance.mjs --full --timeout-ms 1800000
//
// 退出码：0 = 通过；1 = 有失败（或注册表非法）；2 = 用法错误。

import { spawn, spawnSync } from 'node:child_process'
import { closeSync, existsSync, mkdirSync, openSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
import { arch, platform, release } from 'node:os'
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const REPO_ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '..')
const REGISTRY_PATH = join(REPO_ROOT, 'docs', 'conformance.d', 'registry.json')
const DEFAULT_JSON_PATH = join(REPO_ROOT, 'docs', 'conformance-matrix.json')
const DEFAULT_MARKDOWN_PATH = join(REPO_ROOT, 'docs', 'conformance-matrix.md')

// docs/spec/README.md §3：<AREA>-<CATEGORY>-<NNN>，如 RPC-CANCEL-003、ADC-SLOT-011。
const TEST_ID_RE = /^[A-Z][A-Z0-9]*(?:-[A-Z0-9]+)+-\d{3}$/
const STATUSES = new Set(['implemented', 'pending', 'manual', 'blocked'])

// 已知 suite 名与执行方式。注册表 docs/conformance.d/registry.json 的 suites 必须与这里一致。
const SUITE_NAMES = ['schema', 'links', 'cargo']
const SUITE_COMMANDS = {
  schema: 'node schema/check.mjs',
  links: 'node tools/check-links.mjs',
  cargo: 'tools\\build-env.cmd cargo test --offline --workspace',
}
const DEFAULT_TIMEOUT_MS = {
  schema: 120000,
  links: 120000,
  cargo: 1200000, // 20 分钟
}

const USAGE = `tools/conformance.mjs — conformance 矩阵门禁

用法：
  node tools/conformance.mjs [--quick] [--suite <name>]... [--workspace] [--full]
                             [--json [path]] [--markdown [path]] [--timeout-ms <n>]
  node tools/conformance.mjs --list
  node tools/conformance.mjs --help

模式：
  --quick        schema + links（默认模式；秒级，不需要 Rust 工具链）
  --suite <n>    只跑指定 suite，可重复：${SUITE_NAMES.join(' / ')}
  --workspace    额外跑 cargo suite（tools/build-env.cmd cargo test --offline --workspace，默认 20 分钟）
  --full         全部 suite（schema + links + cargo）
  --list         列出注册表中的 suite 与测试 ID，不执行
  --json [p]     写机器可读矩阵（默认 docs/conformance-matrix.json）
  --markdown [p] 写人读矩阵（默认 docs/conformance-matrix.md）
  --timeout-ms n 覆盖本次所有 suite 的超时

退出码：0 通过；1 有失败或注册表非法；2 用法错误。pending/manual/blocked 不算失败。
`

// ---------------------------------------------------------------------------
// argv
// ---------------------------------------------------------------------------

function takeOptionalPath(argv, i) {
  const next = argv[i + 1]
  if (next !== undefined && !next.startsWith('-')) return { value: next, consumed: 2 }
  return { value: null, consumed: 1 }
}

function parseArgs(argv) {
  const opts = {
    quick: false,
    full: false,
    workspace: false,
    suites: [],
    list: false,
    help: false,
    jsonPath: null,
    markdownPath: null,
    timeoutMs: null,
  }
  for (let i = 0; i < argv.length; i += 1) {
    const arg = argv[i]
    if (arg === '--quick') opts.quick = true
    else if (arg === '--full') opts.full = true
    else if (arg === '--workspace') opts.workspace = true
    else if (arg === '--list') opts.list = true
    else if (arg === '--help' || arg === '-h') opts.help = true
    else if (arg === '--suite') {
      const value = argv[i + 1]
      if (!value || value.startsWith('-')) throw new UsageError('--suite 需要一个 suite 名')
      opts.suites.push(value)
      i += 1
    } else if (arg === '--json') {
      const { value, consumed } = takeOptionalPath(argv, i)
      opts.jsonPath = value ?? DEFAULT_JSON_PATH
      i += consumed - 1
    } else if (arg === '--markdown') {
      const { value, consumed } = takeOptionalPath(argv, i)
      opts.markdownPath = value ?? DEFAULT_MARKDOWN_PATH
      i += consumed - 1
    } else if (arg === '--timeout-ms') {
      const value = argv[i + 1]
      const parsed = Number(value)
      if (!value || !Number.isFinite(parsed) || parsed <= 0) {
        throw new UsageError('--timeout-ms 需要一个正整数毫秒数')
      }
      opts.timeoutMs = Math.floor(parsed)
      i += 1
    } else {
      throw new UsageError(`未知参数：${arg}`)
    }
  }
  return opts
}

class UsageError extends Error {}

// ---------------------------------------------------------------------------
// registry 加载与校验
// ---------------------------------------------------------------------------

function loadRegistry() {
  if (!existsSync(REGISTRY_PATH)) {
    throw new Error(`找不到注册表：${REGISTRY_PATH}`)
  }
  let raw
  try {
    raw = readFileSync(REGISTRY_PATH, 'utf8')
  } catch (err) {
    throw new Error(`读取注册表失败：${err.message}`)
  }
  try {
    return JSON.parse(raw)
  } catch (err) {
    throw new Error(`注册表不是合法 JSON：${err.message}`)
  }
}

function isPlainObject(value) {
  return typeof value === 'object' && value !== null && !Array.isArray(value)
}

function validateRegistry(registry) {
  const errors = []
  const add = (msg) => errors.push(msg)

  if (!isPlainObject(registry)) {
    add('注册表顶层必须是对象')
    return { tests: [], suites: {}, errors }
  }
  if (registry.schema_version !== 1) add(`schema_version 必须是 1（当前 ${JSON.stringify(registry.schema_version)}）`)
  if (typeof registry.id_spec !== 'string' || !registry.id_spec) add('id_spec 必须是非空字符串（指向 docs/spec/README.md §3）')

  // suites
  const suites = isPlainObject(registry.suites) ? registry.suites : {}
  if (!isPlainObject(registry.suites)) add('suites 必须是对象')
  for (const [name, meta] of Object.entries(suites)) {
    if (!SUITE_NAMES.includes(name)) {
      add(`suites.${name}: 未知 suite（harness 支持：${SUITE_NAMES.join(', ')}）`)
      continue
    }
    if (!isPlainObject(meta)) {
      add(`suites.${name}: 必须是对象`)
      continue
    }
    if (typeof meta.title !== 'string' || !meta.title) add(`suites.${name}.title 必须是非空字符串`)
    if (typeof meta.command !== 'string' || !meta.command) add(`suites.${name}.command 必须是非空字符串`)
    else if (meta.command !== SUITE_COMMANDS[name]) {
      add(`suites.${name}.command 与 harness 实现不一致：注册表 "${meta.command}"，harness "${SUITE_COMMANDS[name]}"`)
    }
  }
  for (const name of SUITE_NAMES) {
    if (!(name in suites)) add(`suites 缺少 harness 已知 suite "${name}"`)
  }

  // notes：可选，注册表级说明会原样进入矩阵报告的「说明与已知缺口」。
  let registryNotes = []
  if (registry.notes !== undefined) {
    if (!Array.isArray(registry.notes) || registry.notes.some((note) => typeof note !== 'string' || !note.trim())) {
      add('notes 必须是字符串数组（可选字段）')
    } else {
      registryNotes = registry.notes
    }
  }

  // tests
  if (!Array.isArray(registry.tests)) {
    add('tests 必须是数组')
    return { tests: [], suites, errors }
  }
  const seen = new Set()
  const tests = []
  registry.tests.forEach((test, index) => {
    const where = `tests[${index}]`
    if (!isPlainObject(test)) {
      add(`${where}: 必须是对象`)
      return
    }
    for (const field of ['id', 'area', 'title', 'status']) {
      if (typeof test[field] !== 'string' || !test[field]) add(`${where}.${field} 必须是非空字符串`)
    }
    if (typeof test.id !== 'string') return
    if (!TEST_ID_RE.test(test.id)) {
      add(`${where}.id "${test.id}" 不符合 <AREA>-<CATEGORY>-<NNN>（docs/spec/README.md §3）`)
    }
    if (seen.has(test.id)) add(`${where}.id "${test.id}" 重复`)
    seen.add(test.id)
    if (typeof test.area === 'string' && !test.id.startsWith(`${test.area}-`)) {
      add(`${where}.area "${test.area}" 与 id "${test.id}" 的首段不一致`)
    }
    if (typeof test.status === 'string' && !STATUSES.has(test.status)) {
      add(`${where}.status "${test.status}" 非法（允许：${[...STATUSES].join(', ')}）`)
    }
    if (!('suite' in test) || !('selector' in test) || !('notes' in test)) {
      add(`${where}: 必须包含 suite / selector / notes 字段（无证据时写 null）`)
    }
    if (test.status === 'implemented') {
      if (typeof test.suite !== 'string' || !test.suite) {
        add(`${where}.suite: implemented 的测试必须声明可执行来源 suite`)
      } else if (!SUITE_NAMES.includes(test.suite)) {
        add(`${where}.suite "${test.suite}" 是未知 suite（harness 支持：${SUITE_NAMES.join(', ')}）`)
      } else if (!(test.suite in suites)) {
        add(`${where}.suite "${test.suite}" 未在 suites 中声明`)
      }
      if (typeof test.selector !== 'string' || !test.selector.trim()) {
        add(`${where}.selector: implemented 的测试必须有 selector（suite 内可执行选择器）`)
      }
    } else if (test.suite !== null && test.suite !== undefined) {
      add(`${where}.suite: 非 implemented 的测试不应声明 suite（当前 status=${test.status}）`)
    } else if (test.selector !== null && test.selector !== undefined) {
      add(`${where}.selector: 非 implemented 的测试不应声明 selector（当前 status=${test.status}）`)
    }
    tests.push({
      id: test.id,
      area: test.area,
      title: test.title,
      status: test.status,
      suite: test.suite ?? null,
      selector: test.selector ?? null,
      notes: typeof test.notes === 'string' ? test.notes : '',
    })
  })
  return { tests, suites, errors, notes: registryNotes }
}

// ---------------------------------------------------------------------------
// 进程执行
//
// 注意：这里用临时文件而不是 pipe 捕获 stdout/stderr。原因有两条：
//   1. 某些受限环境（Windows 沙箱 / 容器）禁止子进程通过命名管道回传输出，
//      Node child_process 的 stdio:'pipe' 会直接 EPERM；文件句柄重定向可跨环境工作。
//   2. 文件捕获对长输出（cargo test）更稳，不会因为背压丢字节。
// ---------------------------------------------------------------------------

const RUN_TMP_DIR = join(REPO_ROOT, '.tmp', 'conformance')
let runCounter = 0

function killTree(child) {
  if (!child || child.pid === undefined) return
  if (process.platform === 'win32') {
    try {
      spawnSync('taskkill', ['/pid', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true })
    } catch {
      // 忽略：进程可能已经退出
    }
  }
  try {
    child.kill('SIGKILL')
  } catch {
    // 忽略
  }
}

function runProcess(command, args, { cwd, timeoutMs, env } = {}) {
  return new Promise((resolvePromise) => {
    const startedAt = Date.now()
    runCounter += 1
    mkdirSync(RUN_TMP_DIR, { recursive: true })
    const stem = `run-${process.pid}-${runCounter}-${startedAt}`
    const outPath = join(RUN_TMP_DIR, `${stem}.out`)
    const errPath = join(RUN_TMP_DIR, `${stem}.err`)

    let outFd
    let errFd
    try {
      outFd = openSync(outPath, 'w')
      errFd = openSync(errPath, 'w')
    } catch (err) {
      resolvePromise({
        code: null,
        signal: null,
        stdout: '',
        stderr: `无法创建输出捕获文件：${err.message}`,
        timedOut: false,
        spawnError: true,
        durationMs: 0,
      })
      return
    }

    let stdout = ''
    let stderr = ''
    let timedOut = false
    let settled = false
    let timer = null
    let graceTimer = null
    let child = null

    const readFileSafe = (target) => {
      try {
        return readFileSync(target, 'utf8')
      } catch {
        return ''
      }
    }
    const cleanup = () => {
      try {
        rmSync(outPath, { force: true })
        rmSync(errPath, { force: true })
      } catch {
        // 忽略清理失败
      }
    }

    const finish = (code, signal, spawnError = false, spawnMessage = '') => {
      if (settled) return
      settled = true
      if (timer) clearTimeout(timer)
      if (graceTimer) clearTimeout(graceTimer)
      try {
        closeSync(outFd)
      } catch {
        // 忽略
      }
      try {
        closeSync(errFd)
      } catch {
        // 忽略
      }
      stdout = readFileSafe(outPath)
      stderr = readFileSafe(errPath)
      if (spawnMessage) stderr += `\n${spawnMessage}`
      cleanup()
      resolvePromise({
        code,
        signal,
        stdout,
        stderr,
        timedOut,
        spawnError,
        durationMs: Date.now() - startedAt,
      })
    }

    if (Number.isFinite(timeoutMs) && timeoutMs > 0) {
      timer = setTimeout(() => {
        timedOut = true
        killTree(child)
        // 兜底：即使进程树拒绝退出，也在宽限后结算，避免 harness 永久挂起。
        graceTimer = setTimeout(() => finish(null, 'SIGKILL'), 5000)
      }, timeoutMs)
    }

    try {
      child = spawn(command, args, {
        cwd,
        env: env ?? process.env,
        windowsHide: true,
        stdio: ['ignore', outFd, errFd],
      })
    } catch (err) {
      finish(null, null, true, `spawn 失败：${err.message}`)
      return
    }

    child.on('error', (err) => {
      finish(null, null, true, `spawn 失败：${err.message}`)
    })
    child.on('close', (code, signal) => finish(code, signal))
  })
}

function cargoInvocation() {
  if (process.platform === 'win32' && existsSync(join(REPO_ROOT, 'tools', 'build-env.cmd'))) {
    // Windows 上必须经 build-env.cmd（本机 %TEMP% 不可写，见 docs/m0-runbook.md / ADR-0002）。
    // 用 cmd /c 执行 .cmd；命令串里用仓库相对路径，避免 cwd 含空格时的引号问题。
    return {
      command: process.env.ComSpec || 'cmd.exe',
      args: ['/d', '/s', '/c', 'tools\\build-env.cmd cargo test --offline --workspace'],
      label: 'tools\\build-env.cmd cargo test --offline --workspace',
    }
  }
  return {
    command: 'cargo',
    args: ['test', '--offline', '--workspace'],
    label: 'cargo test --offline --workspace',
  }
}

// ---------------------------------------------------------------------------
// suite 输出解析
// ---------------------------------------------------------------------------

function escapeRegExp(text) {
  return text.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')
}

function parseSchemaOkLines(stdout) {
  const lines = []
  for (const line of stdout.split(/\r?\n/)) {
    const match = line.match(/^ok\s+\[([^\]]+)\]\s+(.*)$/)
    if (match) lines.push({ schema: match[1], label: match[2].trim(), raw: `[${match[1]}] ${match[2].trim()}` })
  }
  return lines
}

function parseCargoSummary(output) {
  const lines = output.split(/\r?\n/)
  let suites = 0
  let passed = 0
  let failed = 0
  let ignored = 0
  for (const line of lines) {
    const match = line.match(/test result: (ok|FAILED)\.\s+(\d+) passed;\s+(\d+) failed;\s+(\d+) ignored/)
    if (!match) continue
    suites += 1
    passed += Number(match[2])
    failed += Number(match[3])
    ignored += Number(match[4])
  }
  return { suites, passed, failed, ignored }
}

function cargoPackageSeen(output, selector) {
  const normalized = selector.replace(/-/g, '_')
  const pattern = new RegExp(`^\\s*(?:Running|Doc-tests)[^\\n]*${escapeRegExp(normalized)}(?:[-\\s]|$)`, 'm')
  return pattern.test(output)
}

// ---------------------------------------------------------------------------
// suite 执行
// ---------------------------------------------------------------------------

async function runSuite(name, timeoutMs) {
  const startedAt = Date.now()
  let invocation
  if (name === 'schema' || name === 'links') {
    const script = name === 'schema' ? 'schema/check.mjs' : 'tools/check-links.mjs'
    invocation = {
      command: process.execPath,
      args: [script],
      label: SUITE_COMMANDS[name],
    }
  } else {
    invocation = cargoInvocation()
  }

  console.log(`[conformance] suite ${name}: 运行 ${invocation.label}（超时 ${Math.round(timeoutMs / 1000)}s）`)
  const result = await runProcess(invocation.command, invocation.args, {
    cwd: REPO_ROOT,
    timeoutMs,
  })
  const output = `${result.stdout}\n${result.stderr}`
  const durationMs = result.durationMs

  let summary = ''
  const details = {}
  if (name === 'schema') {
    details.okLines = parseSchemaOkLines(result.stdout)
    const match = output.match(/(\d+)\/(\d+) cases passed/)
    summary = match ? `${match[1]}/${match[2]} cases passed` : '未找到用例汇总行'
  } else if (name === 'links') {
    const match = output.match(/(\d+) link\(s\) checked, (\d+) broken, (\d+) file\(s\) scanned/)
    summary = match ? `${match[1]} link(s) checked, ${match[2]} broken, ${match[3]} file(s) scanned` : '未找到链接汇总行'
  } else {
    details.cargo = parseCargoSummary(output)
    summary = `${details.cargo.suites} suite(s), ${details.cargo.passed} passed, ${details.cargo.failed} failed, ${details.cargo.ignored} ignored`
  }

  let status
  if (result.spawnError) status = 'error'
  else if (result.timedOut) status = 'timeout'
  else if (result.code === 0) status = 'pass'
  else status = 'fail'

  const run = {
    name,
    title: '',
    command: invocation.label,
    status,
    exit_code: result.code,
    signal: result.signal ?? null,
    duration_ms: durationMs,
    timeout_ms: timeoutMs,
    summary,
    details,
    output,
    stdout: result.stdout,
    stderr: result.stderr,
    timedOut: result.timedOut,
  }
  console.log(
    `[conformance] suite ${name}: ${status.toUpperCase()}（${(durationMs / 1000).toFixed(2)}s）${summary ? `— ${summary}` : ''}`,
  )
  return run
}

// ---------------------------------------------------------------------------
// 逐 ID 判定
// ---------------------------------------------------------------------------

function makeResult(test, result, message, evidence) {
  return {
    id: test.id,
    area: test.area,
    title: test.title,
    status: test.status,
    suite: test.suite,
    selector: test.selector,
    result,
    evidence: evidence ?? (test.suite && test.selector ? `${test.suite}: ${test.selector}` : '—'),
    message,
    notes: test.notes,
  }
}

function evaluateTests(tests, suiteRuns) {
  const byName = new Map(suiteRuns.map((run) => [run.name, run]))
  return tests.map((test) => {
    if (test.status !== 'implemented') {
      const labels = { pending: '待实现（不计失败）', manual: '需人工验证（不计失败）', blocked: '被阻塞（不计失败）' }
      return makeResult(test, test.status, test.notes || labels[test.status] || '', '—')
    }

    const evidence = `${test.suite}: ${test.selector}`
    const run = byName.get(test.suite)
    if (!run) {
      return makeResult(
        test,
        'skip',
        `suite "${test.suite}" 未在本次调用中运行（用 --suite ${test.suite} / --workspace / --full 运行）`,
        evidence,
      )
    }
    if (run.status === 'timeout') {
      return makeResult(test, 'fail', `suite 超时（>${run.timeout_ms}ms）；${run.summary}`, evidence)
    }
    if (run.status === 'error') {
      return makeResult(test, 'fail', `suite 无法启动（${run.summary}）`, evidence)
    }

    if (test.suite === 'schema') {
      const hit = run.details.okLines.some((entry) => entry.raw === test.selector || entry.raw.includes(test.selector))
      if (hit) {
        const overall = run.status === 'pass' ? 'suite 通过' : '该用例输出 ok，但 schema suite 整体失败'
        return makeResult(test, 'pass', `${overall}（${run.summary}）`, evidence)
      }
      return makeResult(
        test,
        'fail',
        `selector 未在 schema 输出中找到（用例可能被改名/删除；${run.summary}）`,
        evidence,
      )
    }

    if (test.suite === 'links') {
      return run.status === 'pass'
        ? makeResult(test, 'pass', `链接检查通过（${run.summary}）`, evidence)
        : makeResult(test, 'fail', `链接检查失败（${run.summary}）`, evidence)
    }

    if (test.suite === 'cargo') {
      if (run.status !== 'pass') {
        return makeResult(test, 'fail', `cargo suite ${run.status}（${run.summary}）`, evidence)
      }
      if (test.selector === 'all') {
        return makeResult(test, 'pass', `cargo workspace 通过（${run.summary}）`, evidence)
      }
      if (cargoPackageSeen(run.output, test.selector)) {
        return makeResult(test, 'pass', `包 ${test.selector} 的测试二进制出现在 cargo 输出中（${run.summary}）`, evidence)
      }
      return makeResult(test, 'fail', `包 ${test.selector} 未在 cargo 输出中出现（包名错误或该包无单测）`, evidence)
    }

    return makeResult(test, 'fail', `harness 未实现 suite "${test.suite}" 的判定逻辑`, evidence)
  })
}

// ---------------------------------------------------------------------------
// 报告
// ---------------------------------------------------------------------------

async function collectGitInfo() {
  const runGit = async (args) => {
    try {
      // 与 suite 一样走文件捕获，避免受限环境禁止 pipe 时 spawnSync 直接 EPERM。
      const result = await runProcess('git', args, { cwd: REPO_ROOT, timeoutMs: 30000 })
      if (result.spawnError || result.code !== 0) return null
      return result.stdout.trim()
    } catch {
      return null
    }
  }
  const head = await runGit(['rev-parse', 'HEAD'])
  const branch = await runGit(['rev-parse', '--abbrev-ref', 'HEAD'])
  const status = await runGit(['status', '--porcelain'])
  const dirtyFiles = status ? status.split(/\r?\n/).filter(Boolean) : []
  return {
    head: head || null,
    head_short: head ? head.slice(0, 7) : null,
    branch: branch || null,
    dirty: dirtyFiles.length > 0,
    dirty_files: dirtyFiles.length,
  }
}

function relativeToRepo(target) {
  const rel = relative(REPO_ROOT, target)
  return rel && !rel.startsWith('..') ? rel.replace(/\\/g, '/') : target
}

function displayCommand(opts, suites) {
  const parts = ['node', 'tools/conformance.mjs']
  if (opts.quick) parts.push('--quick')
  if (opts.full) parts.push('--full')
  if (opts.workspace) parts.push('--workspace')
  for (const name of opts.suites) parts.push('--suite', name)
  if (opts.jsonPath && opts.jsonPath !== DEFAULT_JSON_PATH) parts.push('--json', relativeToRepo(opts.jsonPath))
  if (opts.markdownPath && opts.markdownPath !== DEFAULT_MARKDOWN_PATH) {
    parts.push('--markdown', relativeToRepo(opts.markdownPath))
  }
  if (opts.timeoutMs) parts.push('--timeout-ms', String(opts.timeoutMs))
  if (parts.length === 2 && suites.length) parts.push('--quick')
  return parts.join(' ')
}

function buildStats(tests) {
  const stats = {
    total: tests.length,
    implemented: 0,
    implemented_passed: 0,
    implemented_failed: 0,
    implemented_skipped: 0,
    pending: 0,
    manual: 0,
    blocked: 0,
    failed: 0,
  }
  for (const test of tests) {
    if (test.status === 'implemented') {
      stats.implemented += 1
      if (test.result === 'pass') stats.implemented_passed += 1
      else if (test.result === 'fail') stats.implemented_failed += 1
      else stats.implemented_skipped += 1
    } else if (test.status === 'pending') stats.pending += 1
    else if (test.status === 'manual') stats.manual += 1
    else if (test.status === 'blocked') stats.blocked += 1
  }
  stats.failed = stats.implemented_failed
  return stats
}

function publicSuite(run) {
  const output = run.status === 'pass' ? '' : run.output.trim().split(/\r?\n/).slice(-40).join('\n')
  return {
    name: run.name,
    title: run.title,
    command: run.command,
    status: run.status,
    exit_code: run.exit_code,
    duration_ms: run.duration_ms,
    timeout_ms: run.timeout_ms,
    summary: run.summary,
    output_tail: output || null,
  }
}

function buildReport({ opts, suiteRuns, evaluated, stats, exitCode, registry, registryPath, git, registryNotes }) {
  const suitesRun = suiteRuns.map((run) => run.name)
  return {
    schema_version: 1,
    tool: 'tools/conformance.mjs',
    generated_at: new Date().toISOString(),
    invocation: {
      mode: opts.full ? 'full' : opts.workspace ? 'workspace' : opts.suites.length ? 'suite' : 'quick',
      command: displayCommand(opts, suitesRun),
      argv: process.argv.slice(2),
      suites_run: suitesRun,
    },
    environment: {
      platform: process.platform,
      os_release: release(),
      arch: arch(),
      node: process.version,
      cwd: process.cwd(),
      repo_root: REPO_ROOT,
    },
    git,
    registry: {
      path: relativeToRepo(registryPath),
      schema_version: registry.schema_version,
      tests: evaluated.length,
      suites: Object.keys(registry.suites ?? {}),
    },
    suites: suiteRuns.map(publicSuite),
    stats,
    tests: evaluated,
    notes: [
      ...(registryNotes ?? []),
      'quick 模式不运行 cargo suite：CARGO-UNIT-* 显示 SKIP 属正常，需 --workspace 或 --full。',
      'cargo suite 在 Windows 上经 tools/build-env.cmd 调用（cargo test --offline --workspace）。',
      '.github/workflows/conformance.yml 未在 GitHub Actions 实跑，本报告由本机命令生成。',
      '本文件由 tools/conformance.mjs 生成，请勿手改；新增证据请改 docs/conformance.d/registry.json。',
    ],
    exit_code: exitCode,
  }
}

function markdownEscape(text) {
  return String(text ?? '')
    .replace(/\|/g, '\\|')
    .replace(/\r?\n/g, ' ')
    .trim()
}

function renderMarkdown(report) {
  const lines = []
  lines.push('# Conformance Matrix（自动生成）')
  lines.push('')
  lines.push('> 本文件由 `tools/conformance.mjs` 生成，请勿手改；测试清单事实源是 `docs/conformance.d/registry.json`。')
  lines.push('> **CI 状态**：`.github/workflows/conformance.yml` 尚未在 GitHub Actions 实跑；本报告由本机命令生成。')
  lines.push('')
  lines.push('## 元数据')
  lines.push('')
  lines.push('| 字段 | 值 |')
  lines.push('|---|---|')
  lines.push(`| generated_at | ${markdownEscape(report.generated_at)} |`)
  lines.push(
    `| git HEAD | ${markdownEscape(report.git.head ?? 'unknown')}（${report.git.dirty ? `dirty，${report.git.dirty_files} 个文件未提交` : 'clean'}） |`,
  )
  lines.push(`| git branch | ${markdownEscape(report.git.branch ?? 'unknown')} |`)
  lines.push(`| platform | ${markdownEscape(`${report.environment.platform} ${report.environment.os_release} ${report.environment.arch}`)} |`)
  lines.push(`| node | ${markdownEscape(report.environment.node)} |`)
  lines.push(`| invocation | ${markdownEscape(report.invocation.mode)} — \`${markdownEscape(report.invocation.command)}\` |`)
  lines.push(`| registry | ${markdownEscape(report.registry.path)}（schema_version=${report.registry.schema_version}，${report.registry.tests} 条测试） |`)
  lines.push('')
  lines.push('## 统计')
  lines.push('')
  lines.push('| 指标 | 数量 |')
  lines.push('|---|---:|')
  lines.push(`| 总测试 ID | ${report.stats.total} |`)
  lines.push(`| implemented | ${report.stats.implemented} |`)
  lines.push(`| implemented passed | ${report.stats.implemented_passed} |`)
  lines.push(`| implemented failed | ${report.stats.implemented_failed} |`)
  lines.push(`| implemented skipped | ${report.stats.implemented_skipped} |`)
  lines.push(`| pending | ${report.stats.pending} |`)
  lines.push(`| manual | ${report.stats.manual} |`)
  lines.push(`| blocked | ${report.stats.blocked} |`)
  lines.push(`| **failed（导致退出码 1）** | **${report.stats.failed}** |`)
  lines.push('')
  lines.push('## Suite 结果')
  lines.push('')
  if (report.suites.length === 0) {
    lines.push('本次调用未运行任何 suite（`--list` 模式或全部被跳过）。')
  } else {
    lines.push('| suite | 状态 | 退出码 | 耗时 | 命令 | 摘要 |')
    lines.push('|---|---|---:|---:|---|---|')
    for (const suite of report.suites) {
      const seconds = (suite.duration_ms / 1000).toFixed(2)
      lines.push(
        `| ${markdownEscape(suite.name)} | ${suite.status} | ${suite.exit_code ?? '—'} | ${seconds}s | \`${markdownEscape(suite.command)}\` | ${markdownEscape(suite.summary)} |`,
      )
    }
  }
  lines.push('')
  lines.push('## 测试矩阵')
  lines.push('')
  const resultLabel = {
    pass: 'PASS',
    fail: 'FAIL',
    skip: 'SKIP',
    pending: 'PENDING',
    manual: 'MANUAL',
    blocked: 'BLOCKED',
  }
  lines.push('| ID | 领域 | 标题 | 状态 | 结果 | 证据 | 消息 |')
  lines.push('|---|---|---|---|---|---|---|')
  for (const test of report.tests) {
    lines.push(
      `| ${test.id} | ${markdownEscape(test.area)} | ${markdownEscape(test.title)} | ${test.status} | ${resultLabel[test.result] ?? test.result} | ${markdownEscape(test.evidence)} | ${markdownEscape(test.message)} |`,
    )
  }
  lines.push('')
  lines.push('## 说明与已知缺口')
  lines.push('')
  for (const note of report.notes) lines.push(`- ${note}`)
  lines.push('')
  lines.push(`退出码：\`${report.exit_code}\`。pending / manual / blocked 不计失败。`)
  lines.push('')
  return `${lines.join('\n')}\n`
}

function writeFileAt(targetPath, content) {
  const absolute = isAbsolute(targetPath) ? targetPath : resolve(process.cwd(), targetPath)
  try {
    mkdirSync(dirname(absolute), { recursive: true })
    writeFileSync(absolute, content, 'utf8')
    console.log(`[conformance] 已写入 ${relativeToRepo(absolute)}`)
    return { ok: true, path: absolute, fallback: null }
  } catch (err) {
    // 目标目录可能被沙箱/CI 设为只读。不要把已算好的矩阵丢掉：回退写进 .tmp，
    // 并给出可操作的提示；CI 侧另有 artifact 上传步骤兜底（if-no-files-found: error）。
    const fallback = join(RUN_TMP_DIR, `fallback-${Date.now()}-${basename(absolute)}`)
    const reason = err?.code ? `${err.code}: ${err.message}` : String(err?.message ?? err)
    console.error(`[conformance] 警告：写入 ${relativeToRepo(absolute)} 失败（${reason}）`)
    try {
      mkdirSync(RUN_TMP_DIR, { recursive: true })
      writeFileSync(fallback, content, 'utf8')
      console.error(`[conformance] 已回退写入 ${relativeToRepo(fallback)}；请在有写权限的会话重跑，或用文件工具复制到目标路径。`)
      return { ok: false, path: absolute, fallback, error: reason }
    } catch (fallbackErr) {
      console.error(`[conformance] 回退写入也失败：${fallbackErr?.message ?? fallbackErr}`)
      return { ok: false, path: absolute, fallback: null, error: reason }
    }
  }
}

// ---------------------------------------------------------------------------
// --list
// ---------------------------------------------------------------------------

function listRegistry(registry, validated) {
  console.log(`[conformance] registry: ${relativeToRepo(REGISTRY_PATH)}`)
  const stats = { implemented: 0, pending: 0, manual: 0, blocked: 0 }
  for (const test of validated.tests) stats[test.status] += 1
  console.log(
    `[conformance] ${validated.tests.length} 个测试 ID：implemented=${stats.implemented} pending=${stats.pending} manual=${stats.manual} blocked=${stats.blocked}`,
  )
  console.log('[conformance] suites：')
  for (const name of SUITE_NAMES) {
    const meta = registry.suites?.[name]
    const quick = meta?.quick ? 'quick' : 'slow '
    console.log(`  ${name.padEnd(7)} ${quick}  ${meta?.command ?? SUITE_COMMANDS[name]}`)
  }
  console.log('[conformance] tests：')
  console.log(`  ${'ID'.padEnd(22)} ${'STATUS'.padEnd(12)} ${'SUITE'.padEnd(7)} SELECTOR`)
  for (const test of validated.tests) {
    const selector = test.selector ?? (test.status === 'implemented' ? '—' : '—')
    console.log(`  ${test.id.padEnd(22)} ${test.status.padEnd(12)} ${String(test.suite ?? '—').padEnd(7)} ${selector}`)
  }
}

// ---------------------------------------------------------------------------
// main
// ---------------------------------------------------------------------------

async function main() {
  let opts
  try {
    opts = parseArgs(process.argv.slice(2))
  } catch (err) {
    if (err instanceof UsageError) {
      console.error(`[conformance] 错误：${err.message}\n`)
      console.error(USAGE)
      process.exitCode = 2
      return
    }
    throw err
  }

  if (opts.help) {
    console.log(USAGE)
    return
  }

  let registry
  try {
    registry = loadRegistry()
  } catch (err) {
    console.error(`[conformance] 错误：${err.message}`)
    process.exitCode = 1
    return
  }
  const validated = validateRegistry(registry)
  if (validated.errors.length > 0) {
    console.error(`[conformance] 注册表校验失败（${validated.errors.length} 个错误）：`)
    for (const error of validated.errors) console.error(`  - ${error}`)
    process.exitCode = 1
    return
  }
  console.log(
    `[conformance] 注册表 OK：${validated.tests.length} 个测试 ID（implemented=${validated.tests.filter((t) => t.status === 'implemented').length}，pending=${validated.tests.filter((t) => t.status === 'pending').length}，manual=${validated.tests.filter((t) => t.status === 'manual').length}，blocked=${validated.tests.filter((t) => t.status === 'blocked').length}）`,
  )

  if (opts.list) {
    listRegistry(registry, validated)
    return
  }

  // 选择 suite：--full > --suite/--workspace > 默认/--quick。
  let selected
  if (opts.full) {
    selected = new Set(SUITE_NAMES)
  } else {
    selected = new Set()
    if (opts.quick) {
      selected.add('schema')
      selected.add('links')
    }
    if (opts.workspace) selected.add('cargo')
    for (const name of opts.suites) {
      if (!SUITE_NAMES.includes(name)) {
        console.error(`[conformance] 错误：未知 suite "${name}"（支持：${SUITE_NAMES.join(', ')}）`)
        process.exitCode = 2
        return
      }
      selected.add(name)
    }
    if (selected.size === 0) {
      // 裸调用等价 quick；显式提示，避免误以为跑了全量。
      selected.add('schema')
      selected.add('links')
      console.log('[conformance] 未指定模式，按 --quick 执行（schema + links）。')
    }
  }

  const suiteRuns = []
  for (const name of SUITE_NAMES.filter((entry) => selected.has(entry))) {
    const timeoutMs = opts.timeoutMs ?? DEFAULT_TIMEOUT_MS[name]
    const run = await runSuite(name, timeoutMs)
    run.title = registry.suites?.[name]?.title ?? ''
    suiteRuns.push(run)
  }

  const evaluated = evaluateTests(validated.tests, suiteRuns)
  const stats = buildStats(evaluated)
  const suitesFailed = suiteRuns.filter((run) => run.status !== 'pass').length
  const exitCode = stats.implemented_failed > 0 || suitesFailed > 0 ? 1 : 0

  const git = await collectGitInfo()
  const report = buildReport({
    opts,
    suiteRuns,
    evaluated,
    stats,
    exitCode,
    registry,
    registryPath: REGISTRY_PATH,
    git,
    registryNotes: validated.notes,
  })

  let reportWriteFailures = 0
  if (opts.jsonPath) {
    const outcome = writeFileAt(opts.jsonPath, `${JSON.stringify(report, null, 2)}\n`)
    if (!outcome.ok) reportWriteFailures += 1
  }
  if (opts.markdownPath) {
    const outcome = writeFileAt(opts.markdownPath, renderMarkdown(report))
    if (!outcome.ok) reportWriteFailures += 1
  }

  // 退出码只由测试结果决定；报告写不进去会打印警告并回退到 .tmp。CI 由 artifact
  // 上传步骤（if-no-files-found: error）兜底，避免把“没写文件”误当成功。
  const finalExitCode = exitCode
  console.log(
    `[conformance] 结果：implemented ${stats.implemented_passed}/${stats.implemented} 通过，失败 ${stats.implemented_failed}，跳过 ${stats.implemented_skipped}；pending=${stats.pending} manual=${stats.manual} blocked=${stats.blocked}；退出码 ${finalExitCode}`,
  )
  if (reportWriteFailures > 0) {
    console.error(`[conformance] 注意：${reportWriteFailures} 个矩阵报告未能写入目标路径（已回退 .tmp/conformance/）。`)
  }
  process.exitCode = finalExitCode
}

main().catch((err) => {
  console.error(`[conformance] 未预期错误：${err?.stack ?? err}`)
  process.exitCode = 1
})
