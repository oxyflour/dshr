// Probe plugin for the L1 sidecar experiment.
// Shaped like a DSH tool plugin: a register() entry that receives a context with
// tool registration, not a framework import.
//
// Protocol shape used by the probe sidecar: a tool's run() is an async generator
// that yields strings (streamed chunks) and finally yields one terminal object
// ({ content }) or ({ aborted }).

export const name = 'probe.plugin'

export function register(ctx) {
  ctx.tool({
    name: 'probe_echo',
    description: 'Echo arguments back, streaming a few chunks first',
    presentation: 'generic',
    async *run(args, signal) {
      for (let i = 0; i < 3; i += 1) {
        if (signal.cancelled) {
          yield { aborted: 'cancel' }
          return
        }
        yield `chunk-${i}`
        await sleep(5)
      }
      yield { content: [{ type: 'json', value: { echo: args } }], value: { echo: args } }
    },
  })

  ctx.tool({
    name: 'probe_immediate',
    description: 'Settle without streaming or sleeping, to measure protocol-only cost',
    async *run(args, signal) {
      yield { content: [{ type: 'text', text: 'ok' }], value: { ok: true } }
    },
  })

  ctx.tool({
    name: 'probe_slow',
    description: 'Sleep two seconds so cancellation can be exercised',
    async *run(args, signal) {
      for (let i = 0; i < 10; i += 1) {
        if (signal.cancelled) {
          yield { aborted: 'cancel' }
          return
        }
        await sleep(200)
      }
      yield { content: [{ type: 'text', text: 'done' }], value: { done: true } }
    },
  })
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}
