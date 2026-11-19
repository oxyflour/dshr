// 批调用测试夹具：并发 tool.invoke、取消宽限、session_id 透传。
//
// 工具：
// - batch_fast：立即返回 {session_id, tool}
// - batch_sleep：每 50ms 检查 cancel，收到 tool.cancel → tool.aborted
// - batch_ignore_cancel：睡满 args.ms 并**忽略** tool.cancel（验证宿主 2s 宽限）
//
// 这是测试夹具，不是生产 sidecar；由 crates/compat-dsh-host/tests/parallel_batch.rs 使用。

import { createInterface } from 'node:readline'

let generation = 'node-0001'
const inflight = new Map()
const callbackErrors = {}
const TOOLS = ['batch_fast', 'batch_sleep', 'batch_ignore_cancel']

const send = (kind, requestId, payload, extra = {}) => {
  process.stdout.write(
    JSON.stringify({
      version: 1,
      kind,
      request_id: requestId,
      runtime_generation: generation,
      depth: 0,
      trace_id: 't-batch',
      ts: new Date().toISOString(),
      payload,
      ...extra,
    }) + '\n',
  )
}

const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

const result = (msg, value, extra = {}) =>
  send(
    'tool.result',
    msg.request_id,
    { call_id: msg.call_id, is_error: false, content: [{ type: 'json', value }], value },
    { call_id: msg.call_id, ...extra },
  )

async function runTool(msg) {
  const name = msg.payload?.name
  const args = msg.payload?.arguments ?? {}
  const state = { cancelled: false }
  inflight.set(msg.call_id, state)
  const sessionId = msg.session_id ?? null
  try {
    if (name === 'batch_fast') {
      result(msg, { session_id: sessionId, tool: name })
      return
    }
    const duration = Number.isFinite(Number(args.ms)) ? Number(args.ms) : 2_000
    if (name === 'batch_ignore_cancel') {
      await sleep(duration)
      if (args.callbacks) {
        const scope = { tool_request_id: msg.request_id }
        send('fs.write.request', 'late-fs', scope, { call_id: msg.call_id })
        send('host.service.request', 'late-service', { method: 'get', args: {}, ...scope }, {
          session_id: sessionId, service_key: 'working-directory/v1',
        })
      }
      result(msg, { session_id: sessionId, tool: name, ignored_cancel: true })
      return
    }
    if (name === 'batch_sleep') {
      let waited = 0
      while (waited < duration) {
        await sleep(50)
        waited += 50
        if (state.cancelled) {
          send(
            'tool.aborted',
            msg.request_id,
            { call_id: msg.call_id, reason: 'cancel' },
            { call_id: msg.call_id },
          )
          return
        }
      }
      result(msg, { session_id: sessionId, tool: name, callback_errors: callbackErrors })
      return
    }
    send(
      'tool.result',
      msg.request_id,
      { call_id: msg.call_id, is_error: true },
      {
        call_id: msg.call_id,
        error: { code: 'E_UNKNOWN_PLUGIN', message: `unknown tool ${name}`, retryable: false },
      },
    )
  } finally {
    inflight.delete(msg.call_id)
  }
}

async function handle(msg) {
  switch (msg.kind) {
    case 'runtime.hello':
      generation = msg.payload?.generation ?? generation
      send('runtime.ready', msg.request_id, {
        protocol: 1,
        dsh_version: 'batch-fixture',
        sidecar_caps: ['tool.cancel'],
        generation,
      })
      for (const name of TOOLS) {
        send(
          'tool.register',
          `reg-${name}`,
          { name, description: `${name} (batch fixture)`, input_schema: { type: 'object' } },
          { plugin_id: 'batch.fixture' },
        )
      }
      return
    case 'tool.invoke':
      await runTool(msg)
      return
    case 'tool.cancel': {
      const state = inflight.get(msg.call_id)
      if (state) state.cancelled = true
      return
    }
    case 'fs.write.result':
      callbackErrors.fs = msg.payload?.error?.code
      return
    case 'host.service.result':
      callbackErrors.service = msg.error?.code
      return
    default:
      return
  }
}

const rl = createInterface({ input: process.stdin })
rl.on('line', (line) => {
  const text = line.trim()
  if (!text) return
  let msg
  try {
    msg = JSON.parse(text)
  } catch {
    return
  }
  handle(msg).catch((error) =>
    send('log.emit', 'fixture-error', {
      level: 'error',
      message: String(error?.stack ?? error),
    }),
  )
})
process.stdin.on('end', () => process.exit(0))
