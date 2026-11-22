export const SESSION_QUERY_CAP = 'agent.session-query'
const SERVICE = 'tool-session-query/v1'
export const SESSION_QUERY_TOOLS = [
  'session_search', 'session_event_search', 'session_trace', 'session_event_trace', 'session_event_read',
]
export const SESSION_QUERY_CATALOG = {
  service_key: SERVICE,
  methods: ['catalog', 'describe', 'events'].map(name => ({
    name, kind: 'read', timeout_ms: 5000, idempotent: true,
  })),
}

const MAX_SCAN = 4096
const MAX_CANDIDATES = 128

export async function createSessionQuery({ currentCall, callService }) {
  const {
    SessionQueryError, compileSessionTextFilter, filterSessionResults, filterSessionEventDocuments,
  } = await import('@deepseek-ai/dsh-session-query')

  const invoke = async (method, args = {}) => {
    const call = currentCall()
    call.controller.signal.throwIfAborted()
    return callService({ serviceKey: SERVICE, method, args, sessionId: call.sessionId, depth: call.depth + 1 })
  }
  const records = () => {
    const call = currentCall()
    call.queryRecords ??= invoke('catalog').then(value => value.records)
    return call.queryRecords
  }
  const describe = async (session) => {
    try {
      return await invoke('describe', { session })
    } catch (error) {
      if (error?.code !== 'E_SERVICE_DENIED') throw error
      throw new SessionQueryError('session was not found', 'SESSION_QUERY_SESSION_NOT_FOUND')
    }
  }
  const readPages = async (session, from = 0, to, limit = MAX_SCAN) => {
    if (to !== undefined && from > to) return []
    let cursor = from
    const output = []
    while (true) {
      const page = await invoke('events', { session, from: cursor, ...(to === undefined ? {} : { to }) })
      for (const event of page.events) output.push(event)
      if (page.next === null || output.length >= limit) return output.slice(0, limit)
      cursor = page.next
    }
  }
  const missingEvent = () => new SessionQueryError('event was not found', 'SESSION_QUERY_EVENT_NOT_FOUND')
  const title = async (sessionId) => {
    const known = (await records()).find(record => record.header.id === sessionId)
    const record = known ?? (await describe(sessionId)).record
    return { session: record.header, ...(record.title ? { title: { title: record.title } } : {}) }
  }
  const search = async (session, query, filters) => {
    if (query.length > 512) throw new SessionQueryError('query exceeds the supported size', 'SESSION_QUERY_INVALID_QUERY')
    const pattern = compileSessionTextFilter(query)
    const range = filters.find(filter => filter.kind === 'seq')
    const events = await readPages(session, range?.from, range?.to)
    const hits = []
    for (const event of filterSessionEventDocuments(events, filters)) {
      const match = pattern.exec(event.text)
      if (!match) continue
      const start = Math.max(0, match.index - 80)
      const end = Math.min(event.text.length, match.index + Math.min(match[0].length, 160) + 120)
      const snippet = event.text.slice(start, end).replace(/\s+/gu, ' ').trim()
      hits.push({ seq: event.seq, type: event.type, surface: event.surface, time: event.time, snippet })
    }
    return hits.reverse()
  }

  return {
    async prepare() {
      const call = currentCall()
      const { record, before } = await describe(call.sessionId)
      call.queryHeader = record.header
      call.queryBefore = before
    },
    service: {
      async filterSessions(filters) {
        const ids = filters.find(filter => filter.kind === 'id')?.values
        if (!ids) return filterSessionResults(await records(), filters)
        const available = []
        for (const id of [...new Set(ids)].slice(0, MAX_CANDIDATES)) {
          try { available.push((await describe(id)).record) } catch (error) {
            if (error?.code !== 'SESSION_QUERY_SESSION_NOT_FOUND') throw error
          }
        }
        return filterSessionResults(available, filters)
      },
      async readTitleSnapshots(ids) {
        return Promise.all([...new Set(ids)].map(async (sessionId) => {
          try { return { status: 'fulfilled', sessionId, value: await title(sessionId) } } catch (error) {
            return { status: 'rejected', sessionId, reason: error }
          }
        }))
      },
      async searchSessions({ query, sessionFilters, eventFilters }) {
        const caller = currentCall().sessionId
        const found = []
        for (const record of filterSessionResults(await records(), sessionFilters)) {
          if (record.header.id === caller) continue
          const [bestMatch] = await search(record.header.id, query, eventFilters)
          if (bestMatch) found.push({ ...record, bestMatch })
        }
        return { items: found }
      },
      async searchEvents({ sessionId, query, filters }) {
        return { session: (await describe(sessionId)).record.header, items: await search(sessionId, query, filters) }
      },
      async traceSession(sessionId) {
        const target = (await describe(sessionId)).record
        const available = await records()
        const ancestors = []
        let parent = target.header.parentSession
        while (parent !== undefined) {
          const entry = await describe(parent)
          ancestors.push(entry.record)
          parent = entry.record.header.parentSession
        }
        const descendants = (id) => available.filter(record => record.header.parentSession === id)
          .map(session => ({ session, descendants: descendants(session.header.id) }))
        return { target, ancestors, descendants: descendants(sessionId), complete: true }
      },
      async traceEvent({ sessionId, seq }) {
        const session = (await describe(sessionId)).record.header
        const [target] = await readPages(sessionId, seq, seq)
        if (!target) throw missingEvent()
        const later = await readPages(sessionId, seq + 1)
        const derivedEventSeqs = later.filter(event => event.data.callSeq === seq
          || (event.type === 'context/compacted' && event.data.fromSeq <= seq && seq <= event.data.toSeq))
          .map(event => event.seq)
        const sourceEventSeqs = Number.isSafeInteger(target.data.callSeq) ? [target.data.callSeq] : []
        return { session, target, sourceEventSeqs, derivedEventSeqs, replacementChain: [], replacedEventSeqs: [] }
      },
      async readEvent({ sessionId, seq, before = 0, after = 0 }) {
        if (before > 50 || after > 50) throw new SessionQueryError('event window exceeds 50', 'SESSION_QUERY_INVALID_WINDOW')
        const session = (await describe(sessionId)).record.header
        const events = await readPages(sessionId, Math.max(0, seq - before), seq + after)
        const target = events.find(event => event.seq === seq)
        if (!target) throw missingEvent()
        return { session, target, events }
      },
    },
  }
}
