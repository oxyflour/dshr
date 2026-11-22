import { setTimeout as delay } from 'node:timers/promises'

export const name = 'execution-probe'
export const inject = ['tools']

export function apply(ctx) {
  ctx.tools.register({
    name: 'execution_probe_delay',
    description: 'Wait until the execution signal cancels the probe.',
    parameters: { type: 'object', properties: {}, additionalProperties: false },
    output: {
      schema: { type: 'string' },
      render: (_args, value) => [{ type: 'text', text: value }],
    },
    timeoutMs: 80,
    async execute(_args, exec) {
      await delay(3_000, undefined, { signal: exec.signal })
      return 'probe finished without cooperative cancellation'
    },
  })
  ctx.tools.register({
    name: 'execution_probe_echo',
    description: 'Return the supplied test text.',
    parameters: { type: 'object', required: ['text'], properties: { text: { type: 'string' } } },
    output: {
      schema: { type: 'string' },
      render: (_args, value) => [{ type: 'text', text: value }],
    },
    async execute(args) { return args.text },
  })
}
