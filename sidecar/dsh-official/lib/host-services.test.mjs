// `host-services.mjs` 的单测：纯 Node（node:test），不依赖 DSH/Cordis 运行时。
//
// 覆盖：信封形状/请求关联/超时/取消/迟到答复/事件白名单/workingDirectory shim/
// storage-kv unit_id 代理/hub 形状/握手开关矩阵/组合 runtime。

import test from 'node:test'
import assert from 'node:assert/strict'

import {
  DEFAULT_HOST_SERVICE_TIMEOUT_MS,
  EVENT_WHITELIST,
  HOST_SERVICE_CATALOG,
  PROTOCOL_MAX,
  SERVICE_KEYS,
  buildEventSubscription,
  buildHostServiceRequest,
  createEventHub,
  createHostServiceClient,
  createHostServicesRuntime,
  createKvFacet,
  createStorageHub,
  createWorkingDirectoryShim,
  handleHostEventEmit,
  normalizeUnitDescriptor,
  parseHostServices,
  shouldEnableHostServices,
} from './host-services.mjs'

const settleTick = () => new Promise((resolve) => setImmediate(resolve))

function captureTransport() {
  const sent = []
  return {
    sent,
    send(envelope) {
      sent.push(envelope)
    },
  }
}

function resultEnvelope(request, result) {
  return {
    version: PROTOCOL_MAX,
    kind: 'host.service.result',
    request_id: request.request_id,
    payload: { result },
    error: null,
  }
}

function errorEnvelope(request, { code, message, retryable = false, detail } = {}) {
  return {
    version: PROTOCOL_MAX,
    kind: 'host.service.result',
    request_id: request.request_id,
    payload: detail ? { detail } : {},
    error: { code, message, retryable },
  }
}

// L2JS-001：host.service.request 信封形状（version/service_key/session/depth）。
test('L2JS-001 builds host.service.request envelope', () => {
  const envelope = buildHostServiceRequest({
    requestId: 'svc-1',
    serviceKey: SERVICE_KEYS.workingDirectory,
    method: 'set',
    args: { path: 'src' },
    sessionId: 's-0001',
    depth: 2,
    generation: 'node-0007',
    ts: '2026-11-14T00:00:00.000Z',
  })
  assert.equal(envelope.version, PROTOCOL_MAX)
  assert.equal(envelope.kind, 'host.service.request')
  assert.equal(envelope.request_id, 'svc-1')
  assert.equal(envelope.service_key, 'working-directory/v1')
  assert.equal(envelope.session_id, 's-0001')
  assert.equal(envelope.depth, 2)
  assert.equal(envelope.runtime_generation, 'node-0007')
  assert.deepEqual(envelope.payload, { method: 'set', args: { path: 'src' } })

  assert.throws(
    () =>
      buildHostServiceRequest({
        requestId: 'svc-2',
        serviceKey: 'working-directory',
        method: 'get',
      }),
    (error) => error.code === 'E_PROTOCOL',
  )
  assert.throws(
    () =>
      buildHostServiceRequest({
        requestId: 'svc-3',
        serviceKey: SERVICE_KEYS.sessionLog,
        method: 'events.read',
        args: [],
      }),
    (error) => error.code === 'E_SERVICE_ARGS',
  )
})

// L2JS-002：请求/答复按 request_id 关联（含迟到答复计数）。
test('L2JS-002 correlates results by request_id and counts late results', async () => {
  const transport = captureTransport()
  const diagnostics = []
  const client = createHostServiceClient({
    send: transport.send,
    onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
  })

  const pending = client.call({
    serviceKey: SERVICE_KEYS.sessionLog,
    method: 'events.read',
    args: { from_seq: 1, max_events: 10 },
    sessionId: 's-0001',
    depth: 1,
  })
  assert.equal(transport.sent.length, 1)
  const request = transport.sent[0]
  assert.equal(client.handleResult(resultEnvelope(request, { events: [], next_seq: 1, eof: true })), true)
  assert.deepEqual(await pending, { events: [], next_seq: 1, eof: true })
  assert.equal(client.stats.settled, 1)

  // 同一 request_id 的迟到答复：被丢弃并诊断，不抛未处理异常。
  assert.equal(client.handleResult(resultEnvelope(request, { ignored: true })), false)
  assert.equal(client.stats.lateResults, 1)
  assert.equal(diagnostics.at(-1).code, 'service.late_result')
  await settleTick()
})

// L2JS-003：host.service.result 的 error 映射为 HostServiceError（code/message/detail）。
test('L2JS-003 maps envelope errors to structured HostServiceError', async () => {
  const transport = captureTransport()
  const client = createHostServiceClient({ send: transport.send })
  const pending = client.call({
    serviceKey: SERVICE_KEYS.workingDirectory,
    method: 'set',
    args: { path: 'missing' },
    sessionId: 's-0001',
  })
  const request = transport.sent[0]
  client.handleResult(
    errorEnvelope(request, {
      code: 'E_SERVICE_ARGS',
      message: 'path must exist',
      retryable: false,
      detail: { path: '/args/path', reason: 'not-a-directory' },
    }),
  )
  await assert.rejects(pending, (error) => {
    assert.equal(error.code, 'E_SERVICE_ARGS')
    assert.equal(error.message, 'path must exist')
    assert.equal(error.detail.reason, 'not-a-directory')
    return true
  })
})

// L2JS-004：超时 → E_SERVICE_TIMEOUT（retryable）并发 host.service.cancel。
test('L2JS-004 timeout settles with E_SERVICE_TIMEOUT and sends cancel', async () => {
  const transport = captureTransport()
  const client = createHostServiceClient({ send: transport.send, timeoutMs: 25 })
  const pending = client.call({
    serviceKey: SERVICE_KEYS.workingDirectory,
    method: 'ensure',
    sessionId: 's-0001',
  })
  const request = transport.sent[0]
  await assert.rejects(pending, (error) => {
    assert.equal(error.code, 'E_SERVICE_TIMEOUT')
    assert.equal(error.retryable, true)
    return true
  })
  assert.equal(client.stats.timedOut, 1)
  assert.equal(client.pendingCount(), 0)
  const cancel = transport.sent.find((envelope) => envelope.kind === 'host.service.cancel')
  assert.ok(cancel, 'must send host.service.cancel on timeout')
  assert.equal(cancel.request_id, request.request_id)
  assert.equal(cancel.service_key, SERVICE_KEYS.workingDirectory)
  assert.equal(cancel.payload.method, 'ensure')
})

// L2JS-005：tool.cancel → cancelForCall 取消同一 call_id 的在途调用（其余不受影响）。
test('L2JS-005 cancelForCall cancels only that tool call chain', async () => {
  const transport = captureTransport()
  const client = createHostServiceClient({ send: transport.send, timeoutMs: 5_000 })
  const first = client.call({
    serviceKey: SERVICE_KEYS.workingDirectory,
    method: 'get',
    sessionId: 's-0001',
    callId: 'call-1',
  })
  const second = client.call({
    serviceKey: SERVICE_KEYS.storageKv,
    method: 'unit.loadAll',
    args: { unit_id: 'unit-000001' },
    callId: 'call-1',
  })
  const other = client.call({
    serviceKey: SERVICE_KEYS.workingDirectory,
    method: 'get',
    sessionId: 's-0001',
    callId: 'call-2',
  })
  assert.equal(client.pendingCount(), 3)
  assert.equal(client.cancelForCall('call-1'), 2)
  assert.equal(client.pendingCount(), 1)
  await assert.rejects(first, (error) => error.code === 'E_CANCELLED')
  await assert.rejects(second, (error) => error.code === 'E_CANCELLED')
  const cancels = transport.sent.filter((envelope) => envelope.kind === 'host.service.cancel')
  assert.equal(cancels.length, 2)
  for (const cancel of cancels) {
    assert.equal(cancel.payload.method !== undefined, true)
  }
  // 未取消的调用仍可正常结算。
  const otherRequest = transport.sent.find(
    (envelope) => envelope.kind === 'host.service.request' && envelope.payload.method === 'get' &&
      envelope.request_id !== transport.sent[0].request_id,
  )
  client.handleResult(resultEnvelope(otherRequest, { cwd: 'C:\\ws' }))
  assert.deepEqual(await other, { cwd: 'C:\\ws' })
  assert.equal(client.cancel('svc-unknown'), false, 'cancel is idempotent')
})

// L2JS-006：事件白名单：session.event/service.event 放行，其余拒绝并诊断。
test('L2JS-006 event hub enforces the v2 whitelist', () => {
  const diagnostics = []
  const received = []
  const hub = createEventHub({ onDiagnostic: (diagnostic) => diagnostics.push(diagnostic) })
  const unsubscribe = hub.subscribe('session.event', (payload) => received.push(payload))
  assert.equal(hub.isAllowed('session.event'), true)
  assert.equal(hub.isAllowed('tool.invoke'), false)
  assert.equal(hub.dispatch('session.event', { seq: 7 }), true)
  assert.deepEqual(received, [{ seq: 7 }])
  assert.equal(hub.dispatch('unknown.event', { x: 1 }), false)
  assert.equal(hub.stats.rejected, 1)
  assert.equal(diagnostics.at(-1).code, 'event.rejected')
  assert.throws(
    () => hub.subscribe('unknown.event', () => {}),
    (error) => error.code === 'E_SERVICE_DENIED',
  )
  unsubscribe()
  assert.equal(hub.listenerCount('session.event'), 0)

  // event.emit 入站信封：白名单内的 session.event 被分发。
  const emit = handleHostEventEmit(
    { kind: 'event.emit', payload: { event: 'session.event', payload: { seq: 8 } } },
    hub,
  )
  assert.equal(emit.ok, true)
  assert.equal(emit.event, 'session.event')
  // service.event 也在白名单内。
  assert.equal(hub.dispatch('service.event', { topic: 'x' }), true)
  assert.deepEqual(EVENT_WHITELIST, ['session.event', 'service.event'])
})

// L2JS-007：握手开关矩阵（version / host_caps / DSHR_HOST_SERVICES）。
test('L2JS-007 host service enablement matrix', () => {
  assert.equal(shouldEnableHostServices({ negotiatedProtocol: 1, hostCaps: ['service.proxy'] }), false)
  assert.equal(
    shouldEnableHostServices({ negotiatedProtocol: 2, hostCaps: ['service.proxy'] }),
    true,
  )
  assert.equal(shouldEnableHostServices({ negotiatedProtocol: 2, hostCaps: [] }), false)
  assert.equal(
    shouldEnableHostServices({
      negotiatedProtocol: 2,
      hostCaps: ['service.proxy'],
      env: { DSHR_HOST_SERVICES: '0' },
    }),
    false,
    'explicit off wins',
  )
  assert.equal(
    shouldEnableHostServices({ negotiatedProtocol: 2, hostCaps: [], env: { DSHR_HOST_SERVICES: '1' } }),
    true,
  )
  assert.equal(
    shouldEnableHostServices({ negotiatedProtocol: 1, hostCaps: [], env: { DSHR_HOST_SERVICES: '1' } }),
    false,
    'v1 negotiation must stay off',
  )
})

// L2JS-008：catalog/self-description 与 parseHostServices 一致。
test('L2JS-008 catalog describes the proxied host services', () => {
  assert.equal(HOST_SERVICE_CATALOG.length, 3)
  const keys = HOST_SERVICE_CATALOG.map((table) => table.service_key)
  assert.deepEqual(keys, [
    SERVICE_KEYS.workingDirectory,
    SERVICE_KEYS.sessionLog,
    SERVICE_KEYS.storageKv,
  ])
  const parsed = parseHostServices({ host_services: HOST_SERVICE_CATALOG })
  assert.equal(parsed.size, 3)
  assert.equal(parsed.get(SERVICE_KEYS.storageKv).methods.has('unit.putRecord'), true)
  assert.equal(parsed.get(SERVICE_KEYS.sessionLog).methods.has('events.read'), true)
  assert.equal(parsed.get(SERVICE_KEYS.workingDirectory).methods.size, 3)
  assert.equal(parseHostServices(undefined).size, 0)
})

// L2JS-009：workingDirectory shim：{cwd} → string、ensure recovered、callContext 传 session/depth。
test('L2JS-009 workingDirectory shim unwraps wire shapes', async () => {
  const calls = []
  const shim = createWorkingDirectoryShim({
    callService: async (request) => {
      calls.push(request)
      if (request.method === 'ensure') return { cwd: 'C:\\ws', recovered: true }
      return { cwd: 'C:\\ws\\src' }
    },
    callContext: () => ({ callId: 'call-9', sessionId: 's-0001', depth: 2 }),
  })
  assert.equal(await shim.get(), 'C:\\ws\\src')
  assert.equal(await shim.set('src'), 'C:\\ws\\src')
  assert.equal(await shim.ensure(), 'C:\\ws')
  assert.equal(shim.lastRecovered, true)
  // 官方 dsh-working-directory 的签名：get(agent) / ensure(agent, signal) / set(agent, path, signal)。
  const fakeAgent = { id: 'agent-1' }
  assert.equal(await shim.get(fakeAgent), 'C:\\ws\\src')
  assert.equal(await shim.set(fakeAgent, 'src', undefined), 'C:\\ws\\src')
  assert.equal(await shim.ensure(fakeAgent, undefined), 'C:\\ws')
  assert.equal(calls.length, 6)
  for (const request of calls) {
    assert.equal(request.serviceKey, SERVICE_KEYS.workingDirectory)
    assert.equal(request.sessionId, 's-0001')
    assert.equal(request.depth, 3, 'service call depth = tool depth + 1')
  }
  assert.equal(shim.lastRecovered, true)
})

// L2JS-010：storage-kv kv facet：descriptor 映射 + 所有 unit 方法带 unit_id。
test('L2JS-010 storage kv facet translates units to host.service.request', async () => {
  const calls = []
  const facet = createKvFacet({
    callService: async (request) => {
      calls.push(request)
      if (request.method === 'unit.open') return { unit_id: 'unit-000001' }
      return {}
    },
  })
  assert.deepEqual(
    normalizeUnitDescriptor({
      name: 'todos',
      version: 2,
      compatibleVersions: [1],
      layout: 'per-record',
      tables: ['items'],
      hasGlobal: true,
    }),
    {
      name: 'todos',
      version: 2,
      compatible_versions: [1],
      layout: 'per-record',
      tables: ['items'],
      has_global: true,
    },
  )
  const unit = await facet.open({
    name: 'todos',
    version: 2,
    compatibleVersions: [1],
    layout: 'single',
    tables: ['items'],
  })
  assert.equal(unit.unit_id, 'unit-000001')
  assert.equal(calls[0].serviceKey, SERVICE_KEYS.storageKv)
  assert.equal(calls[0].args.unit.layout, 'single')
  assert.equal(calls[0].args.unit.compatible_versions.length, 1)

  await unit.putRecord('items', 'a', { n: 1 })
  await unit.loadAll()
  await unit.deleteRecord('items', 'a')
  await unit.deleteRecords('items', ['a'])
  await unit.setGlobal({ theme: 'dark' })
  await unit.close()
  const unitCalls = calls.slice(1)
  assert.equal(unitCalls.length, 6)
  for (const call of unitCalls) {
    assert.equal(call.args.unit_id, 'unit-000001')
  }
  assert.equal(unitCalls[0].args.key, 'a')
  assert.equal(calls[0].args.unit.name, 'todos')
  assert.equal(facet.units.size, 0, 'close unregisters the unit')
})

// L2JS-011：ctx.storage hub 形状（backend 注册表 + form 挂载）。
test('L2JS-011 storage hub exposes the official storage surface', async () => {
  const hub = createStorageHub({
    callService: async () => ({ unit_id: 'unit-000002' }),
  })
  assert.deepEqual(hub.backend.names(), ['storage-kv'])
  const backend = hub.backend.get('storage-kv')
  assert.equal(backend.name, 'storage-kv')
  const unit = await backend.kv.open({ name: 'todos', version: 1 })
  assert.equal(unit.unit_id, 'unit-000002')

  assert.throws(
    () => hub.backend.register('storage-kv', backend),
    (error) => error.code === 'E_SERVICE_CONFLICT',
  )
  assert.throws(
    () => hub.backend.get('missing'),
    (error) => error.code === 'E_SERVICE_UNAVAILABLE',
  )
  const facility = { name: 'domain' }
  hub.mount('domain', facility)
  assert.equal(hub.domain, facility)
  assert.equal(hub.form('domain'), facility)
  assert.throws(() => hub.mount('domain', facility), (error) => error.code === 'E_SERVICE_CONFLICT')
})

// L2JS-012：组合 runtime：client/eventHub/workingDirectory/storage 接线正确。
test('L2JS-012 createHostServicesRuntime wires the sidecar surface', async () => {
  const transport = captureTransport()
  const runtime = createHostServicesRuntime({
    sendEnvelope: transport.send,
    callContext: () => ({ callId: 'call-12', sessionId: 's-0001', depth: 0 }),
    timeoutMs: 1_000,
  })
  assert.equal(runtime.enabled, true)
  assert.equal(runtime.catalog.length, 3)
  assert.equal(typeof runtime.createWorkingDirectory, 'function')
  assert.equal(typeof runtime.createStorage, 'function')
  assert.equal(typeof runtime.handleResult, 'function')
  assert.equal(DEFAULT_HOST_SERVICE_TIMEOUT_MS, 30_000)

  const workingDirectory = runtime.createWorkingDirectory()
  const pending = workingDirectory.get()
  const envelope = transport.sent[0]
  assert.equal(envelope.kind, 'host.service.request')
  assert.equal(envelope.service_key, SERVICE_KEYS.workingDirectory)
  assert.equal(envelope.session_id, 's-0001')
  assert.equal(envelope.depth, 1)
  assert.equal(runtime.handleResult(resultEnvelope(envelope, { cwd: 'C:\\ws' })), true)
  assert.equal(await pending, 'C:\\ws')

  const storage = runtime.createStorage()
  assert.deepEqual(storage.backend.names(), ['storage-kv'])
})

// L2JS-013：event.subscribe/unsubscribe 信封：首个监听者订阅、最后一个退订。
test('L2JS-013 event subscriptions reach the host channel', () => {
  const subscription = buildEventSubscription({
    subscribe: true,
    event: 'session.event',
    filter: { session_id: 's-0001' },
    requestId: 'ev-1',
    generation: 'node-0007',
  })
  assert.equal(subscription.kind, 'event.subscribe')
  assert.equal(subscription.version, PROTOCOL_MAX)
  assert.deepEqual(subscription.payload, {
    event: 'session.event',
    filter: { session_id: 's-0001' },
  })
  assert.throws(
    () => buildEventSubscription({ subscribe: true, event: 'tool.invoke', requestId: 'ev-2' }),
    (error) => error.code === 'E_SERVICE_DENIED',
  )

  const transport = captureTransport()
  const runtime = createHostServicesRuntime({
    sendEnvelope: transport.send,
    callContext: () => ({ callId: 'call-13', sessionId: 's-0001', depth: 0 }),
    timeoutMs: 1_000,
  })
  const first = runtime.eventHub.subscribe('service.event', () => {})
  const second = runtime.eventHub.subscribe('service.event', () => {})
  const subscribes = transport.sent.filter((envelope) => envelope.kind === 'event.subscribe')
  assert.equal(subscribes.length, 1, 'only the first listener triggers subscribe')
  assert.equal(subscribes[0].payload.event, 'service.event')
  first()
  assert.equal(
    transport.sent.filter((envelope) => envelope.kind === 'event.unsubscribe').length,
    0,
    'still has a listener',
  )
  second()
  const unsubscribes = transport.sent.filter((envelope) => envelope.kind === 'event.unsubscribe')
  assert.equal(unsubscribes.length, 1, 'last listener triggers unsubscribe')
  assert.equal(unsubscribes[0].payload.event, 'service.event')
})
