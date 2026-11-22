import { PROGRAM_TOOL } from './executors.mjs'
import { IMAGE_ATTACHMENT_CAP } from './image-attachments.mjs'

export const TOOL_OWNER = 'dsh-official'

export function createToolRegistry({ send, log, hostCaps, currentCall, boundRevision }) {
  const globalTools = new Map()
  const sessionTools = new Map()
  const revisionTools = new Map()
  const toolsByRequest = new Map()
  const toolsById = new Map()
  let toolRegistrationSeq = 0

  // ---------------------------------------------------------------------------
  // 官方插件的入口：ctx.tools.register(definition)。
  // 这里只做"定义 → 协议"与"调用 → execute"的翻译，不复制 DSH 的执行管线。
  // ---------------------------------------------------------------------------

  function removeTool(binding, notifyHost) {
    const { scope, sessionId, revision, definition, requestId, registrationId } = binding
    if (scope.get(definition.name) !== binding) return
    scope.delete(definition.name)
    if (sessionId && scope.size === 0) sessionTools.delete(sessionId)
    toolsByRequest.delete(requestId)
    if (registrationId) toolsById.delete(registrationId)
    if (notifyHost) {
      send(
        'tool.unregister',
        `tool-unreg-${++toolRegistrationSeq}`,
        {
          ...(registrationId ? {} : { register_request_id: requestId }),
          ...(revision ? { preset_revision: revision } : {}),
        },
        {
          plugin_id: TOOL_OWNER,
          ...(sessionId ? { session_id: sessionId } : {}),
          ...(registrationId ? { registration_id: registrationId } : {}),
        },
      )
    }
  }

  const toolsService = {
    register(definition, { sessionId, when } = {}, revision) {
      if (!definition?.name) throw new Error('tool definition without name')
      if (sessionId != null && (typeof sessionId !== 'string' || !sessionId)) {
        throw new Error('invalid tool registration session')
      }
      if (revision && sessionId) throw new Error('preset tools cannot declare a session scope')
      const revisionScope = revision && revisionTools.get(revision)
      if (revision && !revisionScope) throw new Error('preset revision is no longer active')
      let scope = revisionScope || globalTools
      if (sessionId) {
        scope = sessionTools.get(sessionId)
        if (!scope) {
          scope = new Map()
          sessionTools.set(sessionId, scope)
        }
      }
      if (scope.has(definition.name)) throw new Error(`duplicate tool ${definition.name} in scope`)
      const requestId = `tool-reg-${++toolRegistrationSeq}`
      const binding = {
        definition, sessionId, revision, scope, requestId,
        when: definition.name === 'office_render' ? selection => (
          hostCaps.has(IMAGE_ATTACHMENT_CAP) && selection.modelSelection.image_input === true
            && when(selection)
        ) : when,
      }
      scope.set(definition.name, binding)
      toolsByRequest.set(requestId, binding)
      send(
        'tool.register',
        requestId,
        {
          name: definition.name,
          ...(revision ? { preset_revision: revision } : {}),
          description: definition.description ?? '',
          input_schema: definition.parameters ?? { type: 'object' },
          output_schema: definition.output?.schema ?? null,
          presentation: 'generic',
          // 每个工具自己的超时（DSH `ToolDefinition.timeoutMs`；未声明则不设限）。
          // 宿主按它执行（Rust 侧 clamp 到 1s..=600s），不再统一写死 5s。
          ...(Number.isFinite(definition.timeoutMs) && definition.timeoutMs > 0
            ? { timeout_ms: definition.timeoutMs }
            : {}),
        },
        { plugin_id: TOOL_OWNER, ...(sessionId ? { session_id: sessionId } : {}) },
      )
      return () => removeTool(binding, true)
    },
    get(name, scope = {}, revision) {
      const active = currentCall()
      const sessionId = scope.sessionId ?? scope.session?.id
      if (active?.stepBindings && (!sessionId || active.sessionId === sessionId)) {
        return active.stepBindings[name]?.definition
      }
      return (sessionTools.get(sessionId)?.get(name)
        ?? revisionTools.get(revision ?? boundRevision(sessionId))?.get(name)
        ?? globalTools.get(name))?.definition
    },
    schemas(scope) {
      const active = currentCall()
      if (!active?.agent || (scope && scope !== active.agent)) return []
      const names = active.tool === PROGRAM_TOOL ? active.programDefinitions : active.stepBindings
      return Object.values(names ?? {}).map(({ definition }) => ({
        name: definition.name, description: definition.description ?? '', parameters: definition.parameters,
      }))
    },
    // 官方插件在注册阶段可能触到这些方法；装载期没有对应能力，先做成空实现。
    presentAs() {},
    restrict() {},
    guard() {},
    unregister(name, { sessionId } = {}, revision) {
      const scope = revision ? revisionTools.get(revision) : sessionId ? sessionTools.get(sessionId) : globalTools
      const binding = scope?.get(name)
      if (binding) removeTool(binding, true)
    },
  }

  return {
    service: toolsService,
    globalNames: () => globalTools.keys(),
    find: id => toolsById.get(id),
    openRevision(revision) {
      const scope = new Map()
      revisionTools.set(revision, scope)
      return () => {
        for (const binding of scope.values()) removeTool(binding, true)
        revisionTools.delete(revision)
      }
    },
    acknowledge(msg, generation) {
      const binding = toolsByRequest.get(msg.request_id)
      if (!binding || msg.runtime_generation !== generation) return
      if (msg.error) {
        removeTool(binding, false)
        log('error', 'could not register tool ' + binding.definition.name + ': ' + msg.error.message)
        return
      }
      const id = msg.registration_id ?? msg.payload?.registration_id
      if (id) {
        binding.registrationId = id
        toolsByRequest.delete(msg.request_id)
        toolsById.set(id, binding)
      }
    },
    revoke(msg, generation) {
      const binding = toolsById.get(msg.registration_id)
      if (binding?.sessionId && binding.sessionId === msg.session_id &&
          msg.plugin_id === TOOL_OWNER && msg.runtime_generation === generation) {
        removeTool(binding, false)
      }
    },
  }
}
