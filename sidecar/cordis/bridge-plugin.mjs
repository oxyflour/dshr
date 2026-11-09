// A plugin written against the real Cordis API (same package version DSH vendors).
// It declares `inject: ['bridge']`, so Cordis waits for that service before apply(),
// and registers its tool through the service — the shape a DSH host plugin has.

export const name = 'probe.cordis'
export const inject = ['bridge']

export function apply(ctx) {
  // Reversible effect: Cordis disposes it automatically when the plugin unloads.
  ctx.effect(() =>
    ctx.bridge.tool({
      name: 'cordis_echo',
      description: 'Echo through a real Cordis plugin (loaded by the Cordis runtime)',
      async *run(args, signal) {
        yield 'cordis-chunk-0'
        if (signal.cancelled) {
          yield { aborted: 'cancel' }
          return
        }
        yield 'cordis-chunk-1'
        yield { content: [{ type: 'json', value: { cordis: args } }] }
      },
    }),
  )
}
