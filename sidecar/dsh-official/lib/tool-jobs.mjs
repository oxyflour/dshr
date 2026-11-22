import { setTimeout as delay } from 'node:timers/promises'

export const BACKGROUND_JOBS_CAP = 'process.jobs'
const JOBS_SERVICE = 'tool-jobs/v1'

export const JOBS_CATALOG = {
  service_key: JOBS_SERVICE,
  methods: [
    { name: 'reserve', kind: 'write' },
    { name: 'start', kind: 'write' },
    { name: 'confirm', kind: 'write' },
    { name: 'list', kind: 'read' },
    { name: 'notices', kind: 'write' },
    { name: 'inspect', kind: 'read' },
    { name: 'read', kind: 'read' },
    { name: 'advance', kind: 'write' },
    { name: 'stop', kind: 'write' },
  ].map(method => ({ ...method, timeout_ms: 5000, idempotent: method.kind === 'read' })),
}

const live = ({ status }) => status === 'running' || status === 'stopping'

function jobView(row) {
  return {
    id: row.id, owner: row.owner, kind: row.kind, label: row.label, status: row.status,
    ...(row.detail == null ? {} : { detail: row.detail }),
    startedAt: row.created_at,
    ...(row.finished_at == null ? {} : { finishedAt: row.finished_at }),
    output: { total: row.output_bytes },
  }
}

export function createToolJobs({ currentCall, callService }) {
  let controllerAttached = false
  const subscribers = new Set()
  const scoped = owner => {
    const call = currentCall()
    if (!call?.sessionId || (owner !== undefined && call.sessionId !== owner)) {
      throw new Error('background jobs require the owning active session')
    }
    return call
  }
  const invoke = (method, args = {}) => {
    const call = scoped()
    return callService({
      serviceKey: JOBS_SERVICE, method, args, sessionId: call.sessionId, depth: call.depth + 1,
    })
  }
  const lookup = (id, owner) => {
    const job = scoped(owner).jobs?.find(job => job.id === id)
    if (!job) throw new Error('background job does not belong to this session')
    return job
  }

  const service = {
    attachController() {
      controllerAttached = true
    },
    events: {
      subscribe(_scope, handler) {
        subscribers.add(handler)
        return () => subscribers.delete(handler)
      },
    },
    start(spec) {
      const call = scoped(spec.owner)
      if (!controllerAttached || !call.jobReservation || !call.backgroundShell
          || call.jobStarted || spec.kind !== call.tool) {
        throw new Error('background job requires the authorized shell call')
      }
      call.jobStarted = true
      call.defer(invoke('start', call.backgroundShell).then(({ id }) => {
        if (id !== call.jobReservation) throw new Error('background job ID mismatch')
      }))
      return call.jobReservation
    },
    list(owner) {
      return scoped(owner).jobs ?? []
    },
    get: lookup,
    read(id, owner) {
      const call = scoped(owner)
      const page = call.jobPage
      if (page?.job.id !== id) throw new Error('background job output page is unavailable')
      if (page.to > page.from || !live(page.job)) {
        call.defer(invoke('advance', { id, from: page.from, to: page.to }))
      }
      return {
        job: jobView(page.job),
        chunks: page.text ? [{ channel: 'stdout', text: page.text }] : [],
        lossy: page.end && page.job.truncated,
      }
    },
    async wait(id, timeoutMs, owner, signal) {
      const call = scoped(owner)
      let state = lookup(id, owner)
      const end = performance.now() + timeoutMs
      while (live(state) && performance.now() < end) {
        await delay(Math.max(1, Math.min(100, end - performance.now())), undefined, { signal })
        state = jobView((await invoke('inspect', { id })).job)
      }
      call.jobPage = await invoke('read', { id })
      call.jobs = [jobView(call.jobPage.job)]
      return call.jobs[0]
    },
    kill(id, owner) {
      const call = scoped(owner)
      if (lookup(id, owner).id !== id || !call.jobStop) {
        throw new Error('background job cancellation is unavailable')
      }
      return call.jobStop
    },
  }

  return {
    service,
    async prepare(exec) {
      const call = scoped()
      if ((exec.name === 'pwsh' || exec.name === 'bash') && exec.arguments.run_in_background === true) {
        call.jobReservation = (await invoke('reserve')).id
      } else if (exec.name === 'job_list') {
        call.jobs = (await invoke('list')).jobs.map(jobView)
      } else if (exec.name === 'job_output') {
        call.jobPage = await invoke('read', { id: exec.arguments.job_id })
        call.jobs = [jobView(call.jobPage.job)]
      } else if (exec.name === 'job_kill') {
        const id = exec.arguments.job_id
        call.jobStop = (await invoke('stop', { id })).outcome
        call.jobs = [jobView((await invoke('inspect', { id })).job)]
      }
    },
    async confirm() {
      if (scoped().jobStarted) await invoke('confirm')
    },
    async notify() {
      if (!subscribers.size) return
      const { jobs } = await invoke('notices')
      for (const row of jobs) {
        const event = { type: 'settled', job: jobView(row), awaited: false }
        for (const subscriber of subscribers) subscriber(event)
      }
    },
  }
}
