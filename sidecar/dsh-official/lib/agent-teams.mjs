export const TEAM_CAP = 'agent.teams'
const SERVICE = 'tool-teams/v1'
export const TEAM_TOOLS = ['team_task_create', 'team_task_get', 'team_task_list', 'team_task_update']
export const TEAM_PLUGIN = '@deepseek-ai/dsh-tool-agent-team'
export const TEAM_CATALOG = {
  service_key: SERVICE,
  methods: ['list', 'create', 'update'].map(name => ({
    name, kind: name === 'list' ? 'read' : 'write', timeout_ms: 5000, idempotent: name === 'list',
  })),
}

export function createAgentTeams({ currentCall, callService }) {
  const active = (owner) => {
    const call = currentCall()
    if (!call?.agent || (owner && call.agent !== owner)) throw new Error('team requires its active agent')
    return call
  }
  const invoke = async (method, args) => {
    const { sessionId, depth } = active()
    return (await callService({ serviceKey: SERVICE, method, args, sessionId, depth: depth + 1 })).value
  }
  return {
    async prepare() {
      active().teamTasks = await invoke('list', {})
    },
    service: {
      isEnabled: owner => currentCall()?.agent === owner,
      listTasks(owner) { return active(owner).teamTasks.filter(task => task.status !== 'deleted') },
      getTask(owner, id) {
        const task = active(owner).teamTasks.find(task => task.id === id)
        if (!task) throw new Error('team task was not found')
        return task
      },
      createTask: owner => invoke('create', { arguments: active(owner).arguments }),
      updateTask: owner => invoke('update', { arguments: active(owner).arguments }),
    },
  }
}
