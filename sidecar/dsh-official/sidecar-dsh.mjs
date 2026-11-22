// Sidecar：装载**未修改的 DSH 官方插件**（Cordis 4.1.0 + 官方 @deepseek-ai/dsh-* 包）。
//
// 与 probe/cordis 侧车使用完全相同的 NDJSON 协议（docs/spec/sidecar-rpc.md），
// 区别只在插件来源：
//   - 由 `tools` 服务收到的 DSH ToolDefinition 直接映射成 `tool.register`；
//   - 调用时执行 `definition.execute(args, exec)`，用 `output.render` 生成 content。
//
// 依赖的 node_modules 不提交仓库，由 tools/dsh-vendor.mjs 从本机安装的
// DeepSeek Harness app.asar 抽取（见 docs/m2-dsh-official.md）。
//
// M2 W3 —— 官方写工具（write/edit/apply_patch）在 Rust 审批 + 沙箱下打开：
//   - 能力协商：runtime.hello.host_caps 含 `fs.write` 才装载写工具（否则保持只读，
//     --selftest-dsh 不回归）；
//   - `fs` 服务把官方 dsh-fs 的写方法（writeText/editText/patchText/deleteFile/
//     moveFile/lstat）翻译成 `fs.write.request`，由 Rust 裁决（路径边界/审批/写前
//     快照/原子写/版本），结果走 `fs.write.result`；
//   - 官方 dsh-tool-fs 源码一行不改：sandbox seam 由官方 `FsSandboxController` 在
//     `fs.sandboxMode === undefined` 时自动退化为 escalationModes=[]（无 escalate、
//     无 sandboxPolicy 依赖），真实约束全部在 Rust 侧。
//
// 环境变量：
//   DSHR_DSH_PLUGINS  JSON 数组：插件名 + 配置。缺省加载 workspace-dependencies + fs 工具。
//   DSHR_DSH_SOURCE   primary-runtime 目录（workspace-dependencies 的 source）；
//                     缺省按 LOCALAPPDATA 下的 DSH 安装路径推断。
//   DSHR_WORKSPACE    fs 服务的工作区根；缺省为 sidecar 进程 cwd（宿主握手时会设置）。

import { createInterface } from 'node:readline'
import { AsyncLocalStorage } from 'node:async_hooks'
import { existsSync, lstatSync } from 'node:fs'
import { open, readFile, readdir, realpath, stat as fsStat, lstat as fsLstat } from 'node:fs/promises'
import {
  isAbsolute,
  join,
  relative as relativePath,
  resolve as resolvePath,
  sep,
} from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import {
  createHostServicesRuntime,
  handleHostEventEmit,
  shouldEnableHostServices,
  SERVICE_KEYS,
} from './lib/host-services.mjs'
import { createSessionProjectionsShim } from './lib/session-projections.mjs'
import { createAgentPresets } from './lib/agent-presets.mjs'
import { collectAgentStep } from './lib/agent-step.mjs'
import { createImageAttachments, IMAGE_ATTACHMENT_CAP } from './lib/image-attachments.mjs'
import {
  createUserQuestions,
  USER_QUESTIONS_CAP,
  USER_QUESTIONS_CATALOG,
} from './lib/user-questions.mjs'
import {
  createToolProcesses,
  SEARCH_PROCESS_CAP,
  SHELL_PROCESS_CAP,
  TOOL_PROCESS_CATALOG,
} from './lib/tool-processes.mjs'
import { BACKGROUND_JOBS_CAP, createToolJobs, JOBS_CATALOG } from './lib/tool-jobs.mjs'
import { TERMINAL_CAP, createToolTerminals, TERMINAL_CATALOG } from './lib/tool-terminals.mjs'
import { GOALS_CAP, GOALS_CATALOG, GOAL_TOOLS, createToolGoals, goalTurnSnapshot } from './lib/tool-goals.mjs'
import { SESSION_QUERY_CAP, SESSION_QUERY_CATALOG, SESSION_QUERY_TOOLS, createSessionQuery } from './lib/session-query.mjs'
import { SUBAGENT_CAP, SUBAGENT_CATALOG, SUBAGENT_TOOLS, SUBAGENT_PLUGINS, createSubagents } from './lib/subagents.mjs'
import { TEAM_CAP, TEAM_CATALOG, TEAM_TOOLS, TEAM_PLUGIN, createAgentTeams } from './lib/agent-teams.mjs'
import { WORKFLOW_TOOLS, WORKFLOW_PLUGIN, RALPH_PLUGIN, mountWorkflowEngine } from './lib/workflow-runtime.mjs'
import { SCHEDULE_CAP, SCHEDULE_CATALOG, SCHEDULE_TOOLS, SCHEDULE_PLUGIN, createSchedules } from './lib/schedules.mjs'
import { INSPECT_PLUGIN, INSPECT_TOOLS, mountCordisInspection } from './lib/cordis-inspect.mjs'
import { NATIVE_EXECUTOR_PLUGIN, PTC_EXECUTOR_PLUGIN, PROGRAM_TOOL, PROGRAM_CAP, PROGRAM_SERVICE, PROGRAM_CATALOG,
  executeWithHooks, executeWithOfficialExecutor } from './lib/executors.mjs'

const PROTOCOL_MAX = 2
const AGENT_TOOL_CONTEXT_CAP = 'agent.tool-context'
// 协商结果：min(host, sidecar)。v1 宿主（M2 及以前）保持纯 v1 行为。
let negotiatedProtocol = 1
let generation = 'node-0001'
const globalTools = new Map()
const sessionTools = new Map()
const revisionTools = new Map()
const revisionContexts = new Map()
const jobBridges = new WeakMap()
const terminalBridges = new WeakMap()
const goalBridges = new WeakMap()
const queryBridges = new WeakMap()
const subagentBridges = new WeakMap()
const teamBridges = new WeakMap()
const scheduleBridges = new WeakMap()
const stepScopes = new Map()
const executorScopes = new WeakMap()
const presets = createAgentPresets(mountPreset)
let bootComplete = Promise.resolve()
let presetOperations = Promise.resolve()
const toolsByRequest = new Map()
const toolsById = new Map()
let toolRegistrationSeq = 0
const TOOL_OWNER = 'dsh-official'
const inflight = new Map()

// 握手协商出的 host_caps；`fs.write` 是官方写工具的唯一门闩。
const hostCaps = new Set()
let writeEnabled = false

// L2 Host Service 代理（spec §13）：仅协商到 v2 且宿主支持 service.proxy 时启用。
let hostServices = null
// P1-5：投影注册表留在 Sidecar（`ctx.sessionProjections`），宿主只提供 session-log 数据面。
let sessionProjections = null
// 当前工具调用上下文（callId/sessionId/depth）：服务代理用它关联 tool.cancel 与 session_id。
const callStore = new AsyncLocalStorage()
const agentActors = new Map()
const OFFICE_TOOLS = new Set(['office_convert', 'office_recalculate', 'office_render'])
// 只有这些官方工具使用最小的会话与模型接口。
const AGENT_FACADE_TOOLS = new Set([
  'working_directory', 'todo_write', 'read_image', 'glob', 'grep', 'pwsh', 'bash', 'present',
  'skill', 'web_search', 'web_fetch', 'job_list', 'job_output', 'job_kill', ...OFFICE_TOOLS,
  'terminal_open', 'terminal_send', 'terminal_read', 'terminal_signal', 'terminal_list', 'terminal_close',
  ...GOAL_TOOLS,
  ...SESSION_QUERY_TOOLS,
  ...SUBAGENT_TOOLS,
  ...TEAM_TOOLS,
  ...WORKFLOW_TOOLS,
  ...SCHEDULE_TOOLS,
  ...INSPECT_TOOLS,
  PROGRAM_TOOL,
])
const TERMINAL_PLUGIN_NAMES = new Set([
  '@deepseek-ai/dsh-tool-terminal',
  '@deepseek-ai/dsh-tool-bash-persistent',
  '@deepseek-ai/dsh-tool-pwsh-persistent',
])
const WEB_PLUGIN_NAMES = new Set([
  '@deepseek-ai/dsh-tool-web', '@deepseek-ai/dsh-web-deepseek-api',
  '@deepseek-ai/dsh-web-fetch-http', '@deepseek-ai/dsh-web-search-deepseek',
])
const SKILL_PLUGIN_NAMES = new Set([
  '@deepseek-ai/dsh-skill-filesystem', '@deepseek-ai/dsh-skill-office', '@deepseek-ai/dsh-tool-skill',
])
// Cordis 根 Context：bootPlugins 后用于把服务事件桥接回插件总线。
let cordisRoot = null

const HOST_SERVICE_TIMEOUT_MS = (() => {
  const configured = Number(process.env.DSHR_HOST_SERVICE_TIMEOUT_MS)
  return Number.isFinite(configured) && configured > 0 ? configured : 30_000
})()

// 工作区根：宿主在 spawn 时通过 DSHR_WORKSPACE 注入；独立运行时退回 cwd。
const WORKSPACE_ROOT = resolvePath(process.env.DSHR_WORKSPACE || process.cwd())

// sidecar → host 的请求（当前只有 fs.write.request）；按 request_id 关联答复。
const pendingHostRequests = new Map()
let hostRequestSeq = 0
const HOST_REQUEST_TIMEOUT_MS = 120_000

// 工具调用的执行上下文：官方 fs 方法的签名里没有 call_id，用 AbortSignal 关联
// 到发起它的 tool.invoke（审批审计需要同一个 call_id）。
const callContexts = new WeakMap()

const send = (kind, requestId, payload, extra = {}) => {
  process.stdout.write(
    JSON.stringify({
      version: negotiatedProtocol,
      kind,
      request_id: requestId,
      runtime_generation: generation,
      depth: 0,
      trace_id: 't-dsh-official',
      ts: new Date().toISOString(),
      payload,
      ...extra,
    }) + '\n',
  )
}

/**
 * 把 host-services 模块构造的完整信封接到 NDJSON `send` 上。
 * 信封的可选字段走 extra 覆盖。
 */
const sendEnvelope = (envelope) =>
  send(envelope.kind, envelope.request_id, envelope.payload, {
    version: envelope.version,
    plugin_id: envelope.plugin_id ?? 'dsh-official',
    ...(envelope.service_key ? { service_key: envelope.service_key } : {}),
    ...(envelope.session_id ? { session_id: envelope.session_id } : {}),
    ...(envelope.call_id ? { call_id: envelope.call_id } : {}),
    ...(Number.isInteger(envelope.depth) ? { depth: envelope.depth } : {}),
  })

const log = (level, message, extra = {}) =>
  send('log.emit', `log-${Date.now()}`, { level, message }, extra)

// ---------------------------------------------------------------------------
// 官方插件的入口：ctx.tools.register(definition)。
// 这里只做"定义 → 协议"与"调用 → execute"的翻译，不复制 DSH 的执行管线。
// ---------------------------------------------------------------------------

function removeTool(binding, notifyHost) {
  const { scope, sessionId, revision, definition, requestId, registrationId } = binding
  if (scope.get(definition.name) !== binding) return
  scope.delete(definition.name)
  if (sessionId && scope.size === 0) sessionTools.delete(sessionId)
  toolsByRequest.delete(requestId)
  if (registrationId) toolsById.delete(registrationId)
  if (notifyHost) {
    send(
      'tool.unregister',
      `tool-unreg-${++toolRegistrationSeq}`,
      {
        ...(registrationId ? {} : { register_request_id: requestId }),
        ...(revision ? { preset_revision: revision } : {}),
      },
      {
        plugin_id: TOOL_OWNER,
        ...(sessionId ? { session_id: sessionId } : {}),
        ...(registrationId ? { registration_id: registrationId } : {}),
      },
    )
  }
}

const toolsService = {
  register(definition, { sessionId, when } = {}, revision) {
    if (!definition?.name) throw new Error('tool definition without name')
    if (sessionId != null && (typeof sessionId !== 'string' || !sessionId)) {
      throw new Error('invalid tool registration session')
    }
    if (revision && sessionId) throw new Error('preset tools cannot declare a session scope')
    const revisionScope = revision && revisionTools.get(revision)
    if (revision && !revisionScope) throw new Error('preset revision is no longer active')
    let scope = revisionScope || globalTools
    if (sessionId) {
      scope = sessionTools.get(sessionId)
      if (!scope) {
        scope = new Map()
        sessionTools.set(sessionId, scope)
      }
    }
    if (scope.has(definition.name)) throw new Error(`duplicate tool ${definition.name} in scope`)
    const requestId = `tool-reg-${++toolRegistrationSeq}`
    const binding = {
      definition, sessionId, revision, scope, requestId,
      when: definition.name === 'office_render' ? selection => (
        hostCaps.has(IMAGE_ATTACHMENT_CAP) && selection.modelSelection.image_input === true
          && when(selection)
      ) : when,
    }
    scope.set(definition.name, binding)
    toolsByRequest.set(requestId, binding)
    send(
      'tool.register',
      requestId,
      {
        name: definition.name,
        ...(revision ? { preset_revision: revision } : {}),
        description: definition.description ?? '',
        input_schema: definition.parameters ?? { type: 'object' },
        output_schema: definition.output?.schema ?? null,
        presentation: 'generic',
        // 每个工具自己的超时（DSH `ToolDefinition.timeoutMs`；未声明则不设限）。
        // 宿主按它执行（Rust 侧 clamp 到 1s..=600s），不再统一写死 5s。
        ...(Number.isFinite(definition.timeoutMs) && definition.timeoutMs > 0
          ? { timeout_ms: definition.timeoutMs }
          : {}),
      },
      { plugin_id: TOOL_OWNER, ...(sessionId ? { session_id: sessionId } : {}) },
    )
    return () => removeTool(binding, true)
  },
  get(name, scope = {}, revision) {
    const active = callStore.getStore()
    const sessionId = scope.sessionId ?? scope.session?.id
    if (active?.stepBindings && (!sessionId || active.sessionId === sessionId)) {
      return active.stepBindings[name]?.definition
    }
    return (sessionTools.get(sessionId)?.get(name)
      ?? revisionTools.get(revision ?? presets.binding(sessionId))?.get(name)
      ?? globalTools.get(name))?.definition
  },
  schemas(scope) {
    const active = callStore.getStore()
    if (!active?.agent || (scope && scope !== active.agent)) return []
    const names = active.tool === PROGRAM_TOOL ? active.programDefinitions : active.stepBindings
    return Object.values(names ?? {}).map(({ definition }) => ({
      name: definition.name, description: definition.description ?? '', parameters: definition.parameters,
    }))
  },
  // 官方插件在注册阶段可能触到这些方法；装载期没有对应能力，先做成空实现。
  presentAs() {},
  restrict() {},
  guard() {},
  unregister(name, { sessionId } = {}, revision) {
    const scope = revision ? revisionTools.get(revision) : sessionId ? sessionTools.get(sessionId) : globalTools
    const binding = scope?.get(name)
    if (binding) removeTool(binding, true)
  },
}

// ---------------------------------------------------------------------------
// 最小服务子集：官方插件通过 `inject`/`ctx.<service>` 使用这些 seam。
// ---------------------------------------------------------------------------

/**
 * 构造一个官方 `FsError`。用真实包是为了让 dsh-tool-fs 的 `instanceof FsError`
 * 判定成立（否则 FS_STALE_VERSION 的"re-read the file"补救提示不会出现）。
 * 包不存在时退回带 `code` 的普通 Error。
 */
async function makeFsError(message, code) {
  try {
    const { FsError } = await import('@deepseek-ai/dsh-fs')
    return new FsError(message, code)
  } catch {
    const error = new Error(message)
    error.code = code
    return error
  }
}

/**
 * Rust 裁决层的错误码 → 官方 dsh-fs 词表。
 *
 * 注意 `FS_SANDBOX_DENIED`：官方 dsh-tool-fs 的 `FsSandboxController.mapError`
 * 只在"挂载了 confining 后端"（`ctx.fs.sandboxMode !== undefined`）时才有 policy；
 * 本侧车是非 confining 后端（真实约束在 Rust），收到 `FS_SANDBOX_DENIED` 会
 * 解引用 undefined 的 policy 而崩溃。因此把 Rust 的沙箱/审批拒绝翻译成
 * `FS_PERMISSION_DENIED`（消息里保留"denied / approval"细节）。
 */
function officialFsErrorCode(code) {
  switch (code) {
    case 'FS_SANDBOX_DENIED':
    case 'E_FS_CAPABILITY':
      return 'FS_PERMISSION_DENIED'
    case 'E_FS_REQUEST':
    case 'E_PROTOCOL':
      return 'FS_IO_ERROR'
    default:
      return code
  }
}

function versionOf(info) {
  // 与 Rust 侧 FsWriteService 的 version_of 同格式：floor(mtime_ms):size。
  return `${Math.floor(info.mtimeMs)}:${info.size}`
}

function officeOutputPath(root, path) {
  if (typeof path !== 'string' || !isAbsolute(path)) return undefined
  const relative = relativePath(root, path)
  if (!relative || relative === '..' || relative.startsWith(`..${sep}`) || isAbsolute(relative)) {
    return undefined
  }
  let current = root
  for (const segment of relative.split(sep)) {
    current = join(current, segment)
    try {
      if (lstatSync(current).isSymbolicLink()) return undefined
    } catch (error) {
      if (error.code !== 'ENOENT') return undefined
      break
    }
  }
  return resolvePath(path)
}

function createFsService(options = {}) {
  const root = resolvePath(options.root ?? WORKSPACE_ROOT)
  const targetMeta = new WeakMap()

  const describeTarget = (absolutePath) => {
    const target = { targetKey: absolutePath, displayPath: absolutePath }
    targetMeta.set(target, withinRoot(absolutePath))
    return target
  }

  const withinRoot = (absolutePath) => {
    const rel = relativePath(root, absolutePath)
    const inside =
      rel !== '' && rel !== '..' && !rel.startsWith(`..${sep}`) && !isAbsolute(rel)
    return { inside, relative: rel }
  }

  const metaOf = (target) => {
    const known = targetMeta.get(target)
    if (known) return known
    const absolute = resolvePath(String(target?.targetKey ?? target))
    return withinRoot(absolute)
  }

  const sessionDirectory = async () => {
    const call = callStore.getStore()
    if (!call?.sessionId || !options.workingDirectory) return root
    call.workingDirectory ??= options.workingDirectory.ensure(undefined, call.controller.signal)
    return await call.workingDirectory || root
  }

  const targetOf = async (requestedPath, callOptions) => {
    callOptions.signal?.throwIfAborted()
    const requested = String(requestedPath)
    if (isAbsolute(requested)) return describeTarget(resolvePath(requested))
    const base = typeof callOptions.cwd === 'string' && callOptions.cwd.length > 0
      ? callOptions.cwd
      : await sessionDirectory()
    callOptions.signal?.throwIfAborted()
    return describeTarget(resolvePath(base, requested))
  }

  const checkScopedFile = async (target) => {
    const tool = callStore.getStore()?.tool
    if (tool !== 'present' && !OFFICE_TOOLS.has(tool)) return target.targetKey
    if (!metaOf(target).inside) {
      throw await makeFsError('file path is outside the workspace', 'FS_PERMISSION_DENIED')
    }
    const actual = await realpath(target.targetKey).catch(error => {
      if (error.code !== 'ENOENT') throw error
      return null
    })
    if (!actual) return target.targetKey
    const relative = relativePath(await realpath(root), actual)
    if (!relative || relative === '..' || relative.startsWith(`..${sep}`) || isAbsolute(relative)) {
      throw await makeFsError('file path is outside the workspace', 'FS_PERMISSION_DENIED')
    }
    return actual
  }

  async function readOfficeBytes(target, signal, offset, length, wholeFile) {
    signal?.throwIfAborted()
    const file = await open(await checkScopedFile(target), 'r')
    try {
      const info = await file.stat()
      if (!info.isFile()) throw await makeFsError('Office source must be a regular file', 'FS_NOT_REGULAR_FILE')
      if (wholeFile && info.size > length) {
        throw await makeFsError('Office source exceeds the file size limit', 'FS_TOO_LARGE')
      }
      const data = Buffer.alloc(Math.max(0, Math.min(length, info.size - offset)))
      let position = 0
      while (position < data.length) {
        signal?.throwIfAborted()
        const { bytesRead } = await file.read(data, position, data.length - position, offset + position)
        if (bytesRead === 0) throw await makeFsError('Office source changed during read', 'FS_STALE_VERSION')
        position += bytesRead
      }
      signal?.throwIfAborted()
      return data
    } finally {
      await file.close()
    }
  }

  const callOf = (signal) => {
    const call = callContexts.get(signal) ?? callStore.getStore()
    if (!call) throw new Error('filesystem mutation requires an active tool call')
    return call
  }

  const readIfExists = async (target) => {
    try {
      return await readFile(target.targetKey, 'utf8')
    } catch (error) {
      if (error?.code === 'ENOENT') return null
      throw error
    }
  }

  const currentVersion = async (target) => {
    const info = await fsStat(target.targetKey)
    return versionOf(info)
  }

  /**
   * 所有写操作的唯一出口：把官方 fs 语义翻译成 fs.write.request。
   * 路径以"工作区相对路径"发送，Rust 再做一次归一化/边界校验与审批。
   */
  const performWrite = async ({ operation, target, content, expectedVersion, call, intent }) => {
    if (!writeEnabled) {
      throw await makeFsError(
        'filesystem write is not enabled: host did not negotiate fs.write',
        'FS_PERMISSION_DENIED',
      )
    }
    const meta = metaOf(target)
    if (!meta.inside) {
      throw await makeFsError(
        `path escapes the workspace root "${root}": ${target.displayPath}`,
        'FS_PERMISSION_DENIED',
      )
    }
    const payload = {
      call_id: call.callId,
      tool_request_id: call.toolRequestId,
      tool: call.tool,
      operation,
      path: meta.relative,
      workspace_root: root,
      content: operation === 'write' ? content : undefined,
      expected_version: expectedVersion ?? undefined,
      intent: intent ?? undefined,
      mode: 'workspace-write',
    }
    const result = await requestHost('fs.write.request', payload, {
      call_id: call.callId,
      plugin_id: 'dsh-official',
    }, call.controller.signal)
    if (!result?.ok) {
      const code = result?.error?.code ?? 'FS_IO_ERROR'
      const message = result?.error?.message ?? `write failed (${code})`
      throw await makeFsError(message, officialFsErrorCode(code))
    }
    return result
  }

  return {
    processPath(target) {
      return target.targetKey
    },
    processPathFromHostPath(path) {
      return officeOutputPath(root, path)
    },
    /**
     * 沙箱能力事实：本侧车不挂载 confining 后端（真实约束在 Rust）。
     * 官方 dsh-tool-fs 的 `FsSandboxController` 因此退化为 escalationModes=[]、
     * 无 sandboxPolicy 依赖、无 sandbox_permissions 参数——即"无 escalate 模式"。
     */
    get sandboxMode() {
      return undefined
    },
    async resolve(requestedPath, callOptions = {}) {
      return targetOf(requestedPath, callOptions)
    },
    async stat(target, signal) {
      if (signal?.aborted) throw new Error('stat aborted')
      const path = await checkScopedFile(target)
      try {
        const info = await fsStat(path)
        return {
          version: versionOf(info),
          type: info.isDirectory() ? 'directory' : 'file',
          size: info.size,
        }
      } catch {
        return undefined
      }
    },
    async lstat(requestedPath, callOptions = {}) {
      const target = await targetOf(requestedPath, callOptions)
      await checkScopedFile(target)
      try {
        const info = await fsLstat(target.targetKey)
        return {
          version: versionOf(info),
          type: info.isSymbolicLink()
            ? 'symlink'
            : info.isDirectory()
              ? 'directory'
              : 'file',
          size: info.size,
        }
      } catch {
        return undefined
      }
    },
    async readText(target, signal) {
      return readFile(target.targetKey, { encoding: 'utf8', signal })
    },
    async listDir(target, signal) {
      signal?.throwIfAborted()
      let entries
      try {
        entries = await readdir(target.targetKey, { withFileTypes: true })
      } catch (error) {
        const code = error?.code === 'ENOENT' ? 'FS_NOT_FOUND'
          : error?.code === 'ENOTDIR' ? 'FS_NOT_DIRECTORY'
          : error?.code === 'EACCES' || error?.code === 'EPERM' ? 'FS_PERMISSION_DENIED'
          : 'FS_IO_ERROR'
        throw await makeFsError(String(error?.message ?? error), code)
      }
      signal?.throwIfAborted()
      return entries.map(entry => ({
        name: entry.name,
        target: describeTarget(join(target.targetKey, entry.name)),
        type: entry.isDirectory() ? 'directory'
          : entry.isFile() ? 'file'
          : entry.isSymbolicLink() ? 'symlink' : 'other',
      }))
    },
    async streamText(target, signal) {
      // 工具侧用 `for await` 消费分块；一次返回整块也合法（只是不分块）。
      return [await this.readText(target, signal)]
    },
    async readBytes(target, signal, cap) {
      if (options.images && callStore.getStore()?.tool === 'read_image') {
        return options.images.readBytes(target, signal, cap)
      }
      if (OFFICE_TOOLS.has(callStore.getStore()?.tool)) {
        const limit = Number.isSafeInteger(cap) && cap >= 0 ? Math.min(cap, 100 * 1024 * 1024) : 0
        return readOfficeBytes(target, signal, 0, limit, true)
      }
      if (signal?.aborted) throw new Error('read aborted')
      const buffer = await readFile(target.targetKey)
      return typeof cap === 'number' ? buffer.subarray(0, cap) : buffer
    },
    async readByteRange(target, { offset, length }, signal) {
      if (!OFFICE_TOOLS.has(callStore.getStore()?.tool)
          || !Number.isSafeInteger(offset) || offset < 0
          || !Number.isSafeInteger(length) || length < 0 || length > 64 * 1024) {
        throw await makeFsError('invalid Office byte range', 'FS_PERMISSION_DENIED')
      }
      return readOfficeBytes(target, signal, offset, length, false)
    },
    /**
     * 官方 write 工具调用：无 policy 时是"无条件原子 create-or-overwrite"。
     * `before/after/operation` 是官方 output schema / diff 卡片需要的字段。
     */
    async writeText(target, content, intent, signal) {
      const call = callOf(signal)
      const before = await readIfExists(target)
      if (intent?.kind === 'createIfAbsent' && before !== null) {
        throw await makeFsError(
          `cannot overwrite existing "${target.displayPath}" without reading it first`,
          'FS_NOT_OBSERVED',
        )
      }
      const expectedVersion =
        typeof intent?.version === 'string' ? intent.version : undefined
      const operation = before === null ? 'create' : 'update'
      const result = await performWrite({
        operation: 'write',
        target,
        content,
        expectedVersion,
        call,
        intent,
      })
      return {
        version: result.version,
        before,
        after: content,
        operation: result.operation ?? operation,
      }
    },
    /** 官方 edit 工具调用：唯一匹配（默认）或 replace_all，写前带版本守卫。 */
    async editText(target, edit, intent, signal) {
      const call = callOf(signal)
      const before = await readIfExists(target)
      if (before === null) {
        throw await makeFsError(
          `cannot edit "${target.displayPath}": not found`,
          'FS_NOT_FOUND',
        )
      }
      const version = await currentVersion(target)
      if (typeof intent?.version === 'string' && intent.version !== version) {
        throw await makeFsError(
          `cannot edit "${target.displayPath}": file changed since it was read`,
          'FS_STALE_VERSION',
        )
      }
      const occurrences = countOccurrences(before, edit.oldString)
      if (occurrences === 0) {
        throw await makeFsError(
          `cannot edit "${target.displayPath}": old_string was not found`,
          'FS_EDIT_NOT_FOUND',
        )
      }
      if (occurrences > 1 && !edit.replaceAll) {
        throw await makeFsError(
          `cannot edit "${target.displayPath}": old_string appears ${occurrences} times; pass a unique string or replace_all`,
          'FS_AMBIGUOUS_EDIT',
        )
      }
      const after = edit.replaceAll
        ? before.split(edit.oldString).join(edit.newString)
        : replaceOnce(before, edit.oldString, edit.newString)
      const result = await performWrite({
        operation: 'write',
        target,
        content: after,
        expectedVersion: version,
        call,
        intent,
      })
      return { version: result.version, before, after }
    },
    /** 官方 apply_patch update 调用：hunks 由官方 dsh-fs-patch 纯函数应用。 */
    async patchText(target, hunks, intent, signal) {
      const call = callOf(signal)
      const before = await readIfExists(target)
      if (before === null) {
        throw await makeFsError(
          `cannot patch "${target.displayPath}": not found`,
          'FS_NOT_FOUND',
        )
      }
      const version = await currentVersion(target)
      const { applyHunks } = await import('@deepseek-ai/dsh-fs-patch')
      const applied = applyHunks(before.replace(/\r\n/g, '\n'), hunks)
      if (!applied?.ok) {
        throw await makeFsError(
          `cannot apply patch to "${target.displayPath}": hunk did not match the current file`,
          'FS_PATCH_MISMATCH',
        )
      }
      const result = await performWrite({
        operation: 'write',
        target,
        content: applied.after,
        expectedVersion: version,
        call,
        intent,
      })
      return { version: result.version, before, after: applied.after }
    },
    /** 官方 apply_patch delete 调用。 */
    async deleteFile(target, intent, signal) {
      const call = callOf(signal)
      const before = await readIfExists(target)
      if (before === null) {
        throw await makeFsError(
          `cannot delete "${target.displayPath}": not found`,
          'FS_NOT_FOUND',
        )
      }
      const version = await currentVersion(target)
      await performWrite({
        operation: 'delete',
        target,
        expectedVersion: version,
        call,
        intent,
      })
      return { before }
    },
    /**
     * 官方 apply_patch move 调用：写目标 + 删源。两步各自审批、各自快照；
     * 目标写成功而源删除失败时按官方契约返回 sourceFailure（不假装原子）。
     */
    async moveFile(target, destination, moveOptions = {}, signal) {
      const call = callOf(signal)
      const sourceText = await readIfExists(target)
      if (sourceText === null) {
        throw await makeFsError(
          `cannot move "${target.displayPath}": not found`,
          'FS_NOT_FOUND',
        )
      }
      const sourceVersion = await currentVersion(target)
      if (
        typeof moveOptions.expectedSource?.version === 'string' &&
        moveOptions.expectedSource.version !== sourceVersion
      ) {
        throw await makeFsError(
          `cannot move "${target.displayPath}": file changed since it was read`,
          'FS_STALE_VERSION',
        )
      }
      let after = sourceText
      if (Array.isArray(moveOptions.hunks) && moveOptions.hunks.length > 0) {
        const { applyHunks } = await import('@deepseek-ai/dsh-fs-patch')
        const applied = applyHunks(sourceText.replace(/\r\n/g, '\n'), moveOptions.hunks)
        if (!applied?.ok) {
          throw await makeFsError(
            `cannot apply patch to "${target.displayPath}": hunk did not match the current file`,
            'FS_PATCH_MISMATCH',
          )
        }
        after = applied.after
      }
      const destinationBefore = await readIfExists(destination)
      if (
        moveOptions.expectedDestination?.kind === 'createIfAbsent' &&
        destinationBefore !== null
      ) {
        throw await makeFsError(
          `cannot overwrite existing "${destination.displayPath}" without reading it first`,
          'FS_NOT_OBSERVED',
        )
      }
      const writeResult = await performWrite({
        operation: 'write',
        target: destination,
        content: after,
        expectedVersion:
          typeof moveOptions.expectedDestination?.version === 'string'
            ? moveOptions.expectedDestination.version
            : undefined,
        call,
        intent: moveOptions.expectedDestination,
      })
      const destinationResult = {
        version: writeResult.version,
        before: destinationBefore,
        after,
      }
      try {
        await performWrite({
          operation: 'delete',
          target,
          expectedVersion: sourceVersion,
          call,
          intent: moveOptions.expectedSource,
        })
      } catch (error) {
        return {
          destination: destinationResult,
          sourceDeleted: false,
          sourceBefore: sourceText,
          sourceFailure: {
            code: error?.code ?? 'FS_IO_ERROR',
            message: String(error?.message ?? error),
          },
        }
      }
      return {
        destination: destinationResult,
        sourceDeleted: true,
        sourceBefore: sourceText,
      }
    },
  }
}

function countOccurrences(text, search) {
  if (typeof search !== 'string' || search.length === 0) return 0
  return text.split(search).length - 1
}

function replaceOnce(text, search, replacement) {
  const index = text.indexOf(search)
  if (index < 0) return text
  return text.slice(0, index) + replacement + text.slice(index + search.length)
}

/**
 * sidecar → host 请求。当前只有 `fs.write.request`，答复 kind 为 `fs.write.result`。
 * Rust 在等待 tool.result 的读循环里同步服务这些请求（不会死锁）。
 */
async function requestHost(kind, payload, extra, signal) {
  signal.throwIfAborted()
  hostRequestSeq += 1
  const requestId = `host-${hostRequestSeq}`
  const response = await new Promise((resolve, reject) => {
    const cleanup = () => {
      clearTimeout(timer)
      signal.removeEventListener('abort', onAbort)
      pendingHostRequests.delete(requestId)
    }
    const onAbort = () => {
      cleanup()
      reject(signal.reason)
    }
    const timer = setTimeout(() => {
      cleanup()
      reject(
        Object.assign(new Error(`sidecar request ${kind} timed out`), {
          code: 'FS_IO_ERROR',
        }),
      )
    }, HOST_REQUEST_TIMEOUT_MS)
    signal.addEventListener('abort', onAbort, { once: true })
    pendingHostRequests.set(requestId, {
      resolve(result) {
        cleanup()
        resolve(result)
      },
    })
    try {
      send(kind, requestId, payload, extra)
    } catch (error) {
      cleanup()
      reject(error)
    }
  })
  return response
}

function createSystemPromptService() {
  const sections = []
  return {
    section(definition) {
      sections.push(definition)
    },
    getSectionOrder(name) {
      return sections.find((section) => section.name === name)?.order ?? 100
    },
    context(definition) { sections.push(definition) },
    getContextOrder() { return 100 },
    sections() {
      return sections
    },
  }
}

function createPluginContext(revision) {
  const root = new Context()
  const workingDirectory = hostServices?.createWorkingDirectory()
  const images = hostServices && hostCaps.has(IMAGE_ATTACHMENT_CAP)
    ? createImageAttachments({
        workspace: WORKSPACE_ROOT,
        currentCall: () => callStore.getStore(),
        callService: hostServices.call,
      })
    : null
  root.provide('tools', {
    ...toolsService,
    register: (definition, options) => toolsService.register(definition, options, revision),
    get: (name, options) => toolsService.get(name, options, revision),
    unregister: (name, options) => toolsService.unregister(name, options, revision),
    executable(name, agent) {
      const active = callStore.getStore()
      if (active?.tool === name && active.agent === agent) return active.exactDefinition
      const binding = active?.tool === PROGRAM_TOOL && active.agent === agent && active.programDefinitions?.[name]
      if (binding) {
        return { ...binding.definition, execute: (args, exec) => executeNested(binding, args, exec) }
      }
      return undefined
    },
    checkGuards: () => undefined,
    setExecutor(executor) {
      const previous = executorScopes.get(root)
      previous?.removeTransport?.()
      const selected = { executor, ptc: typeof executor.requirePtcTransport === 'function' }
      if (selected.ptc) selected.removeTransport = toolsService.register(executor.requirePtcTransport(), {}, revision)
      executorScopes.set(root, selected)
    },
  })
  root.effect(() => () => {
    executorScopes.get(root)?.removeTransport?.()
    executorScopes.delete(root)
  }, 'dispose scoped executor')
  root.provide('fs', createFsService({ workingDirectory, images }))
  root.provide('systemPrompt', createSystemPromptService())
  if (hostServices && hostCaps.has(AGENT_TOOL_CONTEXT_CAP)) {
    root.provide('agents', {
      currentInitiator() {
        return callStore.getStore()?.agent
      },
      get(sessionId) {
        const active = callStore.getStore()
        if (active?.sessionId === sessionId) return active.agent
        const entry = active?.subagentTree?.entries.find(entry => entry.id === sessionId)
        return entry ? { id: entry.id, status: entry.running ? 'running' : 'inactive' } : undefined
      },
      turnBoundaryOf(session) {
        const active = callStore.getStore()
        if (!active?.agent || active.agent.session !== session) {
          throw new Error('turn boundary requires the active agent session')
        }
        return {
          lastTurn: active.turn?.number ?? 0,
          openTurnStartSeq: active.turn?.start_seq ?? null,
          lastStepStartSeq: active.queryBefore,
        }
      },
    })
  }
  if (images) {
    root.provide('attachments', images.attachmentService)
    root.provide('llm', images.modelService)
  }
  if (hostServices && hostCaps.has(USER_QUESTIONS_CAP)) {
    root.provide('userQuestions', createUserQuestions({
      currentCall: () => callStore.getStore(),
      callService: hostServices.call,
    }))
  }
  if (hostServices && (hostCaps.has(SEARCH_PROCESS_CAP) || hostCaps.has(SHELL_PROCESS_CAP))) {
    const processes = createToolProcesses({
      currentCall: () => callStore.getStore(),
      callService: hostServices.call,
      workspace: WORKSPACE_ROOT,
    })
    if (hostCaps.has(SEARCH_PROCESS_CAP)) root.provide('subprocess', processes.subprocess)
    if (hostCaps.has(SHELL_PROCESS_CAP)) {
      root.provide('shell', processes.shell)
      root.provide('shellEnv', processes.shellEnv)
    }
  }
  if (hostServices) {
    // L2（spec §13）：只有协商到 service.proxy 时才注入这些代理；
    // 未协商时官方插件对 `workingDirectory`/`storage` 的 inject 失败，保持 M2 行为。
    root.provide('workingDirectory', workingDirectory)
    if (sessionProjections) root.provide('sessionProjections', sessionProjections)
    const storageHub = hostServices.createStorage()
    root.provide('storage', storageHub)
    // 官方 `dsh-storage-domain` 通过 `ctx.inject(['storage.backend.<name>'])` 等后端
    // 生命周期键；我们的 `storage-kv` backend（Rust 介质的 adapter）在这里以同一
    // 实例挂上，等价于官方 json/sqlite backend 的 provide（介质后端本来就由 Rust 实现）。
    const storageBackend = storageHub.backend.get('storage-kv')
    root.provide('storage.backend.storage-kv', storageBackend)
  }
  return root
}

async function bootPlugins(entries, root = createPluginContext(), strict = false) {
  const teamEnabled = entries.some(entry => entry.enabled !== false && entry.name === TEAM_PLUGIN)
  const workflowEnabled = entries.some(entry => entry.enabled !== false && [WORKFLOW_PLUGIN, RALPH_PLUGIN].includes(entry.name))
  let inspectBootError
  if (entries.some(entry => entry.enabled !== false && entry.name === INSPECT_PLUGIN)) {
    try {
      if (!hostServices || !hostCaps.has(AGENT_TOOL_CONTEXT_CAP)) throw new Error('inspection requires a scoped host')
      await mountCordisInspection(root, hostServices)
    } catch (error) { inspectBootError = error }
  }
  let scheduleBootError
  if (hostServices && hostCaps.has(SCHEDULE_CAP)
      && entries.some(entry => entry.enabled !== false && entry.name === SCHEDULE_PLUGIN)) {
    try {
      const schedules = await createSchedules({ currentCall: () => callStore.getStore(), callService: hostServices.call })
      scheduleBridges.set(root, schedules)
      root.provide('schedule', schedules.service)
    } catch (error) { scheduleBootError = error }
  }
  if (hostServices && hostCaps.has(SUBAGENT_CAP)
      && (teamEnabled || workflowEnabled || entries.some(entry => entry.enabled !== false && SUBAGENT_PLUGINS.has(entry.name)))) {
    const subagents = createSubagents({ parent: root, currentCall: () => callStore.getStore(), callService: hostServices.call })
    subagentBridges.set(root, subagents)
    root.provide('subagents', subagents.service)
  }
  if (teamEnabled && hostServices && hostCaps.has(TEAM_CAP)) {
    const teams = createAgentTeams({ currentCall: () => callStore.getStore(), callService: hostServices.call })
    teamBridges.set(root, teams)
    root.provide('agentTeams', teams.service)
  }
  let workflowBootError
  if (workflowEnabled) {
    try {
      if (!root.get('subagents')) throw new Error('workflow requires host subagent execution')
      await mountWorkflowEngine(root, WORKSPACE_ROOT)
    } catch (error) { workflowBootError = error }
  }
  let ptcBootError
  if (entries.some(entry => entry.enabled !== false && entry.name === PTC_EXECUTOR_PLUGIN) && !root.get('ptcRuntime')) {
    try {
      if (!hostCaps.has(PROGRAM_CAP)) throw new Error('programmatic calls require Rust program admission')
      const { mountReadOnlyPtc } = await import('./lib/workflow-runtime.mjs')
      await mountReadOnlyPtc(root, WORKSPACE_ROOT)
    } catch (error) { ptcBootError = error }
  }
  if (hostServices && hostCaps.has(GOALS_CAP)
      && entries.some(entry => entry.enabled !== false && entry.name === '@deepseek-ai/dsh-tool-goal')) {
    const goals = createToolGoals({ currentCall: () => callStore.getStore(), callService: hostServices.call })
    goalBridges.set(root, goals)
    root.provide('goals', goals.service)
  }
  if (hostServices && hostCaps.has(SESSION_QUERY_CAP)
      && entries.some(entry => entry.enabled !== false && entry.name === '@deepseek-ai/dsh-tool-session-query')) {
    const query = await createSessionQuery({ currentCall: () => callStore.getStore(), callService: hostServices.call })
    queryBridges.set(root, query)
    root.provide('sessionQuery', query.service)
  }
  const jobsEnabled = hostServices && hostCaps.has(BACKGROUND_JOBS_CAP)
    && entries.some(entry => entry.enabled !== false && entry.name === '@deepseek-ai/dsh-tool-jobs')
  if (jobsEnabled) {
    const jobs = createToolJobs({ currentCall: () => callStore.getStore(), callService: hostServices.call })
    jobBridges.set(root, jobs)
    root.provide('jobs', jobs.service)
  }
  let terminalBootError
  if (entries.some(entry => entry.enabled !== false && TERMINAL_PLUGIN_NAMES.has(entry.name))) {
    try {
      if (!hostServices || !hostCaps.has(TERMINAL_CAP) || !process.env.DSHR_TERMINAL_SHELL) {
        throw new Error('official persistent terminals require a desktop shell')
      }
      const terminals = await createToolTerminals({
        parent: root,
        currentCall: () => callStore.getStore(),
        callService: hostServices.call,
        findAgent: id => agentActors.get(id),
        shellPath: process.env.DSHR_TERMINAL_SHELL,
      })
      terminalBridges.set(root, terminals)
      root.provide('terminals', terminals.service)
    } catch (error) {
      terminalBootError = error
    }
  }
  const fibers = []
  for (const entry of entries) {
    if (entry.enabled === false) continue
    try {
      const enabledEntry = gateEntry(entry, writeEnabled)
      if (enabledEntry.name === NATIVE_EXECUTOR_PLUGIN && !hostCaps.has(AGENT_TOOL_CONTEXT_CAP)) {
        throw new Error('native execution requires the scoped host driver')
      }
      if (enabledEntry.name === PTC_EXECUTOR_PLUGIN && !hostCaps.has(PROGRAM_CAP)) {
        throw new Error('programmatic calls require Rust program admission')
      }
      if (ptcBootError && enabledEntry.name === PTC_EXECUTOR_PLUGIN) throw ptcBootError
      if (inspectBootError && enabledEntry.name === INSPECT_PLUGIN) throw inspectBootError
      if (scheduleBootError && enabledEntry.name === SCHEDULE_PLUGIN) throw scheduleBootError
      if (workflowBootError && [WORKFLOW_PLUGIN, RALPH_PLUGIN].includes(enabledEntry.name)) throw workflowBootError
      if (enabledEntry.name === WORKFLOW_PLUGIN) {
        const config = enabledEntry.config ?? {}
        if (config.enableRunInBackground === true || (config.toolName && config.toolName !== 'workflow')) {
          throw new Error('the host supports the foreground workflow tool')
        }
        enabledEntry.config = { ...config, enableRunInBackground: false }
      }
      if (enabledEntry.name === RALPH_PLUGIN) {
        const config = enabledEntry.config ?? {}
        enabledEntry.config = { ...config, subagentProvider: config.subagentProvider ?? 'local', maxRounds: config.maxRounds ?? 64 }
      }
      if (enabledEntry.name === '@deepseek-ai/dsh-tool-subagent') {
        const config = enabledEntry.config ?? {}
        if (config.toolName && config.toolName !== 'subagent') throw new Error('the Rust host requires the subagent tool name')
        if (config.agentOptions || config.agentPreset || config.allowedAgentPresets || config.modelDiscovery || config.modelSelection) {
          throw new Error('subagents use the current host model and profile')
        }
      }
      if (terminalBootError && TERMINAL_PLUGIN_NAMES.has(enabledEntry.name)) throw terminalBootError
      if (hostServices && hostCaps.has(AGENT_TOOL_CONTEXT_CAP)
          && SKILL_PLUGIN_NAMES.has(enabledEntry.name)
          && !root.get('skills')) {
        const { SkillRegistry } = await import('@deepseek-ai/dsh-skill')
        await root.plugin(SkillRegistry)
      }
      if (hostServices && hostCaps.has(AGENT_TOOL_CONTEXT_CAP)
          && WEB_PLUGIN_NAMES.has(enabledEntry.name) && !root.get('web')) {
        const { WebRuntime } = await import('@deepseek-ai/dsh-web')
        const credentials = process.env.DSHR_WEB_CREDENTIAL_VALUES
        if (credentials) {
          const { createLaunchEnvironmentSnapshot } = await import('@deepseek-ai/dsh-launch-environment')
          root.provide('launchEnvironment', createLaunchEnvironmentSnapshot([
            { source: 'process', values: JSON.parse(credentials) },
          ]))
        }
        class AvailableWebRuntime extends WebRuntime {
          async searchReady(provider) {
            return this.listProviders().search.length > 0 && await super.searchReady(provider)
          }
          async fetchReady(provider) {
            return this.listProviders().fetch.length > 0 && await super.fetchReady(provider)
          }
        }
        await root.plugin(AvailableWebRuntime, {})
      }
      const module = await import(enabledEntry.name)
      if (enabledEntry.name === '@deepseek-ai/dsh-office-kit') {
        if (!hostServices || !hostCaps.has(AGENT_TOOL_CONTEXT_CAP)) {
          throw new Error('office kit requires scoped agent tool context')
        }
        const raw = enabledEntry.config ?? {}
        const outputDirectory = officeOutputPath(
          WORKSPACE_ROOT, raw.outputDirectory ?? join(WORKSPACE_ROOT, '.dshr', 'office-outputs'),
        )
        if (!outputDirectory) throw new Error('office output directory must be inside the workspace')
        await root.plugin(module.OfficeKit, { ...raw, outputDirectory })
        continue
      }
      if (typeof module.apply !== 'function') throw new Error('plugin must export apply')
      // Schemastery 的默认值必须显式解析：Cordis 直接收原始 config 时不会补默认，
      // 官方插件随后会因 `undefined` 的配置项在 apply 里抛错。
      let rawConfig = enabledEntry.config ?? {}
      if (enabledEntry.name === '@deepseek-ai/dsh-tool-terminal') {
        rawConfig = { ...rawConfig, enableRunInBackground: false }
      }
      if (jobsEnabled && enabledEntry.name === '@deepseek-ai/dsh-tool-jobs') {
        rawConfig = { ...rawConfig, inlineOutputBytes: 0 }
      }
      if (jobsEnabled && ['@deepseek-ai/dsh-tool-pwsh', '@deepseek-ai/dsh-tool-bash'].includes(enabledEntry.name)) {
        if (rawConfig.promoteOnTimeout === true) throw new Error('foreground timeout promotion is unavailable')
        rawConfig = { ...rawConfig, promoteOnTimeout: false }
      }
      // Volatile web settings are wrapped when the fiber resolves its schema.
      const volatileWeb = enabledEntry.name === '@deepseek-ai/dsh-web-deepseek-api'
        || enabledEntry.name === '@deepseek-ai/dsh-web-search-deepseek'
      const config = module.Config && !volatileWeb ? module.Config(rawConfig) : rawConfig
      if (enabledEntry.name === '@deepseek-ai/dsh-tool-ask-user' && config.mode === 'timed') {
        throw new Error('only legacy foreground user questions are supported')
      }
      // 官方插件只导出它"声明"的 inject；但部分插件在 apply 里直接访问未声明的服务
      // （DSH 的 Loader 用 entry 级 inject 覆盖补上）。这里用同样的方式包一层。
      let inject = entry.inject ?? module.inject ?? []
      if (enabledEntry.name === '@deepseek-ai/dsh-tool-fs' && config.readImage &&
          hostServices && hostCaps.has(IMAGE_ATTACHMENT_CAP)) {
        inject = [...new Set([...inject, 'workingDirectory', 'llm'])]
      }
      const plugin = {
        name: module.name,
        Config: module.Config,
        inject,
        apply: module.apply,
      }
      // 等待插件装载完成：官方插件的 apply 可能是异步的，错误也在这里冒出来。
      const handle = root.plugin(plugin, config)
      const fiber = await handle
      fibers.push({ handle, fiber, name: entry.name })
      send('log.emit', `boot-${entry.name}`, {
        level: 'info',
        message: `loaded official plugin ${entry.name} (tools now: ${[...globalTools.keys()].join(', ') || '<none>'})`,
      })
    } catch (error) {
      if (strict) throw new Error(`${entry.name}: ${String(error?.message ?? error)}`)
      // 单个插件失败不拖垮整台侧车：其余插件照常可用，诊断走 log.emit。
      send('log.emit', `boot-failed-${entry.name}`, {
        level: 'error',
        message: `failed to load ${entry.name}: ${String(error?.message ?? error)}`,
      })
    }
  }
  if (strict) {
    for (const { handle, fiber, name } of fibers) {
      await handle.await()
      const missing = Object.keys(fiber.inject).filter(key => fiber.ctx.get(key) === undefined)
      if (missing.length) throw new Error(`${name}: missing services ${missing.join(', ')}`)
    }
  }
  return root
}

async function mountPreset(revision, entries) {
  const scope = new Map()
  revisionTools.set(revision, scope)
  const root = createPluginContext(revision)
  revisionContexts.set(revision, root)
  const dispose = async () => {
    try {
      await root.fiber.dispose()
    } finally {
      for (const binding of scope.values()) removeTool(binding, true)
      revisionTools.delete(revision)
      revisionContexts.delete(revision)
    }
  }
  try {
    await bootPlugins(entries, root, true)
    return dispose
  } catch (error) {
    await dispose()
    throw error
  }
}

function agentFacade(sessionId) {
  const current = () => {
    const active = callStore.getStore()
    return active?.sessionId === sessionId ? active : undefined
  }
  const agent = agentActors.get(sessionId) ?? {
    id: sessionId,
    status: 'running',
    get ctx() { return current()?.terminalOwnerContext ?? current()?.pluginContext },
    inbox: {
      get nextStep() { return current()?.subagentInbox ?? [] },
      remove(id) {
        const notices = current()?.jobNotices
        const index = notices?.findIndex(notice => notice.id === id) ?? -1
        if (index >= 0) notices.splice(index, 1)
      },
    },
    inject(message) {
      if (message.source?.kind === 'tool-jobs') current()?.jobNotices?.push(message)
    },
    get options() { return current()?.model ?? {} },
    session: {
      id: sessionId,
      get header() {
        const call = current()
        return call?.queryHeader ?? { id: sessionId, ...(call?.goal?.subagent ? { origin: 'subagent' } : {}) }
      },
      seq: 0,
      snapshotEvents: () => goalTurnSnapshot(current()?.goal),
      surface: { nodes: [] },
      eventAt: () => undefined,
      append(type, data) {
        const append = current()?.appendSessionEvent
        if (!append) throw new Error('session event append requires an active tool')
        return append(type, data)
      },
      requestHeader: () => undefined,
      requestContext: () => current()?.model,
      deriveMessages: () => [],
    },
  }
  agentActors.delete(sessionId)
  agentActors.set(sessionId, agent)
  if (agentActors.size > 128) agentActors.delete(agentActors.keys().next().value)
  return agent
}

async function prepareAgentStep(msg) {
  const sessionId = msg.session_id
  const { step, model, user_text: userText, registrations, turn } = msg.payload
  if (!sessionId || !Number.isInteger(step) || step < 1 || typeof model?.provider !== 'string'
      || typeof model?.model !== 'string' || !registrations || Array.isArray(registrations)) {
    throw new Error('invalid agent step context')
  }
  const revision = presets.binding(sessionId)
  const contexts = [cordisRoot, revisionContexts.get(revision)].filter(Boolean)
  const bindings = Object.create(null)
  for (const [name, registrationId] of Object.entries(registrations)) {
    const binding = toolsById.get(registrationId)
    if (binding?.definition.name !== name || (binding.sessionId && binding.sessionId !== sessionId)
        || (binding.revision && binding.revision !== revision)) continue
    bindings[name] = binding
  }
  const agent = agentFacade(sessionId)
  const controller = new AbortController()
  const timer = setTimeout(() => controller.abort(new Error('agent pre-step timed out')), 7000)
  const jobNotices = []
  try {
    return await callStore.run(
      { sessionId, depth: 0, controller, agent, turn, model, stepBindings: bindings, jobNotices },
      async () => {
        if (bindings.job_output) {
          const jobs = contexts.map(context => jobBridges.get(context)).reverse().find(Boolean)
          if (jobs) await jobs.notify()
        }
        if (SUBAGENT_TOOLS.some(name => bindings[name])) {
          const subagents = contexts.map(context => subagentBridges.get(context)).reverse().find(Boolean)
          if (subagents) jobNotices.push(...await subagents.notices())
        }
        if (SCHEDULE_TOOLS.some(name => bindings[name])) {
          const schedules = contexts.map(context => scheduleBridges.get(context)).reverse().find(Boolean)
          if (schedules) jobNotices.push(...await schedules.notices())
        }
        const result = await collectAgentStep({
          contexts, agent, signal: controller.signal, step, userText,
          modelSelection: model, toolSet: undefined, bindings, jobNotices,
          reportError: (name, error) => log('warn', `could not resolve availability of ${name}: ${error.message}`),
        })
        const selected = contexts.map(context => executorScopes.get(context)).filter(Boolean).at(-1)
        let programDefinitions
        if (selected?.ptc && bindings[PROGRAM_TOOL] && hostCaps.has(PROGRAM_CAP)) {
          programDefinitions = Object.fromEntries(Object.entries(bindings).filter(([name]) => name !== PROGRAM_TOOL))
          try {
            const sdk = selected.executor.renderSdk(Object.values(programDefinitions).map(binding => binding.definition))
            if (typeof sdk !== 'string' || Buffer.byteLength(sdk) > 64 * 1024) throw new Error('program SDK is too large')
            result.system_sections.push('Call run_code to use the scoped tools from a TypeScript program. The functions are available on the tools object.')
            result.system_sections.push(sdk)
            result.deferred_tools = Object.keys(programDefinitions)
          } catch (error) {
            log('warn', `program SDK unavailable: ${error.message}`)
            programDefinitions = undefined
            result.unavailable_tools.push(PROGRAM_TOOL)
            delete bindings[PROGRAM_TOOL]
          }
        } else if (bindings[PROGRAM_TOOL]) {
          result.unavailable_tools.push(PROGRAM_TOOL)
          delete bindings[PROGRAM_TOOL]
        }
        stepScopes.delete(sessionId)
        stepScopes.set(sessionId, { revision, bindings, programDefinitions })
        if (stepScopes.size > 128) stepScopes.delete(stepScopes.keys().next().value)
        return result
      },
    )
  } finally {
    clearTimeout(timer)
  }
}

function toolEffects(call) {
  const deferred = []
  let firstError
  call.defer = pending => {
    deferred.push(pending.catch(error => { firstError ??= error }))
    return pending
  }
  call.appendSessionEvent = (type, data) => call.defer(hostServices
    ? hostServices.call({
        serviceKey: SERVICE_KEYS.sessionLog, method: 'events.append',
        args: { event_type: String(type), data: data ?? null },
        sessionId: call.sessionId, callId: call.callId, depth: call.depth,
      })
    : Promise.reject(new Error('session-log service is not negotiated (v2 host required)')))
  return async () => {
    await Promise.all(deferred)
    call.controller.signal.throwIfAborted()
    if (firstError) throw firstError
  }
}

function adaptersFor(hooks, name) {
  const from = map => hooks.map(context => map.get(context)).findLast(Boolean)
  return {
    jobs: from(jobBridges),
    terminals: (name.startsWith('terminal_') || ['pwsh', 'bash'].includes(name)) ? from(terminalBridges) : undefined,
    goals: GOAL_TOOLS.includes(name) ? from(goalBridges) : undefined,
    query: SESSION_QUERY_TOOLS.includes(name) ? from(queryBridges) : undefined,
    subagents: SUBAGENT_TOOLS.includes(name) || TEAM_TOOLS.includes(name) ? from(subagentBridges) : undefined,
    teams: TEAM_TOOLS.includes(name) ? from(teamBridges) : undefined,
  }
}

async function prepareAdapters(adapters, exec) {
  const { jobs, goals, query, subagents, teams, terminals } = adapters
  if (jobs) await jobs.prepare(exec)
  if (goals) await goals.prepare(exec)
  if (query) await query.prepare()
  if (subagents && ['list_agents', 'send_message', 'interrupt_agent', 'team_task_list', 'team_task_update'].includes(exec.name)) {
    await subagents.prepare()
  }
  if (teams && ['team_task_list', 'team_task_get'].includes(exec.name)) await teams.prepare()
  if (terminals) await terminals.prepare(exec)
}

async function settleAdapters(adapters) {
  if (adapters.jobs) await adapters.jobs.confirm()
  if (adapters.goals) await adapters.goals.commit()
}

async function executeNested(binding, args, exec) {
  const outer = callStore.getStore()
  if (!outer?.programDefinitions || outer.programDefinitions[binding.definition.name] !== binding || !exec.parent) {
    throw new Error('nested calls require an active run_code invocation')
  }
  const request = (method, fields) => hostServices.call({
    serviceKey: PROGRAM_SERVICE, method, args: fields, sessionId: outer.sessionId, depth: outer.depth + 1,
    timeoutMs: 120_000,
  })
  const admitted = await request('begin', {
    name: binding.definition.name, registration: binding.registrationId, call_id: exec.callId, arguments: args,
  })
  const signal = AbortSignal.any([exec.signal, outer.controller.signal, AbortSignal.timeout(admitted.timeout_ms)])
  const child = {
    tool: binding.definition.name, sessionId: outer.sessionId, depth: outer.depth + 1,
    callId: exec.callId, toolRequestId: admitted.id, controller: { signal },
    turn: outer.turn, model: outer.model, agent: outer.agent, arguments: args,
    timeoutMs: admitted.timeout_ms, pluginContext: outer.pluginContext, stepBindings: outer.stepBindings,
  }
  callContexts.set(signal, child)
  const flush = toolEffects(child)
  const hooks = [cordisRoot, revisionContexts.get(presets.binding(child.sessionId))].filter(Boolean)
  const adapters = adaptersFor(hooks, child.tool)
  // The official executor owns the execution token and final tool result. Each
  // host callback from the body uses the independently approved child identity.
  const nested = {
    ...exec, signal,
    ...(AGENT_FACADE_TOOLS.has(child.tool) ? {} : { agent: undefined }),
  }
  let value
  let failure
  try {
    value = await callStore.run(child, async () => {
      await prepareAdapters(adapters, nested)
      const run = () => binding.definition.execute(args, nested)
      const result = adapters.subagents && child.tool === 'wait_agent'
        ? await adapters.subagents.deliverWait(nested, run)
        : await run()
      await flush()
      await settleAdapters(adapters)
      if (child.goal?.notice) exec.deferContext({ source: { kind: 'tool-goal' }, content: child.goal.notice })
      return result
    })
  } catch (error) { failure = error }
  try { await request('finish', { id: admitted.id }) } catch (error) { failure ??= error }
  if (failure) throw failure
  return value
}

async function runTool(definition, msg) {
  const controller = new AbortController()
  // 服务代理读取的调用上下文：session_id 只从信封来（spec §5），depth 用于 +1。
  const callContext = {
    callId: msg.call_id,
    toolRequestId: msg.request_id,
    controller,
    tool: definition.name,
    timeoutMs: definition.timeoutMs,
    sessionId: msg.session_id,
    turn: msg.payload?.turn,
    model: definition.name === 'read_image'
      ? { provider: 'dshr', model: 'active session model' }
      : msg.payload?.model,
    depth: Number.isInteger(msg.depth) ? msg.depth : 0,
    exactDefinition: definition,
  }
  const activeRevision = presets.binding(msg.session_id)
  const stepScope = stepScopes.get(msg.session_id)
  if (stepScope && stepScope.revision === activeRevision) {
    callContext.stepBindings = Object.fromEntries(Object.entries(stepScope.bindings)
      .filter(([, binding]) => toolsById.get(binding.registrationId) === binding))
    if (definition.name === PROGRAM_TOOL && stepScope.programDefinitions) {
      callContext.programDefinitions = Object.fromEntries(Object.entries(stepScope.programDefinitions)
        .filter(([, binding]) => toolsById.get(binding.registrationId) === binding))
    }
  }
  inflight.set(msg.request_id, callContext)
  callContexts.set(controller.signal, callContext)
  const started = Date.now()
  try {
    // M4 W6：给官方工具一个最小 `exec.agent.session.append(type, data)`（todo 等需要）。
    // 写入走 session-log/v1 events.append；官方代码不 await 时，这里在 execute 结束后
    // 统一 flush 并抛出首个错误，避免"写入丢失但工具显示成功"。
    const flush = toolEffects(callContext)
    const exec = {
      signal: controller.signal,
      callId: msg.call_id,
      rootCallId: msg.request_id,
      name: definition.name,
      arguments: msg.payload?.arguments ?? {},
      // 只给明确支持最小 agent facade 的工具；fs 等工具在 agent 存在时会走
      // `ctx.workingDirectory.ensure` 分支，但它们没有 inject workingDirectory，
      // 这会让原本无 agent 的写路径回归。逐工具放开，避免"给了 agent 反而坏"。
      ...(AGENT_FACADE_TOOLS.has(definition.name)
        ? { agent: agentFacade(msg.session_id) }
        : {}),
    }
    callContext.agent = exec.agent
    callContext.arguments = exec.arguments
    const argumentsValue = exec.arguments
    if (exec.name === PROGRAM_TOOL && (argumentsValue.sandbox_permissions !== undefined || argumentsValue.justification !== undefined)) {
      throw new Error('the program process uses the host read-only sandbox')
    }
    const hooks = hostServices && hostCaps.has(AGENT_TOOL_CONTEXT_CAP)
      ? [cordisRoot, revisionContexts.get(presets.binding(msg.session_id))].filter(Boolean)
      : []
    callContext.pluginContext = hooks.at(-1)
    const executionContext = hooks.findLast(context => executorScopes.has(context))
    const selectedExecutor = executionContext && executorScopes.get(executionContext)
    const adapters = adaptersFor(hooks, exec.name)
    const notifyToolResult = async (result, skip) => {
      for (const context of hooks) {
        if (context !== skip) await context.parallel('tools/result', exec, result)
      }
    }
    // AsyncLocalStorage：ctx.workingDirectory/ctx.storage 的代理在官方 execute 的
    // 任意 await 点都能拿到同一个 call_id/session_id（tool.cancel 据此取消服务调用）。
    const executed = await callStore.run(callContext, async () => {
      await prepareAdapters(adapters, exec)
      let result
      const activateExecutor = selectedExecutor && (!selectedExecutor.ptc || exec.name === PROGRAM_TOOL)
      try {
        const run = () => activateExecutor
          ? executeWithOfficialExecutor(selectedExecutor.executor, exec)
          : executeWithHooks(hooks, exec, () => definition.execute(argumentsValue, exec))
        result = adapters.subagents && exec.name === 'wait_agent' ? await adapters.subagents.deliverWait(exec, run) : await run()
      } catch (error) {
        await notifyToolResult({ isError: true, error }, activateExecutor ? executionContext : undefined)
        throw error
      }
      controller.signal.throwIfAborted()
      await notifyToolResult({ isError: false, ...result }, activateExecutor ? executionContext : undefined)
      return result
    })
    const { value } = executed
    await flush()
    await callStore.run(callContext, () => settleAdapters(adapters))

    let content
    try {
      content = executed.content ?? definition.output?.render?.(msg.payload?.arguments ?? {}, value)
    } catch {
      content = undefined
    }
    if (!Array.isArray(content) || content.length === 0) {
      content = [{ type: 'json', value }]
    }
    if (callContext.goal?.notice) content = [...content, ...callContext.goal.notice]
    for (const context of executed.contexts ?? []) {
      if (context.source?.kind === 'tool-goal' || context.source?.kind === 'ptc-mode') {
        content = [...content, ...context.content]
      }
    }
    let meta = null
    try {
      meta = executed.meta ?? definition.output?.presentationMeta?.(msg.payload?.arguments ?? {}, value) ?? null
    } catch {
      meta = null
    }

    send(
      'tool.result',
      msg.request_id,
      { call_id: msg.call_id, is_error: false, content, value, meta },
      { call_id: msg.call_id, plugin_id: 'dsh-official' },
    )
    send('log.emit', `call-${msg.call_id}`, {
      level: 'debug',
      message: `dsh tool ${definition.name} done in ${Date.now() - started}ms`,
    })
  } catch (error) {
    const code = controller.signal.aborted
      ? 'E_CANCELLED'
      : error?.code === 'TOOL_TIMEOUT' ? 'E_TOOL_TIMEOUT'
      : typeof error?.code === 'string' ? error.code : 'E_TOOL'
    send(
      'tool.result',
      msg.request_id,
      {
        call_id: msg.call_id,
        is_error: true,
        content: [{ type: 'text', text: String(error?.message ?? error) }],
        value: null,
        meta: null,
      },
      {
        call_id: msg.call_id,
        error: {
          code,
          message: String(error?.message ?? error),
          retryable: false,
        },
      },
    )
  } finally {
    inflight.delete(msg.request_id)
  }
}

async function handle(msg) {
  switch (msg.kind) {
    case 'runtime.hello': {
      generation = msg.payload?.generation ?? generation
      hostCaps.clear()
      for (const capability of msg.payload?.host_caps ?? []) hostCaps.add(String(capability))
      writeEnabled = hostCaps.has('fs.write')

      // 版本协商（spec §3.2）：取 min(protocol_max)；v1 宿主保持 M2 行为。
      const hostProtocol = Number(msg.payload?.protocol_max)
      negotiatedProtocol =
        Number.isFinite(hostProtocol) && hostProtocol >= 1
          ? Math.min(PROTOCOL_MAX, hostProtocol)
          : 1
      const serviceProxyEnabled = shouldEnableHostServices({
        negotiatedProtocol,
        hostCaps,
        env: process.env,
      })
      sessionProjections = serviceProxyEnabled
        ? createSessionProjectionsShim({
            callService: (request) => hostServices.call(request),
            onDiagnostic: (diagnostic) =>
              send('log.emit', `proj-${Date.now()}`, {
                level: 'debug',
                message: JSON.stringify(diagnostic),
              }),
            onCreated: (sessionView) => {
              if (cordisRoot && typeof cordisRoot.emit === 'function') {
                cordisRoot.emit('session/created', sessionView)
              }
            },
            onEvent: (sessionView, event) => {
              if (cordisRoot && typeof cordisRoot.emit === 'function') {
                cordisRoot.emit('session/event', sessionView, event)
              }
            },
          })
        : null
      hostServices = serviceProxyEnabled
        ? createHostServicesRuntime({
            sendEnvelope,
            generation: () => generation,
            timeoutMs: HOST_SERVICE_TIMEOUT_MS,
            callContext: () => callStore.getStore(),
            onDiagnostic: (diagnostic) =>
              send('log.emit', `diag-${Date.now()}`, {
                level: 'debug',
                message: JSON.stringify(diagnostic),
              }),
            busEmit: (eventName, payload) => {
              // TODO(cordis)：DSH Cordis Context 的事件 API 在官方 Loader 里是
              // root.emit(name, ...args)；这里先桥接最小形态，集成轮再核对签名。
              if (cordisRoot && typeof cordisRoot.emit === 'function') {
                cordisRoot.emit(eventName, payload)
              }
            },
          })
        : null
      const processCaps = hostServices
        ? [SEARCH_PROCESS_CAP, SHELL_PROCESS_CAP].filter(cap => hostCaps.has(cap))
        : []
      const imageCap = hostServices && hostCaps.has(IMAGE_ATTACHMENT_CAP)
      const questionsCap = hostServices && hostCaps.has(USER_QUESTIONS_CAP)
      const agentToolContext = hostServices && hostCaps.has(AGENT_TOOL_CONTEXT_CAP)
      const jobsCap = hostServices && hostCaps.has(BACKGROUND_JOBS_CAP)
      const terminalCap = hostServices && hostCaps.has(TERMINAL_CAP)
      const goalsCap = hostServices && hostCaps.has(GOALS_CAP)
      const queryCap = hostServices && hostCaps.has(SESSION_QUERY_CAP)
      const subagentCap = hostServices && hostCaps.has(SUBAGENT_CAP)
      const teamCap = hostServices && hostCaps.has(TEAM_CAP)
      const scheduleCap = hostServices && hostCaps.has(SCHEDULE_CAP)
      const programCap = hostServices && hostCaps.has(PROGRAM_CAP)
      send('runtime.ready', msg.request_id, {
        protocol: negotiatedProtocol,
        dsh_version: '0.2.2-rc.1',
        sidecar_caps: [
          'tool.cancel',
          'dsh-official',
          'agent.presets',
          ...(writeEnabled ? ['fs.write'] : []),
          ...(hostServices ? ['service.proxy'] : []),
          ...(imageCap ? [IMAGE_ATTACHMENT_CAP] : []),
          ...(questionsCap ? [USER_QUESTIONS_CAP] : []),
          ...(agentToolContext ? [AGENT_TOOL_CONTEXT_CAP] : []),
          ...(jobsCap ? [BACKGROUND_JOBS_CAP] : []),
          ...(terminalCap ? [TERMINAL_CAP] : []),
          ...(goalsCap ? [GOALS_CAP] : []),
          ...(queryCap ? [SESSION_QUERY_CAP] : []),
          ...(subagentCap ? [SUBAGENT_CAP] : []),
          ...(teamCap ? [TEAM_CAP] : []),
          ...(scheduleCap ? [SCHEDULE_CAP] : []),
          ...(programCap ? [PROGRAM_CAP] : []),
          ...processCaps,
        ],
        ...(hostServices ? {
          host_services: [
            ...hostServices.catalog,
            ...(imageCap ? [{
                service_key: SERVICE_KEYS.imageAttachments,
                methods: [
                  { name: 'route', kind: 'read', timeout_ms: 5000, idempotent: true },
                  { name: 'save', kind: 'write', timeout_ms: 10000, idempotent: true },
                  { name: 'rendered.page', kind: 'write', timeout_ms: 10000, idempotent: false },
                ],
              }] : []),
            ...(processCaps.length ? [TOOL_PROCESS_CATALOG] : []),
            ...(questionsCap ? [USER_QUESTIONS_CATALOG] : []),
            ...(jobsCap ? [JOBS_CATALOG] : []),
            ...(terminalCap ? [TERMINAL_CATALOG] : []),
            ...(goalsCap ? [GOALS_CATALOG] : []),
            ...(queryCap ? [SESSION_QUERY_CATALOG] : []),
            ...(subagentCap ? [SUBAGENT_CATALOG] : []),
            ...(teamCap ? [TEAM_CATALOG] : []),
            ...(scheduleCap ? [SCHEDULE_CATALOG] : []),
            ...(programCap ? [PROGRAM_CATALOG] : []),
          ],
        } : {}),
        generation,
      })
      // 必须在 ready 之后装载：否则 registration 带着握手前的占位 generation 被丢弃。
      bootComplete = bootPlugins(loadPluginConfig()).then((root) => { cordisRoot = root })
      await bootComplete
      send('log.emit', 'boot-capabilities', {
        level: 'info',
        message: `host_caps=[${[...hostCaps].join(', ')}] fs.write=${writeEnabled} protocol=${negotiatedProtocol} host_services=${Boolean(hostServices)} workspace=${WORKSPACE_ROOT}`,
      })
      return
    }
    case 'health.ping':
      send('health.pong', msg.request_id, {
        rss_mb: Math.round(process.memoryUsage().rss / 1048576),
      })
      return
    case 'fs.write.result': {
      const pending = pendingHostRequests.get(msg.request_id)
      if (!pending) {
        send('log.emit', `late-${msg.request_id}`, {
          level: 'debug',
          message: `late/unknown fs.write.result ${msg.request_id}`,
        })
        return
      }
      pending.resolve(
        msg.error
          ? { ok: false, error: { code: msg.error.code, message: msg.error.message } }
          : (msg.payload ?? {}),
      )
      return
    }
    case 'preset.sync':
    case 'preset.bind':
    case 'preset.release': {
      if (msg.runtime_generation !== generation) return
      const operation = presetOperations.then(async () => {
        await bootComplete
        if (msg.kind === 'preset.sync') {
          return { presets: await presets.sync(msg.payload?.definitions) }
        }
        if (msg.kind === 'preset.bind') {
          const bound = presets.bind(msg.session_id, msg.payload)
          return { preset: bound.id, revision: bound.key }
        }
        await presets.release(msg.session_id)
        return {}
      })
      presetOperations = operation.catch(() => {})
      try {
        send(msg.kind, msg.request_id, await operation)
      } catch (error) {
        send(msg.kind, msg.request_id, {}, {
          error: { code: 'E_PRESET', message: String(error?.message ?? error), retryable: false },
        })
      }
      return
    }
    case 'tool.register': {
      const binding = toolsByRequest.get(msg.request_id)
      if (!binding || msg.runtime_generation !== generation) return
      if (msg.error) {
        removeTool(binding, false)
        log('error', `could not register tool ${binding.definition.name}: ${msg.error.message}`)
        return
      }
      const id = msg.registration_id ?? msg.payload?.registration_id
      if (id) {
        binding.registrationId = id
        toolsByRequest.delete(msg.request_id)
        toolsById.set(id, binding)
      }
      return
    }
    case 'tool.unregister': {
      const binding = toolsById.get(msg.registration_id)
      if (binding?.sessionId && binding.sessionId === msg.session_id &&
          msg.plugin_id === TOOL_OWNER && msg.runtime_generation === generation) {
        removeTool(binding, false)
      }
      return
    }
    case 'agent.step': {
      if (!hostServices || !hostCaps.has(AGENT_TOOL_CONTEXT_CAP)
          || msg.runtime_generation !== generation) return
      await bootComplete
      const extra = { session_id: msg.session_id }
      try {
        send('agent.step', msg.request_id, await prepareAgentStep(msg), extra)
      } catch (error) {
        send('agent.step', msg.request_id, {}, {
          ...extra,
          error: { code: 'E_TOOL', message: String(error.message ?? error).slice(0, 1024), retryable: false },
        })
      }
      return
    }
    case 'tool.invoke': {
      const name = msg.payload?.name
      const binding = toolsById.get(msg.registration_id)
      const definition = msg.registration_id
        ? (msg.runtime_generation === generation &&
           msg.plugin_id === TOOL_OWNER &&
           binding?.definition.name === name &&
           (!binding.sessionId || binding.sessionId === msg.session_id) &&
           (!binding.revision || binding.revision === presets.binding(msg.session_id))
          ? binding.definition
          : undefined)
        : toolsService.get(name, { sessionId: msg.session_id })
      if (!definition) {
        send(
          'tool.result',
          msg.request_id,
          { call_id: msg.call_id, is_error: true },
          {
            call_id: msg.call_id,
            error: { code: 'E_TOOL_UNAVAILABLE', message: `unavailable tool ${name}`, retryable: false },
          },
        )
        return
      }
      await runTool(definition, msg)
      return
    }
    case 'tool.cancel': {
      const requestId = msg.payload?.tool_request_id
      for (const call of inflight.values()) {
        if (call.callId !== msg.call_id || (requestId && requestId !== call.toolRequestId)) continue
        inflight.delete(call.toolRequestId)
        call.controller.abort()
        hostServices?.cancelForCall(call.callId, call.toolRequestId)
      }
      return
    }
    case 'host.service.result': {
      // 与 tool.result 严格分开：只按 request_id 关联 sidecar 发起的服务调用。
      if (!hostServices) {
        send('log.emit', `drop-${msg.request_id}`, {
          level: 'error',
          message: `host.service.result without negotiated service.proxy: ${msg.request_id}`,
        })
        return
      }
      hostServices.handleResult(msg)
      return
    }
    case 'event.emit': {
      // 事件白名单在 EventHub 内（session.event / service.event）；其余拒绝并诊断。
      if (!hostServices) return
      handleHostEventEmit(msg, hostServices.eventHub)
      // P1-5：session.event 再折进投影注册表（gap 时由 shim 用 events.read 补齐）。
      if (sessionProjections) {
        sessionProjections.handleHostEvent(msg).catch((error) =>
          send('log.emit', `proj-${Date.now()}`, {
            level: 'error',
            message: `session projection failed: ${String(error?.message ?? error)}`,
          }),
        )
      }
      return
    }
    default:
      send('tool.result', msg.request_id, {}, {
        error: { code: 'E_PROTOCOL', message: `unknown kind ${msg.kind}`, retryable: false },
      })
  }
}

function defaultPrimaryRuntime() {
  const local = process.env.LOCALAPPDATA
  if (!local) return ''
  return join(local, 'Programs', 'DeepSeek Harness', 'resources', 'runtime', 'primary-runtime')
}

/**
 * 能力门控：host_caps 不含 fs.write 时强制关闭 write/edit/applyPatch（保持只读）。
 * 含 fs.write 时，profile 未显式配置的写开关缺省为 true；显式 false 尊重用户意图。
 */
function gateToolFsConfig(config, writeAllowed) {
  const gated = { ...(config ?? {}) }
  for (const key of ['write', 'edit', 'applyPatch']) {
    if (!writeAllowed) gated[key] = false
    else if (gated[key] === undefined) gated[key] = true
  }
  return gated
}

function gateEntry(entry, writeAllowed) {
  if (entry?.name !== '@deepseek-ai/dsh-tool-fs') return entry
  return { ...entry, config: gateToolFsConfig(entry.config, writeAllowed) }
}

function loadPluginConfig() {
  const writeAllowed = writeEnabled && packageVendored('@deepseek-ai/dsh-tool-fs')
  const raw = process.env.DSHR_DSH_PLUGINS
  if (raw) {
    const entries = JSON.parse(raw)
    if (!Array.isArray(entries)) throw new Error('DSHR_DSH_PLUGINS must be a JSON array')
    return entries.map((entry) => gateEntry(entry, writeAllowed))
  }
  const source = process.env.DSHR_DSH_SOURCE || defaultPrimaryRuntime()
  if (!source || !existsSync(join(source, 'runtime.json'))) {
    throw new Error(
      `找不到 DSH primary-runtime（set DSHR_DSH_SOURCE）。已尝试: ${source || '<empty>'}`,
    )
  }
  const entries = [
    {
      name: '@deepseek-ai/dsh-tool-workspace-dependencies',
      config: { source },
    },
  ]
  if (packageVendored('@deepseek-ai/dsh-tool-fs')) {
    entries.push({
      name: '@deepseek-ai/dsh-tool-fs',
      // 该插件 apply 里直接访问 systemPrompt/fs，但导出的 inject 只声明了 tools；
      // DSH Loader 通过 entry 级 inject 覆盖补齐，这里照做。
      inject: ['tools', 'fs', 'systemPrompt'],
      config: gateToolFsConfig(
        { read: true, readImage: false },
        writeAllowed,
      ),
    })
  }
  return entries
}

function packageVendored(name) {
  const dir = join(import.meta.dirname ?? '.', 'node_modules', name)
  return existsSync(dir)
}

const rl = createInterface({ input: process.stdin })
rl.on('line', (line) => {
  const text = line.trim()
  if (!text) return
  let msg
  try {
    msg = JSON.parse(text)
  } catch {
    send('log.emit', 'req-parse-error', { level: 'error', message: 'bad json' })
    return
  }
  handle(msg).catch((error) => {
    send('log.emit', msg.request_id ?? 'req-internal', {
      level: 'error',
      message: String(error?.stack ?? error),
    })
  })
})

process.stdin.on('end', () => process.exit(0))
