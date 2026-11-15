// 仅测试/验收用的 L2 driver（不是生产插件）：把**未修改官方包**的公开调用面暴露成
// 一个可 `tool.invoke` 的工具，供 `crates/compat-dsh-host/tests/official_packages.rs`
// 在真实 v2 sidecar 中取证。
//
// 覆盖：
// - `@deepseek-ai/dsh-working-directory` 的官方签名（`get(agent)` / `ensure(agent, signal)` /
//   `set(agent, path, signal)`）经 sidecar 的 workingDirectory shim → host.service.request；
// - `@deepseek-ai/dsh-storage-domain` 的未修改 `defineDomain`/`domainTable`（zod schema）
//   与 `DomainFacility.open/table/global/close`，经 storage-kv backend 代理 → 宿主
//   `StorageKvService`（Rust 介质）。
//
// 装载方式由测试通过 `DSHR_DSH_PLUGINS` 指定；生产 path 不会加载这个文件。

import { z } from 'zod'
import { defineDomain, domainTable } from '@deepseek-ai/dsh-storage-domain'

export const name = 'official-l2-driver'

export const inject = ['tools', 'workingDirectory', 'storage']

export function apply(ctx) {
  ctx.tools.register({
    name: 'l2_official_roundtrip',
    description:
      'acceptance driver: official dsh-storage-domain + dsh-working-directory L2 roundtrip (test only)',
    parameters: {
      type: 'object',
      additionalProperties: false,
      required: ['target'],
      properties: { target: { type: 'string' } },
    },
    async execute(args) {
      const evidence = { working_directory: {}, storage: {} }
      const fakeAgent = { id: 'official-acceptance' }

      // ---- working-directory：官方 dsh-working-directory 的调用签名 ----
      evidence.working_directory.get = await ctx.workingDirectory.get(fakeAgent)
      evidence.working_directory.set = await ctx.workingDirectory.set(
        fakeAgent,
        args.target,
        undefined,
      )
      evidence.working_directory.get_after_set =
        await ctx.workingDirectory.get(fakeAgent)
      evidence.working_directory.ensure = await ctx.workingDirectory.ensure(
        fakeAgent,
        undefined,
      )
      evidence.working_directory.recovered = Boolean(
        ctx.workingDirectory.lastRecovered,
      )

      // ---- storage：未修改官方 dsh-storage-domain + zod 记录 schema ----
      const spec = defineDomain({
        name: 'official_acceptance',
        version: 1,
        tables: {
          items: domainTable(z.object({ n: z.number().int() }).strict()),
        },
        global: {
          schema: z.object({ theme: z.string() }).strict(),
          initial: { theme: 'dark' },
        },
      })
      const facility = ctx.storage.form('domain')
      const domain = await facility.open(spec)
      evidence.storage.initial_global = domain.global.get()

      const table = domain.table('items')
      await table.put('a', { n: 1 })
      evidence.storage.get = table.get('a')
      await table.put('b', { n: 2 })
      evidence.storage.delete = await table.delete('a')
      evidence.storage.size_after_delete = table.size

      await domain.global.set({ theme: 'light' })
      evidence.storage.global = domain.global.get()

      // 错误面：官方 DomainFacility 对同名 domain 的重复 open 必须是 `already-open`。
      let duplicateOpenCode = null
      try {
        await facility.open(spec)
      } catch (error) {
        duplicateOpenCode = error?.code ?? null
      }
      evidence.storage.duplicate_open_code = duplicateOpenCode

      await domain.close()
      evidence.storage.closed = true
      return evidence
    },
  })
}
