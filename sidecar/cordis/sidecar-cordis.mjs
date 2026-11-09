// Sidecar that boots a real Cordis runtime and loads the plugin through ctx.plugin().
// Protocol is identical to probe-sidecar.mjs; only the plugin loading differs.

import { createInterface } from 'node:readline'
import { pathToFileURL } from 'node:url'
import { resolve } from 'node:path'
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
      trace_id: 't-cordis',
      ts: new Date().toISOString(),
      payload,
      ...extra,
    }) + '\n',
  )
}

// The service the plugin injects. Tools registered here become sidecar tools.
const bridge = {
  tool(spec) {
    tools.set(spec.name, spec)
    send(
      'tool.register',
      `reg-${spec.name}`,
      {
        name: spec.name,
        description: spec.description,
        input_schema: {},
        output_schema: {},
      },
      { plugin_id: 'probe.cordis' },
    )
    return () => tools.delete(spec.name)
  },
}

async function bootCordis(pluginPath) {
  const root = new Context()
  root.provide('bridge', bridge)
  const plugin = await import(pathToFileURL(resolve(pluginPath)).href)
  root.plugin(plugin)
  return root
}

async function runTool(tool, msg) {
  const state = { cancelled: false }
  inflight.set(msg.call_id, state)
  let seq = 0
  try {
    for await (const chunk of tool.run(msg.payload?.arguments ?? {}, state)) {
      if (typeof chunk === 'string') {
        send(
          'tool.chunk',
          msg.request_id,
          { call_id: msg.call_id, seq: seq++, text: chunk },
          { call_id: msg.call_id },
        )
        continue
      }
      if (chunk?.aborted) {
        send('tool.aborted', msg.request_id, { call_id: msg.call_id, reason: chunk.aborted }, {
          call_id: msg.call_id,
        })
        return
      }
      send(
        'tool.result',
        msg.request_id,
        { call_id: msg.call_id, is_error: false, content: chunk.content ?? [], metadata: { chunks: seq } },
        { call_id: msg.call_id },
      )
      return
    }
    send(
      'tool.result',
      msg.request_id,
      { call_id: msg.call_id, is_error: false, content: [] },
      { call_id: msg.call_id },
    )
  } catch (error) {
    send('tool.result', msg.request_id, { call_id: msg.call_id, is_error: true }, {
      error: { code: 'E_INTERNAL', message: String(error?.message ?? error), retryable: false },
    })
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
        dsh_version: '0.2.1-rc.3',
        sidecar_caps: ['tool.stream', 'tool.cancel', 'cordis'],
        generation,
        cordis: true,
      })
      // 必须在 ready 之后才装载插件：否则 registration 会带着握手前的
      // 占位 generation 提前发出，被宿主当成旧代丢弃。
      await bootCordis(pluginPath)
      send('log.emit', 'boot', { level: 'info', message: `cordis booted with ${pluginPath}` })
      return
    }
    case 'health.ping':
      send('health.pong', msg.request_id, { rss_mb: Math.round(process.memoryUsage().rss / 1048576) })
      return
    case 'tool.invoke': {
      const tool = tools.get(msg.payload?.name)
      if (!tool) {
        send('tool.result', msg.request_id, {}, {
          error: { code: 'E_UNKNOWN_PLUGIN', message: `unknown tool ${msg.payload?.name}`, retryable: false },
        })
        return
      }
      await runTool(tool, msg)
      return
    }
    case 'tool.cancel': {
      const state = inflight.get(msg.call_id)
      if (state) state.cancelled = true
      return
    }
    default:
      send('tool.result', msg.request_id, {}, {
        error: { code: 'E_PROTOCOL', message: `unknown kind ${msg.kind}`, retryable: false },
      })
  }
}

const pluginPath = process.argv[2] ?? 'bridge-plugin.mjs'

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
