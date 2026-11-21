import { appendFileSync } from 'node:fs'

export const name = 'preset-counter'
export const inject = ['tools']

export function apply(ctx, { label, disposed }) {
  let count = 0
  if (disposed) ctx.effect(() => () => appendFileSync(disposed, label + '\n', 'utf8'))
  ctx.effect(() => ctx.tools.register({
    name: 'preset_counter',
    description: label,
    execute: () => ({ label, count: ++count }),
  }))
}
