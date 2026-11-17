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
import { closeSync, existsSync, mkdirSync, openSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs'
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
const SUITE_NAMES = ['schema', 'links', 'node-test', 'cargo']
const SUITE_COMMANDS = {
  schema: 'node schema/check.mjs',
  links: 'node tools/check-links.mjs',
  // sidecar JS 单测（node:test）。--test-isolation=none 避免受限环境禁止子进程隔离。
  'node-test':
    'node --test --test-isolation=none sidecar/dsh-official/lib/host-services.test.mjs sidecar/dsh-official/lib/session-projections.test.mjs',
  // --no-fail-fast：workspace 里一个包失败时仍跑完其余 target，避免 cargo 中止后
  // 后续包（如 ui-*）被误判为“未运行/失败”，也便于逐包判定。
  cargo: 'tools\\build-env.cmd cargo test --offline --workspace --no-fail-fast',
}
const DEFAULT_TIMEOUT_MS = {
  schema: 120000,
  links: 120000,
  'node-test': 120000,
  cargo: 1200000, // 20 分钟
}

// 矩阵与 --list 的领域排序（便于 300+ 条 ID 的表格按领域阅读）。
const AREA_ORDER = ['SCHEMA', 'DOC', 'CARGO', 'ADC', 'CPP', 'RPC', 'MCP', 'JS', 'M3', 'M4', 'SELF', 'GATE']
function compareTests(a, b) {
  const ai = AREA_ORDER.indexOf(a.area)
  const bi = AREA_ORDER.indexOf(b.area)
  if (ai !== bi) return (ai < 0 ? AREA_ORDER.length : ai) - (bi < 0 ? AREA_ORDER.length : bi)
  return a.id.localeCompare(b.id)
}

const USAGE = `tools/conformance.mjs — conformance 矩阵门禁

用法：
  node tools/conformance.mjs [--quick] [--suite <name>]... [--workspace] [--full]
                             [--json [path]] [--markdown [path]] [--timeout-ms <n>]
  node tools/conformance.mjs --list
  node tools/conformance.mjs --help

模式：
  --quick        schema + links + node-test（默认模式；秒级，不需要 Rust 工具链）
  --suite <n>    只跑指定 suite，可重复：${SUITE_NAMES.join(' / ')}
  --workspace    额外跑 cargo suite（tools/build-env.cmd cargo test --offline --workspace，默认 20 分钟）
  --full         全部 suite（schema + links + cargo）
  --list         列出注册表中的 suite 与测试 ID，不执行
  --compact      矩阵用紧凑模式（逐 ID 行只保留 ID/领域/状态/结果/证据/消息，去掉 title/notes）
  --registry <p> 使用指定注册表（默认 docs/conformance.d/registry.json）
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
    compact: false,
    registryPath: null,
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
    else if (arg === '--compact') opts.compact = true
    else if (arg === '--help' || arg === '-h') opts.help = true
    else if (arg === '--registry') {
      const value = argv[i + 1]
      if (!value || value.startsWith('-')) throw new UsageError('--registry 需要一个注册表路径')
      opts.registryPath = value
      i += 1
    } else if (arg === '--suite') {
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

function loadRegistry(registryPath = REGISTRY_PATH) {
  if (!existsSync(registryPath)) {
    throw new Error(`找不到注册表：${registryPath}`)
  }
  let raw
  try {
    raw = readFileSync(registryPath, 'utf8')
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

// 从测试函数名推导测试 ID：cpp_fiber_001_xxx → CPP-FIBER-001、adc_slot_001_xxx → ADC-SLOT-001、
// rpc_l2_101_xxx → RPC-L2-101。ui-gpui / profile 的测试用注释标注 ID，由扫描器关联到下一条 fn。
function idFromTestFn(fnName) {
  const match = fnName.match(/^(cpp|adc|rpc|m3|m4)_([a-z0-9]+)_(\d{3})_/)
  if (!match) return null
  return `${match[1].toUpperCase()}-${match[2].toUpperCase()}-${match[3]}`
}

// 扫描 source_dirs 下的 .rs 文件，建立 测试 ID -> { title: 测试函数名, file: 相对路径 } 映射。
// 两种标注方式都支持：函数名内编码（cpp_fiber_001_*）与注释标注（// ADC-ELEM-SPACING-001）。
function scanTestIds(sourceDirs) {
  const result = new Map()
  const files = []
  const walk = (dir) => {
    for (const entry of readdirSync(dir, { withFileTypes: true })) {
      const full = join(dir, entry.name)
      if (entry.isDirectory()) {
        if (entry.name === 'target' || entry.name === 'node_modules') continue
        walk(full)
      } else if (entry.name.endsWith('.rs')) {
        files.push(full)
      }
    }
  }
  for (const dir of sourceDirs) {
    const absolute = resolve(REPO_ROOT, dir)
    if (!existsSync(absolute)) continue
    walk(absolute)
  }
  const put = (id, value) => {
    if (!result.has(id)) result.set(id, value)
  }
  for (const file of files) {
    const rel = relativeToRepo(file)
    const lines = readFileSync(file, 'utf8').split(/\r?\n/)
    let pendingId = null
    for (const line of lines) {
      const comment = line.match(/^\s*\/{2,3}\s*((?:ADC|CPP|RPC|M3|M4)(?:-[A-Z0-9]+)+-\d{3})\b/)
      if (comment) pendingId = comment[1]
      const fn = line.match(/^\s*(?:pub\s+)?fn\s+([a-z0-9_]+)\s*\(/)
      if (!fn) continue
      const fnName = fn[1]
      const derived = idFromTestFn(fnName)
      if (derived) put(derived, { title: fnName, file: rel })
      else if (pendingId) put(pendingId, { title: fnName, file: rel })
      pendingId = null
    }
  }
  return result
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
  const acceptTest = (raw, where) => {
    if (!isPlainObject(raw)) {
      add(`${where}: 必须是对象`)
      return
    }
    for (const field of ['id', 'area', 'title', 'status']) {
      if (typeof raw[field] !== 'string' || !raw[field]) add(`${where}.${field} 必须是非空字符串`)
    }
    if (typeof raw.id !== 'string') return
    if (!TEST_ID_RE.test(raw.id)) {
      add(`${where}.id "${raw.id}" 不符合 <AREA>-<CATEGORY>-<NNN>（docs/spec/README.md §3）`)
    }
    if (seen.has(raw.id)) add(`${where}.id "${raw.id}" 重复`)
    seen.add(raw.id)
    if (typeof raw.area === 'string' && !raw.id.startsWith(`${raw.area}-`)) {
      add(`${where}.area "${raw.area}" 与 id "${raw.id}" 的首段不一致`)
    }
    if (typeof raw.status === 'string' && !STATUSES.has(raw.status)) {
      add(`${where}.status "${raw.status}" 非法（允许：${[...STATUSES].join(', ')}）`)
    }
    if (!('suite' in raw) || !('selector' in raw) || !('notes' in raw)) {
      add(`${where}: 必须包含 suite / selector / notes 字段（无证据时写 null）`)
    }
    if (raw.status === 'implemented') {
      if (typeof raw.suite !== 'string' || !raw.suite) {
        add(`${where}.suite: implemented 的测试必须声明可执行来源 suite`)
      } else if (!SUITE_NAMES.includes(raw.suite)) {
        add(`${where}.suite "${raw.suite}" 是未知 suite（harness 支持：${SUITE_NAMES.join(', ')}）`)
      } else if (!(raw.suite in suites)) {
        add(`${where}.suite "${raw.suite}" 未在 suites 中声明`)
      }
      if (typeof raw.selector !== 'string' || !raw.selector.trim()) {
        add(`${where}.selector: implemented 的测试必须有 selector（suite 内可执行选择器）`)
      }
    } else if (raw.suite !== null && raw.suite !== undefined) {
      add(`${where}.suite: 非 implemented 的测试不应声明 suite（当前 status=${raw.status}）`)
    } else if (raw.selector !== null && raw.selector !== undefined) {
      add(`${where}.selector: 非 implemented 的测试不应声明 selector（当前 status=${raw.status}）`)
    }
    tests.push({
      id: raw.id,
      area: raw.area,
      title: raw.title,
      status: raw.status,
      suite: raw.suite ?? null,
      selector: raw.selector ?? null,
      notes: typeof raw.notes === 'string' ? raw.notes : '',
    })
  }
  registry.tests.forEach((test, index) => acceptTest(test, `tests[${index}]`))

  // groups：批量生成条目（本轮 200+ 条已实现测试用）。组声明 source_dirs + ids，
  // harness 扫描 source_dirs 中的测试函数（函数名内编码，或注释标注），据此校验每个
  // ID 确实有测试来源并取函数名作为 title；展开后与显式 tests 同等参与校验。
  if (registry.groups !== undefined) {
    if (!Array.isArray(registry.groups)) {
      add('groups 必须是数组（可选字段）')
    } else {
      registry.groups.forEach((group, index) => {
        const where = `groups[${index}]`
        if (!isPlainObject(group)) {
          add(`${where}: 必须是对象`)
          return
        }
        if (typeof group.name !== 'string' || !group.name) add(`${where}.name 必须是非空字符串`)
        if (typeof group.area !== 'string' || !group.area) add(`${where}.area 必须是非空字符串`)
        if (group.status !== 'implemented') {
          add(`${where}.status 目前只支持 implemented（批量组必须有可执行证据；pending/manual 请写显式 tests）`)
        }
        if (typeof group.suite !== 'string' || !SUITE_NAMES.includes(group.suite)) {
          add(`${where}.suite "${group.suite}" 是未知 suite（harness 支持：${SUITE_NAMES.join(', ')}）`)
        }
        if (typeof group.selector !== 'string' || !group.selector.trim()) {
          add(`${where}.selector 必须是非空字符串（cargo selector：包名或 all）`)
        }
        if (typeof group.notes !== 'string' || !group.notes) add(`${where}.notes 必须是非空字符串`)
        if (!Array.isArray(group.source_dirs) || group.source_dirs.length === 0) {
          add(`${where}.source_dirs 必须是非空字符串数组（测试所在目录）`)
          return
        }
        if (!Array.isArray(group.ids) || group.ids.length === 0) {
          add(`${where}.ids 必须是非空 ID 数组`)
          return
        }
        const scanned = scanTestIds(group.source_dirs)
        if (scanned.size === 0) {
          add(`${where}.source_dirs 中没有扫描到任何测试 ID：${group.source_dirs.join(', ')}`)
          return
        }
        for (const id of group.ids) {
          const found = scanned.get(id)
          if (!found) {
            add(`${where}.ids: "${id}" 在 source_dirs 中未找到对应测试函数（禁止把未实现/无测试的 ID 标 implemented）`)
            continue
          }
          acceptTest(
            {
              id,
              area: group.area,
              title: found.title,
              status: 'implemented',
              suite: group.suite,
              selector: group.selector,
              notes: `${group.notes}（${found.file}: ${found.title}）`,
            },
            `${where}.ids["${id}"]`,
          )
        }
      })
    }
  }
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

function runProcess(command, args, { cwd, timeoutMs, env, combineOutput = false } = {}) {
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
      errFd = combineOutput ? outFd : openSync(errPath, 'w')
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
      stderr = combineOutput ? '' : readFileSafe(errPath)
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
        // combineOutput：stdout/stderr 写同一个文件，保证 cargo 的 Running 行与
        // test result 行按真实顺序交错（否则 binary→ignored 映射会错位）。
        stdio: ['ignore', outFd, combineOutput ? outFd : errFd],
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
      args: ['/d', '/s', '/c', 'tools\\build-env.cmd cargo test --offline --workspace --no-fail-fast'],
      label: SUITE_COMMANDS.cargo,
    }
  }
  return {
    command: 'cargo',
    args: ['test', '--offline', '--workspace', '--no-fail-fast'],
    label: SUITE_COMMANDS.cargo,
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

// workspace 里某个包的测试失败不影响其它包的实测结论：从 cargo 的
// `error: test failed, to rerun pass `-p <pkg>`` 行提取失败包名。
function parseCargoFailingPackages(output) {
  const failing = new Set()
  for (const match of output.matchAll(/error: test failed, to rerun pass `-p ([A-Za-z0-9_-]+)/g)) {
    failing.add(match[1])
  }
  return [...failing]
}

// 逐测试二进制统计 passed/failed/ignored（P2-1/P2-2：`#[ignore]` 不算通过）。
function parseCargoBinaryResults(output) {
  const entries = []
  let current = null
  for (const line of output.split(/\r?\n/)) {
    const running = line.match(/^\s*Running [^(]*\(target[\\/][^)]*[\\/]([A-Za-z0-9_]+)-[0-9a-f]+\.exe\)/)
    if (running) {
      current = { name: running[1], suites: 0, passed: 0, failed: 0, ignored: 0 }
      entries.push(current)
      continue
    }
    const doc = line.match(/^\s*Doc-tests ([A-Za-z0-9_]+)/)
    if (doc) {
      current = { name: doc[1], suites: 0, passed: 0, failed: 0, ignored: 0 }
      entries.push(current)
      continue
    }
    const result = line.match(/test result: (ok|FAILED)\.\s+(\d+) passed;\s+(\d+) failed;\s+(\d+) ignored/)
    if (result && current) {
      current.suites += 1
      current.passed += Number(result[2])
      current.failed += Number(result[3])
      current.ignored += Number(result[4])
    }
  }
  return entries
}

// 判断某个测试二进制是否属于该包：lib/doctest 二进制名 = 包名（- → _）；
// integration test 二进制名是测试文件名，需要按 crate 源码目录反查 tests/<binary>.rs。
function packageOwnsBinary(selector, binaryName) {
  const base = selector.replace(/^dshr-/, '')
  const candidates = [
    join(REPO_ROOT, 'crates', base, 'tests', `${binaryName}.rs`),
    join(REPO_ROOT, 'crates', base, 'src', 'bin', `${binaryName}.rs`),
    join(REPO_ROOT, 'crates', base, 'benches', `${binaryName}.rs`),
  ]
  if (base === 'desktop-gpui') candidates.push(join(REPO_ROOT, 'apps', 'desktop-gpui', 'tests', `${binaryName}.rs`))
  return candidates.some((candidate) => existsSync(candidate))
}

function cargoPackageBinaries(output, selector) {
  const normalized = selector.replace(/-/g, '_')
  return parseCargoBinaryResults(output).filter(
    (entry) =>
      entry.name === normalized ||
      entry.name.startsWith(`${normalized}-`) ||
      entry.name.startsWith(`${normalized}_`) ||
      packageOwnsBinary(selector, entry.name),
  )
}

// 该包名下测试二进制的 `#[ignore]` 统计：tainted = 0 passed 且 ignored>0
// （即整文件被忽略、没有执行过任何用例），executed = 至少执行过一条用例。
function cargoPackageIgnoredState(output, selector) {
  const binaries = cargoPackageBinaries(output, selector)
  return {
    binaries,
    tainted: binaries.filter((entry) => entry.failed === 0 && entry.passed === 0 && entry.ignored > 0),
    executed: binaries.some((entry) => entry.passed > 0 || entry.failed > 0),
    ignored: binaries.reduce((sum, entry) => sum + entry.ignored, 0),
  }
}

// ---------------------------------------------------------------------------
// suite 执行
// ---------------------------------------------------------------------------

// node:test 输出解析：兼容 spec reporter（✔ name (1.2ms)）与 TAP（ok 1 - name）。
function parseNodeTestOutput(output) {
  const okTests = []
  const failedTests = []
  for (const rawLine of output.split(/\r?\n/)) {
    const line = rawLine.replace(/\x1b\[[0-9;]*m/g, '').trimEnd()
    const specOk = line.match(/^\s*[✔✓]\s+(.+?)(?:\s+\(\d+(?:\.\d+)?ms\))?\s*$/)
    if (specOk) {
      okTests.push(specOk[1].trim())
      continue
    }
    const specFail = line.match(/^\s*[✖✗×]\s+(.+?)(?:\s+\(\d+(?:\.\d+)?ms\))?\s*$/)
    if (specFail) {
      failedTests.push(specFail[1].trim())
      continue
    }
    const tapOk = line.match(/^\s*ok\s+\d+\s+-\s+(.+)$/)
    if (tapOk) {
      okTests.push(tapOk[1].trim())
      continue
    }
    const tapFail = line.match(/^\s*not ok\s+\d+\s+-\s+(.+)$/)
    if (tapFail) failedTests.push(tapFail[1].trim())
  }
  const count = (label) => {
    const match = output.match(new RegExp(`(?:ℹ|#)\\s*${label}\\s+(\\d+)`))
    return match ? Number(match[1]) : null
  }
  return { okTests, failedTests, total: count('tests'), passed: count('pass'), failed: count('fail') }
}

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
  } else if (name === 'node-test') {
    const args = ['--test']
    // Node 22 的 CI runtime 不接受 --test-isolation（本地较新 Node 才支持）。
    if (process.allowedNodeEnvironmentFlags.has('--test-isolation')) {
      args.push('--test-isolation=none')
    }
    args.push(
      'sidecar/dsh-official/lib/host-services.test.mjs',
      'sidecar/dsh-official/lib/session-projections.test.mjs',
    )
    invocation = {
      command: process.execPath,
      args,
      label: SUITE_COMMANDS[name],
    }
  } else {
    invocation = cargoInvocation()
  }

  console.log(`[conformance] suite ${name}: 运行 ${invocation.label}（超时 ${Math.round(timeoutMs / 1000)}s）`)
  const result = await runProcess(invocation.command, invocation.args, {
    cwd: REPO_ROOT,
    timeoutMs,
    combineOutput: name === 'cargo',
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
  } else if (name === 'node-test') {
    details.node = parseNodeTestOutput(output)
    const node = details.node
    summary = `${node.passed ?? '?'}/${node.total ?? '?'} node tests passed${node.failed ? `, ${node.failed} failed` : ''}`
  } else {
    details.cargo = parseCargoSummary(output)
    details.cargo.failing_packages = parseCargoFailingPackages(output)
    details.cargo.binaries = parseCargoBinaryResults(output)
    const failingNote =
      details.cargo.failing_packages.length > 0 ? `；失败包：${details.cargo.failing_packages.join(', ')}` : ''
    // P1-6：整文件被 #[ignore]（0 passed / N ignored）的二进制必须显式列出，
    // 不能藏在全局 ignored 计数里；包级 selector 也不得据此判 pass。
    const skippedBinaries = details.cargo.binaries.filter(
      (entry) => entry.failed === 0 && entry.passed === 0 && entry.ignored > 0,
    )
    const skippedNote =
      skippedBinaries.length > 0
        ? `；未执行: ${skippedBinaries.map((entry) => `${entry.name}(${entry.ignored} ignored)`).join(', ')}`
        : ''
    summary = `${details.cargo.suites} suite(s), ${details.cargo.passed} passed, ${details.cargo.failed} failed, ${details.cargo.ignored} ignored${failingNote}${skippedNote}`
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

    if (test.suite === 'node-test') {
      const node = run.details.node
      if (!node) return makeResult(test, 'fail', `node-test 输出无法解析（${run.summary}）`, evidence)
      const ok = node.okTests.some((name) => name.includes(test.selector))
      if (ok) return makeResult(test, 'pass', `node 测试通过：${test.selector}（${run.summary}）`, evidence)
      const failed = node.failedTests.some((name) => name.includes(test.selector))
      return makeResult(
        test,
        'fail',
        failed ? `node 测试失败：${test.selector}` : `selector 未在 node --test 输出中找到（${run.summary}）`,
        evidence,
      )
    }

    if (test.suite === 'cargo') {
      const describeTainted = (state) =>
        state.tainted.length > 0
          ? `；另有 ${state.tainted.map((entry) => `${entry.name}(${entry.ignored} ignored/0 passed)`).join(', ')} 未执行（不计为通过）`
          : ''
      if (run.status !== 'pass') {
        // workspace 整体失败时，逐包判定：该包测试二进制出现且不在失败包列表里 → PASS，
        // 只有失败包本身（以及编译失败/超时等无法区分的情况）才 FAIL。
        const failing = run.details.cargo?.failing_packages ?? []
        const normalized = (name) => String(name).replace(/-/g, '_')
        const packageFailed = failing.some((name) => normalized(name) === normalized(test.selector))
        if (
          test.selector !== 'all' &&
          failing.length > 0 &&
          !packageFailed &&
          cargoPackageSeen(run.output, test.selector)
        ) {
          const state = cargoPackageIgnoredState(run.output, test.selector)
          if (state.tainted.length > 0 && !state.executed) {
            return makeResult(
              test,
              'fail',
              `包 ${test.selector} 的测试二进制全部被 #[ignore]（0 passed / ${state.ignored} ignored，不计为通过；${run.summary}）`,
              evidence,
            )
          }
          return makeResult(
            test,
            'pass',
            `包 ${test.selector} 通过${describeTainted(state)}；workspace 仅 ${failing.join(', ')} 失败（${run.summary}）`,
            evidence,
          )
        }
        if (test.selector === 'all') {
          return makeResult(test, 'fail', `cargo workspace 失败（${run.summary}）`, evidence)
        }
        return makeResult(test, 'fail', `cargo suite ${run.status}（${run.summary}）`, evidence)
      }
      if (test.selector === 'all') {
        const ignored = run.details.cargo?.ignored ?? 0
        const skippedBinaries = (run.details.cargo?.binaries ?? []).filter(
          (entry) => entry.failed === 0 && entry.passed === 0 && entry.ignored > 0,
        )
        const ignoredNote =
          ignored > 0
            ? `；${ignored} 个 #[ignore] 未执行，不计为通过${
                skippedBinaries.length > 0
                  ? `（${skippedBinaries.map((entry) => `${entry.name}: 0 passed/${entry.ignored} ignored`).join(', ')}）`
                  : ''
              }；官方包证据见 manual RPC-L2-201/501..505 的 tools/official-l2-gate.ps1`
            : ''
        return makeResult(test, 'pass', `cargo workspace 通过（${run.summary}${ignoredNote}）`, evidence)
      }
      if (cargoPackageSeen(run.output, test.selector)) {
        const state = cargoPackageIgnoredState(run.output, test.selector)
        if (state.tainted.length > 0 && !state.executed) {
          return makeResult(
            test,
            'fail',
            `包 ${test.selector} 的测试二进制全部被 #[ignore]（0 passed / ${state.ignored} ignored，不计为通过；${run.summary}）`,
            evidence,
          )
        }
        return makeResult(
          test,
          'pass',
          `包 ${test.selector} 的测试二进制出现在 cargo 输出中${describeTainted(state)}（${run.summary}）`,
          evidence,
        )
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
  // 矩阵产物本身由本命令写入，不应把“重新生成矩阵”算成脏工作区；
  // 否则每次刷新都要二次提交才能得到 dirty=false。
  const generated = new Set([
    'docs/conformance-matrix.json',
    'docs/conformance-matrix.md',
  ])
  const dirtyFiles = status
    ? status
        .split(/\r?\n/)
        .filter(Boolean)
        .filter((line) => {
          // `runProcess` trims stdout, so a leading space of " M path" may be gone;
          // parse the two status columns as a prefix instead of slicing 3.
          const path = line
            .replace(/^[ MARCUD?!]{1,2}\s+/, '')
            .replace(/^.* -> /, '')
            .trim()
            .replace(/\\/g, '/')
          return !generated.has(path)
        })
    : []
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
  if (opts.compact) parts.push('--compact')
  for (const name of opts.suites) parts.push('--suite', name)
  const registryPath = opts.registryPath ? resolve(process.cwd(), opts.registryPath) : REGISTRY_PATH
  if (registryPath !== REGISTRY_PATH) parts.push('--registry', relativeToRepo(registryPath))
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
    // 行数组而非单个长字符串：矩阵 JSON 可逐行阅读/复制，避免一行几 KB。
    output_tail: output ? output.split(/\r?\n/) : null,
  }
}

function buildReport({ opts, suiteRuns, evaluated, stats, exitCode, registry, registryPath, git, registryNotes }) {
  const suitesRun = suiteRuns.map((run) => run.name)
  // 按领域 + ID 排序，避免 280+ 条行按注册顺序穿插、难以阅读。
  const ordered = [...evaluated].sort(compareTests)
  // --compact：逐 ID 行用 [id, area, status, result, evidence, message] 紧凑数组，
  // 去掉 title/notes、PASS 行消息与 suite 前缀；SKIP 行统一短消息。280+ 条 ID 时
  // 完整格式会把矩阵撑到 100KB+，难以用文件工具落盘，也不便阅读。
  const compactRows = ordered.map((test) => [
    test.id,
    test.area,
    test.status,
    test.result,
    test.status === 'implemented' ? test.selector : '—',
    test.result === 'pass' || test.result === 'skip' ? '' : test.message,
  ])
  const reportTests = opts.compact
    ? { columns: ['id', 'area', 'status', 'result', 'evidence', 'message'], rows: compactRows }
    : ordered
  return {
    schema_version: 1,
    tool: 'tools/conformance.mjs',
    generated_at: new Date().toISOString(),
    format: opts.compact ? 'compact' : 'full',
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
      groups: Array.isArray(registry.groups) ? registry.groups.length : 0,
      tests: evaluated.length,
      suites: Object.keys(registry.suites ?? {}),
    },
    suites: suiteRuns.map(publicSuite),
    stats,
    tests: reportTests,
    notes: [
      ...(registryNotes ?? []),
      opts.compact
        ? 'compact 矩阵：markdown 保留逐 ID 的 ID/领域/状态/结果/证据/消息；JSON 的 tests.rows 为 [id, area, result]（status/证据/消息见 markdown 与注册表，result=pending|manual 即对应 status）。'
        : null,
      'quick 模式不运行 cargo suite：implemented 且 suite=cargo 的 ID 显示 SKIP 属正常，需 --workspace 或 --full。',
      'cargo suite 在 Windows 上经 tools/build-env.cmd 调用（cargo test --offline --workspace --no-fail-fast）。',
      '.github/workflows/conformance.yml 未在 GitHub Actions 实跑，本报告由本机命令生成。',
      '本文件由 tools/conformance.mjs 生成，请勿手改；新增证据请改 docs/conformance.d/registry.json。',
    ].filter((note) => note !== null),
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
  if (report.format === 'compact') {
    lines.push('> **格式**：compact（逐 ID 行只保留 ID/领域/状态/结果/证据/消息，title/notes 见注册表）。')
  }
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
  if (report.format === 'compact') {
    const rows = report.tests.rows
    const nonPass = rows.filter((row) => row[3] !== 'pass')
    const passRows = rows.filter((row) => row[3] === 'pass')
    lines.push('### 非 PASS（逐 ID 明细）')
    lines.push('')
    lines.push('| ID | 领域 | 状态 | 结果 | 证据 | 消息 |')
    lines.push('|---|---|---|---|---|---|')
    for (const row of nonPass) {
      const [id, area, status, result, evidence, message] = row
      lines.push(
        `| ${id} | ${markdownEscape(area)} | ${status} | ${resultLabel[result] ?? result} | ${markdownEscape(evidence)} | ${markdownEscape(message)} |`,
      )
    }
    lines.push('')
    lines.push(`### PASS（逐 ID；共 ${passRows.length} 条）`)
    lines.push('')
    lines.push('| ID | 结果 |')
    lines.push('|---|---|')
    for (const row of passRows) lines.push(`| ${row[0]} | PASS |`)
  } else {
    lines.push('| ID | 领域 | 标题 | 状态 | 结果 | 证据 | 消息 |')
    lines.push('|---|---|---|---|---|---|---|')
    for (const test of report.tests) {
      lines.push(
        `| ${test.id} | ${markdownEscape(test.area)} | ${markdownEscape(test.title)} | ${test.status} | ${resultLabel[test.result] ?? test.result} | ${markdownEscape(test.evidence)} | ${markdownEscape(test.message)} |`,
      )
    }
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

// JSON 报告：full 格式直接 2 空格缩进；compact 格式把 tests 行排成「一行一条」，
// 避免 JSON.stringify 把每个数组元素拆成多行导致 280+ 条 ID 的文件膨胀。
function renderJsonReport(report) {
  if (report.format !== 'compact') return `${JSON.stringify(report, null, 2)}\n`
  const { tests, ...rest } = report
  const placeholder = '__CONFORMANCE_TESTS_PLACEHOLDER__'
  const skeleton = JSON.stringify({ ...rest, tests: placeholder }, null, 2)
  // JSON 只留 [id, area, result]：status 可由 result 推出（pass/skip/fail → implemented，
  // pending/manual/blocked 原样）；证据/消息在 markdown 与注册表里，避免重复撑大文件。
  const jsonColumns = ['id', 'area', 'result']
  const jsonRows = tests.rows.map((row) => [row[0], row[1], row[3]])
  const testsText =
    '{\n' +
    `    "columns": ${JSON.stringify(jsonColumns)},\n` +
    '    "rows": [\n' +
    jsonRows.map((row) => `      ${JSON.stringify(row)}`).join(',\n') +
    '\n    ]\n  }'
  return `${skeleton.replace(JSON.stringify(placeholder), testsText)}\n`
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

function listRegistry(registry, validated, registryPath = REGISTRY_PATH) {
  console.log(`[conformance] registry: ${relativeToRepo(registryPath)}`)
  console.log(`[conformance] groups: ${Array.isArray(registry.groups) ? registry.groups.length : 0}`)
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
  for (const test of [...validated.tests].sort(compareTests)) {
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

  const registryPath = opts.registryPath ? resolve(process.cwd(), opts.registryPath) : REGISTRY_PATH
  let registry
  try {
    registry = loadRegistry(registryPath)
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
    listRegistry(registry, validated, registryPath)
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
      selected.add('node-test')
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
      selected.add('node-test')
      console.log('[conformance] 未指定模式，按 --quick 执行（schema + links + node-test）。')
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
    registryPath,
    git,
    registryNotes: validated.notes,
  })

  let reportWriteFailures = 0
  if (opts.jsonPath) {
    const outcome = writeFileAt(opts.jsonPath, renderJsonReport(report))
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
