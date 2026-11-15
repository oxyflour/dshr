// `session-projections.mjs`（P1-5 sidecar shim）单测：node:test，无 DSH 依赖。
//
// 覆盖：register/init(header)、事件顺序折入、重复/drop、gap 用 events.read 分页补齐、
// E_* 错误传播、backfill 上限 + projection.backfill 诊断、release 停止更新。
// 对应 m3-l2-services §3.2 的 RPC-L2-201..208 的 sidecar 侧证据（L2JS-021..028）。

import test from 'node:test'
import assert from 'node:assert/strict'

import {
  PROJECTION_BACKFILL_DIAGNOSTIC,
  PROJECTIONS_SERVICE_KEY,
  createSessionProjectionsShim,
} from './session-projections.mjs'

function headerFor(sessionId) {
  return {
    id: sessionId,
    cwd: 'C:\\ws',
    created_at: '2026-11-15T00:00:00Z',
    version: 1,
    parent: null,
    delegation_depth: 0,
  }
}

function makeHost({ eventsBySession = new Map(), readError = null } = {}) {
  const calls = []
  const callService = async (request) => {
    calls.push(request)
    if (request.method === 'session.describe') {
      return { header: headerFor(request.sessionId) }
    }
    if (request.method === 'events.read') {
      if (readError) throw readError
      const all = eventsBySession.get(request.sessionId) ?? []
      const from = request.args.from_seq
      const max = request.args.max_events
      const page = all.filter((event) => event.seq >= from).slice(0, max)
      const lastSeq = all.length === 0 ? 0 : all[all.length - 1].seq
      return {
        events: page,
        next_seq: page.length === 0 ? from : page[page.length - 1].seq + 1,
        eof: page.length === 0 || page[page.length - 1].seq >= lastSeq,
      }
    }
    throw new Error(`unexpected method ${request.method}`)
  }
  return { calls, callService }
}

function projection(key, { init, apply } = {}) {
  return {
    key,
    stateSchema: {},
    init: init ?? ((header) => ({ header, count: 0 })),
    apply: apply ?? ((state, event) => ({ ...state, count: state.count + 1, last: event.type })),
    wire: { viewSchema: {}, view: (state) => state, summary: false },
    stateVersion: 1,
  }
}

// L2JS-021 / RPC-L2-202：首次事件通过 session.describe 建视图并 init(header)。
test('L2JS-021 register + ensureSession init(header)', async () => {
  const host = makeHost()
  const shim = createSessionProjectionsShim({ callService: host.callService })
  await shim.register(projection('todos'))
  await shim.applyEvent({ session_id: 's-1', seq: 1, type: 'turn/start', data: {} })

  assert.equal(host.calls[0].method, 'session.describe')
  assert.equal(host.calls[0].serviceKey, PROJECTIONS_SERVICE_KEY)
  assert.equal(host.calls[0].sessionId, 's-1')
  assert.deepEqual(shim.stateOf('s-1', 'todos').header, headerFor('s-1'))
  assert.equal(shim.sessionView('s-1').seq, 1)
})

// L2JS-022：register 校验（key/init/apply/stateVersion）。
test('L2JS-022 register validates definition', async () => {
  const shim = createSessionProjectionsShim({ callService: async () => ({}) })
  await assert.rejects(
    shim.register({ key: 'bad' }),
    (error) => error.code === 'E_SERVICE_ARGS',
  )
  await assert.rejects(
    shim.register({ key: 'bad', init: () => null, apply: () => null, stateVersion: -1 }),
    (error) => error.code === 'E_SERVICE_ARGS',
  )
  const release = await shim.register(projection('ok'))
  assert.equal(shim.hasUnit('ok'), true)
  release()
  assert.equal(shim.hasUnit('ok'), false)
})

// L2JS-023 / RPC-L2-204：event.emit(session.event) 驱动 apply，状态按序更新。
test('L2JS-023 session.event drives apply in order', async () => {
  const host = makeHost()
  const shim = createSessionProjectionsShim({ callService: host.callService })
  await shim.register(
    projection('todos', {
      apply: (state, event) =>
        event.type === 'todo/write' ? event.data.todos : state,
    }),
  )
  await shim.handleHostEvent({
    kind: 'event.emit',
    payload: {
      event: 'session.event',
      payload: { session_id: 's-1', seq: 1, type: 'todo/write', data: { todos: [{ a: 1 }] } },
    },
  })
  assert.deepEqual(shim.stateOf('s-1', 'todos'), [{ a: 1 }])
  // 其它事件名不消费。
  const other = await shim.handleHostEvent({
    kind: 'event.emit',
    payload: { event: 'service.event', payload: { session_id: 's-1', seq: 2 } },
  })
  assert.equal(other.handled, false)
})

// L2JS-024：重复/过期 seq 丢弃并诊断（不重复 apply）。
test('L2JS-024 duplicate seq is dropped', async () => {
  const host = makeHost()
  const diagnostics = []
  const shim = createSessionProjectionsShim({
    callService: host.callService,
    onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
  })
  await shim.register(projection('todos'))
  await shim.applyEvent({ session_id: 's-1', seq: 1, type: 'a', data: {} })
  const result = await shim.applyEvent({ session_id: 's-1', seq: 1, type: 'a', data: {} })
  assert.equal(result.duplicate, true)
  assert.equal(shim.stats.duplicates, 1)
  assert.ok(diagnostics.some((diagnostic) => diagnostic.code === 'projection.duplicate'))
})

// L2JS-025 / RPC-L2-205：seq gap → 用 events.read 分页补齐后再应用当前事件。
test('L2JS-025 gap detected and backfilled via events.read', async () => {
  const eventsBySession = new Map([
    [
      's-1',
      [
        { seq: 1, type: 'x', data: 1 },
        { seq: 2, type: 'x', data: 2 },
        { seq: 3, type: 'x', data: 3 },
      ],
    ],
  ])
  const host = makeHost({ eventsBySession })
  const diagnostics = []
  const shim = createSessionProjectionsShim({
    callService: host.callService,
    onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
  })
  await shim.register(projection('todos'))
  await shim.applyEvent({ session_id: 's-1', seq: 1, type: 'x', data: 1 })
  await shim.applyEvent({ session_id: 's-1', seq: 3, type: 'x', data: 3 })

  assert.equal(shim.sessionView('s-1').seq, 3)
  const readCalls = host.calls.filter((call) => call.method === 'events.read')
  assert.equal(readCalls.length, 1, 'gap 只需一次 events.read 补齐');
  assert.deepEqual(
    { from_seq: readCalls[0].args.from_seq, max_events: readCalls[0].args.max_events },
    { from_seq: 2, max_events: 1 },
  )
  assert.ok(
    diagnostics.some((diagnostic) => diagnostic.code === PROJECTION_BACKFILL_DIAGNOSTIC),
  )
})

// L2JS-026 / RPC-L2-206：宿主 E_TOO_LARGE 等错误向上传播（不静默吞掉）。
test('L2JS-026 events.read errors are surfaced', async () => {
  const error = Object.assign(new Error('page too large'), { code: 'E_TOO_LARGE' })
  const host = makeHost({ readError: error })
  const shim = createSessionProjectionsShim({ callService: host.callService })
  await shim.register(projection('todos'))
  await shim.applyEvent({ session_id: 's-1', seq: 1, type: 'a', data: {} })
  await assert.rejects(
    shim.applyEvent({ session_id: 's-1', seq: 3, type: 'a', data: {} }),
    (rejected) => rejected.code === 'E_TOO_LARGE',
  )
})

// L2JS-027 / RPC-L2-207：大 gap 的 backfill 受 maxBackfillEvents 上限约束并留诊断。
test('L2JS-027 backfill is bounded and diagnosed', async () => {
  const events = Array.from({ length: 2_500 }, (_, index) => ({
    seq: index + 1,
    type: 'x',
    data: index + 1,
  }))
  const host = makeHost({ eventsBySession: new Map([['s-1', events]]) })
  const diagnostics = []
  const shim = createSessionProjectionsShim({
    callService: host.callService,
    onDiagnostic: (diagnostic) => diagnostics.push(diagnostic),
    maxBackfillEvents: 50,
    maxEventsPerPage: 20,
  })
  await shim.register(projection('todos'))
  await shim.applyEvent({ session_id: 's-1', seq: 1, type: 'x', data: 1 })
  await shim.applyEvent({ session_id: 's-1', seq: 2_500, type: 'x', data: 2_500 })
  assert.ok(shim.stats.backfillEvents <= 50, `bounded: ${shim.stats.backfillEvents}`)
  assert.ok(
    diagnostics.some(
      (diagnostic) =>
        diagnostic.code === PROJECTION_BACKFILL_DIAGNOSTIC &&
        diagnostic.from_seq === 2 &&
        diagnostic.to_seq === 2_499,
    ),
  )
})

// L2JS-028 / RPC-L2-208：release 最后一个引用后单元移除、状态不再更新（卸载/revoke）。
test('L2JS-028 release stops projection updates', async () => {
  const host = makeHost()
  const shim = createSessionProjectionsShim({ callService: host.callService })
  const release1 = await shim.register(projection('todos'))
  const release2 = await shim.register(projection('todos'))
  await shim.applyEvent({ session_id: 's-1', seq: 1, type: 'a', data: {} })
  assert.equal(shim.stateOf('s-1', 'todos').count, 1)
  release1()
  assert.equal(shim.hasUnit('todos'), true, '还有共享引用');
  release2()
  assert.equal(shim.hasUnit('todos'), false)
  assert.equal(shim.stateOf('s-1', 'todos'), undefined);
  await shim.applyEvent({ session_id: 's-1', seq: 2, type: 'a', data: {} })
  assert.equal(shim.sessionView('s-1').seq, 2, '视图仍推进，但无单元可折');
  assert.deepEqual(shim.registeredKeys(), [])
})
