import { mkdirSync, writeFileSync } from 'node:fs'
import { arch, platform, release } from 'node:os'
import { basename, dirname, isAbsolute, join, relative, resolve } from 'node:path'
import { DEFAULT_JSON_PATH, DEFAULT_MARKDOWN_PATH, REGISTRY_PATH, REPO_ROOT, RUN_TMP_DIR } from './paths.mjs'

// 矩阵与 --list 的领域排序（便于 300+ 条 ID 的表格按领域阅读）。
const AREA_ORDER = ['SCHEMA', 'DOC', 'CARGO', 'ADC', 'CPP', 'RPC', 'MCP', 'JS', 'M3', 'M4', 'SELF', 'GATE']
export function compareTests(a, b) {
  const ai = AREA_ORDER.indexOf(a.area)
  const bi = AREA_ORDER.indexOf(b.area)
  if (ai !== bi) return (ai < 0 ? AREA_ORDER.length : ai) - (bi < 0 ? AREA_ORDER.length : bi)
  return a.id.localeCompare(b.id)
}

export function relativeToRepo(target) {
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

export function buildStats(tests) {
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

export function buildReport({ opts, suiteRuns, evaluated, stats, exitCode, registry, registryPath, git, registryNotes }) {
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

export function renderMarkdown(report) {
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
export function renderJsonReport(report) {
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

export function writeFileAt(targetPath, content) {
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
