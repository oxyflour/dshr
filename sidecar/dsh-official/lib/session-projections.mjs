// Sidecar 的 `ctx.sessionProjections` 最小实现（P1-5）。
//
// 设计（docs/m3-l2-services.md §3.2）：投影单元是 JS 函数，**注册表留在 Sidecar**；
// 宿主只提供数据面：`session-projections/v1.session.describe`（header 给 init）与
// `events.read`（分页事件流，gap 时用它补齐）。本模块不 import Cordis/DSH，
// 可被 node:test 直接单测；sidecar-dsh.mjs 负责把 `session/created` / `session/event`
// 桥接到 Cordis 事件总线。
//
// 事件形状：host 的 `event.emit {event:"session.event", payload:{session_id,seq,type,data}}`
// → 本模块折进已注册单元；`seq` 必须单调（session-log.md I2），gap 用 events.read 补齐。

export const PROJECTIONS_SERVICE_KEY = 'session-projections/v1'
export const DEFAULT_MAX_EVENTS_PER_PAGE = 500
export const DEFAULT_MAX_BACKFILL_EVENTS = 2_000
export const PROJECTION_BACKFILL_DIAGNOSTIC = 'projection.backfill'

function projectionError(code, message) {
  const error = new Error(message)
  error.code = code
  return error
}

/**
 * @param {object} options
 * @param {(request: object) => Promise<any>} options.callService 宿主服务调用（host.service.request）
 * @param {(diagnostic: object) => void} [options.onDiagnostic]
 * @param {(view: object) => void} [options.onCreated] 首次建立会话视图（桥 Cordis session/created）
 * @param {(view: object, event: object) => void} [options.onEvent] 每条已提交事件（桥 session/event）
 */
export function createSessionProjectionsShim({
  callService,
  onDiagnostic,
  onCreated,
  onEvent,
  maxEventsPerPage = DEFAULT_MAX_EVENTS_PER_PAGE,
  maxBackfillEvents = DEFAULT_MAX_BACKFILL_EVENTS,
} = {}) {
  if (typeof callService !== 'function') {
    throw projectionError('E_INTERNAL', 'session projections shim needs callService')
  }
  const units = new Map() // key -> { definition, refs }
  const sessions = new Map() // sessionId -> { view, header, cells: Map(key -> {state, observedSeq}) }
  const stats = { registered: 0, events: 0, duplicates: 0, backfills: 0, backfillEvents: 0 }

  const requireSessionId = (sessionId) => {
    if (typeof sessionId !== 'string' || sessionId.length === 0) {
      throw projectionError('E_SERVICE_ARGS', 'session.event requires session_id')
    }
  }

  const ensureSession = async (sessionId) => {
    requireSessionId(sessionId)
    const existing = sessions.get(sessionId)
    if (existing) return existing
    const reply = await callService({
      serviceKey: PROJECTIONS_SERVICE_KEY,
      method: 'session.describe',
      args: {},
      sessionId,
      depth: 1,
    })
    const header = reply?.header ?? {}
    const view = { id: sessionId, header, seq: 0, inheritedEventCount: 0 }
    const cells = new Map()
    for (const [key, unit] of units) {
      if (unit.refs === 0) continue
      cells.set(key, {
        state: unit.definition.init(header, view.inheritedEventCount),
        observedSeq: -1,
      })
    }
    const session = { view, header, cells }
    sessions.set(sessionId, session)
    onDiagnostic?.({ code: 'projection.session_created', session_id: sessionId })
    onCreated?.(view)
    return session
  }

  const applyCommitted = (session, event) => {
    for (const [key, unit] of units) {
      if (unit.refs === 0) continue
      const cell = session.cells.get(key)
      if (!cell) {
        session.cells.set(key, {
          state: unit.definition.init(session.header, session.view.inheritedEventCount),
          observedSeq: -1,
        })
      }
      const current = session.cells.get(key)
      current.state = unit.definition.apply(current.state, event)
      current.observedSeq = event.seq
    }
    session.view.seq = event.seq
    stats.events += 1
    onEvent?.(session.view, event)
  }

  /** gap 补齐：从 expected 读到 targetSeq（分页，单次 ≤ maxBackfillEvents）。 */
  const backfill = async (session, sessionId, fromSeq, targetSeq) => {
    let cursor = fromSeq
    let fetched = 0
    stats.backfills += 1
    onDiagnostic?.({
      code: PROJECTION_BACKFILL_DIAGNOSTIC,
      session_id: sessionId,
      from_seq: fromSeq,
      to_seq: targetSeq,
    })
    while (cursor <= targetSeq && fetched < maxBackfillEvents) {
      const remaining = Math.min(
        maxBackfillEvents - fetched,
        maxEventsPerPage,
        targetSeq - cursor + 1,
      )
      const page = await callService({
        serviceKey: PROJECTIONS_SERVICE_KEY,
        method: 'events.read',
        args: { from_seq: cursor, max_events: remaining },
        sessionId,
        depth: 1,
      })
      const events = Array.isArray(page?.events) ? page.events : []
      for (const event of events) {
        if (event.seq > session.view.seq) applyCommitted(session, event)
        fetched += 1
        stats.backfillEvents += 1
        if (event.seq >= targetSeq) break
      }
      if (events.length === 0 || page?.eof === true) break
      cursor = Number.isInteger(page?.next_seq) ? page.next_seq : cursor + events.length
    }
    return fetched
  }

  /** 处理一条 host `session.event`（`event.emit` 的 payload）。 */
  const applySessionEvent = async (payload) => {
    const sessionId = payload?.session_id
    requireSessionId(sessionId)
    const seq = Number(payload?.seq)
    if (!Number.isInteger(seq) || seq < 1) {
      throw projectionError('E_SERVICE_ARGS', 'session.event seq must be a positive integer')
    }
    const event = { seq, type: payload.type, data: payload.data }
    const session = await ensureSession(sessionId)
    if (seq <= session.view.seq) {
      stats.duplicates += 1
      onDiagnostic?.({
        code: 'projection.duplicate',
        session_id: sessionId,
        seq,
        last_seq: session.view.seq,
      })
      return { applied: false, duplicate: true }
    }
    const expected = session.view.seq + 1
    if (seq > expected) {
      await backfill(session, sessionId, expected, seq - 1)
    }
    applyCommitted(session, event)
    return { applied: true, duplicate: false }
  }

  return {
    stats,
    serviceKey: PROJECTIONS_SERVICE_KEY,

    /** 注册一个投影单元（官方 `ctx.sessionProjections.register` 形状的异步 API）。 */
    async register(definition) {
      if (!definition || typeof definition.key !== 'string' || definition.key.length === 0) {
        throw projectionError('E_SERVICE_ARGS', 'projection definition requires key')
      }
      if (typeof definition.init !== 'function' || typeof definition.apply !== 'function') {
        throw projectionError('E_SERVICE_ARGS', 'projection requires init/apply functions')
      }
      if (!Number.isSafeInteger(definition.stateVersion) || definition.stateVersion < 0) {
        throw projectionError('E_SERVICE_ARGS', 'projection stateVersion must be a non-negative integer')
      }
      const existing = units.get(definition.key)
      if (existing) {
        existing.refs += 1
      } else {
        units.set(definition.key, { definition, refs: 1 })
        stats.registered += 1
      }
      let released = false
      const release = () => {
        if (released) return
        released = true
        const unit = units.get(definition.key)
        if (!unit) return
        unit.refs -= 1
        if (unit.refs <= 0) {
          units.delete(definition.key)
          for (const session of sessions.values()) session.cells.delete(definition.key)
        }
      }
      release.stateOf = (session) => sessions.get(session.id)?.cells.get(definition.key)?.state
        ?? definition.init(session.header, session.inheritedEventCount ?? 0)
      return release
    },

    /** 取消注册（测试/卸载路径；等价 release 一次）。 */
    release(key) {
      const unit = units.get(key)
      if (!unit) return false
      unit.refs -= 1
      if (unit.refs <= 0) {
        units.delete(key)
        for (const session of sessions.values()) session.cells.delete(key)
      }
      return true
    },

    hasUnit: (key) => units.has(key),
    registeredKeys: () => [...units.keys()],
    sessionView: (sessionId) => sessions.get(sessionId)?.view,
    stateOf(sessionId, key) {
      const session = sessions.get(sessionId)
      return session?.cells.get(key)?.state
    },
    /** 所有单元在同一会话上的快照（UI/carrier 读取面）。 */
    snapshot(sessionId) {
      const session = sessions.get(sessionId)
      if (!session) return undefined
      const out = {}
      for (const [key, cell] of session.cells) out[key] = cell.state
      return out
    },

    /** 处理 host `event.emit` 信封（`event: "session.event"` 才折进投影）。 */
    async handleHostEvent(message) {
      if (message?.kind !== 'event.emit') return { handled: false }
      const eventName = message.payload?.event
      if (eventName !== 'session.event') return { handled: false }
      await applySessionEvent(message.payload?.payload ?? {})
      return { handled: true }
    },

    /** 测试/集成入口：直接注入一条已提交事件。 */
    applyEvent: applySessionEvent,
    ensureSession,
  }
}
