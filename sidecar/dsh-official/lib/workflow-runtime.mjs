export const WORKFLOW_TOOLS = ['workflow', 'ralph']
export const WORKFLOW_PLUGIN = '@deepseek-ai/dsh-tool-workflow'
export const RALPH_PLUGIN = '@deepseek-ai/dsh-tool-ralph'

export async function mountReadOnlyPtc(root, workspaceRoot) {
  const [{ default: NodePtcRuntime }, { default: LocalSandboxProvider }, { LocalSubprocessRuntime }] =
    await Promise.all([
      import('@deepseek-ai/dsh-ptc-runtime-node'), import('@deepseek-ai/dsh-sandbox-local'),
      import('@deepseek-ai/dsh-subprocess-local'),
    ])
  const policy = {
    defaultMode: 'read-only',
    resolve: ({ session } = {}) => ({ mode: 'read-only', workspaceRoot, ...(session ? { sessionId: session.id } : {}) }),
    overrideOf: () => undefined,
  }
  const runtime = new Context()
  runtime.provide('sandboxPolicy', policy)
  runtime.provide('fs', root.get('fs'))
  root.effect(() => () => runtime.fiber.dispose(), 'dispose PTC runtime')
  await runtime.plugin(LocalSubprocessRuntime)
  await runtime.plugin(LocalSandboxProvider, LocalSandboxProvider.Config({}))
  const bootstrapPath = fileURLToPath(new URL('./process.js', import.meta.resolve('@deepseek-ai/dsh-ptc-runtime-node')))
  await runtime.plugin(NodePtcRuntime, NodePtcRuntime.Config({
    launch: { kind: 'node-script', executable: process.execPath, bootstrapPath },
    timeoutMs: 60_000, maxTimeoutMs: 120_000, maxOldGenerationSizeMb: 128,
    maxOutputBytes: 256 * 1024, maxMessageBytes: 2 * 1024 * 1024, maxPendingCalls: 32,
  }))
  root.provide('sandboxPolicy', policy)
  root.provide('ptcRuntime', runtime.get('ptcRuntime'))
}

export async function mountWorkflowEngine(root, workspace) {
  await mountReadOnlyPtc(root, workspace)
  const { default: Engine } = await import('@deepseek-ai/dsh-workflow-ptc')
  await root.plugin(Engine, Engine.Config({ provider: 'local', maxConcurrentAgents: 4, maxTotalAgents: 64, maxItemsPerCall: 128 }))
}
import { Context } from '@deepseek-ai/cordis'
import { fileURLToPath } from 'node:url'
