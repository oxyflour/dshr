export const NATIVE_EXECUTOR_PLUGIN = '@deepseek-ai/dsh-tool-executor-native'
export const PTC_EXECUTOR_PLUGIN = '@deepseek-ai/dsh-tool-executor-ptc'
export const TIMEOUT_PLUGIN = '@deepseek-ai/dsh-tool-call-timeout-policy'
export const PROGRAM_TOOL = 'run_code'
export const PROGRAM_CAP = 'agent.program-tools'
export const PROGRAM_SERVICE = 'tool-program/v1'
export const PROGRAM_CATALOG = {
  service_key: PROGRAM_SERVICE,
  methods: ['begin', 'finish'].map(name => ({ name, kind: 'write', timeout_ms: 120_000, idempotent: false })),
}

export async function executeWithHooks(contexts, exec, execute) {
  const dispatch = async () => {
    try { return { value: await execute(), isError: false } } catch (error) {
      return { isError: true, thrown: error }
    }
  }
  const next = index => index >= contexts.length
    ? dispatch()
    : contexts[index].waterfall('tools/execute', exec, () => next(index + 1))
  return unpack(await next(0))
}

export async function executeWithOfficialExecutor(executor, exec) {
  const batch = await executor.execute({
    calls: [{ id: exec.callId, name: exec.name, arguments: JSON.stringify(exec.arguments) }],
    signal: exec.signal, agent: exec.agent, maxParallelToolCalls: 1,
  })
  const result = batch.results.find(entry => entry.callId === exec.callId)?.result
  if (!result) throw new Error('official executor did not settle the selected registration')
  return { ...unpack(result), contexts: batch.additionalContexts }
}

function unpack(result) {
  if (result.isError) {
    if (result.thrown) throw result.thrown
    const message = result.error?.message ?? 'official execution failed'
    throw Object.assign(new Error(message), { code: result.error?.info?.code })
  }
  return { value: result.value, content: result.content, meta: result.meta }
}
