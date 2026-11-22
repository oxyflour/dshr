export const SCHEDULE_CAP = 'agent.schedules'
export const SCHEDULE_PLUGIN = '@deepseek-ai/dsh-tool-schedule'
export const SCHEDULE_TOOLS = ['schedule_create', 'schedule_list', 'schedule_view', 'schedule_edit']
const SERVICE = 'tool-schedules/v1'
export const SCHEDULE_CATALOG = {
  service_key: SERVICE,
  methods: ['create', 'list', 'view', 'edit', 'due', 'claim'].map(name => {
    const read = ['list', 'view', 'due'].includes(name)
    return { name, kind: read ? 'read' : 'write', timeout_ms: 5000, idempotent: read }
  }),
}

export async function createSchedules({ currentCall, callService }) {
  const [{ RRule }, { ScheduleInputError, renderRecurringReminderBatchFraming }] = await Promise.all([
    import('@deepseek-ai/dsh-util-time'), import('@deepseek-ai/dsh-schedule'),
  ])
  const call = () => {
    const active = currentCall()
    if (!active?.agent) throw new Error('schedule requires its active session')
    return active
  }
  const invoke = async (method, args = {}) => {
    const active = call()
    try {
      return (await callService({ serviceKey: SERVICE, method, args, sessionId: active.sessionId, depth: active.depth + 1 })).result
    } catch (error) {
      if (error.detail?.schedule_code) throw new ScheduleInputError(error.detail.schedule_code, error.message)
      throw error
    }
  }
  const rule = (expression) => {
    const result = RRule.parse(expression, { rejectIneffectiveSetPositions: true })
    if (result.kind === 'accepted') return result.rule
    const code = { syntax: 'invalid_rule', time_zone: 'invalid_time_zone', density: 'frequency_too_high' }[result.reason]
    throw new ScheduleInputError(code ?? 'invalid_rule', result.detail)
  }
  const prepare = (expression) => {
    const recurrence = rule(expression)
    const when = recurrence.next(Date.now())
    if (!when) throw new ScheduleInputError('not_future', 'rrule has no future occurrence')
    return { rrule: recurrence.expression, due: Date.parse(when) }
  }
  const iso = (milliseconds) => new Date(milliseconds).toISOString()
  const task = ({ lastDelivery, ...item }) => ({
    ...item,
    kind: 'rrule',
    target: { kind: 'session', sessionId: call().sessionId },
    scheduledAt: iso(item.scheduledAt),
    updatedAt: iso(item.updatedAt),
    ...(lastDelivery ? { lastDelivery: {
      scheduledAt: iso(lastDelivery.scheduledAt), deliveredAt: iso(lastDelivery.deliveredAt), targetSessionId: call().sessionId,
    } } : {}),
  })
  return {
    async notices() {
      let pending
      try { pending = await invoke('due') } catch (error) {
        if (error.code === 'subagent_session') return []
        throw error
      }
      const reminders = []
      for (const item of pending.tasks) {
        const { occurrenceAt, nextScheduledAt } = rule(item.rrule).resolve(iso(item.scheduledAt), pending.time)
        const claimed = await invoke('claim', {
          id: item.id, due: item.scheduledAt,
          latest: Date.parse(occurrenceAt), next: nextScheduledAt ? Date.parse(nextScheduledAt) : null,
        })
        if (claimed) reminders.push({ record: claimed, occurrenceAt: iso(claimed.occurrence) })
      }
      if (reminders.length === 0) return []
      return [{ content: [{ type: 'text', text: renderRecurringReminderBatchFraming(reminders) }] }]
    },
    service: {
      deliveryLimits: () => ({ newSessionIntervalSeconds: 3600 }),
      async create(request) {
        if (request.target.kind !== 'session') throw new ScheduleInputError('invalid_rule', 'scheduled delivery supports this_session')
        return task(await invoke('create', { arguments: call().arguments, timing: prepare(request.timing.rrule) }))
      },
      async list({ active, limit = 20, offset = 0 }) {
        const visible = (await invoke('list')).filter(item => active === undefined || (item.status === 'active') === active)
        const items = visible.slice(offset, offset + limit).map(task)
        return { items, shown: items.length, total: visible.length, limit, offset }
      },
      async view({ id }) {
        const item = await invoke('view', { id })
        return item ? task(item) : { id, code: 'schedule_not_found' }
      },
      async edit(request) {
        if (request.patch?.target && request.patch.target.kind !== 'session') {
          throw new ScheduleInputError('invalid_selector', 'scheduled delivery supports this_session')
        }
        let expression = request.patch?.timing?.rrule
        if (request.patch?.active === true && expression === undefined) {
          const current = await invoke('view', { id: request.id })
          if (!current) return { id: request.id, code: 'schedule_not_found' }
          expression = current.rrule
        }
        const timing = expression === undefined ? {} : prepare(expression)
        if (request.expectedUpdatedAt !== undefined) timing.expected = Date.parse(request.expectedUpdatedAt)
        const item = await invoke('edit', { arguments: call().arguments, timing })
        return item.status ? task(item) : item
      },
    },
  }
}
