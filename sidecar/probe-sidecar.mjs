// Minimal DSH-compatible sidecar for the L1 unit-cost experiment.
// Speaks NDJSON on stdin/stdout per docs/spec/sidecar-rpc.md.
//
// It deliberately does NOT depend on DSH internals: the goal is to measure the
// protocol cost (spawn, handshake, register, invoke, cancel), not Cordis itself.

import { createInterface } from 'node:readline'
import { pathToFileURL } from 'node:url'
import { resolve } from 'node:path'

const PROTOCOL_VERSION = 1
let generation = 'node-0001'
const tools = new Map()
const inflight = new Map()

const send = (kind, requestId, payload, extra = {}) => {
  const envelope = {
    version: PROTOCOL_VERSION,
    kind,
    request_id: requestId,
    runtime_generation: generation,
    depth: 0,
    trace_id: 't-sidecar',
    ts: new Date().toISOString(),
    payload,
    ...extra,
  }
  process.stdout.write(JSON.stringify(envelope) + '\n')
}

const handle = async (msg) => {
  switch (msg.kind) {
    case 'runtime.hello': {
      // Generation is assigned by the Rust host (spec §5), not by us.
      generation = msg.payload?.generation ?? generation
      if (msg.payload?.protocol_max < PROTOCOL_VERSION) {
        send('runtime.reject', msg.request_id, { reason: 'protocol mismatch' })
        process.exit(1)
      }
      send('runtime.ready', msg.request_id, {
        protocol: PROTOCOL_VERSION,
        dsh_version: '0.2.1-rc.3',
        sidecar_caps: ['tool.stream', 'tool.cancel'],
        generation,
      })
      await loadPlugins()
      return
    }
    case 'health.ping':
      send('health.pong', msg.request_id, { rss_mb: Math.round(process.memoryUsage().rss / 1048576) })
      return
    case 'tool.register': {
      // Host acknowledges registration with the id it assigned.
      if (msg.payload?.registration_id) {
        for (const tool of tools.values()) {
          if (tool.name === msg.payload.name) {
            tool.registration_id = msg.payload.registration_id
          }
        }
      }
      return
    }
    case 'tool.invoke': {
      const name = msg.payload?.name
      const tool = tools.get(name)
      if (!tool) {
        send('tool.result', msg.request_id, { call_id: msg.call_id, is_error: true }, {
          error: { code: 'E_UNKNOWN_PLUGIN', message: `unknown tool ${name}`, retryable: false },
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

async function runTool(tool, msg) {
  const state = { cancelled: false }
  inflight.set(msg.call_id, state)
  const started = Date.now()
  let chunks = 0
  try {
    for await (const chunk of tool.run(msg.payload?.arguments ?? {}, state)) {
      if (typeof chunk === 'string') {
        chunks += 1
        send(
          'tool.chunk',
          msg.request_id,
          { call_id: msg.call_id, seq: chunks - 1, text: chunk },
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
        {
          call_id: msg.call_id,
          is_error: false,
          content: chunk.content ?? [],
          metadata: { duration_ms: Date.now() - started, chunks },
        },
        { call_id: msg.call_id },
      )
      return
    }
    // Generator produced no terminal value.
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

async function loadPlugins() {
  const pluginPath = resolve(process.argv[2] ?? 'probe-plugin.mjs')
  const plugin = await import(pathToFileURL(pluginPath).href)
  const ctx = {
    tool(spec) {
      tools.set(spec.name, { ...spec, registration_id: null })
      send('tool.register', `reg-${spec.name}`, {
        name: spec.name,
        description: spec.description,
        input_schema: {},
        output_schema: {},
      }, { plugin_id: plugin.name ?? 'probe.plugin' })
    },
  }
  plugin.register(ctx)
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
