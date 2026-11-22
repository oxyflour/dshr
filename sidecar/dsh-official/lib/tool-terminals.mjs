import { Context } from '@deepseek-ai/cordis'

export const TERMINAL_CAP = 'process.terminal'
const TERMINAL_SERVICE = 'tool-terminals/v1'

export const TERMINAL_CATALOG = {
  service_key: TERMINAL_SERVICE,
  methods: [
    { name: 'reserve', kind: 'write' },
    { name: 'confirm', kind: 'write' },
    { name: 'list', kind: 'read' },
    { name: 'allow', kind: 'read' },
    { name: 'closed', kind: 'write' },
  ].map(method => ({ ...method, timeout_ms: 5000, idempotent: method.kind === 'read' })),
}

const isPersistent = name => name === 'pwsh' || name === 'bash'

export async function createToolTerminals({ parent, currentCall, callService, findAgent, shellPath }) {
  const [{ TerminalSessionService }, { LocalSubprocessRuntime }, backend] = await Promise.all([
    import('@deepseek-ai/dsh-terminal'),
    import('@deepseek-ai/dsh-subprocess-local'),
    import('@deepseek-ai/dsh-terminal-bash'),
  ])
  const runtime = new Context()
  runtime.provide('agents', { get: findAgent })
  runtime.provide('workingDirectory', parent.get('workingDirectory'))
  // The Rust host admits the invocation and checks its in-workspace start directory.
  runtime.provide('sandboxPolicy', {
    defaultMode: 'danger-full-access',
    resolve: () => ({ mode: 'danger-full-access' }),
    overrideOf: () => undefined,
  })
  parent.effect(() => () => runtime.fiber.dispose(), 'dispose terminal runtime')
  await runtime.plugin(LocalSubprocessRuntime)
  await runtime.plugin(TerminalSessionService)
  await runtime.plugin({
    name: backend.name,
    inject: backend.inject,
    Config: backend.Config,
    apply: backend.apply,
  }, {
    shellDialect: process.platform === 'win32' ? 'pwsh' : 'bash',
    shellPath,
    scrollbackMaxBytes: 1024 * 1024,
    maxReadBytes: 128 * 1024,
  })
  const native = runtime.get('terminals')
  const terminals = new Map()
  const scoped = owner => {
    const call = currentCall()
    if (!call?.agent || (owner && owner !== call.agent)) {
      throw new Error('terminal requires the owning active session')
    }
    return call
  }
  const invoke = (method, args = {}) => {
    const call = scoped()
    return callService({
      serviceKey: TERMINAL_SERVICE,
      method,
      args,
      sessionId: call.sessionId,
      depth: call.depth + 1,
    })
  }
  const lookup = (owner, id) => {
    scoped(owner)
    const terminal = terminals.get(id)
    if (terminal?.owner !== owner) throw new Error('terminal is not active in this session')
    return terminal.nativeId
  }
  const allow = (id, operation, args = {}) => invoke('allow', { id, operation, ...args })

  return {
    async prepare(exec) {
      const call = scoped()
      call.terminalOwnerContext = runtime
      if (exec.name === 'terminal_read') {
        await allow(exec.arguments.sessionId, 'read')
        call.approvedTerminalRead = exec.arguments.sessionId
      }
      if (exec.name === 'terminal_list' || isPersistent(exec.name)) {
        call.terminalRows = (await invoke('list')).terminals
      }
    },
    service: {
      async spawn(owner, request, signal) {
        const call = scoped(owner)
        call.terminalOwnerContext = runtime
        const { id, cwd } = await invoke('reserve', {
          type: request.type,
          cwd: request.cwd,
          ...(request.name ? { name: request.name } : {}),
        })
        const terminal = await native.spawn(owner, { ...request, cwd }, signal)
        try {
          signal?.throwIfAborted()
          await invoke('confirm', { id })
        } catch (error) {
          await native.kill(owner, terminal.sessionId, 'terminal admission cancelled').catch(() => {})
          throw error
        }
        terminals.set(id, { owner, nativeId: terminal.sessionId })
        return { ...terminal, sessionId: id }
      },
      startSend(owner, id, request) {
        const call = scoped(owner)
        const nativeId = lookup(owner, id)
        const controller = new AbortController()
        const signal = AbortSignal.any([controller.signal, request.signal ?? call.controller.signal])
        let running
        const done = (async () => {
          await allow(id, 'send', {
            value: request.text ?? '',
            ...(isPersistent(call.tool) ? { origin_command: call.arguments.command } : {}),
          })
          signal.throwIfAborted()
          running = native.startSend(owner, nativeId, { ...request, signal })
          return running.done
        })()
        return {
          done,
          readOutput() {
            return running?.readOutput() ?? { delta: '', truncated: false }
          },
          cancel() {
            controller.abort()
            running?.cancel()
          },
        }
      },
      read(owner, id, request) {
        const call = scoped(owner)
        if (!isPersistent(call.tool) && call.approvedTerminalRead !== id) {
          throw new Error('terminal read was not authorized')
        }
        return native.read(owner, lookup(owner, id), request)
      },
      async signal(owner, id, signal) {
        const nativeId = lookup(owner, id)
        await allow(id, 'signal', { value: signal })
        return native.signal(owner, nativeId, signal)
      },
      async kill(owner, id, reason) {
        const nativeId = lookup(owner, id)
        await allow(id, 'close')
        const result = await native.kill(owner, nativeId, reason)
        await invoke('closed', { id })
        terminals.delete(id)
        return result
      },
      list(owner) {
        const call = scoped(owner)
        const views = new Map((call.terminalRows ?? []).map(row => [row.sessionId, row]))
        const actual = new Map(native.list(owner).map(row => [row.sessionId, row]))
        for (const [id, entry] of terminals) {
          if (entry.owner !== owner) continue
          const view = actual.get(entry.nativeId)
          if (view) views.set(id, { ...view, sessionId: id })
        }
        return [...views.values()]
      },
    },
  }
}
