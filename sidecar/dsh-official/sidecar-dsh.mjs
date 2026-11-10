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
// 环境变量：
//   DSHR_DSH_PLUGINS  JSON 数组：插件名 + 配置。缺省加载 workspace-dependencies 工具。
//   DSHR_DSH_SOURCE   primary-runtime 目录（workspace-dependencies 的 source）；
//                     缺省按 LOCALAPPDATA 下的 DSH 安装路径推断。

import { createInterface } from 'node:readline'
import { existsSync } from 'node:fs'
import { readFile, stat as fsStat } from 'node:fs/promises'
import { isAbsolute, join, resolve as resolvePath } from 'node:path'
import { Context } from '@deepseek-ai/cordis'

const PROTOCOL_VERSION = 1
let generation = 'node-0001'
const tools = new Map()
const inflight = new Map()

const send = (kind, requestId, payload, extra = {}) => {
  process.stdout.write(
    JSON.stringify({
      version: PROTOCOL_VERSION,
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

// ---------------------------------------------------------------------------
// 官方插件的入口：ctx.tools.register(definition)。
// 这里只做"定义 → 协议"与"调用 → execute"的翻译，不复制 DSH 的执行管线。
// ---------------------------------------------------------------------------

const toolsService = {
  register(definition) {
    if (!definition?.name) throw new Error('tool definition without name')
    tools.set(definition.name, definition)
    send(
      'tool.register',
      `reg-${definition.name}`,
      {
        name: definition.name,
        description: definition.description ?? '',
        input_schema: definition.parameters ?? { type: 'object' },
        output_schema: definition.output?.schema ?? null,
        presentation: 'generic',
      },
      { plugin_id: 'dsh-official' },
    )
    return () => {
      tools.delete(definition.name)
    }
  },
  get(name) {
    return tools.get(name)
  },
  // 官方插件在注册阶段可能触到这些方法；装载期没有对应能力，先做成空实现。
  presentAs() {},
  restrict() {},
  guard() {},
  unregister(name) {
    tools.delete(name)
  },
}

// ---------------------------------------------------------------------------
// 最小服务子集：官方插件通过 `inject`/`ctx.<service>` 使用这些 seam。
//
// 实现原则：只做我们真的能兑现的语义（本机只读文件、系统提示段落收集），
// 不假装支持沙箱/写入/会话——做不到的配置就不要打开（见 loadPluginConfig）。
// ---------------------------------------------------------------------------

function createFsService() {
  const describeTarget = (absolutePath) => ({
    targetKey: absolutePath,
    displayPath: absolutePath,
  })

  return {
    processPath(target) {
      return target.targetKey
    },
    async resolve(requestedPath, options = {}) {
      const base = options.cwd ?? process.cwd()
      return describeTarget(
        isAbsolute(requestedPath) ? resolvePath(requestedPath) : resolvePath(base, requestedPath),
      )
    },
    async stat(target, signal) {
      if (signal?.aborted) throw new Error('stat aborted')
      try {
        const info = await fsStat(target.targetKey)
        return {
          version: `${info.mtimeMs}:${info.size}`,
          type: info.isDirectory() ? 'directory' : 'file',
          size: info.size,
        }
      } catch {
        return undefined
      }
    },
    async readText(target, signal) {
      if (signal?.aborted) throw new Error('read aborted')
      return readFile(target.targetKey, 'utf8')
    },
    async streamText(target, signal) {
      // 工具侧用 `for await` 消费分块；一次返回整块也合法（只是不分块）。
      return [await this.readText(target, signal)]
    },
  }
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
    sections() {
      return sections
    },
  }
}

async function bootPlugins(entries) {
  const root = new Context()
  root.provide('tools', toolsService)
  root.provide('fs', createFsService())
  root.provide('systemPrompt', createSystemPromptService())
  for (const entry of entries) {
    try {
      const module = await import(entry.name)
      // Schemastery 的默认值必须显式解析：Cordis 直接收原始 config 时不会补默认，
      // 官方插件随后会因 `undefined` 的配置项在 apply 里抛错。
      const config = module.Config ? module.Config(entry.config ?? {}) : (entry.config ?? {})
      // 官方插件只导出它"声明"的 inject；但部分插件在 apply 里直接访问未声明的服务
      // （DSH 的 Loader 用 entry 级 inject 覆盖补上）。这里用同样的方式包一层。
      const plugin = {
        name: module.name,
        Config: module.Config,
        inject: entry.inject ?? module.inject ?? [],
        apply: module.apply,
      }
      // 等待插件装载完成：官方插件的 apply 可能是异步的，错误也在这里冒出来。
      await root.plugin(plugin, config)
      send('log.emit', `boot-${entry.name}`, {
        level: 'info',
        message: `loaded official plugin ${entry.name} (tools now: ${[...tools.keys()].join(', ') || '<none>'})`,
      })
    } catch (error) {
      // 单个插件失败不拖垮整台侧车：其余插件照常可用，诊断走 log.emit。
      send('log.emit', `boot-failed-${entry.name}`, {
        level: 'error',
        message: `failed to load ${entry.name}: ${String(error?.message ?? error)}`,
      })
    }
  }
  return root
}

async function runTool(definition, msg) {
  const controller = new AbortController()
  inflight.set(msg.call_id, controller)
  const started = Date.now()
  try {
    // ToolRunContext 在官方实现里由执行器组装；这里给最小可用集合：
    // 需要 fs/subprocess 等服务的工具暂不支持（边界写在文档里）。
    const exec = { signal: controller.signal, callId: msg.call_id }
    const value = await definition.execute(msg.payload?.arguments ?? {}, exec)

    let content
    try {
      content = definition.output?.render?.(msg.payload?.arguments ?? {}, value)
    } catch {
      content = undefined
    }
    if (!Array.isArray(content) || content.length === 0) {
      content = [{ type: 'json', value }]
    }
    let meta = null
    try {
      meta = definition.output?.presentationMeta?.(msg.payload?.arguments ?? {}, value) ?? null
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
      { call_id: msg.call_id, error: { code: 'E_TOOL', message: String(error?.message ?? error), retryable: false } },
    )
  } finally {
    inflight.delete(msg.call_id)
  }
}

async function handle(msg) {
  switch (msg.kind) {
    case 'runtime.hello': {
      generation = msg.payload?.generation ?? generation
      send('runtime.ready', msg.request_id, {
        protocol: PROTOCOL_VERSION,
        dsh_version: '0.2.2-rc.1',
        sidecar_caps: ['tool.cancel', 'dsh-official'],
        generation,
      })
      // 必须在 ready 之后装载：否则 registration 带着握手前的占位 generation 被丢弃。
      await bootPlugins(loadPluginConfig())
      return
    }
    case 'health.ping':
      send('health.pong', msg.request_id, {
        rss_mb: Math.round(process.memoryUsage().rss / 1048576),
      })
      return
    case 'tool.invoke': {
      const definition = tools.get(msg.payload?.name)
      if (!definition) {
        send(
          'tool.result',
          msg.request_id,
          { call_id: msg.call_id, is_error: true },
          {
            call_id: msg.call_id,
            error: { code: 'E_UNKNOWN_PLUGIN', message: `unknown tool ${msg.payload?.name}`, retryable: false },
          },
        )
        return
      }
      await runTool(definition, msg)
      return
    }
    case 'tool.cancel': {
      const controller = inflight.get(msg.call_id)
      if (controller) controller.abort()
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

function loadPluginConfig() {
  const raw = process.env.DSHR_DSH_PLUGINS
  if (raw) {
    const entries = JSON.parse(raw)
    if (!Array.isArray(entries)) throw new Error('DSHR_DSH_PLUGINS must be a JSON array')
    return entries
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
  // 只读 fs：写入/打补丁需要 DSH 的沙箱与版本语义，暂不打开。
  if (packageVendored('@deepseek-ai/dsh-tool-fs')) {
    entries.push({
      name: '@deepseek-ai/dsh-tool-fs',
      // 该插件 apply 里直接访问 systemPrompt/fs，但导出的 inject 只声明了 tools；
      // DSH Loader 通过 entry 级 inject 覆盖补齐，这里照做。
      inject: ['tools', 'fs', 'systemPrompt'],
      config: { read: true, readImage: false, write: false, edit: false, applyPatch: false },
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
