// v2 Host Service 集成测试 fixture sidecar（NDJSON / JSON-RPC 风格，无 DSH 依赖）。
//
// 行为：
//   1. runtime.hello → runtime.ready（protocol = min(2, hello.protocol_max, L2_PROTOCOL_MAX)；
//      协商到 2 且 host_caps 含 service.proxy 时回声 sidecar_caps=["service.proxy"]）
//   2. 注册一个 `l2_probe` 工具
//   3. 收到 tool.invoke 后按 L2_SCENARIO 主动发起一组 host.service.request（或 capabilities.list），
//      把答复摘要放进 tool.result.payload.value 返回给宿主
//
// 环境变量：
//   L2_SCENARIO      storage | working-directory | unknown | cancel | late-result | capabilities
//   L2_TARGET_DIR    working-directory 场景 set 的目标目录
//   L2_PROTOCOL_MAX  夹具支持的最高版本（默认 2；设为 1 可测"宿主 v2、sidecar v1"回退）
//
// 这是测试/自检夹具，不是生产 sidecar。

import { createInterface } from 'node:readline'

const SCENARIO = process.env.L2_SCENARIO || 'capabilities'
const TARGET_DIR = process.env.L2_TARGET_DIR || '.'
const FIXTURE_MAX = Number(process.env.L2_PROTOCOL_MAX || 2)

let protocol = 1
let generation = 'node-0001'
let serviceProxy = false
let nextRequest = 0
const pending = new Map()

const write = (envelope) => {
  process.stdout.write(JSON.stringify(envelope) + '\n')
}

const send = (kind, requestId, payload, extra = {}) => {
  write({
    version: protocol,
    kind,
    request_id: requestId,
    runtime_generation: generation,
    depth: 0,
    trace_id: 't-l2-fixture',
    ts: new Date().toISOString(),
    payload,
    ...extra,
  })
}

/** 发一条请求并等匹配 request_id 的答复（超时 5s → {timeout:true}）。 */
const roundTrip = (kind, requestId, payload, extra = {}) =>
  new Promise((resolve) => {
    const timer = setTimeout(() => {
      pending.delete(requestId)
      resolve({ timeout: true })
    }, 5000)
    pending.set(requestId, (message) => {
      clearTimeout(timer)
      resolve(message)
    })
    send(kind, requestId, payload, extra)
  })

const callService = (serviceKey, method, args = {}) => {
  nextRequest += 1
  const requestId = `svc-${nextRequest}`
  return roundTrip('host.service.request', requestId, { method, args }, {
    service_key: serviceKey,
    session_id: currentSession,
    depth: callDepth,
    plugin_id: 'l2.fixture',
  })
}

let currentSession = 's-probe'
let callDepth = 1

const summarize = (message, label) => ({
  label,
  ok: !message?.error && !message?.timeout,
  timeout: Boolean(message?.timeout),
  error: message?.error?.code ?? null,
  error_message: message?.error?.message ?? null,
  payload: message?.payload ?? null,
  result: message?.payload?.result ?? null,
})

async function runScenario() {
  const steps = []
  switch (SCENARIO) {
    case 'storage': {
      const open = await callService('storage-kv/v1', 'unit.open', {
        unit: { name: 'todos', version: 1, layout: 'single', tables: ['items'] },
      })
      steps.push(summarize(open, 'unit.open'))
      const unitId = open?.payload?.result?.unit_id
      steps.push(
        summarize(
          await callService('storage-kv/v1', 'unit.putRecord', {
            unit_id: unitId,
            table: 'items',
            key: 'a',
            value: { n: 1 },
          }),
          'unit.putRecord',
        ),
      )
      steps.push(
        summarize(
          await callService('storage-kv/v1', 'unit.loadAll', { unit_id: unitId }),
          'unit.loadAll',
        ),
      )
      steps.push(
        summarize(await callService('storage-kv/v1', 'unit.close', { unit_id: unitId }), 'unit.close'),
      )
      break
    }
    case 'working-directory': {
      steps.push(summarize(await callService('working-directory/v1', 'get', {}), 'get'))
      steps.push(
        summarize(
          await callService('working-directory/v1', 'set', { path: TARGET_DIR }),
          'set',
        ),
      )
      steps.push(summarize(await callService('working-directory/v1', 'ensure', {}), 'ensure'))
      break
    }
    case 'unknown': {
      steps.push(summarize(await callService('missing-service/v1', 'get', {}), 'missing'))
      break
    }
    case 'cancel': {
      // 先发 cancel（同 request_id）再发请求：宿主先标记取消，请求应回 E_CANCELLED。
      const cancelId = 'cancelled-1'
      send('host.service.cancel', cancelId, { method: 'get' }, {
        service_key: 'working-directory/v1',
        session_id: currentSession,
        depth: callDepth,
        plugin_id: 'l2.fixture',
      })
      steps.push(
        summarize(
          await roundTrip(
            'host.service.request',
            cancelId,
            { method: 'get', args: {} },
            {
              service_key: 'working-directory/v1',
              session_id: currentSession,
              depth: callDepth,
              plugin_id: 'l2.fixture',
            },
          ),
          'cancelled-get',
        ),
      )
      // 取消不应破坏后续调用。
      steps.push(summarize(await callService('working-directory/v1', 'get', {}), 'get-after-cancel'))
      break
    }
    case 'late-result': {
      // 先发一条无法关联的迟到 host.service.result（宿主应忽略），再发 capabilities.list。
      send('host.service.result', 'late-unknown', { result: { ignored: true } }, {
        service_key: 'working-directory/v1',
      })
      steps.push(
        summarize(await roundTrip('capabilities.list', 'cap-late', {}, {}), 'capabilities'),
      )
      break
    }
    case 'capabilities':
    default: {
      steps.push(
        summarize(await roundTrip('capabilities.list', 'cap-1', {}, {}), 'capabilities'),
      )
      break
    }
  }
  return steps
}

const handleLine = async (line) => {
  let message
  try {
    message = JSON.parse(line)
  } catch {
    return
  }
  const kind = message?.kind
  if (kind === 'runtime.hello') {
    const hostMax = Number(message?.payload?.protocol_max ?? 1)
    protocol = Math.max(1, Math.min(FIXTURE_MAX, Number.isFinite(hostMax) ? hostMax : 1))
    const caps = new Set(message?.payload?.host_caps ?? [])
    serviceProxy = protocol >= 2 && caps.has('service.proxy')
    generation = message?.payload?.generation ?? generation
    send('runtime.ready', message.request_id, {
      protocol,
      dsh_version: 'l2-fixture',
      sidecar_caps: ['tool.cancel', ...(serviceProxy ? ['service.proxy'] : [])],
      generation,
    })
    send(
      'tool.register',
      'reg-l2_probe',
      {
        name: 'l2_probe',
        description: 'L2 host-service integration probe',
        input_schema: { type: 'object', additionalProperties: false, properties: {} },
      },
      { plugin_id: 'l2.fixture' },
    )
    return
  }
  if (kind === 'host.service.result' || kind === 'capabilities.list') {
    const resolver = pending.get(message?.request_id)
    if (resolver) {
      pending.delete(message.request_id)
      resolver(message)
    }
    return
  }
  if (kind === 'tool.invoke') {
    currentSession = message.session_id ?? 's-probe'
    callDepth = (Number.isInteger(message.depth) ? message.depth : 0) + 1
    const steps = await runScenario()
    send(
      'tool.result',
      message.request_id,
      {
        call_id: message.call_id,
        is_error: false,
        content: [{ type: 'text', text: JSON.stringify(steps) }],
        value: { scenario: SCENARIO, protocol, service_proxy: serviceProxy, steps },
      },
      { call_id: message.call_id, plugin_id: 'l2.fixture' },
    )
    return
  }
  // tool.register ack / log.emit / health.pong 等无关消息：忽略。
}

const rl = createInterface({ input: process.stdin })
rl.on('line', (line) => {
  const text = line.trim()
  if (!text) return
  handleLine(text).catch((error) => {
    send('log.emit', 'fixture-error', {
      level: 'error',
      message: String(error?.stack ?? error),
    })
  })
})
process.stdin.on('end', () => process.exit(0))
