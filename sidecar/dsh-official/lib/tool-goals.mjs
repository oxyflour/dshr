export const GOALS_CAP = 'agent.goals'
const SERVICE = 'tool-goals/v1'
export const GOAL_TOOLS = ['get_goal', 'create_goal', 'update_goal']
export const GOALS_CATALOG = {
  service_key: SERVICE,
  methods: ['prepare', 'commit'].map(name => ({
    name, kind: name === 'prepare' ? 'read' : 'write', timeout_ms: 5000, idempotent: false,
  })),
}

export function createToolGoals({ currentCall, callService }) {
  const state = (owner) => {
    const call = currentCall()
    if (!call?.goal || (owner && call.agent !== owner)) throw new Error('goal requires its active agent')
    return call.goal
  }
  const invoke = (method) => {
    const { sessionId, depth, turn, tool, arguments: args } = currentCall()
    return callService({
      serviceKey: SERVICE, method, sessionId, depth: depth + 1,
      args: { start_seq: turn?.start_seq, tool, arguments: args },
    })
  }
  const mutate = (owner, operation) => {
    const prepared = state(owner)
    if (!prepared.next || prepared.operation !== operation || prepared.used) {
      throw new Error('goal transition did not match its prepared operation')
    }
    prepared.used = true
    return prepared.next
  }
  return {
    async prepare(exec) {
      const call = currentCall()
      call.goal = {
        ...await invoke('prepare'),
        operation: exec.name === 'create_goal' ? 'create' : exec.arguments.action,
        turnSeq: call.turn.start_seq,
      }
      exec.deferContext = (message) => {
        if (message?.source?.kind === 'tool-goal') call.goal.notice = message.content
      }
    },
    async commit() {
      if (state().used) await invoke('commit')
    },
    service: {
      get: owner => state(owner).current ?? undefined,
      create: owner => mutate(owner, 'create'),
      edit: owner => mutate(owner, 'edit'),
      pause: owner => mutate(owner, 'pause'),
      resume: owner => mutate(owner, 'resume'),
      complete: owner => mutate(owner, 'complete'),
      block: owner => mutate(owner, 'blocked'),
    },
  }
}

export function goalTurnSnapshot(goal) {
  if (!goal?.source) return []
  return {
    length: goal.turnSeq + 2,
    [goal.turnSeq + 1]: { type: 'user/message', data: { source: goal.source } },
  }
}
