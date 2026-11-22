import { collectAgentStep } from './agent-step.mjs'
import { OFFICE_TOOLS } from './file-service.mjs'
import { SERVICE_KEYS } from './host-services.mjs'
import { GOAL_TOOLS, goalTurnSnapshot } from './tool-goals.mjs'
import { SESSION_QUERY_TOOLS } from './session-query.mjs'
import { SUBAGENT_TOOLS } from './subagents.mjs'
import { TEAM_TOOLS } from './agent-teams.mjs'
import { WORKFLOW_TOOLS } from './workflow-runtime.mjs'
import { SCHEDULE_TOOLS } from './schedules.mjs'
import { INSPECT_TOOLS } from './cordis-inspect.mjs'
import { PROGRAM_TOOL, PROGRAM_CAP, PROGRAM_SERVICE, executeWithHooks, executeWithOfficialExecutor } from './executors.mjs'

export const AGENT_TOOL_CONTEXT_CAP = 'agent.tool-context'

// 只有这些官方工具使用最小的会话与模型接口。
const AGENT_FACADE_TOOLS = new Set([
  'working_directory', 'todo_write', 'read_image', 'glob', 'grep', 'pwsh', 'bash', 'present',
  'skill', 'web_search', 'web_fetch', 'job_list', 'job_output', 'job_kill', ...OFFICE_TOOLS,
  'terminal_open', 'terminal_send', 'terminal_read', 'terminal_signal', 'terminal_list', 'terminal_close',
  ...GOAL_TOOLS,
  ...SESSION_QUERY_TOOLS,
  ...SUBAGENT_TOOLS,
  ...TEAM_TOOLS,
  ...WORKFLOW_TOOLS,
  ...SCHEDULE_TOOLS,
  ...INSPECT_TOOLS,
  PROGRAM_TOOL,
])

export function createToolExecution({
  send, log, callStore, toolRegistry, hostCaps, getHostServices, getRootContext, boundRevision,
  revisionContexts, executorScopes, bridges,
}) {
  const { jobBridges, terminalBridges, goalBridges, queryBridges, subagentBridges, teamBridges, scheduleBridges } = bridges
  const stepScopes = new Map()
  const inflight = new Map()
  const callContexts = new WeakMap()
  const agentActors = new Map()

  function agentFacade(sessionId) {
    const current = () => {
      const active = callStore.getStore()
      return active?.sessionId === sessionId ? active : undefined
    }
    const agent = agentActors.get(sessionId) ?? {
      id: sessionId,
      status: 'running',
      get ctx() { return current()?.terminalOwnerContext ?? current()?.pluginContext },
      inbox: {
        get nextStep() { return current()?.subagentInbox ?? [] },
        remove(id) {
          const notices = current()?.jobNotices
          const index = notices?.findIndex(notice => notice.id === id) ?? -1
          if (index >= 0) notices.splice(index, 1)
        },
      },
      inject(message) {
        if (message.source?.kind === 'tool-jobs') current()?.jobNotices?.push(message)
      },
      get options() { return current()?.model ?? {} },
      session: {
        id: sessionId,
        get header() {
          const call = current()
          return call?.queryHeader ?? { id: sessionId, ...(call?.goal?.subagent ? { origin: 'subagent' } : {}) }
        },
        seq: 0,
        snapshotEvents: () => goalTurnSnapshot(current()?.goal),
        surface: { nodes: [] },
        eventAt: () => undefined,
        append(type, data) {
          const append = current()?.appendSessionEvent
          if (!append) throw new Error('session event append requires an active tool')
          return append(type, data)
        },
        requestHeader: () => undefined,
        requestContext: () => current()?.model,
        deriveMessages: () => [],
      },
    }
    agentActors.delete(sessionId)
    agentActors.set(sessionId, agent)
    if (agentActors.size > 128) agentActors.delete(agentActors.keys().next().value)
    return agent
  }

  async function prepareAgentStep(msg) {
    const sessionId = msg.session_id
    const { step, model, user_text: userText, registrations, turn } = msg.payload
    if (!sessionId || !Number.isInteger(step) || step < 1 || typeof model?.provider !== 'string'
        || typeof model?.model !== 'string' || !registrations || Array.isArray(registrations)) {
      throw new Error('invalid agent step context')
    }
    const revision = boundRevision(sessionId)
    const contexts = [getRootContext(), revisionContexts.get(revision)].filter(Boolean)
    const bindings = Object.create(null)
    for (const [name, registrationId] of Object.entries(registrations)) {
      const binding = toolRegistry.find(registrationId)
      if (binding?.definition.name !== name || (binding.sessionId && binding.sessionId !== sessionId)
          || (binding.revision && binding.revision !== revision)) continue
      bindings[name] = binding
    }
    const agent = agentFacade(sessionId)
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(new Error('agent pre-step timed out')), 7000)
    const jobNotices = []
    try {
      return await callStore.run(
        { sessionId, depth: 0, controller, agent, turn, model, stepBindings: bindings, jobNotices },
        async () => {
          if (bindings.job_output) {
            const jobs = contexts.map(context => jobBridges.get(context)).reverse().find(Boolean)
            if (jobs) await jobs.notify()
          }
          if (SUBAGENT_TOOLS.some(name => bindings[name])) {
            const subagents = contexts.map(context => subagentBridges.get(context)).reverse().find(Boolean)
            if (subagents) jobNotices.push(...await subagents.notices())
          }
          if (SCHEDULE_TOOLS.some(name => bindings[name])) {
            const schedules = contexts.map(context => scheduleBridges.get(context)).reverse().find(Boolean)
            if (schedules) jobNotices.push(...await schedules.notices())
          }
          const result = await collectAgentStep({
            contexts, agent, signal: controller.signal, step, userText,
            modelSelection: model, toolSet: undefined, bindings, jobNotices,
            reportError: (name, error) => log('warn', `could not resolve availability of ${name}: ${error.message}`),
          })
          const selected = contexts.map(context => executorScopes.get(context)).filter(Boolean).at(-1)
          let programDefinitions
          if (selected?.ptc && bindings[PROGRAM_TOOL] && hostCaps.has(PROGRAM_CAP)) {
            programDefinitions = Object.fromEntries(Object.entries(bindings).filter(([name]) => name !== PROGRAM_TOOL))
            try {
              const sdk = selected.executor.renderSdk(Object.values(programDefinitions).map(binding => binding.definition))
              if (typeof sdk !== 'string' || Buffer.byteLength(sdk) > 64 * 1024) throw new Error('program SDK is too large')
              result.system_sections.push('Call run_code to use the scoped tools from a TypeScript program. The functions are available on the tools object.')
              result.system_sections.push(sdk)
              result.deferred_tools = Object.keys(programDefinitions)
            } catch (error) {
              log('warn', `program SDK unavailable: ${error.message}`)
              programDefinitions = undefined
              result.unavailable_tools.push(PROGRAM_TOOL)
              delete bindings[PROGRAM_TOOL]
            }
          } else if (bindings[PROGRAM_TOOL]) {
            result.unavailable_tools.push(PROGRAM_TOOL)
            delete bindings[PROGRAM_TOOL]
          }
          stepScopes.delete(sessionId)
          stepScopes.set(sessionId, { revision, bindings, programDefinitions })
          if (stepScopes.size > 128) stepScopes.delete(stepScopes.keys().next().value)
          return result
        },
      )
    } finally {
      clearTimeout(timer)
    }
  }

  function toolEffects(call) {
    const deferred = []
    let firstError
    call.defer = pending => {
      deferred.push(pending.catch(error => { firstError ??= error }))
      return pending
    }
    call.appendSessionEvent = (type, data) => call.defer(getHostServices()
      ? getHostServices().call({
          serviceKey: SERVICE_KEYS.sessionLog, method: 'events.append',
          args: { event_type: String(type), data: data ?? null },
          sessionId: call.sessionId, callId: call.callId, depth: call.depth,
        })
      : Promise.reject(new Error('session-log service is not negotiated (v2 host required)')))
    return async () => {
      await Promise.all(deferred)
      call.controller.signal.throwIfAborted()
      if (firstError) throw firstError
    }
  }

  function adaptersFor(hooks, name) {
    const from = map => hooks.map(context => map.get(context)).findLast(Boolean)
    return {
      jobs: from(jobBridges),
      terminals: (name.startsWith('terminal_') || ['pwsh', 'bash'].includes(name)) ? from(terminalBridges) : undefined,
      goals: GOAL_TOOLS.includes(name) ? from(goalBridges) : undefined,
      query: SESSION_QUERY_TOOLS.includes(name) ? from(queryBridges) : undefined,
      subagents: SUBAGENT_TOOLS.includes(name) || TEAM_TOOLS.includes(name) ? from(subagentBridges) : undefined,
      teams: TEAM_TOOLS.includes(name) ? from(teamBridges) : undefined,
    }
  }

  async function prepareAdapters(adapters, exec) {
    const { jobs, goals, query, subagents, teams, terminals } = adapters
    if (jobs) await jobs.prepare(exec)
    if (goals) await goals.prepare(exec)
    if (query) await query.prepare()
    if (subagents && ['list_agents', 'send_message', 'interrupt_agent', 'team_task_list', 'team_task_update'].includes(exec.name)) {
      await subagents.prepare()
    }
    if (teams && ['team_task_list', 'team_task_get'].includes(exec.name)) await teams.prepare()
    if (terminals) await terminals.prepare(exec)
  }

  async function settleAdapters(adapters) {
    if (adapters.jobs) await adapters.jobs.confirm()
    if (adapters.goals) await adapters.goals.commit()
  }

  async function executeNested(binding, args, exec) {
    const outer = callStore.getStore()
    if (!outer?.programDefinitions || outer.programDefinitions[binding.definition.name] !== binding || !exec.parent) {
      throw new Error('nested calls require an active run_code invocation')
    }
    const request = (method, fields) => getHostServices().call({
      serviceKey: PROGRAM_SERVICE, method, args: fields, sessionId: outer.sessionId, depth: outer.depth + 1,
      timeoutMs: 120_000,
    })
    const admitted = await request('begin', {
      name: binding.definition.name, registration: binding.registrationId, call_id: exec.callId, arguments: args,
    })
    const signal = AbortSignal.any([exec.signal, outer.controller.signal, AbortSignal.timeout(admitted.timeout_ms)])
    const child = {
      tool: binding.definition.name, sessionId: outer.sessionId, depth: outer.depth + 1,
      callId: exec.callId, toolRequestId: admitted.id, controller: { signal },
      turn: outer.turn, model: outer.model, agent: outer.agent, arguments: args,
      timeoutMs: admitted.timeout_ms, pluginContext: outer.pluginContext, stepBindings: outer.stepBindings,
    }
    callContexts.set(signal, child)
    const flush = toolEffects(child)
    const hooks = [getRootContext(), revisionContexts.get(boundRevision(child.sessionId))].filter(Boolean)
    const adapters = adaptersFor(hooks, child.tool)
    // The official executor owns the execution token and final tool result. Each
    // host callback from the body uses the independently approved child identity.
    const nested = {
      ...exec, signal,
      ...(AGENT_FACADE_TOOLS.has(child.tool) ? {} : { agent: undefined }),
    }
    let value
    let failure
    try {
      value = await callStore.run(child, async () => {
        await prepareAdapters(adapters, nested)
        const run = () => binding.definition.execute(args, nested)
        const result = adapters.subagents && child.tool === 'wait_agent'
          ? await adapters.subagents.deliverWait(nested, run)
          : await run()
        await flush()
        await settleAdapters(adapters)
        if (child.goal?.notice) exec.deferContext({ source: { kind: 'tool-goal' }, content: child.goal.notice })
        return result
      })
    } catch (error) { failure = error }
    try { await request('finish', { id: admitted.id }) } catch (error) { failure ??= error }
    if (failure) throw failure
    return value
  }

  async function runTool(definition, msg) {
    const controller = new AbortController()
    // 服务代理读取的调用上下文：session_id 只从信封来（spec §5），depth 用于 +1。
    const callContext = {
      callId: msg.call_id,
      toolRequestId: msg.request_id,
      controller,
      tool: definition.name,
      timeoutMs: definition.timeoutMs,
      sessionId: msg.session_id,
      turn: msg.payload?.turn,
      model: definition.name === 'read_image'
        ? { provider: 'dshr', model: 'active session model' }
        : msg.payload?.model,
      depth: Number.isInteger(msg.depth) ? msg.depth : 0,
      exactDefinition: definition,
    }
    const activeRevision = boundRevision(msg.session_id)
    const stepScope = stepScopes.get(msg.session_id)
    if (stepScope && stepScope.revision === activeRevision) {
      callContext.stepBindings = Object.fromEntries(Object.entries(stepScope.bindings)
        .filter(([, binding]) => toolRegistry.find(binding.registrationId) === binding))
      if (definition.name === PROGRAM_TOOL && stepScope.programDefinitions) {
        callContext.programDefinitions = Object.fromEntries(Object.entries(stepScope.programDefinitions)
          .filter(([, binding]) => toolRegistry.find(binding.registrationId) === binding))
      }
    }
    inflight.set(msg.request_id, callContext)
    callContexts.set(controller.signal, callContext)
    const started = Date.now()
    try {
      // M4 W6：给官方工具一个最小 `exec.agent.session.append(type, data)`（todo 等需要）。
      // 写入走 session-log/v1 events.append；官方代码不 await 时，这里在 execute 结束后
      // 统一 flush 并抛出首个错误，避免"写入丢失但工具显示成功"。
      const flush = toolEffects(callContext)
      const exec = {
        signal: controller.signal,
        callId: msg.call_id,
        rootCallId: msg.request_id,
        name: definition.name,
        arguments: msg.payload?.arguments ?? {},
        // 只给明确支持最小 agent facade 的工具；fs 等工具在 agent 存在时会走
        // `ctx.workingDirectory.ensure` 分支，但它们没有 inject workingDirectory，
        // 这会让原本无 agent 的写路径回归。逐工具放开，避免"给了 agent 反而坏"。
        ...(AGENT_FACADE_TOOLS.has(definition.name)
          ? { agent: agentFacade(msg.session_id) }
          : {}),
      }
      callContext.agent = exec.agent
      callContext.arguments = exec.arguments
      const argumentsValue = exec.arguments
      if (exec.name === PROGRAM_TOOL && (argumentsValue.sandbox_permissions !== undefined || argumentsValue.justification !== undefined)) {
        throw new Error('the program process uses the host read-only sandbox')
      }
      const hooks = getHostServices() && hostCaps.has(AGENT_TOOL_CONTEXT_CAP)
        ? [getRootContext(), revisionContexts.get(boundRevision(msg.session_id))].filter(Boolean)
        : []
      callContext.pluginContext = hooks.at(-1)
      const executionContext = hooks.findLast(context => executorScopes.has(context))
      const selectedExecutor = executionContext && executorScopes.get(executionContext)
      const adapters = adaptersFor(hooks, exec.name)
      const notifyToolResult = async (result, skip) => {
        for (const context of hooks) {
          if (context !== skip) await context.parallel('tools/result', exec, result)
        }
      }
      // AsyncLocalStorage：ctx.workingDirectory/ctx.storage 的代理在官方 execute 的
      // 任意 await 点都能拿到同一个 call_id/session_id（tool.cancel 据此取消服务调用）。
      const executed = await callStore.run(callContext, async () => {
        await prepareAdapters(adapters, exec)
        let result
        const activateExecutor = selectedExecutor && (!selectedExecutor.ptc || exec.name === PROGRAM_TOOL)
        try {
          const run = () => activateExecutor
            ? executeWithOfficialExecutor(selectedExecutor.executor, exec)
            : executeWithHooks(hooks, exec, () => definition.execute(argumentsValue, exec))
          result = adapters.subagents && exec.name === 'wait_agent' ? await adapters.subagents.deliverWait(exec, run) : await run()
        } catch (error) {
          await notifyToolResult({ isError: true, error }, activateExecutor ? executionContext : undefined)
          throw error
        }
        controller.signal.throwIfAborted()
        await notifyToolResult({ isError: false, ...result }, activateExecutor ? executionContext : undefined)
        return result
      })
      const { value } = executed
      await flush()
      await callStore.run(callContext, () => settleAdapters(adapters))

      let content
      try {
        content = executed.content ?? definition.output?.render?.(msg.payload?.arguments ?? {}, value)
      } catch {
        content = undefined
      }
      if (!Array.isArray(content) || content.length === 0) {
        content = [{ type: 'json', value }]
      }
      if (callContext.goal?.notice) content = [...content, ...callContext.goal.notice]
      for (const context of executed.contexts ?? []) {
        if (context.source?.kind === 'tool-goal' || context.source?.kind === 'ptc-mode') {
          content = [...content, ...context.content]
        }
      }
      let meta = null
      try {
        meta = executed.meta ?? definition.output?.presentationMeta?.(msg.payload?.arguments ?? {}, value) ?? null
      } catch {
        meta = null
      }

      send(
        'tool.result',
        msg.request_id,
        { call_id: msg.call_id, is_error: false, content, value, meta },
        { call_id: msg.call_id, plugin_id: 'dsh-official' },
      )
      send('log.emit', `call-${msg.call_id}`, {
        level: 'debug',
        message: `dsh tool ${definition.name} done in ${Date.now() - started}ms`,
      })
    } catch (error) {
      const code = controller.signal.aborted
        ? 'E_CANCELLED'
        : error?.code === 'TOOL_TIMEOUT' ? 'E_TOOL_TIMEOUT'
        : typeof error?.code === 'string' ? error.code : 'E_TOOL'
      send(
        'tool.result',
        msg.request_id,
        {
          call_id: msg.call_id,
          is_error: true,
          content: [{ type: 'text', text: String(error?.message ?? error) }],
          value: null,
          meta: null,
        },
        {
          call_id: msg.call_id,
          error: {
            code,
            message: String(error?.message ?? error),
            retryable: false,
          },
        },
      )
    } finally {
      inflight.delete(msg.request_id)
    }
  }

  function cancel(msg) {
    const requestId = msg.payload?.tool_request_id
    for (const call of inflight.values()) {
      if (call.callId !== msg.call_id || (requestId && requestId !== call.toolRequestId)) continue
      inflight.delete(call.toolRequestId)
      call.controller.abort()
      getHostServices()?.cancelForCall(call.callId, call.toolRequestId)
    }
  }

  return {
    currentCall: signal => callContexts.get(signal) ?? callStore.getStore(),
    findAgent: id => agentActors.get(id),
    prepareAgentStep,
    executeNested,
    runTool,
    cancel,
  }
}
