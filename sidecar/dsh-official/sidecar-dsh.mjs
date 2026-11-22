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
import { existsSync } from 'node:fs'
import { join, resolve as resolvePath } from 'node:path'
import { Context } from '@deepseek-ai/cordis'
import { createFsService, officeOutputPath } from './lib/file-service.mjs'
import { createToolRegistry, TOOL_OWNER } from './lib/tool-registry.mjs'
import { createToolExecution, AGENT_TOOL_CONTEXT_CAP } from './lib/tool-execution.mjs'
import {
  createHostServicesRuntime,
  handleHostEventEmit,
  shouldEnableHostServices,
  SERVICE_KEYS,
} from './lib/host-services.mjs'
import { createSessionProjectionsShim } from './lib/session-projections.mjs'
import { createAgentPresets } from './lib/agent-presets.mjs'
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
import { GOALS_CAP, GOALS_CATALOG, createToolGoals } from './lib/tool-goals.mjs'
import { SESSION_QUERY_CAP, SESSION_QUERY_CATALOG, createSessionQuery } from './lib/session-query.mjs'
import { SUBAGENT_CAP, SUBAGENT_CATALOG, SUBAGENT_PLUGINS, createSubagents } from './lib/subagents.mjs'
import { TEAM_CAP, TEAM_CATALOG, TEAM_PLUGIN, createAgentTeams } from './lib/agent-teams.mjs'
import { WORKFLOW_PLUGIN, RALPH_PLUGIN, mountWorkflowEngine } from './lib/workflow-runtime.mjs'
import { SCHEDULE_CAP, SCHEDULE_CATALOG, SCHEDULE_PLUGIN, createSchedules } from './lib/schedules.mjs'
import { INSPECT_PLUGIN, mountCordisInspection } from './lib/cordis-inspect.mjs'
import { NATIVE_EXECUTOR_PLUGIN, PTC_EXECUTOR_PLUGIN, PROGRAM_TOOL, PROGRAM_CAP, PROGRAM_CATALOG } from './lib/executors.mjs'

const PROTOCOL_MAX = 2
// 协商结果：min(host, sidecar)。v1 宿主（M2 及以前）保持纯 v1 行为。
let negotiatedProtocol = 1
let generation = 'node-0001'
const revisionContexts = new Map()
const jobBridges = new WeakMap()
const terminalBridges = new WeakMap()
const goalBridges = new WeakMap()
const queryBridges = new WeakMap()
const subagentBridges = new WeakMap()
const teamBridges = new WeakMap()
const scheduleBridges = new WeakMap()
const executorScopes = new WeakMap()
const presets = createAgentPresets(mountPreset)
let bootComplete = Promise.resolve()
let presetOperations = Promise.resolve()

// 握手协商出的 host_caps；`fs.write` 是官方写工具的唯一门闩。
const hostCaps = new Set()
let writeEnabled = false

// L2 Host Service 代理（spec §13）：仅协商到 v2 且宿主支持 service.proxy 时启用。
let hostServices = null
// P1-5：投影注册表留在 Sidecar（`ctx.sessionProjections`），宿主只提供 session-log 数据面。
let sessionProjections = null
// 当前工具调用上下文（callId/sessionId/depth）：服务代理用它关联 tool.cancel 与 session_id。
const callStore = new AsyncLocalStorage()
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

const toolRegistry = createToolRegistry({
  send, log, hostCaps,
  currentCall: () => callStore.getStore(),
  boundRevision: sessionId => presets.binding(sessionId),
})
const toolsService = toolRegistry.service
const toolExecution = createToolExecution({
  send, log, callStore, toolRegistry, hostCaps, revisionContexts, executorScopes,
  getHostServices: () => hostServices,
  getRootContext: () => cordisRoot,
  boundRevision: sessionId => presets.binding(sessionId),
  bridges: { jobBridges, terminalBridges, goalBridges, queryBridges, subagentBridges, teamBridges, scheduleBridges },
})

// ---------------------------------------------------------------------------
// 最小服务子集：官方插件通过 `inject`/`ctx.<service>` 使用这些 seam。
// ---------------------------------------------------------------------------

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
        return { ...binding.definition, execute: (args, exec) => toolExecution.executeNested(binding, args, exec) }
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
  root.provide('fs', createFsService({
    workspaceRoot: WORKSPACE_ROOT,
    currentCall: toolExecution.currentCall,
    writeAllowed: () => writeEnabled,
    requestHost,
  }, { workingDirectory, images }))
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
        findAgent: toolExecution.findAgent,
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
        message: `loaded official plugin ${entry.name} (tools now: ${[...toolRegistry.globalNames()].join(', ') || '<none>'})`,
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
  const releaseTools = toolRegistry.openRevision(revision)
  const root = createPluginContext(revision)
  revisionContexts.set(revision, root)
  const dispose = async () => {
    try {
      await root.fiber.dispose()
    } finally {
      releaseTools()
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
    case 'tool.register':
      toolRegistry.acknowledge(msg, generation)
      return
    case 'tool.unregister':
      toolRegistry.revoke(msg, generation)
      return
    case 'agent.step': {
      if (!hostServices || !hostCaps.has(AGENT_TOOL_CONTEXT_CAP)
          || msg.runtime_generation !== generation) return
      await bootComplete
      const extra = { session_id: msg.session_id }
      try {
        send('agent.step', msg.request_id, await toolExecution.prepareAgentStep(msg), extra)
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
      const binding = toolRegistry.find(msg.registration_id)
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
      await toolExecution.runTool(definition, msg)
      return
    }
    case 'tool.cancel':
      toolExecution.cancel(msg)
      return
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
