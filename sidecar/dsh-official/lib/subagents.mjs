export const SUBAGENT_CAP = 'agent.subagents'
const SERVICE = 'tool-subagents/v1'
export const SUBAGENT_TOOLS = ['subagent', 'list_agents', 'send_message', 'wait_agent', 'interrupt_agent']
export const SUBAGENT_PLUGINS = new Set([
  '@deepseek-ai/dsh-tool-subagent', '@deepseek-ai/dsh-tool-subagent-control',
])
export const SUBAGENT_CATALOG = {
  service_key: SERVICE,
  methods: ['list', 'pending', 'notices', 'start', 'message', 'interrupt', 'workflow.start', 'workflow.poll', 'workflow.stop'].map(name => {
    const read = name === 'list' || name === 'pending' || name === 'workflow.poll'
    return { name, kind: read ? 'read' : 'write', timeout_ms: 5000, idempotent: read }
  }),
}

export function createSubagents({ parent, currentCall, callService }) {
  const provider = {
    name: 'local', capabilities: { depthLimit: true, agentOptions: false, outputSchema: true },
    inheritsParentContext: false, prepareContinuable: true,
  }
  const call = (owner) => {
    const active = currentCall()
    if (!active?.agent || (owner && owner !== active.agent)) throw new Error('subagent service requires its active caller')
    return active
  }
  const invoke = (method, args = {}) => {
    const active = call()
    return callService({ serviceKey: SERVICE, method, args, sessionId: active.sessionId, depth: active.depth + 1 })
  }
  const requireTree = () => {
    const tree = call().subagentTree
    if (!tree) throw new Error('subagent tree is unavailable')
    return tree
  }
  const notices = async (method) => (await invoke(method)).notices.map(notice => ({
    id: notice.id,
    source: { kind: 'subagent' },
    content: [{ type: 'text', text: `Subagent report data: ${JSON.stringify(notice.text)}` }],
  }))
  const prepare = async () => { call().subagentTree = (await invoke('list')).tree }
  return {
    prepare,
    notices: () => notices('notices'),
    async deliverWait(exec, execute) {
      const active = call(exec.agent)
      let settled = false
      let timer
      let pending
      const poll = async () => {
        if (settled || exec.signal.aborted) return
        try {
          active.subagentInbox = await notices('pending')
          if (active.subagentInbox.length) await parent.parallel('agent/inbox/inserted', { agent: exec.agent })
        } catch (error) {
          if (!settled && !exec.signal.aborted) active.controller.abort(error)
        }
        if (!settled && !exec.signal.aborted) timer = setTimeout(() => { pending = poll() }, 150)
      }
      pending = poll()
      try { return await execute() } finally {
        settled = true
        clearTimeout(timer)
        await pending
      }
    },
    service: {
      getProvider: name => name === provider.name ? provider : undefined,
      resolveMaxDepth: value => value === 'provider-managed' ? undefined : value ?? 4,
      async startActivation(request) {
        const active = call(request.parent)
        if (request.delivery === 'caller') {
          if (request.agentOptions) throw new Error('workflow children inherit the current host model')
          const prompt = request.prompt.filter(block => block.type === 'text').map(block => block.text).join('\n')
          const output = request.outputSchema
            ? `${prompt}\n\nReturn only a JSON object conforming to this schema:\n${JSON.stringify(request.outputSchema)}`
            : prompt
          const row = (await invoke('workflow.start', {
            arguments: { prompt: output, description: request.label, ...(request.cwd ? { cwd: request.cwd } : {}) },
          })).result
          const result = (async () => {
            while (true) {
              request.signal?.throwIfAborted()
              const { status, reply } = await invoke('workflow.poll', { child: row.id })
              if (status === 'running') {
                await delay(150, undefined, { signal: request.signal })
                continue
              }
              const output = [{ type: 'text', text: reply }]
              if (status !== 'completed') return { output, stopReason: status === 'interrupted' ? 'cancelled' : 'error' }
              if (!request.outputSchema) return { output, stopReason: 'completed' }
              try {
                const { validateJsonSchemaValue } = await import('@deepseek-ai/dsh-tools')
                const structured = JSON.parse(reply)
                if (validateJsonSchemaValue(request.outputSchema, structured, '').length) return { output, stopReason: 'error' }
                return { output, stopReason: 'completed', structured }
              } catch {
                return { output, stopReason: 'error' }
              }
            }
          })()
          return { childId: row.id, result, dispose: () => invoke('workflow.stop', { child: row.id }).catch(() => {}) }
        }
        return (await invoke('start', {
          arguments: active.arguments,
          ...(request.maxDepth === undefined ? {} : { max_depth: request.maxDepth }),
        })).result
      },
      rootOf(owner) { call(owner); return { id: requireTree().root } },
      listDescendants: async root => {
        const tree = requireTree()
        if (root !== tree.root) throw new Error('subagent caller does not own this tree')
        return tree.entries
      },
      namesOf(owner) {
        call(owner)
        const tree = call().subagentTree
        return tree ? { self: tree.self, ...(tree.parent ? { parent: tree.parent } : {}) } : { self: 'lead' }
      },
      async resolveName(owner, name) {
        call(owner)
        const tree = requireTree()
        if (name === 'lead') return tree.root
        const row = tree.entries.find(entry => entry.path === name || (entry.owner === owner.id && entry.name === name))
        if (!row) throw new Error('agent is outside the caller tree')
        return row.id
      },
      async sendMessage(owner) {
        const active = call(owner)
        return (await invoke('message', { arguments: active.arguments })).result
      },
      interrupt(_target, { agent }) {
        const active = call(agent)
        active.defer(invoke('interrupt', { arguments: active.arguments }))
      },
    },
  }
}
import { setTimeout as delay } from 'node:timers/promises'
