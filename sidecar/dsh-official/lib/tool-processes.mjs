import { resolve } from 'node:path'
import { setTimeout as delay } from 'node:timers/promises'

export const SEARCH_PROCESS_CAP = 'process.search'
export const SHELL_PROCESS_CAP = 'process.shell'
export const TOOL_PROCESS_SERVICE = 'tool-process/v1'
export const TOOL_PROCESS_CATALOG = {
  service_key: TOOL_PROCESS_SERVICE,
  methods: [
    { name: 'start', kind: 'write', timeout_ms: 5000, idempotent: false },
    { name: 'poll', kind: 'read', timeout_ms: 5000, idempotent: true },
    { name: 'read', kind: 'read', timeout_ms: 5000, idempotent: true },
    { name: 'release', kind: 'write', timeout_ms: 5000, idempotent: true },
  ],
}

const DEFAULT_SHELL_TIMEOUT = 30_000
const DEFAULT_SHELL_OUTPUT = 128 * 1024
const MAX_TIMEOUT = 120_000

const timeoutMs = value => Math.max(1, Math.min(MAX_TIMEOUT, Math.ceil(value ?? DEFAULT_SHELL_TIMEOUT)))

export function createToolProcesses({ currentCall, callService, workspace }) {
  const invoke = (method, args) => {
    const call = currentCall()
    if (!call?.sessionId) throw new Error('tool processes require an active session')
    return callService({
      serviceKey: TOOL_PROCESS_SERVICE,
      method,
      args,
      sessionId: call.sessionId,
      depth: call.depth + 1,
    })
  }

  async function readOutput(id, stream) {
    const chunks = []
    let offset = 0
    for (;;) {
      const page = await invoke('read', { id, stream, offset })
      if (page.text) chunks.push(page.text)
      if (page.end) return chunks.join('')
      if (page.next_offset <= offset) throw new Error('host returned a non-advancing output cursor')
      offset = page.next_offset
    }
  }

  async function run(spec, signal) {
    signal?.throwIfAborted()
    const { id } = await invoke('start', spec)
    try {
      let completion = await invoke('poll', { id })
      while (!completion.done) {
        await delay(25, undefined, { signal })
        completion = await invoke('poll', { id })
      }
      const [stdout, stderr] = await Promise.all([readOutput(id, 'stdout'), readOutput(id, 'stderr')])
      return {
        exitCode: completion.exit_code,
        signal: completion.signal,
        timedOut: completion.timed_out,
        aborted: completion.aborted,
        timeoutMs: spec.timeout_ms,
        stdout: { text: stdout, truncated: completion.stdout_truncated },
        stderr: { text: stderr, truncated: completion.stderr_truncated },
      }
    } finally {
      await invoke('release', { id }).catch(() => {})
    }
  }

  return {
    subprocess: {
      spawn({ argv, cwd, stdio, signal }) {
        const call = currentCall()
        if (call?.tool !== 'glob' && call?.tool !== 'grep') {
          throw new Error('the search subprocess seam requires glob or grep')
        }
        let completion
        const done = run({
          engine: 'ripgrep',
          cwd,
          // The executable is selected by the host independently of the upstream path.
          argv: argv.slice(1),
          timeout_ms: timeoutMs(call.timeoutMs),
          stdout_max: Math.min(stdio.stdout.maxBytes, 20_000_000),
          stderr_max: Math.min(stdio.stderr.maxBytes, 1024 * 1024),
        }, signal).then(result => {
          completion = result
          return { exitCode: result.exitCode, signal: result.signal }
        })
        const collected = stream => ({
          readFrom(offset) {
            const { text, truncated } = completion[stream]
            const bytes = Buffer.from(text, 'utf8')
            return {
              text: bytes.subarray(offset).toString('utf8'),
              lossy: truncated,
              nextOffset: bytes.length,
            }
          },
        })
        return { done, collected: { stdout: collected('stdout'), stderr: collected('stderr') } }
      },
    },
    shell: {
      resolve(spec) {
        const resolved = { ...spec, workdir: resolve(workspace, spec.workdir || '.'), timeoutMs: timeoutMs(spec.timeoutMs) }
        const call = currentCall()
        if (call?.jobReservation) {
          call.backgroundShell = { engine: call.tool, command: spec.command, cwd: resolved.workdir }
        }
        return resolved
      },
      async execute(spec) {
        const engine = currentCall()?.tool
        if (engine !== 'pwsh' && engine !== 'bash') {
          throw new Error('the shell seam requires pwsh or bash')
        }
        const result = run({
          engine,
          command: spec.command,
          cwd: spec.workdir,
          timeout_ms: timeoutMs(spec.timeoutMs),
          stdout_max: DEFAULT_SHELL_OUTPUT,
          stderr_max: DEFAULT_SHELL_OUTPUT,
        }, spec.signal)
        return { result: () => result }
      },
    },
    shellEnv: {
      collect() {
        return { DSH_SESSION_ID: currentCall()?.sessionId, DSH_WORKSPACE: workspace }
      },
    },
  }
}
