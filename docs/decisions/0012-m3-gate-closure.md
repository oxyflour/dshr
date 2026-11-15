# ADR-0012 — M3 收尾口径修正（独立 review 后）

- 状态：Accepted
- 日期：2026-11-15
- 关联：[`../review-m3-implementation.md`](../review-m3-implementation.md)、
  [`0007-m3-scope.md`](0007-m3-scope.md)、[`0010-isolation-runtime-quota.md`](0010-isolation-runtime-quota.md)、
  [`0011-adc-v1-freeze.md`](0011-adc-v1-freeze.md)、[`../m3-gate-evidence.md`](../m3-gate-evidence.md)

## 背景

M3 实现完成后的独立 review（[`review-m3-implementation.md`](../review-m3-implementation.md)）
判定"按当前 gate 文本不接受"，并指出多处偏离**只写在状态/证据文档里、没有 ADR**：
官方包测试可在缺 fixture 时静默 PASS（P0）、WASM 未接宿主、内存配额可绕过、G2 的 SDK 不可分发、
L2 的 `sessionProjections` 缺失、工具并行只覆盖 sidecar 等。本 ADR 把 M3 收尾的**真实口径**
写进决策层，避免"文档声称 ≥ 实际交付"。

## 决定

1. **G1（ADC-v1 冻结）维持通过**。产品面板已接（`chat` 右栏 + `--selftest-adc-panel`）；窗口内
   layout/paint 与交互/事件/tool-card 属 ADR-0011 记录的 M4 降级，不阻塞 G1。
2. **G2 口径改为"内部作者 Preview"**：`dshr-plugin-api`/`dshr-plugin-sdk` 已有 semver 0.1.0，但
   **未发布到 registry**；模板/生成器通过"复制 SDK 到样例目录"实现无仓库路径的构建（P1-4 修复后）。
   在 SDK 正式发布、`--uninstall-plugin` 生命周期与真实签名安装完成前，**不得对外宣称
   "外部作者可发布"**；G2 的 gate 文本降为"内部作者可用 + Preview 标注"。
3. **G3 拆成两个可判定部分**：
   - **隔离**：WASM 主机（core module ABI）**必须接入宿主/产品路径**（`plugin-host` + profile loader +
     `--selftest-wasm` 安装→装载→调用→trap→卸载），并修资源上限（`StoreLimits` 的
     memories/tables/table_instances/instances + 负面测试）；external-process 退路保留。
     **Component Model/WIT canonical ABI 明确移 M4**（`cpp-v1.wit` 仍草案），M3 不再声称
     "WASM Component 已交付"。
   - **L2 兼容**：首批服务 = `working-directory` + `session-log` + `storage-kv`（`sessionProjections`
     在 P1-5 修复后作为第四个适配器）；官方包证据 = **显式 manual gate**（`tools/official-l2-gate.ps1`，
     依赖本机 DSH 抽取物），缺 fixture 时该 gate **失败**，cargo 默认测试中对应测试 `#[ignore]`，
     **不得把 SKIP 计为 pass**；官方 `dsh-storage` hub 是 Cordis Service class，不按普通 plugin 装载，
     由官方消费者 `dsh-storage-domain` 代表；官方工具 `execute` 受 `exec.agent` 债务限制，
     只验证"装载绿 + 方法往返 + 错误面"。
4. **测试诚实性硬规则**：任何依赖外部 fixture/环境（`.tmp/dsh-tools-all`、真实 DSH 安装）的测试，
   缺失时必须 `#[ignore]` 或失败；conformance registry 不得把这些 ID 标 `implemented`；
   `--quick` 只作为契约/文链门禁，gate 证据必须跑 `--full` 并提交 full matrix。
5. **工具并行口径**：`maxParallelToolCalls=10` **只对 sidecar 工具生效**（`SidecarHost::invoke_batch`）；
   native/MCP/`spawn_child` 仍串行；同 step 事件序为 `ToolCall×N → 终态×N`，UI/回放按 `call_id` 配对。
   文档与 gate evidence 不得写成"全部工具并行"。
6. **质量门槛**：P0/P1 修复后必须由独立 review 复核；复核通过前 M3 不标记完成。

## 后果

**好消息**：gate 口径与真实交付一致，不再有"文档声称 ≥ 实际"；WASM 产品链路与 L2 证据可判定；
测试诚实性规则可复用到 M4。

**坏消息**：M3 的对外表述要降级（G2 Preview、WASM Component 移 M4、L2 官方证据是 manual）；
Component/WIT、SDK 发布、uninstall、真实签名安装、官方 execute 闭环都进 M4 待办。

## 反悔条件

- SDK 正式发布（crates.io 或离线包渠道）+ 真实签名安装 + uninstall 完成 → G2 恢复"外部作者可发布"；
- WASM Component/WIT 主机落地 → G3 隔离回到"Component"字面口径；
- `exec.agent` 债务解除或官方提供无 Agent 的 execute 入口 → 官方包 execute 闭环纳入 gate；
- 若官方包 fixture 能在 CI 里合法 vendor（license/体积允许）→ 把 manual gate 升级为自动 gate。

## 验证

- 修复后重跑：`tools/official-l2-gate.ps1`（缺 fixture 时必须失败）、`cargo test --workspace`
  （official_packages 显示 ignored/failed，绝不 passed）、`--selftest-wasm`、`node --test`、
  `tools/conformance.mjs --full`；
- 独立 review 复核 P0/P1 关闭（第二次 review 报告落盘）；
- 以上未全部满足前，`docs/m3-status.md` 保持"实现完成、等待复核"，不标记 M3 完成。
