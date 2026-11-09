# ADR-0006 — M1 收尾顺序：先 Native Plugin + Tool Registry，LLM 往后

- 状态：**Accepted**
- 日期：2026-11-09
- 关联：PLAN §8 M1 / M2、[`../m1-log.md`](../m1-log.md)

## 背景

M1 已跑完三个判死实验与端到端闭环，但**对照 PLAN 的 M1 退出条件仍有缺口**：

| M1 退出条件 | 现状 |
|---|---|
| 1. 统一 Session Log 回放 Native/JS 两类工具调用 | 回放侧完成（fixture + 闭环）；**Native 工具本身不存在**，只有 fixture 里的假数据 |
| 2. 杀 Sidecar 后会话完整、**Native Tool 仍可用** | 会话完整已验证；Native Tool 未验证（因为没有） |
| 3. 三个实验结论落盘且有复现脚本 | 完成 |
| 4. L1/L2 单位成本表 | L1 完成（p50 148 µs）；L2 属 M3 |

同时，M2 的第一项是"真正可交互的 Agent Loop（LLM provider ≥2、流式、重试、取消）"。

## 选项

| 选项 | 依赖 | 能否自证 | 价值 |
|---|---|---|---|
| **A. 接真实 LLM（DeepSeek）** | **需要 API key** | 依赖外部服务与额度 | 让对话从 mock 变真 |
| **B. Native Plugin + Tool Registry** | 无 | 可完全自证 | 补齐退出条件 1/2；M2 Tool Policy 的前置 |
| **C. 真实 DSH/Cordis 插件装载** | 需要 DSH 的 vendored Cordis 与 Loader 可运行 | 部分可自证（依赖 DSH 内部） | 验证兼容性主张，但风险最高 |
| **D. 继续 UI（流式渲染/多会话）** | 无 | 可自证 | 体验提升，但不解决退出条件 |

## 决定

**选 B，先做 Native Plugin + Tool Registry；C 排第二；A 等有 key 再启动；D 随之。**

理由：

1. **A 当前不可行**：本机环境没有任何 LLM API key（`DEEPSEEK/OPENAI/ANTHROPIC/API_KEY` 变量数为 0），
   DSH 的凭据在它自己的 credential store 里，不应挪用。没有 key 时"接 LLM"只能写出无法执行的代码。
2. **B 直接补齐 M1 退出条件 1/2 的缺口**：条件里明确要求"Native Tool 仍可用"，而目前注册表里
   根本没有 Native 工具。
3. **B 是 M2 Tool Policy 的地基**：审批/拒绝/审计都必须发生在 Registry 的 `authorize` 路径上；
   先把这条路径建好，后面接 Policy 只是填实现。
4. **C 放在 B 之后**：真实 DSH 插件需要 DSH 的 Loader 在 Node 里跑起来，失败面大；
   先用自写 probe 插件把 Registry 与 Native 路径跑通，再替换，能把"协议问题"和"DSH 内部问题"分开。

## 第一个 slice（本次已开始）

`crates/tool-runtime`：`ToolDefinition`（name/description/origin/schema/timeout）、
`ToolOrigin::{Native, Sidecar, Mcp}`、`ToolRegistry::{register, unregister, invalidate_generation, authorize}`、
`ToolPolicy`（AllowAll / DenyList）、JSON Schema 子集校验。9 个测试。

验收：重复注册拒绝、generation 失效只清理对应 runtime、policy 先于 schema、缺参/类型错/多余字段拒绝。

## 后续切片（按顺序）

1. `plugin-api` + `plugin-host`：一个 **Native Plugin** 注册 `probe_native` 工具与一个 Panel，
   卸载时清理注册（对应 M1 清单"一个 Rust Native Plugin 注册 Tool + Native Panel"）。
2. sidecar 注册接入 Registry：`tool.register` → `ToolRegistry.register(Sidecar{generation})`，
   调用统一走 `authorize`；Sidecar 重启触发 `invalidate_generation`。
3. chat 界面同时显示 native / sidecar 两种工具卡片，并验证"杀 sidecar 后 native 工具仍可用"。
4. 真实 DSH/Cordis 插件装载（替换 probe 插件）。
5. LLM provider（等 key）。

## 反悔条件

- 若拿到 API key 且产品优先级是"先看到真对话" → 可把 5 提到 3 之前；Registry 与原生工具仍应先于 LLM，
  因为 LLM 的工具调用最终也要走 `authorize`。
- 若真实 DSH 插件在 2 天内无法装载 → 保持 probe 插件作为 L1 的长期 conformance 样本，不阻塞主线。
