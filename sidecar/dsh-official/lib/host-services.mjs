// Sidecar 侧的 L2 Host Service 代理（spec §13，M3 G3）。
//
// 这个模块**不 import DSH/Cordis**：它是可被 `node:test` 直接单测的纯 Node 代码。
// `sidecar-dsh.mjs` 只负责把它接到 NDJSON 读循环 / Cordis Context 上。
//
// 设计要点：
// - 信封：`host.service.request`（version=协商版本，service_key 路由，payload {method,args}）。
//   `request_id` 由本模块生成并在答复回填；迟到答复丢弃并记 `service.late_result` 诊断。
// - 超时：按方法表/客户端配置的 timeout_ms 结算为 `E_SERVICE_TIMEOUT`，并发
//   `host.service.cancel`（request_id 相同，payload 可带 method）尽力取消。
// - 取消：`tool.cancel` 时对同一工具请求的在途服务调用发 `host.service.cancel`，
//   本地以 `E_CANCELLED` 结算（迟到成功结果不会交给插件，spec §8）。
// - 事件：`event.emit` 只接受白名单 `session.event` / `service.event`，其余拒绝 + 诊断。
// - storage：把官方 `kv` facet 的 unit 方法翻译成 `storage-kv/v1` 的 host.service.request，
//   `unit_id` 由 Sidecar 注册表维护（数据在宿主）。
// - workingDirectory：`get/ensure/set` 的结果 `{cwd}`/`{cwd,recovered}` 解包成官方
//   shim 需要的 `string`（m3-l2-services §3.1）。

export const PROTOCOL_MAX = 2
export const DEFAULT_HOST_SERVICE_TIMEOUT_MS = 30_000
export const WORKING_DIRECTORY_EVENT = 'working-directory/change'
export const EVENT_WHITELIST = Object.freeze(['session.event', 'service.event'])

export const SERVICE_KEYS = Object.freeze({
  workingDirectory: 'working-directory/v1',
  sessionLog: 'session-log/v1',
  storageKv: 'storage-kv/v1',
  imageAttachments: 'image-attachments/v1',
})

const SERVICE_KEY_RE = /^[a-z][a-z0-9-]*\/v[1-9][0-9]*$/
const REQUEST_ID_RE = /^[A-Za-z0-9:_-]{1,64}$/
const CLIENT_INFO = Object.freeze({ name: 'dshr-sidecar', version: '0.2.2-rc.1' })

/** 宿主在 `runtime.ready.host_services` 里声明的方法表（Sidecar 侧代理的目录）。 */
export const HOST_SERVICE_CATALOG = Object.freeze([
  {
    service_key: SERVICE_KEYS.workingDirectory,
    methods: [
      { name: 'get', kind: 'read', timeout_ms: 5_000, idempotent: true },
      { name: 'ensure', kind: 'write', timeout_ms: 30_000, idempotent: true },
      { name: 'set', kind: 'write', timeout_ms: 30_000, idempotent: true },
    ],
  },
  {
    service_key: SERVICE_KEYS.sessionLog,
    methods: [
      { name: 'session.describe', kind: 'read', timeout_ms: 5_000, idempotent: true },
      { name: 'events.read', kind: 'read', timeout_ms: 10_000, idempotent: true },
      // M4 W6：官方工具 exec.agent.session.append 的写通道（仅 session-log/v1）。
      { name: 'events.append', kind: 'write', timeout_ms: 10_000, idempotent: false },
    ],
  },
  {
    service_key: SERVICE_KEYS.storageKv,
    methods: [
      { name: 'unit.open', kind: 'write', timeout_ms: 15_000, idempotent: true },
      { name: 'unit.loadAll', kind: 'read', timeout_ms: 30_000, idempotent: true },
      { name: 'unit.putRecord', kind: 'write', timeout_ms: 10_000, idempotent: true },
      { name: 'unit.deleteRecord', kind: 'write', timeout_ms: 10_000, idempotent: true },
      { name: 'unit.deleteRecords', kind: 'write', timeout_ms: 10_000, idempotent: true },
      { name: 'unit.setGlobal', kind: 'write', timeout_ms: 10_000, idempotent: true },
      { name: 'unit.close', kind: 'write', timeout_ms: 5_000, idempotent: true },
    ],
  },
])

/** 结构化服务错误（`code` 取 spec §9 的 `E_*`）。 */
export class HostServiceError extends Error {
  constructor(code, message, options = {}) {
    super(message)
    this.name = 'HostServiceError'
    this.code = code
    this.retryable = Boolean(options.retryable)
    this.detail = options.detail
  }
}

/**
 * 是否启用 host service 代理（spec §13/§3.1）。
 * - 协商版本必须 ≥ 2；
 * - `DSHR_HOST_SERVICES=0` 强制关闭；`=1` 显式打开（仍需 version≥2）；
 * - 缺省只在宿主 `host_caps` 含 `service.proxy` 时打开（保持 M2 默认只读行为）。
 */
export function shouldEnableHostServices({ negotiatedProtocol = 1, hostCaps = [], env = {} } = {}) {
  if (Number(negotiatedProtocol) < 2) return false
  const flag = String(env.DSHR_HOST_SERVICES ?? '')
    .trim()
    .toLowerCase()
  // 显式开关优先（0 关闭用于只读/受限环境；1 用于集成测试强制打开）。
  if (flag === '0' || flag === 'false' || flag === 'off') return false
  if (flag === '1' || flag === 'true' || flag === 'on') return true
  const caps = hostCaps instanceof Set ? hostCaps : new Set(hostCaps)
  return caps.has('service.proxy')
}

/** 构造 `host.service.request` 信封（纯函数，便于单测）。 */
export function buildHostServiceRequest({
  requestId,
  serviceKey,
  method,
  args = {},
  sessionId,
  toolRequestId,
  callId,
  depth = 0,
  version = PROTOCOL_MAX,
  generation = 'node-0001',
  pluginId = 'dsh-official',
  ts = new Date().toISOString(),
}) {
  if (typeof requestId !== 'string' || !REQUEST_ID_RE.test(requestId)) {
    throw new HostServiceError('E_PROTOCOL', `invalid request_id: ${requestId}`)
  }
  if (typeof serviceKey !== 'string' || !SERVICE_KEY_RE.test(serviceKey)) {
    throw new HostServiceError('E_PROTOCOL', `invalid service_key: ${serviceKey}`)
  }
  if (typeof method !== 'string' || method.length === 0) {
    throw new HostServiceError('E_PROTOCOL', 'missing service method')
  }
  if (args === null || typeof args !== 'object' || Array.isArray(args)) {
    throw new HostServiceError('E_SERVICE_ARGS', 'service args must be a JSON object')
  }
  const envelope = {
    version,
    kind: 'host.service.request',
    request_id: requestId,
    runtime_generation: generation,
    plugin_id: pluginId,
    service_key: serviceKey,
    depth,
    trace_id: 't-dsh-official',
    ts,
    payload: { method, args },
  }
  if (sessionId) envelope.session_id = sessionId
  if (callId) envelope.call_id = callId
  if (toolRequestId) envelope.payload.tool_request_id = toolRequestId
  return envelope
}

/** 构造 `event.subscribe`/`event.unsubscribe` 信封（sidecar → host，spec §6/§13.7）。 */
export function buildEventSubscription({
  subscribe = true,
  event,
  filter,
  requestId,
  version = PROTOCOL_MAX,
  generation = 'node-0001',
  pluginId = 'dsh-official',
  ts = new Date().toISOString(),
}) {
  if (!EVENT_WHITELIST.includes(event)) {
    throw new HostServiceError(
      'E_SERVICE_DENIED',
      `event \`${event}\` is not in the v2 whitelist`,
    )
  }
  const envelope = {
    version,
    kind: subscribe ? 'event.subscribe' : 'event.unsubscribe',
    request_id: requestId,
    runtime_generation: generation,
    plugin_id: pluginId,
    depth: 0,
    trace_id: 't-dsh-official',
    ts,
    payload: { event },
  }
  if (filter !== undefined) envelope.payload.filter = filter
  return envelope
}

/** 构造 `host.service.cancel` 信封：用原请求的 `request_id` 定位在途请求（spec §13.6）。 */
export function buildHostServiceCancel({
  requestId,
  serviceKey,
  method,
  version = PROTOCOL_MAX,
  generation = 'node-0001',
  pluginId = 'dsh-official',
  ts = new Date().toISOString(),
}) {
  return {
    version,
    kind: 'host.service.cancel',
    request_id: requestId,
    runtime_generation: generation,
    plugin_id: pluginId,
    service_key: serviceKey,
    depth: 0,
    trace_id: 't-dsh-official',
    ts,
    payload: method ? { method } : {},
  }
}

/**
 * 服务调用客户端：NDJSON 发送 + `host.service.result` 关联 + 超时/取消。
 *
 * `send(envelope)` 由 sidecar 接到 `send(kind, requestId, payload, extra)`；
 * 单测传入捕获器即可。`setTimer`/`clearTimer` 可注入假时钟。
 */
export function createHostServiceClient({
  send,
  timeoutMs = DEFAULT_HOST_SERVICE_TIMEOUT_MS,
  setTimer = setTimeout,
  clearTimer = clearTimeout,
  onDiagnostic,
  generateRequestId,
  generation = 'node-0001',
} = {}) {
  if (typeof send !== 'function') {
    throw new HostServiceError('E_INTERNAL', 'host service client needs a send function')
  }
  let sequence = 0
  const pending = new Map()
  const stats = { sent: 0, settled: 0, timedOut: 0, cancelled: 0, lateResults: 0 }

  const nextRequestId = () => {
    if (typeof generateRequestId === 'function') return generateRequestId()
    sequence += 1
    return `svc-${sequence}`
  }

  const sendCancel = (requestId, entry, reason) => {
    try {
      send(
        buildHostServiceCancel({
          requestId,
          serviceKey: entry?.serviceKey,
          method: entry?.method,
          generation: typeof generation === 'function' ? generation() : generation,
        }),
      )
    } catch (error) {
      onDiagnostic?.({
        code: 'service.cancel_failed',
        request_id: requestId,
        message: String(error?.message ?? error),
      })
    }
    onDiagnostic?.({ code: 'service.cancelled', request_id: requestId, reason })
  }

  const settle = (requestId, action) => {
    const entry = pending.get(requestId)
    if (!entry) return false
    clearTimer(entry.timer)
    pending.delete(requestId)
    action(entry)
    stats.settled += 1
    return true
  }

  return {
    stats,
    pendingCount: () => pending.size,

    /** 发起一次宿主服务调用；resolve 为 `payload.result`。 */
    call(request) {
      const {
        serviceKey,
        method,
        args = {},
        sessionId,
        depth = 0,
        callId,
        toolRequestId,
        timeoutMs: overrideTimeout,
        version = PROTOCOL_MAX,
        pluginId,
      } = request ?? {}
      const requestId = nextRequestId()
      const effectiveGeneration =
        typeof generation === 'function' ? generation() : generation
      let envelope
      try {
        envelope = buildHostServiceRequest({
          requestId,
          serviceKey,
          method,
          args,
          sessionId,
          toolRequestId,
          callId,
          depth,
          version,
          generation: effectiveGeneration,
          ...(pluginId ? { pluginId } : {}),
        })
      } catch (error) {
        return Promise.reject(error)
      }
      const effectiveTimeout = Number.isFinite(overrideTimeout)
        ? overrideTimeout
        : timeoutMs
      return new Promise((resolve, reject) => {
        const timer = setTimer(() => {
          settle(requestId, (entry) => {
            sendCancel(requestId, entry, 'timeout')
            stats.timedOut += 1
            reject(
              new HostServiceError(
                'E_SERVICE_TIMEOUT',
                `host service ${envelope.service_key}.${method} timed out after ${effectiveTimeout}ms`,
                { retryable: true },
              ),
            )
          })
        }, effectiveTimeout)
        pending.set(requestId, {
          resolve,
          reject,
          timer,
          callId,
          toolRequestId,
          serviceKey: envelope.service_key,
          method,
        })
        stats.sent += 1
        try {
          send(envelope)
        } catch (error) {
          settle(requestId, () => {
            reject(
              new HostServiceError(
                'E_INTERNAL',
                `failed to send host.service.request: ${String(error?.message ?? error)}`,
              ),
            )
          })
        }
      })
    },

    /** 处理 `host.service.result`；返回 true 表示被消费，false 表示迟到/未知。 */
    handleResult(message) {
      const requestId = message?.request_id
      const consumed = settle(requestId, (entry) => {
        const error = message?.error
        if (error) {
          entry.reject(
            new HostServiceError(
              error.code ?? 'E_INTERNAL',
              error.message ?? 'host service call failed',
              { retryable: Boolean(error.retryable), detail: message?.payload?.detail },
            ),
          )
          return
        }
        entry.resolve(message?.payload?.result ?? null)
      })
      if (!consumed) {
        stats.lateResults += 1
        onDiagnostic?.({ code: 'service.late_result', request_id: requestId })
      }
      return consumed
    },

    /** 取消单条在途请求（幂等；已结算返回 false）。 */
    cancel(requestId, reason = 'cancel') {
      return settle(requestId, (entry) => {
        sendCancel(requestId, entry, reason)
        stats.cancelled += 1
        entry.reject(
          new HostServiceError('E_CANCELLED', `host service call ${requestId} was cancelled`),
        )
      })
    },

    /** `tool.cancel` 时取消同一工具请求的在途服务调用；返回取消条数。 */
    cancelForCall(callId, reason = 'tool-cancelled', toolRequestId) {
      let cancelled = 0
      for (const requestId of [...pending.keys()]) {
        const entry = pending.get(requestId)
        if (entry?.callId !== callId) continue
        if (toolRequestId && entry.toolRequestId !== toolRequestId) continue
        if (this.cancel(requestId, reason)) cancelled += 1
      }
      return cancelled
    },
  }
}

/**
 * 解析 `runtime.ready.host_services`（数组或 `{host_services}`）→
 * `Map<service_key, {serviceKey, methods: Map<name, method>}>`。
 */
export function parseHostServices(payload) {
  const tables = Array.isArray(payload)
    ? payload
    : Array.isArray(payload?.host_services)
      ? payload.host_services
      : []
  const services = new Map()
  for (const table of tables) {
    if (!table || typeof table.service_key !== 'string') continue
    const methods = new Map()
    for (const method of table.methods ?? []) {
      if (method?.name) methods.set(method.name, method)
    }
    services.set(table.service_key, {
      serviceKey: table.service_key,
      methods,
    })
  }
  return services
}

/** 事件总线：name 白名单 + 订阅/退订；首个/最后一个订阅者触发 host 订阅通道。 */
export function createEventHub({
  whitelist = EVENT_WHITELIST,
  onDiagnostic,
  busEmit,
  onFirstListener,
  onLastListener,
} = {}) {
  const allowed = new Set(whitelist)
  const listeners = new Map()
  const stats = { accepted: 0, rejected: 0, dispatched: 0, subscriptions: 0, unsubscriptions: 0 }

  const notifyHost = (hook, eventName) => {
    try {
      hook?.(eventName)
    } catch (error) {
      onDiagnostic?.({
        code: 'event.subscription_failed',
        event: eventName,
        message: String(error?.message ?? error),
      })
    }
  }

  return {
    stats,
    isAllowed: (eventName) => allowed.has(eventName),
    subscribe(eventName, handler) {
      if (!allowed.has(eventName)) {
        throw new HostServiceError(
          'E_SERVICE_DENIED',
          `event \`${eventName}\` is not in the v2 whitelist`,
        )
      }
      if (typeof handler !== 'function') {
        throw new HostServiceError('E_PROTOCOL', 'event handler must be a function')
      }
      const handlers = listeners.get(eventName) ?? new Set()
      const firstListener = handlers.size === 0
      handlers.add(handler)
      listeners.set(eventName, handlers)
      if (firstListener) {
        stats.subscriptions += 1
        notifyHost(onFirstListener, eventName)
      }
      return () => {
        const current = listeners.get(eventName)
        if (!current) return
        current.delete(handler)
        if (current.size === 0) {
          listeners.delete(eventName)
          stats.unsubscriptions += 1
          notifyHost(onLastListener, eventName)
        }
      }
    },
    unsubscribe(eventName, handler) {
      const handlers = listeners.get(eventName)
      if (!handlers) return false
      const removed = handlers.delete(handler)
      if (handlers.size === 0) {
        listeners.delete(eventName)
        stats.unsubscriptions += 1
        notifyHost(onLastListener, eventName)
      }
      return removed
    },
    listenerCount(eventName) {
      return listeners.get(eventName)?.size ?? 0
    },
    dispatch(eventName, payload) {
      if (!allowed.has(eventName)) {
        stats.rejected += 1
        onDiagnostic?.({ code: 'event.rejected', event: eventName })
        return false
      }
      stats.accepted += 1
      let dispatched = true
      for (const handler of [...(listeners.get(eventName) ?? [])]) {
        try {
          handler(payload, eventName)
        } catch (error) {
          onDiagnostic?.({
            code: 'event.handler_error',
            event: eventName,
            message: String(error?.message ?? error),
          })
          dispatched = false
        }
      }
      if (typeof busEmit === 'function') {
        try {
          busEmit(eventName, payload)
        } catch (error) {
          onDiagnostic?.({
            code: 'event.bus_error',
            event: eventName,
            message: String(error?.message ?? error),
          })
        }
      }
      stats.dispatched += 1
      return dispatched
    },
  }
}

/** 处理入站 `event.emit`；返回 `{ok, event}`。 */
export function handleHostEventEmit(message, eventHub) {
  if (message?.kind !== 'event.emit') return { ok: false, event: null }
  const eventName = message.payload?.event
  const ok = eventHub.dispatch(eventName, message.payload?.payload ?? null)
  return { ok, event: eventName }
}

/**
 * `ctx.workingDirectory` shim：wire 形状 `{cwd}`/`{cwd,recovered}` → 官方 shim 的
 * `string` 返回值（m3-l2-services §3.1）；`ensure` 的 recovered 通过 `lastRecovered` 暴露。
 *
 * 官方 `dsh-working-directory` 的签名是 `ensure(agent, signal)` / `set(agent, path, signal)` /
 * `get(agent)`；agent 由 Sidecar 的调用上下文（AsyncLocalStorage 的 callId/sessionId）替代，
 * 这里忽略 agent 参数，同时兼容旧的单参调用（`set(path)`）。
 */
export function createWorkingDirectoryShim({ callService, callContext } = {}) {
  let lastRecovered = false
  const sessionId = () => callContext?.()?.sessionId
  const depth = () => Number(callContext?.()?.depth ?? 0) + 1
  const pathOf = (first, second) => (typeof first === 'string' ? first : second)
  const call = (method, args) =>
    callService({
      serviceKey: SERVICE_KEYS.workingDirectory,
      method,
      args,
      sessionId: sessionId(),
      depth: depth(),
    })
  return {
    async get(_agent) {
      const reply = await call('get', {})
      return typeof reply?.cwd === 'string' ? reply.cwd : ''
    },
    async ensure(_agent, _signal) {
      const reply = await call('ensure', {})
      lastRecovered = Boolean(reply?.recovered)
      return typeof reply?.cwd === 'string' ? reply.cwd : ''
    },
    async set(agentOrPath, pathOrSignal, _signal) {
      const path = pathOf(agentOrPath, pathOrSignal)
      const reply = await call('set', { path })
      return typeof reply?.cwd === 'string' ? reply.cwd : ''
    },
    get lastRecovered() {
      return lastRecovered
    },
  }
}

/** 官方 kv facet 的 unit descriptor → wire 形状（snake_case，spec §13.2/m3 §3.3）。 */
export function normalizeUnitDescriptor(descriptor = {}) {
  if (!descriptor || typeof descriptor !== 'object') {
    throw new HostServiceError('E_SERVICE_ARGS', 'unit descriptor must be an object')
  }
  const name = descriptor.name
  if (typeof name !== 'string' || !/^[a-z][a-z0-9_]*$/.test(name)) {
    throw new HostServiceError('E_SERVICE_ARGS', `invalid unit name: ${name}`)
  }
  const version = descriptor.version
  if (!Number.isInteger(version) || version < 0) {
    throw new HostServiceError('E_SERVICE_ARGS', `invalid unit version: ${version}`)
  }
  const layout = descriptor.layout ?? 'single'
  if (layout !== 'single' && layout !== 'per-record') {
    throw new HostServiceError('E_SERVICE_ARGS', `invalid unit layout: ${layout}`)
  }
  return {
    name,
    version,
    compatible_versions:
      descriptor.compatible_versions ?? descriptor.compatibleVersions ?? [],
    layout,
    tables: descriptor.tables ?? [],
    has_global: descriptor.has_global ?? descriptor.hasGlobal ?? false,
  }
}

/**
 * `storage-kv/v1` 的 kv facet 代理：unit 注册表（unit_id ↔ descriptor）留在 Sidecar，
 * 数据与版本戳在宿主（Rust `StorageKvService`）。所有 unit 方法都带 `unit_id`。
 */
export function createKvFacet({ callService } = {}) {
  const units = new Map()
  const call = (method, args) =>
    callService({
      serviceKey: SERVICE_KEYS.storageKv,
      method,
      args,
      depth: 1,
    })

  const createUnit = (unitId, descriptor) => {
    const unitCall = (method, extra = {}) => call(method, { unit_id: unitId, ...extra })
    return {
      unit_id: unitId,
      descriptor,
      loadAll: () => unitCall('unit.loadAll'),
      async putRecord(table, key, value) {
        await unitCall('unit.putRecord', { table, key, value })
      },
      deleteRecord: (table, key) => unitCall('unit.deleteRecord', { table, key }),
      deleteRecords: (table, keys) => unitCall('unit.deleteRecords', { table, keys }),
      async setGlobal(value) {
        await unitCall('unit.setGlobal', { value })
      },
      async close() {
        await unitCall('unit.close')
        units.delete(unitId)
      },
    }
  }

  return {
    units,
    async open(rawDescriptor) {
      const descriptor = normalizeUnitDescriptor(rawDescriptor)
      const reply = await call('unit.open', { unit: descriptor })
      const unitId = reply?.unit_id
      if (typeof unitId !== 'string' || unitId.length === 0) {
        throw new HostServiceError('E_INTERNAL', 'unit.open did not return a unit_id')
      }
      units.set(unitId, descriptor)
      return createUnit(unitId, descriptor)
    },
    descriptorOf(unitId) {
      return units.get(unitId)
    },
  }
}

/** 官方 backend 形状：`{name, kv}`（dsh-storage 的 "The backend contract"）。 */
export function createStorageBackend({ callService, name = 'storage-kv' } = {}) {
  return { name, kv: createKvFacet({ callService }) }
}

/**
 * `ctx.storage` 的最小 hub（与官方 dsh-storage 的公开面一致）：
 * `backend.register/get/names`、`forms`/`mount`/`form`/`domain`，并自动挂 `storage-kv` backend。
 */
export function createStorageHub({ callService } = {}) {
  const backends = new Map()
  const forms = new Map()
  const backend = createStorageBackend({ callService })
  backends.set(backend.name, backend)
  return {
    backend: {
      register(name, registered) {
        if (backends.has(name)) {
          throw new HostServiceError(
            'E_SERVICE_CONFLICT',
            `storage backend '${name}' is already registered`,
          )
        }
        backends.set(name, registered)
        return () => backends.delete(name)
      },
      get(name) {
        const found = backends.get(name)
        if (!found) {
          throw new HostServiceError(
            'E_SERVICE_UNAVAILABLE',
            `storage backend '${name}' is not registered`,
          )
        }
        return found
      },
      names() {
        return [...backends.keys()]
      },
    },
    forms,
    mount(form, facility) {
      if (forms.has(form)) {
        throw new HostServiceError(
          'E_SERVICE_CONFLICT',
          `storage form '${String(form)}' is already mounted`,
        )
      }
      forms.set(form, facility)
      return () => forms.delete(form)
    },
    form(name) {
      if (!forms.has(name)) {
        throw new HostServiceError(
          'E_SERVICE_UNAVAILABLE',
          `storage form '${String(name)}' is not mounted`,
        )
      }
      return forms.get(name)
    },
    get domain() {
      return forms.get('domain')
    },
  }
}

/**
 * Sidecar 的接线组合：client + event hub + workingDirectory/storage 工厂。
 * `callContext()` 通过 AsyncLocalStorage 提供当前工具调用上下文。
 */
export function createHostServicesRuntime({
  sendEnvelope,
  generation = 'node-0001',
  timeoutMs = DEFAULT_HOST_SERVICE_TIMEOUT_MS,
  callContext,
  onDiagnostic,
  setTimer,
  clearTimer,
  busEmit,
} = {}) {
  const client = createHostServiceClient({
    send: sendEnvelope,
    timeoutMs,
    onDiagnostic,
    generation,
    setTimer,
    clearTimer,
  })
  const call = (request) => {
    const active = callContext?.()
    if (active?.controller?.signal.aborted) {
      return Promise.reject(new HostServiceError('E_CANCELLED', 'tool request was cancelled'))
    }
    return client.call({
      generation: typeof generation === 'function' ? generation() : generation,
      ...request,
      ...(active?.toolRequestId ? { callId: active.callId, toolRequestId: active.toolRequestId } : {}),
    })
  }
  // event.subscribe/unsubscribe：白名单事件在首个/最后一个监听者出现时通知宿主。
  let eventSequence = 0
  const sendEventSubscription = (subscribe, eventName) => {
    eventSequence += 1
    sendEnvelope(
      buildEventSubscription({
        subscribe,
        event: eventName,
        requestId: `ev-${eventSequence}`,
        generation: typeof generation === 'function' ? generation() : generation,
      }),
    )
  }
  const eventHub = createEventHub({
    onDiagnostic,
    busEmit,
    onFirstListener: (eventName) => sendEventSubscription(true, eventName),
    onLastListener: (eventName) => sendEventSubscription(false, eventName),
  })
  return {
    enabled: true,
    client,
    call,
    eventHub,
    handleResult: (message) => client.handleResult(message),
    cancelForCall: (callId, toolRequestId) =>
      client.cancelForCall(callId, 'tool-cancelled', toolRequestId),
    createWorkingDirectory: () => createWorkingDirectoryShim({ callService: call, callContext }),
    createStorage: () => createStorageHub({ callService: call }),
    catalog: HOST_SERVICE_CATALOG,
    clientInfo: CLIENT_INFO,
  }
}
